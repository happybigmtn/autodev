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
//! Opt out with `AUTO_PARALLEL_PURGE_PREVIOUS=0`.

use super::*;

const PURGEABLE_SUBDIRS: &[&str] = &["lanes", "worker-bin", "salvage", "lane-caches"];
const PURGEABLE_LOGS: &[&str] = &["host.stdout.log", "host.stderr.log"];

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

fn dir_size_bytes(path: &Path) -> u64 {
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

fn human_bytes(bytes: u64) -> String {
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
}
