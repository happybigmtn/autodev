use super::*;
#[cfg(target_os = "linux")]
use crate::backend_process::{
    linux_process_start_time_ticks, WorkerPidLeaseRecord, WORKER_PID_LEASE_VERSION,
};
use crate::backend_process::{retire_worker_pid_lease, worker_pid_lease_target};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LaneRepoProgress {
    None,
    Dirty(String),
    NewCommits,
    NewCommitsWithDirty(String),
}

pub(crate) fn git_commit_exists(repo_root: &Path, commit: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Return the first incoming lane commit that uses the host-reserved durable
/// verification-receipt trailer namespace. Receipt footers are minted only by
/// canonical host closeout commits; accepting one from a lane would let worker
/// output manufacture future completion authority.
pub(crate) fn lane_range_reserved_verification_receipt_commit(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
) -> Result<Option<String>> {
    let range = format!("{base_commit}..{head_ref}");
    let commits = git_stdout(repo_root, ["rev-list", "--reverse", &range])
        .with_context(|| format!("failed to enumerate incoming lane range {range}"))?;
    for commit in commits
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let body = git_stdout(repo_root, ["show", "-s", "--format=%B", commit])
            .with_context(|| format!("failed to inspect incoming lane commit {commit}"))?;
        if commit_message_has_reserved_verification_receipt_footer(&body) {
            return Ok(Some(commit.to_string()));
        }
    }
    Ok(None)
}

pub(crate) fn git_path(repo_root: &Path, path: &str) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-path", path])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rendered = String::from_utf8(output.stdout).ok()?;
    let rendered = rendered.trim();
    if rendered.is_empty() {
        return None;
    }
    let path = PathBuf::from(rendered);
    Some(if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    })
}

pub(crate) fn repair_stale_git_index_lock(
    repo_root: &Path,
    parallel_logger: &ParallelEventLogger,
    context: &str,
) -> Result<()> {
    let Some(path) = git_path(repo_root, "index.lock").filter(|path| path.exists()) else {
        return Ok(());
    };
    let metadata =
        fs::metadata(&path).with_context(|| format!("failed to stat {}", path.display()))?;
    let age = metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok());
    let safe_to_remove =
        metadata.len() == 0 && age.is_some_and(|age| age >= STALE_GIT_INDEX_LOCK_GRACE);
    if !safe_to_remove {
        bail!(
            "canonical repo has an active git index lock at {}; size={} age_secs={} context={}; remove it only after confirming no git process is using it",
            path.display(),
            metadata.len(),
            age.map(|age| age.as_secs().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            context,
        );
    }
    fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    parallel_logger.warn(format!(
        "repair: removed stale canonical git index lock {} ({context})",
        path.display()
    ));
    Ok(())
}

