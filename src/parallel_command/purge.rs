//! Safe automatic purge of the PREVIOUS parallel run's heavy artifacts.
//!
//! Lane worktrees, per-run worker shims, and salvage/host logs dominate the
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
//!   its lanes and salvage records for resume and forensics).
//!
//! Persistent `lane-caches/` are deliberately excluded. They are bounded at
//! assignment time and make subsequent Rust lanes materially faster.
//!
//! Opt out with `AUTO_PARALLEL_PURGE_PREVIOUS=0`.

use super::*;

const PURGEABLE_SUBDIRS: &[&str] = &["lanes", "worker-bin", "salvage"];
const PURGEABLE_LOGS: &[&str] = &["host.stdout.log", "host.stderr.log"];

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParallelPrunePlan {
    targets: Vec<PathBuf>,
    bytes: u64,
    blocked_by_run_state: bool,
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
    // A live host owns these directories; never purge while the repo's
    // parallel tmux session is running. A tmux probe error means tmux is
    // unavailable, so no tmux-hosted run can be alive either.
    let session = parallel_tmux_session_name(repo_root);
    if matches!(tmux_session_exists(&session), Ok(true)) {
        return;
    }
    let mut roots = vec![run_root.to_path_buf()];
    let legacy = repo_root.join(".auto").join("parallel");
    if legacy != *run_root {
        roots.push(legacy);
    }
    let mut freed: u64 = 0;
    for root in roots {
        if !root.exists() || root.join(".run-state.json").exists() {
            continue;
        }
        for sub in PURGEABLE_SUBDIRS {
            let path = root.join(sub);
            if !path.exists() {
                continue;
            }
            freed += dir_size_bytes(&path);
            if let Err(err) = std::fs::remove_dir_all(&path) {
                eprintln!(
                    "warning: purge-previous-run: failed removing {}: {err:#}",
                    path.display()
                );
            }
        }
        for log in PURGEABLE_LOGS {
            let path = root.join(log);
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            freed += metadata.len();
            if let Err(err) = std::fs::remove_file(&path) {
                eprintln!(
                    "warning: purge-previous-run: failed removing {}: {err:#}",
                    path.display()
                );
            }
        }
    }
    if freed > 0 {
        println!(
            "purge-previous-run: reclaimed {} of prior parallel run artifacts",
            human_bytes(freed)
        );
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
    let host_running = tmux_session_exists(&session)
        .with_context(|| format!("cannot prove parallel host `{session}` is stopped"))?;
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
    println!(
        "run state:   {}",
        if plan.blocked_by_run_state {
            "present (protected)"
        } else {
            "absent"
        }
    );
    println!("lane caches: preserved");
    if plan.targets.is_empty() {
        println!("targets:     none (0 B)");
    } else {
        println!(
            "targets:     {} ({})",
            plan.targets.len(),
            human_bytes(plan.bytes)
        );
        for target in &plan.targets {
            println!("  {}", target.display());
        }
    }

    if !args.apply {
        println!("dry-run: no files removed; pass --apply to remove listed targets");
        return Ok(());
    }
    apply_parallel_prune_plan(&run_root, &plan, host_running, &session)?;
    println!(
        "pruned:      {} ({})",
        plan.targets.len(),
        human_bytes(plan.bytes)
    );
    Ok(())
}

fn apply_parallel_prune_plan(
    run_root: &Path,
    plan: &ParallelPrunePlan,
    host_running: bool,
    session: &str,
) -> Result<()> {
    if host_running {
        bail!("refusing prune while parallel tmux host `{session}` is active");
    }
    if plan.blocked_by_run_state {
        bail!(
            "refusing prune because {} is a resumable run ledger",
            run_root.join(".run-state.json").display()
        );
    }
    for target in &plan.targets {
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
    Ok(())
}

fn parallel_prune_plan(run_root: &Path) -> Result<ParallelPrunePlan> {
    let mut targets = Vec::new();
    let mut bytes = 0u64;
    for name in PURGEABLE_SUBDIRS.iter().chain(PURGEABLE_LOGS.iter()) {
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
        bytes += if metadata.is_dir() {
            dir_size_bytes(&target)
        } else {
            metadata.len()
        };
        targets.push(target);
    }
    Ok(ParallelPrunePlan {
        targets,
        bytes,
        blocked_by_run_state: run_root.join(".run-state.json").exists(),
    })
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

fn remove_parallel_prune_target(run_root: &Path, target: &Path) -> Result<()> {
    validate_parallel_prune_target(run_root, target)?;
    let metadata = fs::symlink_metadata(target)
        .with_context(|| format!("failed to inspect prune target {}", target.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing symlinked parallel prune target: {}",
            target.display()
        );
    }
    if metadata.is_dir() {
        fs::remove_dir_all(target)
    } else {
        fs::remove_file(target)
    }
    .with_context(|| format!("failed to prune {}", target.display()))
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

    #[test]
    fn purge_removes_previous_run_artifacts_but_keeps_semantic_files() {
        let repo = temp_dir("repo");
        let run_root = temp_dir("run-root");
        seed_run_artifacts(&run_root);

        purge_previous_parallel_run_artifacts(&repo, &run_root);

        for sub in PURGEABLE_SUBDIRS {
            assert!(!run_root.join(sub).exists(), "{sub} should be purged");
        }
        for log in PURGEABLE_LOGS {
            assert!(!run_root.join(log).exists(), "{log} should be purged");
        }
        assert!(run_root.join("preflight.txt").exists());
        assert!(run_root.join("gate-holds").exists());
        assert!(run_root.join("lane-caches/lane-1/cache.bin").exists());

        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn purge_keeps_everything_when_run_state_ledger_present() {
        let repo = temp_dir("repo-unclean");
        let run_root = temp_dir("run-root-unclean");
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
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn purge_also_sweeps_legacy_in_repo_root() {
        let repo = temp_dir("repo-legacy");
        let run_root = temp_dir("run-root-legacy");
        let legacy = repo.join(".auto").join("parallel");
        std::fs::create_dir_all(&legacy).expect("create legacy root");
        seed_run_artifacts(&legacy);

        purge_previous_parallel_run_artifacts(&repo, &run_root);

        for sub in PURGEABLE_SUBDIRS {
            assert!(!legacy.join(sub).exists(), "legacy {sub} should be purged");
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
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "lanes",
                "worker-bin",
                "salvage",
                "host.stdout.log",
                "host.stderr.log"
            ]
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
            assert!(!target.exists(), "{} should be removed", target.display());
        }
        assert!(run_root.join("lane-caches/lane-1/cache.bin").exists());
        assert!(run_root.join("preflight.txt").exists());

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
}
