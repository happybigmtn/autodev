//! Safe automatic purge of the PREVIOUS parallel run's heavy artifacts.
//!
//! Lane worktrees, per-run worker shims, and host logs dominate the
//! disk footprint of a parallel run (hundreds of MB per repo), but none of
//! them carry completion truth: receipts live in
//! `.auto/symphony/verification-receipts/` and in
//! `Auto-Verification-Receipt-*` commit footers, and landed work lives in
//! git. This purges the prior run's artifacts at the start of a new run when
//! it is provably safe:
//!
//! - the repo's parallel tmux session is not running (a live host owns the
//!   lane directories), and
//! - the run root has no `.run-state.json` ledger (an unfinished run keeps
//!   its lanes for resume and forensics).
//!
//! Salvage records are semantic recovery evidence for clean commits that did
//! not land. They remain preserved until an exact landing/retirement path can
//! remove the corresponding note.
//!
//! Persistent `lane-caches/` are deliberately excluded. They are bounded at
//! assignment time and make subsequent Rust lanes materially faster.
//!
//! Opt out with `AUTO_PARALLEL_PURGE_PREVIOUS=0`.

use super::*;
use fs2::FileExt;
use std::fs::File;

const PURGEABLE_SUBDIRS: &[&str] = &["lanes", "worker-bin"];
const PURGEABLE_LOGS: &[&str] = &["host.stdout.log", "host.stderr.log"];
const PARALLEL_HOST_LOCK_FILE: &str = ".host.lock";
const PARALLEL_PRUNE_QUARANTINE_DIR: &str = ".prune-quarantine";

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParallelPrunePlan {
    root_identity: FilesystemIdentity,
    targets: Vec<ParallelPruneTarget>,
    bytes: u64,
    blocked_by_run_state: bool,
    lanes_preserved_by_salvage: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParallelPruneTarget {
    path: PathBuf,
    identity: FilesystemIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FilesystemIdentity {
    is_dir: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn purge_previous_run_enabled() -> bool {
    std::env::var("AUTO_PARALLEL_PURGE_PREVIOUS")
        .map(|value| value.trim() != "0")
        .unwrap_or(true)
}

/// Purge the previous run's heavy artifacts from `run_root` (and the legacy
/// in-repo `.auto/parallel` root when it differs). Best-effort: failures are
/// logged and never block the new run.
pub(crate) fn purge_previous_parallel_run_artifacts(repo_root: &Path, run_root: &Path) {
    if !purge_previous_run_enabled() {
        return;
    }
    // A live host owns these directories. Unknown tmux probe results fail
    // closed; absence must be positively identified before automatic purge.
    let session = parallel_tmux_session_name(repo_root);
    if !matches!(tmux_session_exists(&session), Ok(false)) {
        return;
    }
    if !matches!(parallel_host_processes_for_repo_strict(repo_root), Ok(hosts) if hosts.is_empty())
    {
        return;
    }
    let result = (|| -> Result<Option<(usize, u64)>> {
        validate_parallel_prune_root(repo_root, run_root)?;
        let plan = parallel_prune_plan(run_root)?;
        if plan.blocked_by_run_state || plan.targets.is_empty() {
            return Ok(None);
        }
        let count = plan.targets.len();
        let bytes = plan.bytes;
        apply_parallel_prune_plan(run_root, &plan, false, &session)?;
        Ok(Some((count, bytes)))
    })();
    match result {
        Ok(Some((count, bytes))) => println!(
            "purge-previous-run: reclaimed {} from {} prior artifact(s)",
            human_bytes(bytes),
            count
        ),
        Ok(None) => {}
        Err(err) => eprintln!("warning: purge-previous-run skipped: {err:#}"),
    }
}

/// Preview or apply cleanup of disposable parallel artifacts. Unlike the
/// best-effort startup purge, this operator-facing path fails closed: it must
/// prove that tmux has no host session and that no resumable ledger exists.
pub(crate) fn run_parallel_prune(args: &ParallelArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    let run_root = parallel_run_root(&repo_root, args);
    validate_parallel_prune_root(&repo_root, &run_root)?;

    let session = parallel_tmux_session_name(&repo_root);
    let tmux_running = tmux_session_exists(&session)
        .with_context(|| format!("cannot prove parallel host `{session}` is stopped"))?;
    let host_processes = parallel_host_processes_for_repo_strict(&repo_root)
        .context("cannot prove no direct parallel host is running")?;
    let host_running = parallel_prune_host_is_active(tmux_running, &host_processes);
    let plan = parallel_prune_plan(&run_root)?;

    println!("auto parallel prune");
    println!("repo root:   {}", repo_root.display());
    println!("run root:    {}", run_root.display());
    println!(
        "mode:        {}",
        if args.apply { "apply" } else { "dry-run" }
    );
    println!(
        "host:        {}",
        if host_running {
            "ACTIVE (protected)"
        } else {
            "stopped"
        }
    );
    for process in &host_processes {
        println!("  {process}");
    }
    println!(
        "run state:   {}",
        if plan.blocked_by_run_state {
            "present (protected)"
        } else {
            "absent"
        }
    );
    println!("lane caches: preserved");
    if plan.lanes_preserved_by_salvage {
        println!("lanes:       preserved (salvage recovery evidence exists)");
    }
    if plan.targets.is_empty() {
        println!("targets:     none (0 B)");
    } else {
        println!(
            "targets:     {} ({})",
            plan.targets.len(),
            human_bytes(plan.bytes)
        );
        for target in &plan.targets {
            println!("  {}", target.path.display());
        }
    }

    if !args.apply {
        println!("dry-run: no files removed; pass --apply to remove listed targets");
        return Ok(());
    }
    let _prune_lease = acquire_parallel_host_lease(&run_root, "prune")?;
    // Re-probe every ownership signal and rebuild the exact target list at the
    // destructive boundary. Printing a large preview gives another host time
    // to start, and stale preview state must never authorize deletion.
    let apply_tmux_running = tmux_session_exists(&session)
        .with_context(|| format!("cannot prove parallel host `{session}` is stopped"))?;
    let apply_host_processes = parallel_host_processes_for_repo_strict(&repo_root)
        .context("cannot prove no direct parallel host is running")?;
    let apply_plan = parallel_prune_plan(&run_root)?;
    apply_parallel_prune_plan(
        &run_root,
        &apply_plan,
        parallel_prune_host_is_active(apply_tmux_running, &apply_host_processes),
        &session,
    )?;
    println!(
        "pruned:      {} ({})",
        apply_plan.targets.len(),
        human_bytes(apply_plan.bytes)
    );
    Ok(())
}

/// Serialize destructive pruning against hosts built from this version. The
/// process/tmux probes remain necessary for older installed hosts that do not
/// yet participate in this lease.
pub(crate) fn acquire_parallel_host_lease(run_root: &Path, owner: &str) -> Result<File> {
    fs::create_dir_all(run_root)
        .with_context(|| format!("failed to create parallel run root {}", run_root.display()))?;
    let path = run_root.join(PARALLEL_HOST_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open parallel host lease {}", path.display()))?;
    file.try_lock_exclusive().map_err(|err| {
        anyhow::anyhow!(
            "refusing {owner}: parallel run root {} is leased by an active host or prune ({err})",
            run_root.display()
        )
    })?;
    Ok(file)
}

fn parallel_prune_host_is_active(tmux_running: bool, host_processes: &[String]) -> bool {
    tmux_running || !host_processes.is_empty()
}

fn apply_parallel_prune_plan(
    run_root: &Path,
    plan: &ParallelPrunePlan,
    host_running: bool,
    session: &str,
) -> Result<()> {
    apply_parallel_prune_plan_with_hook(run_root, plan, host_running, session, |_| {})
}

fn apply_parallel_prune_plan_with_hook(
    run_root: &Path,
    plan: &ParallelPrunePlan,
    host_running: bool,
    session: &str,
    mut before_remove: impl FnMut(&ParallelPruneTarget),
) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    bail!("applied parallel prune requires Linux renameat2 safety semantics");
    if host_running {
        bail!("refusing prune while parallel tmux host `{session}` is active");
    }
    if plan.blocked_by_run_state {
        bail!(
            "refusing prune because {} is a resumable run ledger",
            run_root.join(".run-state.json").display()
        );
    }
    let current_root = filesystem_identity(&fs::symlink_metadata(run_root)?)?;
    if current_root != plan.root_identity {
        bail!("refusing prune because parallel run root identity changed after planning");
    }
    for target in &plan.targets {
        before_remove(target);
        remove_parallel_prune_target(run_root, target)?;
    }
    Ok(())
}

fn validate_parallel_prune_root(repo_root: &Path, run_root: &Path) -> Result<()> {
    if !run_root.is_absolute() {
        bail!(
            "parallel prune run root must be absolute: {}",
            run_root.display()
        );
    }
    if run_root.parent().is_none() || run_root == Path::new("/") || run_root == repo_root {
        bail!(
            "refusing unsafe parallel prune run root: {}",
            run_root.display()
        );
    }
    if run_root.exists() && fs::symlink_metadata(run_root)?.file_type().is_symlink() {
        bail!(
            "refusing symlinked parallel prune run root: {}",
            run_root.display()
        );
    }
    if run_root.exists() {
        let canonical_repo = fs::canonicalize(repo_root)
            .with_context(|| format!("failed to canonicalize {}", repo_root.display()))?;
        let canonical = fs::canonicalize(run_root)
            .with_context(|| format!("failed to canonicalize {}", run_root.display()))?;
        if canonical != run_root {
            bail!(
                "refusing non-canonical parallel prune run root {} (resolves to {})",
                run_root.display(),
                canonical.display()
            );
        }
        let in_repo_default = canonical_repo.join(".auto/parallel");
        let configured = crate::util::auto_run_root_override(&canonical_repo, "parallel")
            .map(|path| fs::canonicalize(&path).unwrap_or(path));
        if canonical != in_repo_default && configured.as_ref() != Some(&canonical) {
            bail!(
                "refusing unsupported parallel prune run root {}; expected {}{}",
                canonical.display(),
                in_repo_default.display(),
                configured
                    .map(|path| format!(" or configured {}", path.display()))
                    .unwrap_or_default()
            );
        }
    }
    Ok(())
}

fn parallel_prune_plan(run_root: &Path) -> Result<ParallelPrunePlan> {
    let mut targets = Vec::new();
    let mut bytes = 0u64;
    let lanes_preserved_by_salvage = parallel_salvage_records_present(run_root)?;
    for name in PURGEABLE_SUBDIRS.iter().chain(PURGEABLE_LOGS.iter()) {
        if *name == "lanes" && lanes_preserved_by_salvage {
            continue;
        }
        let target = run_root.join(name);
        validate_parallel_prune_target(run_root, &target)?;
        let Ok(metadata) = fs::symlink_metadata(&target) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing symlinked parallel prune target: {}",
                target.display()
            );
        }
        validate_existing_parallel_prune_target(run_root, &target)?;
        bytes += if metadata.is_dir() {
            dir_size_bytes(&target)
        } else {
            metadata.len()
        };
        targets.push(ParallelPruneTarget {
            path: target,
            identity: filesystem_identity(&metadata)?,
        });
    }
    if !targets.is_empty() {
        validate_parallel_run_root_identity(run_root)?;
    }
    Ok(ParallelPrunePlan {
        root_identity: filesystem_identity(&fs::symlink_metadata(run_root)?)?,
        targets,
        bytes,
        blocked_by_run_state: run_root.join(".run-state.json").exists(),
        lanes_preserved_by_salvage,
    })
}

fn parallel_salvage_records_present(run_root: &Path) -> Result<bool> {
    let path = run_root.join(SALVAGE_DIR);
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Ok(false);
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "refusing invalid parallel salvage evidence root: {}",
            path.display()
        );
    }
    Ok(fs::read_dir(&path)
        .with_context(|| format!("failed reading salvage evidence {}", path.display()))?
        .next()
        .transpose()?
        .is_some())
}