pub(crate) fn clone_loop_lane_repo(
    repo_root: &Path,
    target_branch: &str,
    lane_repo_root: &Path,
) -> Result<()> {
    let output = Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg("--local")
        // Copy objects instead of hardlinking them. `--local` alone hardlinks
        // `.git/objects`, which fails with "Invalid cross-device link" when the lane
        // root lives on a different filesystem than the canonical repo (e.g. lanes
        // relocated to a roomy attached volume via AUTO_RUN_ROOT while the repo is on
        // root). Copying is slightly slower but works on the same device and across
        // devices alike.
        .arg("--no-hardlinks")
        .arg("--branch")
        .arg(target_branch)
        .arg("--single-branch")
        .arg(repo_root)
        .arg(lane_repo_root)
        .output()
        .with_context(|| {
            format!(
                "failed to clone loop lane repo from {} to {}",
                repo_root.display(),
                lane_repo_root.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "git clone failed for loop lane {}: {}",
            lane_repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let remotes = git_stdout(lane_repo_root, ["remote"]).unwrap_or_default();
    if remotes.lines().any(|remote| remote.trim() == "origin") {
        run_git(lane_repo_root, ["remote", "rename", "origin", "canonical"])?;
    }

    share_lean_dependency_cache(repo_root, lane_repo_root);
    share_configured_lane_paths(repo_root, lane_repo_root);
    Ok(())
}

/// Best-effort: symlink operator-configured gitignored build artifacts from the
/// canonical repo into a freshly-cloned lane. Lanes are `git clone --local` of
/// the canonical repo, so gitignored paths (build oracles, large fixture blobs,
/// downloaded caches) never come across, and lanes that need them run degraded
/// or rebuild them. The operator lists one repo-relative path per line in
/// `<repo>/.auto/lane-shared-paths` (e.g. `target/oracle` for the Ludii oracle
/// jar that differential lanes verify against). Each listed path that exists in
/// the canonical repo and is absent in the lane is symlinked in (parent dirs
/// created). Blank/`#`-comment lines, absolute paths, and `..` escapes are
/// ignored. Any failure is non-fatal: the lane simply runs without the artifact.
pub(crate) fn share_configured_lane_paths(repo_root: &Path, lane_repo_root: &Path) {
    let config = repo_root.join(".auto").join("lane-shared-paths");
    let Ok(contents) = fs::read_to_string(&config) else {
        return; // no operator config; nothing to share
    };
    for raw in contents.lines() {
        let rel = raw.trim();
        if rel.is_empty() || rel.starts_with('#') {
            continue;
        }
        // Only share repo-relative paths; reject absolute paths and parent escapes.
        if rel.starts_with('/') || rel.split('/').any(|seg| seg == "..") {
            continue;
        }
        let source = repo_root.join(rel);
        if !source.exists() {
            continue; // configured but not present in the canonical repo
        }
        let dest = lane_repo_root.join(rel);
        if dest.exists() {
            continue; // lane already has it; leave it untouched
        }
        if let Some(parent) = dest.parent() {
            if fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        let _ = std::os::unix::fs::symlink(&source, &dest);
    }
}

/// Best-effort: let a freshly-cloned Lean lane reuse the canonical repo's prebuilt
/// dependency cache (`.lake/packages`, which holds Mathlib and other Lake deps).
/// That directory is gitignored, so `git clone --local` never brings it across and
/// each lane would otherwise rebuild Mathlib from scratch — many minutes and gigabytes
/// per lane. We symlink only the read-only package cache; each lane keeps its own
/// `.lake/build`, so lanes never contend over compiled artifacts. Any failure here is
/// non-fatal: the lane simply rebuilds, which is slow but correct.
pub(crate) fn share_lean_dependency_cache(repo_root: &Path, lane_repo_root: &Path) {
    let canonical_packages = repo_root.join(".lake").join("packages");
    if !canonical_packages.is_dir() {
        return; // not a Lean repo with a prebuilt Lake cache
    }
    let lane_lake = lane_repo_root.join(".lake");
    let lane_packages = lane_lake.join("packages");
    if lane_packages.exists() {
        return; // lane already has a package cache; leave it untouched
    }
    if fs::create_dir_all(&lane_lake).is_err() {
        return;
    }
    let _ = std::os::unix::fs::symlink(&canonical_packages, &lane_packages);
}

pub(crate) fn path_modified_elapsed(path: &Path) -> Result<Option<Duration>> {
    if !path.exists() {
        return Ok(None);
    }
    let modified = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .modified()
        .with_context(|| format!("failed to read mtime for {}", path.display()))?;
    Ok(Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or_else(|_| Duration::from_secs(0)),
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkerPidIdentity {
    pid: u32,
    #[cfg(target_os = "linux")]
    linux_start_time_ticks: u64,
    #[cfg(target_os = "linux")]
    lane_root: String,
}

impl WorkerPidIdentity {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkerPidState {
    Absent,
    Live(WorkerPidIdentity),
    Stale,
}

fn inspect_worker_pid(path: &Path) -> Result<WorkerPidState> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WorkerPidState::Absent);
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(WorkerPidState::Absent);
    }

    #[cfg(target_os = "linux")]
    if trimmed.starts_with('{') {
        let record: WorkerPidLeaseRecord = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid process identity in {}", path.display()))?;
        if record.version != WORKER_PID_LEASE_VERSION {
            bail!(
                "unsupported worker pid lease version {} in {}",
                record.version,
                path.display()
            );
        }
        let parent = path
            .parent()
            .with_context(|| format!("{} has no lane root", path.display()))?;
        let expected_lane_root = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        if record.lane_root != expected_lane_root.display().to_string() {
            bail!(
                "worker pid lease lane identity mismatch in {}: recorded `{}`, expected `{}`",
                path.display(),
                record.lane_root,
                expected_lane_root.display()
            );
        }
        let current_start_time = linux_process_start_time_ticks(record.pid)?;
        return Ok(match current_start_time {
            Some(start_time) if start_time == record.linux_start_time_ticks => {
                WorkerPidState::Live(WorkerPidIdentity {
                    pid: record.pid,
                    linux_start_time_ticks: record.linux_start_time_ticks,
                    lane_root: record.lane_root,
                })
            }
            _ => WorkerPidState::Stale,
        });
    }

    let pid = trimmed
        .parse::<u32>()
        .with_context(|| format!("invalid pid in {}", path.display()))?;
    #[cfg(not(target_os = "linux"))]
    {
        return Ok(if worker_pid_is_alive(pid)? {
            WorkerPidState::Live(WorkerPidIdentity { pid })
        } else {
            WorkerPidState::Stale
        });
    }
    #[cfg(target_os = "linux")]
    {
        if worker_pid_is_alive(pid)? {
            bail!(
                "legacy pid-only worker lease {} names live pid {pid}, but process identity cannot be proven; retire or replace the legacy lease before resuming",
                path.display()
            );
        }
        Ok(WorkerPidState::Stale)
    }
}

pub(crate) fn read_worker_pid(path: &Path) -> Result<Option<u32>> {
    Ok(match inspect_worker_pid(path)? {
        WorkerPidState::Live(identity) => Some(identity.pid()),
        WorkerPidState::Absent | WorkerPidState::Stale => None,
    })
}

pub(crate) fn read_worker_pid_identity(path: &Path) -> Result<Option<WorkerPidIdentity>> {
    Ok(match inspect_worker_pid(path)? {
        WorkerPidState::Live(identity) => Some(identity),
        WorkerPidState::Absent | WorkerPidState::Stale => None,
    })
}

pub(crate) fn clear_stale_worker_pid(path: &Path) -> Result<()> {
    let lease_path = worker_pid_lease_target(path)?;
    match inspect_worker_pid(path)? {
        WorkerPidState::Absent | WorkerPidState::Live(_) => return Ok(()),
        WorkerPidState::Stale => {
            if let Some(lease_path) = lease_path {
                retire_worker_pid_lease(&lease_path)?;
            }
        }
    }
    // Legacy regular worker.pid files are intentionally left for the next
    // atomic lease publication to replace. Unlinking the shared path after a
    // liveness check could delete a newer owner that won the intervening race.
    Ok(())
}

pub(crate) fn lane_repo_process_pids(lane_repo_root: &Path) -> Result<Vec<u32>> {
    if !lane_repo_root.exists() {
        return Ok(Vec::new());
    }
    let output = Command::new("ps")
        .args(["-eo", "pid=,command="])
        .output()
        .context("failed to inspect process table")?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(parse_lane_repo_process_pids(
        lane_repo_root,
        &String::from_utf8_lossy(&output.stdout),
    ))
}

pub(crate) fn parse_lane_repo_process_pids(lane_repo_root: &Path, ps_output: &str) -> Vec<u32> {
    let needle = lane_repo_root.display().to_string();
    ps_output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let (pid, command) = trimmed.split_once(char::is_whitespace)?;
            if !command.contains(&needle) {
                return None;
            }
            let command = command.trim_start();
            let executable = command
                .split_whitespace()
                .next()
                .and_then(|word| Path::new(word).file_name())
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if matches!(executable, "rg" | "grep")
                || command.starts_with("auto parallel status")
                || command.contains(" auto parallel status")
                || command.contains("/auto parallel status")
            {
                return None;
            }
            pid.parse::<u32>().ok()
        })
        .collect()
}

pub(crate) fn infer_lane_base_commit(lane_repo_root: &Path, target_branch: &str) -> Result<String> {
    let remote_name = lane_remote_name(lane_repo_root)?;
    run_git(
        lane_repo_root,
        ["fetch", "--quiet", &remote_name, target_branch],
    )?;
    let base_commit = git_stdout(lane_repo_root, ["merge-base", "HEAD", "FETCH_HEAD"])?;
    let base_commit = base_commit.trim();
    if base_commit.is_empty() {
        bail!(
            "failed to infer base commit for resumable lane repo {}",
            lane_repo_root.display()
        );
    }
    Ok(base_commit.to_string())
}

pub(crate) fn lane_remote_name(lane_repo_root: &Path) -> Result<String> {
    let remotes = git_stdout(lane_repo_root, ["remote"])?;
    for remote in remotes.lines().map(str::trim) {
        if remote == "canonical" {
            return Ok("canonical".to_string());
        }
    }
    for remote in remotes.lines().map(str::trim) {
        if remote == "origin" {
            return Ok("origin".to_string());
        }
    }
    bail!(
        "lane repo {} has no `canonical` or `origin` remote",
        lane_repo_root.display()
    );
}

/// Prove that deleting the run's lane repositories cannot discard task work.
/// Every lane must either be an untouched placeholder or a clean clone whose
/// commits are already reachable from, or patch-equivalent to, canonical HEAD.
/// Any unknown shape, live worker, host-pending marker, recovery state, dirty
/// file, foreign remote, fetch failure, or unlanded patch fails closed.
pub(crate) fn parallel_lane_repos_are_disposable(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
) -> Result<bool> {
    let lanes_root = run_root.join("lanes");
    let lanes_metadata = match fs::symlink_metadata(&lanes_root) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to inspect {}", lanes_root.display()));
        }
    };
    if lanes_metadata.file_type().is_symlink() || !lanes_metadata.is_dir() {
        return Ok(false);
    }
    let canonical_repo = fs::canonicalize(repo_root)
        .with_context(|| format!("failed to canonicalize {}", repo_root.display()))?;

    for entry in fs::read_dir(&lanes_root)
        .with_context(|| format!("failed to read {}", lanes_root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", lanes_root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if !file_type.is_dir() || parse_lane_index(&entry.file_name().to_string_lossy()).is_none() {
            return Ok(false);
        }
        let lane_root = entry.path();
        if lane_root
            .join(LANE_HOST_PENDING_FILE)
            .try_exists()
            .with_context(|| format!("failed to inspect {}", lane_root.display()))?
            || read_worker_pid_identity(&lane_root.join("worker.pid"))?.is_some()
        {
            return Ok(false);
        }
        let lane_repo_root = lane_root.join("repo");
        let repo_metadata = match fs::symlink_metadata(&lane_repo_root) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to inspect {}", lane_repo_root.display()));
            }
        };
        if repo_metadata.file_type().is_symlink() || !repo_metadata.is_dir() {
            return Ok(false);
        }
        let git_dir = lane_repo_root.join(".git");
        let git_metadata = match fs::symlink_metadata(&git_dir) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to inspect {}", git_dir.display()));
            }
        };
        if git_metadata.file_type().is_symlink() || !git_metadata.is_dir() {
            return Ok(false);
        }
        if lane_repo_has_active_cherry_pick(&lane_repo_root)
            || lane_repo_has_rebase_recovery(&lane_repo_root)
            || !git_stdout(&lane_repo_root, ["status", "--short"])?
                .trim()
                .is_empty()
        {
            return Ok(false);
        }

        let remote_url = git_stdout(&lane_repo_root, ["remote", "get-url", "canonical"])?;
        let remote_path = PathBuf::from(remote_url.trim());
        let Ok(remote_repo) = fs::canonicalize(&remote_path) else {
            return Ok(false);
        };
        if remote_repo != canonical_repo {
            return Ok(false);
        }
        run_git(
            &lane_repo_root,
            ["fetch", "--quiet", "canonical", target_branch],
        )?;
        let unlanded = git_stdout(&lane_repo_root, ["cherry", "FETCH_HEAD", "HEAD"])?;
        if unlanded
            .lines()
            .map(str::trim)
            .any(|line| line.starts_with('+'))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn worker_pid_is_alive(pid: u32) -> Result<bool> {
    let status = Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .context("failed to run kill -0")?;
    Ok(status.success())
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkerProcessRow {
    pid: u32,
    parent_pid: u32,
    state: char,
    command: String,
}

#[cfg(target_os = "linux")]
fn parse_worker_process_table(table: &str) -> Vec<WorkerProcessRow> {
    table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let parent_pid = fields.next()?.parse::<u32>().ok()?;
            let state = fields.next()?.chars().next()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            Some(WorkerProcessRow {
                pid,
                parent_pid,
                state,
                command,
            })
        })
        .collect()
}

