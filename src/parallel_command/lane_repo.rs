use super::*;

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
    Ok(())
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

pub(crate) fn read_worker_pid(path: &Path) -> Result<Option<u32>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let pid = trimmed
        .parse::<u32>()
        .with_context(|| format!("invalid pid in {}", path.display()))?;
    Ok(Some(pid))
}

pub(crate) fn clear_stale_worker_pid(path: &Path) -> Result<()> {
    let Some(pid) = read_worker_pid(path)? else {
        return Ok(());
    };
    if worker_pid_is_alive(pid)? {
        return Ok(());
    }
    fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
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

pub(crate) fn worker_pid_is_alive(pid: u32) -> Result<bool> {
    let status = Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .context("failed to run kill -0")?;
    Ok(status.success())
}

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
    fn lane_repo_process_parser_finds_orphaned_codex_descendants() {
        let lane_repo = PathBuf::from("/tmp/repo/.auto/parallel/lanes/lane-3/repo");
        let ps = r#"
  100 bash /home/r/.local/bin/codex exec --cd /tmp/repo/.auto/parallel/lanes/lane-3/repo -m gpt-5.5
  101 node /home/r/.npm-global/bin/codex exec --cd /tmp/repo/.auto/parallel/lanes/lane-3/repo -m gpt-5.5
  102 rg /tmp/repo/.auto/parallel/lanes/lane-3/repo
  103 bash /home/r/.local/bin/codex exec --cd /tmp/repo/.auto/parallel/lanes/lane-4/repo -m gpt-5.5
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