fn validate_parallel_run_root_identity(run_root: &Path) -> Result<()> {
    let marker = run_root.join(CURRENT_RUN_ID_FILE);
    let metadata = fs::symlink_metadata(&marker).with_context(|| {
        format!(
            "refusing prune because {} has artifacts but no Autodev run marker {}",
            run_root.display(),
            marker.display()
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("refusing invalid Autodev run marker: {}", marker.display());
    }
    let run_id = fs::read_to_string(&marker)
        .with_context(|| format!("failed reading Autodev run marker {}", marker.display()))?;
    let run_id = run_id.trim();
    if run_id.is_empty() || run_id.contains(['/', '\\']) {
        bail!("refusing invalid Autodev run marker: {}", marker.display());
    }
    Ok(())
}

fn validate_parallel_prune_target(run_root: &Path, target: &Path) -> Result<()> {
    let name = target.file_name().and_then(|name| name.to_str());
    let allowed = name
        .is_some_and(|name| PURGEABLE_SUBDIRS.contains(&name) || PURGEABLE_LOGS.contains(&name));
    if target.parent() != Some(run_root) || !allowed {
        bail!(
            "refusing unexpected parallel prune target: {}",
            target.display()
        );
    }
    Ok(())
}

fn remove_parallel_prune_target(run_root: &Path, target: &ParallelPruneTarget) -> Result<()> {
    remove_parallel_prune_target_with_hook(run_root, target, |_, _| {})
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PruneRemovalStage {
    AfterIdentityValidation,
    AfterQuarantineSelection,
}

fn remove_parallel_prune_target_with_hook(
    run_root: &Path,
    target: &ParallelPruneTarget,
    mut hook: impl FnMut(PruneRemovalStage, &Path),
) -> Result<()> {
    validate_parallel_prune_target(run_root, &target.path)?;
    let metadata = fs::symlink_metadata(&target.path)
        .with_context(|| format!("failed to inspect prune target {}", target.path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing symlinked parallel prune target: {}",
            target.path.display()
        );
    }
    validate_existing_parallel_prune_target(run_root, &target.path)?;
    if filesystem_identity(&metadata)? != target.identity {
        bail!(
            "refusing prune because target identity changed after planning: {}",
            target.path.display()
        );
    }
    hook(PruneRemovalStage::AfterIdentityValidation, &target.path);
    let quarantine = parallel_prune_quarantine_path(run_root, &target.path)?;
    hook(PruneRemovalStage::AfterQuarantineSelection, &quarantine);
    rename_parallel_prune_target_noreplace(&target.path, &quarantine).with_context(|| {
        format!(
            "failed to quarantine prune target {} as {}",
            target.path.display(),
            quarantine.display()
        )
    })?;
    let quarantined_metadata = fs::symlink_metadata(&quarantine)
        .with_context(|| format!("failed to inspect quarantine {}", quarantine.display()))?;
    if quarantined_metadata.file_type().is_symlink()
        || filesystem_identity(&quarantined_metadata)? != target.identity
    {
        let restore = rename_parallel_prune_target_noreplace(&quarantine, &target.path);
        bail!(
            "refusing to remove changed target quarantined at {}; restore: {}",
            quarantine.display(),
            restore
                .map(|_| "restored original path".to_string())
                .unwrap_or_else(|err| format!("left quarantined: {err:#}"))
        );
    }
    validate_parallel_prune_quarantine_target(run_root, &quarantine)?;
    if quarantined_metadata.is_dir() {
        fs::remove_dir_all(&quarantine)
    } else {
        fs::remove_file(&quarantine)
    }
    .with_context(|| format!("failed to prune {}", quarantine.display()))
}

fn parallel_prune_quarantine_path(run_root: &Path, target: &Path) -> Result<PathBuf> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .context("prune target had no UTF-8 file name")?;
    let quarantine_root = run_root.join(PARALLEL_PRUNE_QUARANTINE_DIR);
    create_private_parallel_prune_quarantine(&quarantine_root)?;
    for attempt in 0..100usize {
        let candidate = quarantine_root.join(format!(
            ".prune-{name}-{}-{}-{attempt}",
            std::process::id(),
            timestamp_slug()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("failed reserving quarantine path for {}", target.display())
}

#[cfg(unix)]
fn create_private_parallel_prune_quarantine(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if !path.exists() {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .with_context(|| format!("failed creating quarantine {}", path.display()))?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("refusing invalid prune quarantine: {}", path.display());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn create_private_parallel_prune_quarantine(_path: &Path) -> Result<()> {
    bail!("applied parallel prune requires Unix quarantine safety")
}

#[cfg(target_os = "linux")]
fn rename_parallel_prune_target_noreplace(source: &Path, destination: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new(source.as_os_str().as_bytes()).context("source path contains NUL")?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .context("destination path contains NUL")?;
    // SAFETY: both arguments are live NUL-terminated path strings and the
    // syscall does not retain either pointer.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("renameat2(RENAME_NOREPLACE) failed")
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_parallel_prune_target_noreplace(_source: &Path, _destination: &Path) -> Result<()> {
    bail!("applied parallel prune requires Linux renameat2 safety semantics")
}

fn validate_parallel_prune_quarantine_target(run_root: &Path, target: &Path) -> Result<()> {
    let canonical_root = fs::canonicalize(run_root)?;
    let canonical_quarantine = fs::canonicalize(run_root.join(PARALLEL_PRUNE_QUARANTINE_DIR))?;
    let canonical_target = fs::canonicalize(target)?;
    if canonical_quarantine.parent() != Some(canonical_root.as_path())
        || canonical_target.parent() != Some(canonical_quarantine.as_path())
    {
        bail!("refusing quarantine target outside verified run root");
    }
    Ok(())
}

#[cfg(unix)]
fn filesystem_identity(metadata: &fs::Metadata) -> Result<FilesystemIdentity> {
    use std::os::unix::fs::MetadataExt;
    Ok(FilesystemIdentity {
        is_dir: metadata.is_dir(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn filesystem_identity(metadata: &fs::Metadata) -> Result<FilesystemIdentity> {
    Ok(FilesystemIdentity {
        is_dir: metadata.is_dir(),
    })
}

fn validate_existing_parallel_prune_target(run_root: &Path, target: &Path) -> Result<()> {
    let canonical_root = fs::canonicalize(run_root)
        .with_context(|| format!("failed to canonicalize {}", run_root.display()))?;
    let canonical_target = fs::canonicalize(target)
        .with_context(|| format!("failed to canonicalize {}", target.display()))?;
    if canonical_target.parent() != Some(canonical_root.as_path()) {
        bail!(
            "refusing prune target outside canonical run root: {}",
            canonical_target.display()
        );
    }
    Ok(())
}

pub(crate) fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-purge-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn seed_run_artifacts(root: &Path) {
        std::fs::write(root.join(CURRENT_RUN_ID_FILE), b"test-run-1\n")
            .expect("write run identity");
        for sub in PURGEABLE_SUBDIRS {
            let dir = root.join(sub).join("nested");
            std::fs::create_dir_all(&dir).expect("create artifact dir");
            std::fs::write(dir.join("blob.bin"), vec![0u8; 4096]).expect("write blob");
        }
        for log in PURGEABLE_LOGS {
            std::fs::write(root.join(log), b"log line\n").expect("write log");
        }
        std::fs::write(root.join("preflight.txt"), b"preflight\n").expect("write preflight");
        std::fs::create_dir_all(root.join("gate-holds")).expect("create gate-holds");
        std::fs::create_dir_all(root.join("lane-caches/lane-1"))
            .expect("create persistent lane cache");
        std::fs::write(root.join("lane-caches/lane-1/cache.bin"), vec![0u8; 2048])
            .expect("write cache");
    }

    fn seed_salvage_evidence(root: &Path) {
        let salvage = root.join("salvage/nested");
        std::fs::create_dir_all(&salvage).expect("create salvage evidence dir");
        std::fs::write(salvage.join("recovery.md"), b"unlanded commit\n")
            .expect("write salvage evidence");
    }

    #[test]
    fn purge_removes_previous_run_artifacts_but_keeps_semantic_files() {
        let repo = temp_dir("repo");
        let run_root = repo.join(".auto/parallel");
        std::fs::create_dir_all(&run_root).expect("create run root");
        seed_run_artifacts(&run_root);
        seed_salvage_evidence(&run_root);

        purge_previous_parallel_run_artifacts(&repo, &run_root);

        assert!(
            run_root.join("lanes").exists(),
            "salvage must preserve lanes"
        );
        assert!(!run_root.join("worker-bin").exists());
        for log in PURGEABLE_LOGS {
            assert!(!run_root.join(log).exists(), "{log} should be purged");
        }
        assert!(run_root.join("salvage/nested/recovery.md").exists());
        assert!(run_root.join("preflight.txt").exists());
        assert!(run_root.join("gate-holds").exists());
        assert!(run_root.join("lane-caches/lane-1/cache.bin").exists());

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn purge_keeps_everything_when_run_state_ledger_present() {
        let repo = temp_dir("repo-unclean");
        let run_root = repo.join(".auto/parallel");
        std::fs::create_dir_all(&run_root).expect("create run root");
        seed_run_artifacts(&run_root);
        std::fs::write(run_root.join(".run-state.json"), b"{}").expect("write ledger");

        purge_previous_parallel_run_artifacts(&repo, &run_root);

        for sub in PURGEABLE_SUBDIRS {
            assert!(
                run_root.join(sub).exists(),
                "{sub} must survive an unclean prior run"
            );
        }

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn purge_does_not_sweep_an_unapproved_root_or_legacy_sibling() {
        let repo = temp_dir("repo-legacy");
        let run_root = temp_dir("run-root-legacy");
        let legacy = repo.join(".auto").join("parallel");
        std::fs::create_dir_all(&legacy).expect("create legacy root");
        seed_run_artifacts(&legacy);

        purge_previous_parallel_run_artifacts(&repo, &run_root);

        for sub in PURGEABLE_SUBDIRS {
            assert!(legacy.join(sub).exists(), "legacy {sub} must be preserved");
        }

        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn explicit_prune_plan_lists_only_disposable_artifacts() {
        let run_root = temp_dir("explicit-plan");
        seed_run_artifacts(&run_root);

        let plan = parallel_prune_plan(&run_root).expect("build prune plan");
        let names = plan
            .targets
            .iter()
            .filter_map(|target| target.path.file_name().and_then(|name| name.to_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec!["lanes", "worker-bin", "host.stdout.log", "host.stderr.log"]
        );
        assert!(!plan.blocked_by_run_state);
        assert!(plan.bytes > 0);
        assert!(!names.contains(&"lane-caches"));

        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn explicit_prune_applies_exact_plan_and_preserves_caches() {
        let run_root = temp_dir("explicit-apply");
        seed_run_artifacts(&run_root);
        let plan = parallel_prune_plan(&run_root).expect("build prune plan");

        apply_parallel_prune_plan(&run_root, &plan, false, "test-parallel")
            .expect("apply prune plan");

        for target in plan.targets {
            assert!(
                !target.path.exists(),
                "{} should be removed",
                target.path.display()
            );
        }
        assert!(run_root.join("lane-caches/lane-1/cache.bin").exists());
        assert!(run_root.join("preflight.txt").exists());

        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn graceful_terminal_empty_ledger_unblocks_prune_without_discarding_salvage() {
        let run_root = temp_dir("terminal-empty-prune");
        seed_run_artifacts(&run_root);
        seed_salvage_evidence(&run_root);
        std::fs::write(run_root.join(".run-state.json"), b"{}").expect("write ledger");

        assert!(clear_parallel_run_state_if_terminally_empty(
            &run_root,
            true,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        ));
        let plan = parallel_prune_plan(&run_root).expect("plan after graceful stop");
        assert!(!plan.blocked_by_run_state);
        apply_parallel_prune_plan(&run_root, &plan, false, "test-parallel")
            .expect("prune terminal artifacts");

        assert!(run_root.join("lanes").exists());
        assert!(!run_root.join("worker-bin").exists());
        assert!(run_root.join("salvage/nested/recovery.md").exists());
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn explicit_prune_refuses_active_host_and_resumable_ledger() {
        let run_root = temp_dir("explicit-protected");
        seed_run_artifacts(&run_root);
        let plan = parallel_prune_plan(&run_root).expect("build prune plan");
        let err = apply_parallel_prune_plan(&run_root, &plan, true, "test-parallel")
            .expect_err("active host must block prune");
        assert!(err.to_string().contains("host `test-parallel` is active"));
        assert!(run_root.join("lanes").exists());

        std::fs::write(run_root.join(".run-state.json"), b"{}").expect("write run state");
        let plan = parallel_prune_plan(&run_root).expect("build blocked prune plan");
        assert!(plan.blocked_by_run_state);
        let err = apply_parallel_prune_plan(&run_root, &plan, false, "test-parallel")
            .expect_err("resumable ledger must block prune");
        assert!(err.to_string().contains("resumable run ledger"));
        assert!(run_root.join("lanes").exists());

        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn explicit_prune_treats_direct_non_tmux_host_as_active() {
        let direct_hosts = vec!["1234 /usr/local/bin/auto parallel --threads 1".to_string()];

        assert!(parallel_prune_host_is_active(false, &direct_hosts));
        assert!(parallel_prune_host_is_active(true, &[]));
        assert!(!parallel_prune_host_is_active(false, &[]));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_prune_refuses_symlinked_target() {
        use std::os::unix::fs::symlink;

        let run_root = temp_dir("explicit-symlink");
        let outside = temp_dir("explicit-symlink-outside");
        symlink(&outside, run_root.join("lanes")).expect("create target symlink");

        let err = parallel_prune_plan(&run_root).expect_err("symlink must be rejected");
        assert!(err.to_string().contains("symlinked parallel prune target"));
        assert!(outside.exists());

        std::fs::remove_file(run_root.join("lanes")).ok();
        std::fs::remove_dir_all(&run_root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn explicit_prune_rejects_broad_roots_and_unlisted_targets() {
        assert!(validate_parallel_prune_root(Path::new("/repo"), Path::new("/")).is_err());
        assert!(validate_parallel_prune_root(Path::new("/repo"), Path::new("/repo")).is_err());
        assert!(validate_parallel_prune_target(
            Path::new("/repo/.auto/parallel"),
            Path::new("/repo")
        )
        .is_err());
        assert!(validate_parallel_prune_target(
            Path::new("/repo/.auto/parallel"),
            Path::new("/repo/.auto/parallel/lane-caches")
        )
        .is_err());
    }

    #[test]
    fn explicit_prune_requires_positive_run_root_identity() {
        let run_root = temp_dir("missing-identity");
        std::fs::create_dir_all(run_root.join("lanes")).expect("create unrelated lanes");

        let err = parallel_prune_plan(&run_root).expect_err("missing marker must block prune");
        assert!(err.to_string().contains("no Autodev run marker"));
        assert!(run_root.join("lanes").exists());

        std::fs::remove_dir_all(&run_root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn explicit_prune_refuses_symlinked_run_root_ancestor() {
        use std::os::unix::fs::symlink;

        let parent = temp_dir("ancestor-parent");
        let repo = parent.join("repo");
        let real_auto = parent.join("real-auto");
        std::fs::create_dir_all(&repo).expect("create repo");
        std::fs::create_dir_all(real_auto.join("parallel")).expect("create real run root");
        symlink(&real_auto, repo.join(".auto")).expect("create ancestor symlink");
        let aliased = repo.join(".auto/parallel");

        let err = validate_parallel_prune_root(&repo, &aliased)
            .expect_err("symlinked ancestor must block prune");
        assert!(err
            .to_string()
            .contains("non-canonical parallel prune run root"));

        std::fs::remove_file(repo.join(".auto")).ok();
        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn explicit_prune_rejects_marker_spoofed_arbitrary_root() {
        let parent = temp_dir("spoofed-root-parent");
        let repo = parent.join("repo");
        let spoofed = parent.join("unrelated");
        std::fs::create_dir_all(&repo).expect("create repo");
        std::fs::create_dir_all(spoofed.join("lanes")).expect("create unrelated lanes");
        std::fs::write(spoofed.join(CURRENT_RUN_ID_FILE), b"spoofed\n")
            .expect("write spoofed marker");

        let err = validate_parallel_prune_root(&repo, &spoofed)
            .expect_err("marker must not authorize an arbitrary root");
        assert!(err
            .to_string()
            .contains("unsupported parallel prune run root"));
        assert!(spoofed.join("lanes").exists());

        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn host_lease_blocks_concurrent_prune_or_host() {
        let run_root = temp_dir("host-lease");
        let first = acquire_parallel_host_lease(&run_root, "host").expect("first lease");
        let err = acquire_parallel_host_lease(&run_root, "prune")
            .expect_err("second owner must be refused");
        assert!(err
            .to_string()
            .contains("leased by an active host or prune"));
        drop(first);
        acquire_parallel_host_lease(&run_root, "prune").expect("lease after release");

        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn explicit_prune_refuses_target_swapped_after_plan() {
        let run_root = temp_dir("target-swap");
        seed_run_artifacts(&run_root);
        let plan = parallel_prune_plan(&run_root).expect("build prune plan");
        let lanes = run_root.join("lanes");
        let original = run_root.join("lanes-original");

        let err = apply_parallel_prune_plan_with_hook(
            &run_root,
            &plan,
            false,
            "test-parallel",
            |target| {
                if target.path == lanes && lanes.exists() {
                    std::fs::rename(&lanes, &original).expect("move planned lanes");
                    std::fs::create_dir(&lanes).expect("replace lanes identity");
                    std::fs::write(lanes.join("must-survive"), b"unrelated\n")
                        .expect("write replacement");
                }
            },
        )
        .expect_err("identity swap must block deletion");

        assert!(err.to_string().contains("target identity changed"));
        assert!(lanes.join("must-survive").exists());
        assert!(original.exists());
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn explicit_prune_refuses_run_root_swapped_after_lock_and_plan() {
        let parent = temp_dir("root-swap-parent");
        let run_root = parent.join("parallel");
        std::fs::create_dir_all(&run_root).expect("create run root");
        seed_run_artifacts(&run_root);
        let plan = parallel_prune_plan(&run_root).expect("build prune plan");
        let original = parent.join("parallel-original");
        let mut swapped = false;

        let err =
            apply_parallel_prune_plan_with_hook(&run_root, &plan, false, "test-parallel", |_| {
                if !swapped {
                    std::fs::rename(&run_root, &original).expect("move planned root");
                    std::fs::create_dir(&run_root).expect("replace root");
                    seed_run_artifacts(&run_root);
                    std::fs::write(run_root.join("lanes/must-survive"), b"unrelated\n")
                        .expect("write replacement");
                    swapped = true;
                }
            })
            .expect_err("root swap must block deletion");

        assert!(
            err.to_string().contains("target identity changed")
                || err.to_string().contains("outside canonical run root")
        );
        assert!(run_root.join("lanes/must-survive").exists());
        assert!(original.join("lanes").exists());
        std::fs::remove_dir_all(&parent).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_prune_noreplace_preserves_quarantine_collision() {
        let run_root = temp_dir("quarantine-collision");
        seed_run_artifacts(&run_root);
        let plan = parallel_prune_plan(&run_root).expect("build prune plan");
        let target = plan
            .targets
            .iter()
            .find(|target| target.path.ends_with("host.stdout.log"))
            .expect("host log target");
        let mut collision = None;

        let err = remove_parallel_prune_target_with_hook(&run_root, target, |stage, quarantine| {
            if stage == PruneRemovalStage::AfterQuarantineSelection {
                std::fs::write(quarantine, b"must survive\n")
                    .expect("create destination collision");
                collision = Some(quarantine.to_path_buf());
            }
        })
        .expect_err("atomic no-replace must reject collision");

        assert!(err
            .to_string()
            .contains("failed to quarantine prune target"));
        assert!(target.path.exists(), "source must remain at original path");
        let collision = collision.expect("collision path captured");
        assert_eq!(
            std::fs::read(&collision).expect("read collision"),
            b"must survive\n"
        );
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_prune_restores_source_swapped_after_identity_check() {
        let run_root = temp_dir("post-identity-source-swap");
        seed_run_artifacts(&run_root);
        let plan = parallel_prune_plan(&run_root).expect("build prune plan");
        let target = plan
            .targets
            .iter()
            .find(|target| target.path.ends_with("host.stdout.log"))
            .expect("host log target");
        let original = run_root.join("host.stdout.original");

        let err = remove_parallel_prune_target_with_hook(&run_root, target, |stage, path| {
            if stage == PruneRemovalStage::AfterIdentityValidation {
                std::fs::rename(path, &original).expect("move planned source");
                std::fs::write(path, b"replacement must survive\n").expect("replace source");
            }
        })
        .expect_err("post-check source swap must not be deleted");

        assert!(err.to_string().contains("restored original path"));
        assert_eq!(
            std::fs::read(&target.path).expect("read restored replacement"),
            b"replacement must survive\n"
        );
        assert!(original.exists(), "planned source must also survive");
        std::fs::remove_dir_all(&run_root).ok();
    }
}