/// Agent backends keep a small set of MCP/code-mode children alive for their
/// whole session. Those plumbing subtrees do not mean task work is active.
#[cfg(target_os = "linux")]
fn is_agent_plumbing_process(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    command.contains("codex-code-mode-host")
        || command.contains("@playwright/mcp")
        || command.contains("playwright-mcp")
        || command.contains("mcp_server")
        || command.contains("mcp-server")
        || command.contains(" mcp ")
        || command.ends_with(" mcp")
}

#[cfg(target_os = "linux")]
pub(crate) fn process_table_has_active_worker_verification(worker_pid: u32, table: &str) -> bool {
    parse_worker_process_table(table).into_iter().any(|row| {
        row.parent_pid == worker_pid
            && row.state != 'Z'
            && !is_agent_plumbing_process(&row.command)
            && is_verification_process(&row.command)
    })
}

#[cfg(target_os = "linux")]
fn is_verification_process(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    [
        "run-task-verification",
        "scripts/ci/",
        "cargo test",
        "cargo nextest",
        "cargo check",
        "cargo clippy",
        "cargo build",
        "npm test",
        "npm run test",
        "pnpm test",
        "yarn test",
        "bun test",
        "pytest",
        "playwright test",
        "vitest",
        "jest",
        "go test",
        "make test",
        "gradlew test",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

/// Return whether the identity-bound worker currently owns a non-plumbing
/// verification child. The lease is revalidated after the process-table
/// snapshot so PID recycling cannot turn an unrelated process into harvest
/// authority.
pub(crate) fn worker_identity_has_active_verification(
    worker_pid_path: &Path,
    expected: &WorkerPidIdentity,
) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let before = read_worker_pid_identity(worker_pid_path)?;
        if before.as_ref() != Some(expected) {
            return Ok(false);
        }
        let output = Command::new("ps")
            .args(["-eo", "pid=,ppid=,stat=,args="])
            .output()
            .context("failed to inspect worker process tree before clean-commit harvest")?;
        if !output.status.success() {
            bail!("process-table inspection failed before clean-commit harvest");
        }
        let active = process_table_has_active_worker_verification(
            expected.pid(),
            &String::from_utf8_lossy(&output.stdout),
        );
        let after = read_worker_pid_identity(worker_pid_path)?;
        if after.as_ref() != Some(expected) {
            return Ok(false);
        }
        Ok(active)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (worker_pid_path, expected);
        Ok(false)
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn signal_worker(pid: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .with_context(|| format!("failed to send SIG{signal} to pid {pid}"))?;
    if !status.success() {
        if worker_pid_is_alive(pid)? {
            bail!("kill -{signal} {pid} failed");
        }
        return Ok(());
    }
    Ok(())
}

/// Revalidate the exact lease owner immediately before sending a signal. A
/// grace period can outlive the original process and Linux can recycle its PID;
/// comparing the full token prevents a later process from inheriting the old
/// lane's TERM/KILL authority.
pub(crate) fn signal_worker_identity(
    worker_pid_path: &Path,
    expected: &WorkerPidIdentity,
    signal: &str,
) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let raw_fd = unsafe {
            libc::syscall(
                libc::SYS_pidfd_open,
                libc::pid_t::try_from(expected.pid()).context("worker pid exceeds pid_t")?,
                0_u32,
            )
        };
        if raw_fd < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(false);
            }
            return Err(err).with_context(|| {
                format!(
                    "failed to open stable process handle for pid {}",
                    expected.pid()
                )
            });
        }
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw_fd as libc::c_int) };

        // Open the stable handle before re-reading the lease. If the process
        // exits or its numeric PID is recycled during validation, the handle
        // continues to name only the original process.
        let Some(current) = read_worker_pid_identity(worker_pid_path)? else {
            return Ok(false);
        };
        if current != *expected {
            return Ok(false);
        }

        let signal_number = match signal {
            "TERM" => libc::SIGTERM,
            "KILL" => libc::SIGKILL,
            _ => bail!("unsupported worker signal SIG{signal}"),
        };
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                signal_number,
                std::ptr::null::<libc::siginfo_t>(),
                0_u32,
            )
        };
        if result < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(false);
            }
            return Err(err).with_context(|| {
                format!(
                    "failed to send SIG{signal} through pidfd for pid {}",
                    expected.pid()
                )
            });
        }
        Ok(true)
    }

    #[cfg(not(target_os = "linux"))]
    signal_worker_identity_with(worker_pid_path, expected, signal, signal_worker)
}

#[cfg(any(test, not(target_os = "linux")))]
fn signal_worker_identity_with(
    worker_pid_path: &Path,
    expected: &WorkerPidIdentity,
    signal: &str,
    send: impl FnOnce(u32, &str) -> Result<()>,
) -> Result<bool> {
    let Some(current) = read_worker_pid_identity(worker_pid_path)? else {
        return Ok(false);
    };
    if current != *expected {
        return Ok(false);
    }
    send(expected.pid(), signal)?;
    Ok(true)
}

pub(crate) fn inspect_lane_repo_progress(
    repo_root: &Path,
    base_commit: &str,
) -> Result<LaneRepoProgress> {
    let status = git_stdout(repo_root, ["status", "--short"])?;
    let head = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    let has_new_commits = head.trim() != base_commit;
    let status = status.trim();
    if has_new_commits
        && std::env::var("AUTO_REJECT_DOCS_ONLY_COMMITS")
            .ok()
            .as_deref()
            == Some("1")
        && lane_commit_range_is_docs_only(repo_root, base_commit, head.trim())?
    {
        eprintln!(
            "warning: lane produced only docs-only commits ({}..{}); treating as no progress under AUTO_REJECT_DOCS_ONLY_COMMITS=1",
            base_commit, head.trim()
        );
        return if status.is_empty() {
            Ok(LaneRepoProgress::None)
        } else {
            Ok(LaneRepoProgress::Dirty(status.to_string()))
        };
    }
    match (has_new_commits, status.is_empty()) {
        (false, true) => Ok(LaneRepoProgress::None),
        (false, false) => Ok(LaneRepoProgress::Dirty(status.to_string())),
        (true, true) => Ok(LaneRepoProgress::NewCommits),
        (true, false) => Ok(LaneRepoProgress::NewCommitsWithDirty(status.to_string())),
    }
}

pub(crate) fn lane_commit_range_is_docs_only(
    repo_root: &Path,
    base_commit: &str,
    head_commit: &str,
) -> Result<bool> {
    let range = format!("{base_commit}..{head_commit}");
    let files = git_stdout(repo_root, ["diff", "--name-only", &range])?;
    let lines: Vec<&str> = files
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if lines.is_empty() {
        return Ok(false);
    }
    Ok(lines.iter().all(|file| is_docs_only_path(file)))
}

pub(crate) fn is_docs_only_path(path: &str) -> bool {
    // Meaningful-progress files: plan-status updates, release notes,
    // orientation docs, agent instructions. A commit that touches any of
    // these counts as real progress even if everything else is doc-shaped.
    let meaningful_progress_files = [
        "IMPLEMENTATION_PLAN.md",
        "PLAN.md",
        "CHANGELOG.md",
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
        "VERSION",
    ];
    if meaningful_progress_files.contains(&path) {
        return false;
    }
    // Files that never contain executable/test logic. A commit whose entire
    // changed-file set matches these patterns is considered "docs/evidence
    // only" and (when AUTO_REJECT_DOCS_ONLY_COMMITS=1) treated as no progress.
    path.ends_with(".md")
        || path.starts_with("docs/")
        || path.starts_with("genesis/checkpoints/")
        || path.starts_with("genesis/ASSESSMENT.")
        || path.starts_with("genesis/DESIGN.")
        || path.starts_with("genesis/FOCUS.")
        || path.starts_with("genesis/GENESIS-REPORT.")
        || path.starts_with("genesis/PLANS.")
        || path.starts_with("genesis/SPEC.")
        || path.contains("/operator-evidence/")
        || path.contains("RECEIPTS-DRIFT")
}

pub(crate) fn lane_changed_files(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
) -> Result<Vec<String>> {
    if base_commit == head_ref {
        return Ok(Vec::new());
    }
    let range = format!("{base_commit}..{head_ref}");
    let output = git_stdout(repo_root, ["diff", "--name-only", &range])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

pub(crate) fn git_ref_is_ancestor(
    repo_root: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .with_context(|| {
            format!(
                "failed checking whether {ancestor} is an ancestor of {descendant} in {}",
                repo_root.display()
            )
        })?;
    Ok(output.status.success())
}

pub(crate) fn fetch_lane_commit(
    repo_root: &Path,
    lane_repo_root: &Path,
    lane_head: &str,
) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("fetch")
        .arg(lane_repo_root)
        .arg(lane_head)
        .output()
        .with_context(|| {
            format!(
                "failed to fetch lane commit {} from {}",
                lane_head,
                lane_repo_root.display()
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git fetch failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

pub(crate) fn git_branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_ref_exists(repo_root, &format!("refs/heads/{branch}"))
        || git_ref_exists(repo_root, &format!("refs/remotes/origin/{branch}"))
}

pub(crate) fn git_ref_exists(repo_root: &Path, git_ref: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show-ref", "--verify", "--quiet", git_ref])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use crate::backend_process::WorkerPidGuard;
    #[cfg(target_os = "linux")]
    use crate::backend_process::WorkerPidLeaseRecord;
    use crate::parallel_command::*;
    use std::time::UNIX_EPOCH;

    fn init_git_repo(path: &PathBuf) {
        fs::create_dir_all(path).expect("failed to create repo dir");
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(path)
            .status()
            .expect("failed to run git init");
        assert!(status.success(), "git init should succeed");
        git_ok(path, ["config", "user.email", "test@example.com"]);
        git_ok(path, ["config", "user.name", "Autodev Test"]);
    }

    fn git_ok<const N: usize>(repo: &PathBuf, args: [&str; N]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output<const N: usize>(repo: &PathBuf, args: [&str; N]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_worker_cleanup_retires_the_lease_without_unlinking_its_publication() {
        let root = unique_temp_dir("stale-worker-lease");
        fs::create_dir_all(&root).expect("create worker pid root");
        let path = root.join("worker.pid");
        let pid = std::process::id();
        let guard = WorkerPidGuard::new(Some(&path), Some(pid)).expect("publish worker pid");
        let lease_path = root.join(fs::read_link(&path).expect("read worker pid lease"));
        #[cfg(target_os = "linux")]
        {
            let mut record: WorkerPidLeaseRecord =
                serde_json::from_str(&fs::read_to_string(&lease_path).expect("read worker lease"))
                    .expect("parse worker lease");
            record.linux_start_time_ticks += 1;
            fs::write(
                &lease_path,
                serde_json::to_vec(&record).expect("serialize stale worker lease"),
            )
            .expect("write stale worker lease");
        }

        clear_stale_worker_pid(&path).expect("clear stale worker pid");

        assert!(fs::symlink_metadata(&path)
            .expect("shared worker pid publication should remain")
            .file_type()
            .is_symlink());
        assert!(!lease_path.exists());
        assert_eq!(
            read_worker_pid(&path).expect("dangling publication should be readable as state"),
            None
        );

        drop(guard);
        fs::remove_dir_all(&root).expect("remove worker pid root");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn signal_boundary_rejects_a_changed_worker_identity() {
        let root = unique_temp_dir("worker-signal-identity");
        fs::create_dir_all(&root).expect("create worker pid root");
        let path = root.join("worker.pid");
        let guard = WorkerPidGuard::new(Some(&path), Some(std::process::id()))
            .expect("publish worker identity");
        let expected = read_worker_pid_identity(&path)
            .expect("read worker identity")
            .expect("worker must be live");
        let lease_path = root.join(fs::read_link(&path).expect("read worker pid lease"));
        let mut replacement: WorkerPidLeaseRecord =
            serde_json::from_str(&fs::read_to_string(&lease_path).expect("read worker lease"))
                .expect("parse worker lease");
        replacement.linux_start_time_ticks += 1;
        fs::write(
            &lease_path,
            serde_json::to_vec(&replacement).expect("serialize replacement identity"),
        )
        .expect("replace worker identity");
        let called = std::cell::Cell::new(false);

        let signaled = super::signal_worker_identity_with(&path, &expected, "TERM", |_, _| {
            called.set(true);
            Ok(())
        })
        .expect("identity rejection should not error");

        assert!(!signaled);
        assert!(
            !called.get(),
            "signal callback must not run after identity drift"
        );
        drop(guard);
        fs::remove_dir_all(&root).expect("remove worker pid root");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn signal_worker_identity_uses_live_pidfd_handle() {
        let root = unique_temp_dir("worker-pidfd-signal");
        fs::create_dir_all(&root).expect("create worker pid root");
        let path = root.join("worker.pid");
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn signal target");
        let guard = WorkerPidGuard::new(Some(&path), Some(child.id()))
            .expect("publish child worker identity");
        let expected = read_worker_pid_identity(&path)
            .expect("read child worker identity")
            .expect("child worker must be live");

        assert!(signal_worker_identity(&path, &expected, "TERM")
            .expect("signal through stable process handle"));
        let status = child.wait().expect("reap signal target");
        assert!(!status.success(), "TERM should stop the signal target");

        drop(guard);
        fs::remove_dir_all(&root).expect("remove worker pid root");
    }

    #[test]
    fn terminal_disposal_preserves_unlanded_or_dirty_lane_work_before_clearing_ledger() {
        let root = unique_temp_dir("terminal-lane-disposal");
        let canonical = root.join("canonical");
        let run_root = canonical.join(".auto/parallel");
        let lane_root = run_root.join("lanes/lane-1");
        let lane_repo = lane_root.join("repo");
        init_git_repo(&canonical);
        git_ok(&canonical, ["checkout", "-q", "-b", "main"]);
        fs::write(canonical.join("source.txt"), "base\n").expect("write canonical source");
        git_ok(&canonical, ["add", "source.txt"]);
        git_ok(&canonical, ["commit", "-q", "-m", "base"]);
        fs::create_dir_all(&lane_root).expect("create lane root");
        clone_loop_lane_repo(&canonical, "main", &lane_repo).expect("clone lane repo");
        git_ok(&lane_repo, ["config", "user.email", "test@example.com"]);
        git_ok(&lane_repo, ["config", "user.name", "Autodev Test"]);
        fs::write(lane_repo.join("source.txt"), "task work\n").expect("write lane work");
        git_ok(&lane_repo, ["add", "source.txt"]);
        git_ok(
            &lane_repo,
            ["commit", "-q", "-m", "TASK-BLOCKED implementation"],
        );
        fs::write(run_root.join(".run-state.json"), b"{}").expect("write run ledger");

        assert!(
            !parallel_lane_repos_are_disposable(&canonical, &run_root, "main")
                .expect("inspect unlanded lane")
        );
        assert!(!clear_parallel_run_state_if_terminally_empty(
            &run_root,
            false,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        ));
        assert!(run_root.join(".run-state.json").exists());

        let lane_commit = git_output(&lane_repo, ["rev-parse", "HEAD"]);
        git_ok(
            &canonical,
            [
                "fetch",
                "-q",
                lane_repo.to_str().expect("UTF-8 lane path"),
                &lane_commit,
            ],
        );
        git_ok(&canonical, ["cherry-pick", "FETCH_HEAD"]);
        assert!(
            parallel_lane_repos_are_disposable(&canonical, &run_root, "main")
                .expect("inspect landed lane")
        );

        fs::write(lane_repo.join("dirty.txt"), "uncommitted\n").expect("write dirty lane file");
        assert!(
            !parallel_lane_repos_are_disposable(&canonical, &run_root, "main")
                .expect("inspect dirty lane")
        );
        fs::remove_file(lane_repo.join("dirty.txt")).expect("remove dirty lane file");
        assert!(clear_parallel_run_state_if_terminally_empty(
            &run_root,
            parallel_lane_repos_are_disposable(&canonical, &run_root, "main")
                .expect("prove terminal lanes"),
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        ));
        assert!(!run_root.join(".run-state.json").exists());

        fs::remove_dir_all(&root).expect("remove terminal disposal fixture");
    }

    #[test]
    fn share_lean_dependency_cache_symlinks_prebuilt_packages() {
        let base = unique_temp_dir("lean-cache");
        let canonical = base.join("canonical");
        let lane = base.join("lane");
        fs::create_dir_all(canonical.join(".lake").join("packages").join("mathlib"))
            .expect("failed to create canonical .lake/packages");
        fs::create_dir_all(&lane).expect("failed to create lane dir");

        share_lean_dependency_cache(&canonical, &lane);

        let linked = lane.join(".lake").join("packages");
        assert!(
            fs::symlink_metadata(&linked)
                .expect("lane packages should exist")
                .file_type()
                .is_symlink(),
            "lane .lake/packages should be a symlink, not a copy"
        );
        assert!(
            linked.join("mathlib").exists(),
            "symlinked cache should expose the shared mathlib package"
        );

        fs::remove_dir_all(&base).expect("failed to clean temp dir");
    }

    #[test]
    fn share_lean_dependency_cache_is_noop_for_non_lean_repo() {
        let base = unique_temp_dir("non-lean-cache");
        let canonical = base.join("canonical");
        let lane = base.join("lane");
        fs::create_dir_all(&canonical).expect("failed to create canonical dir");
        fs::create_dir_all(&lane).expect("failed to create lane dir");

        share_lean_dependency_cache(&canonical, &lane);

        assert!(
            !lane.join(".lake").exists(),
            "no .lake should be created for a non-Lean repo"
        );

        fs::remove_dir_all(&base).expect("failed to clean temp dir");
    }

    #[test]
    fn share_configured_lane_paths_symlinks_listed_gitignored_paths() {
        let base = unique_temp_dir("lane-shared-paths");
        let canonical = base.join("canonical");
        let lane = base.join("lane");
        // Canonical has a gitignored build oracle and a config naming it.
        fs::create_dir_all(canonical.join("target").join("oracle"))
            .expect("failed to create canonical oracle dir");
        fs::write(
            canonical.join("target").join("oracle").join("Ludii.jar"),
            b"jar",
        )
        .expect("failed to write oracle jar");
        fs::create_dir_all(canonical.join(".auto")).expect("failed to create .auto");
        fs::write(
            canonical.join(".auto").join("lane-shared-paths"),
            "# build artifacts to share into lanes\ntarget/oracle\n/etc/passwd\n../escape\nmissing/path\n",
        )
        .expect("failed to write lane-shared-paths");
        fs::create_dir_all(&lane).expect("failed to create lane dir");

        share_configured_lane_paths(&canonical, &lane);

        let linked = lane.join("target").join("oracle");
        assert!(
            fs::symlink_metadata(&linked)
                .expect("lane oracle should exist")
                .file_type()
                .is_symlink(),
            "configured path should be symlinked into the lane"
        );
        assert!(
            linked.join("Ludii.jar").exists(),
            "symlinked oracle should expose the shared jar"
        );
        // The absolute path, the `..` escape, and the missing path must be skipped.
        assert!(
            !lane.join("etc").exists(),
            "absolute paths must be rejected"
        );
        assert!(
            !lane.join("escape").exists(),
            "parent escapes must be rejected"
        );
        assert!(
            !lane.join("missing").exists(),
            "absent sources must be skipped"
        );

        fs::remove_dir_all(&base).expect("failed to clean temp dir");
    }

    #[test]
    fn share_configured_lane_paths_is_noop_without_config() {
        let base = unique_temp_dir("lane-shared-paths-none");
        let canonical = base.join("canonical");
        let lane = base.join("lane");
        fs::create_dir_all(&canonical).expect("failed to create canonical dir");
        fs::create_dir_all(&lane).expect("failed to create lane dir");

        share_configured_lane_paths(&canonical, &lane);

        assert!(
            fs::read_dir(&lane)
                .expect("lane should be readable")
                .next()
                .is_none(),
            "no config means the lane is left untouched"
        );

        fs::remove_dir_all(&base).expect("failed to clean temp dir");
    }

    #[test]
    fn lane_repo_process_parser_finds_orphaned_codex_descendants() {
        let lane_repo = PathBuf::from("/tmp/repo/.auto/parallel/lanes/lane-3/repo");
        let ps = r#"
  100 bash /home/r/.local/bin/codex exec --cd /tmp/repo/.auto/parallel/lanes/lane-3/repo -m gpt-5.6-sol
  101 node /home/r/.npm-global/bin/codex exec --cd /tmp/repo/.auto/parallel/lanes/lane-3/repo -m gpt-5.6-sol
  102 rg /tmp/repo/.auto/parallel/lanes/lane-3/repo
  103 bash /home/r/.local/bin/codex exec --cd /tmp/repo/.auto/parallel/lanes/lane-4/repo -m gpt-5.6-sol
"#;

        let pids = parse_lane_repo_process_pids(&lane_repo, ps);

        assert_eq!(pids, vec![100, 101]);
    }

    #[test]
    fn lane_repo_progress_reports_commits_and_dirty_state_independently() {
        let repo = unique_temp_dir("parallel-lane-progress");
        init_git_repo(&repo);
        fs::write(repo.join("file.txt"), "base\n").expect("failed to write base file");
        git_ok(&repo, ["add", "file.txt"]);
        git_ok(&repo, ["commit", "-m", "base"]);
        let base = git_output(&repo, ["rev-parse", "HEAD"]);

        fs::write(repo.join("file.txt"), "dirty\n").expect("failed to dirty file");
        assert_eq!(
            inspect_lane_repo_progress(&repo, &base).expect("progress should inspect"),
            LaneRepoProgress::Dirty("M file.txt".to_string())
        );

        git_ok(&repo, ["add", "file.txt"]);
        git_ok(&repo, ["commit", "-m", "task"]);
        assert_eq!(
            inspect_lane_repo_progress(&repo, &base).expect("progress should inspect"),
            LaneRepoProgress::NewCommits
        );

        fs::write(repo.join("file.txt"), "dirty again\n").expect("failed to dirty file again");
        assert_eq!(
            inspect_lane_repo_progress(&repo, &base).expect("progress should inspect"),
            LaneRepoProgress::NewCommitsWithDirty("M file.txt".to_string())
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }
}
