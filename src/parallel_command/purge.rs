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
const MANAGED_RUNTIME_SUBDIRS: &[&str] = &[
    "lanes",
    "worker-bin",
    "salvage",
    "lane-caches",
    "shared-cargo-target",
    "cargo-target",
    "host-cargo-target",
];
const AUTHORITY_RUNTIME_SUBDIRS: &[&str] =
    &["gate-holds", "verified-source", "reviewer-local-only"];
const MANAGED_RUNTIME_FILES: &[&str] = &[
    ".run-state.json",
    ".current-run-id",
    ".completed-drift-sweep",
    ".workspace-baseline.json",
    "host.stdout.log",
    "host.stderr.log",
    "live.log",
    "preflight.txt",
    "operator-actions.md",
    "review-input-quarantine.json",
    "receipt-handoff.lock",
];

fn purge_previous_run_enabled() -> bool {
    std::env::var("AUTO_PARALLEL_PURGE_PREVIOUS")
        .map(|value| value.trim() != "0")
        .unwrap_or(true)
}

/// Purge the previous run's heavy artifacts from the selected run root and the
/// legacy in-repo authority root, when distinct.
///
/// The caller must already hold both marker-bound host leases. Structural
/// validation always runs, even when purge is disabled, because later gates
/// still read and write authority files under the legacy root.
#[cfg(unix)]
pub(crate) fn purge_previous_parallel_run_artifacts(
    repo_root: &Path,
    authority: &ParallelRunRootAuthority,
) -> Result<()> {
    purge_previous_parallel_run_artifacts_with_enabled(
        repo_root,
        authority,
        purge_previous_run_enabled(),
    )
}

#[cfg(unix)]
fn purge_previous_parallel_run_artifacts_with_enabled(
    _repo_root: &Path,
    authority: &ParallelRunRootAuthority,
    purge_enabled: bool,
) -> Result<()> {
    validate_parallel_startup_roots(authority)?;
    if !purge_enabled {
        return Ok(());
    }
    let mut freed: u64 = 0;
    authority.revalidate_authority()?;
    if authority.has_valid_regular_file(".run-state.json")? {
        for sub in PURGEABLE_SUBDIRS {
            authority.validate_expected_directory_or_absent(sub)?;
        }
        authority.revalidate_authority()?;
        return Ok(());
    }
    for sub in PURGEABLE_SUBDIRS {
        freed = freed.saturating_add(authority.clear_expected_directory(sub)?);
        authority.revalidate_authority()?;
    }
    for log in PURGEABLE_LOGS {
        freed = freed.saturating_add(authority.remove_expected_file(log)?);
        authority.revalidate_authority()?;
    }
    if freed > 0 {
        println!(
            "purge-previous-run: reclaimed {} of prior parallel run artifacts",
            human_bytes(freed)
        );
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn validate_parallel_startup_roots(
    authority: &ParallelRunRootAuthority,
) -> Result<()> {
    authority.revalidate_authority()?;
    for directory in MANAGED_RUNTIME_SUBDIRS {
        authority
            .validate_expected_directory_or_absent(directory)
            .with_context(|| {
                format!(
                    "refusing structurally unsafe parallel authority tree {}",
                    authority.path().display()
                )
            })?;
    }
    for directory in AUTHORITY_RUNTIME_SUBDIRS {
        authority
            .validate_expected_directory_tree_or_absent(directory)
            .with_context(|| {
                format!(
                    "refusing recursively unsafe parallel authority tree {}",
                    authority.path().display()
                )
            })?;
    }
    for file in MANAGED_RUNTIME_FILES {
        authority
            .validate_expected_regular_file_or_absent(file)
            .with_context(|| {
                format!(
                    "refusing structurally unsafe parallel authority tree {}",
                    authority.path().display()
                )
            })?;
    }
    authority.revalidate_authority()
}

#[cfg(not(unix))]
pub(crate) fn purge_previous_parallel_run_artifacts(
    _repo_root: &Path,
    _authority: &ParallelRunRootAuthority,
) -> Result<()> {
    bail!("prior-run purge requires Unix descriptor-relative no-follow deletion")
}

/*
 * Deliberately no path-based recursive size/delete helper lives here. Parallel
 * purge and persistent-cache pruning operate from secured directory
 * descriptors so an internal symlink or path replacement cannot redirect
 * traversal outside the selected run root.
 */

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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
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
        #[cfg(unix)]
        {
            let mut permissions = std::fs::metadata(&path)
                .expect("stat temp dir")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).expect("secure temp dir");
        }
        path
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        let mut permissions = std::fs::metadata(path)
            .unwrap_or_else(|_| panic!("stat {}", path.display()))
            .permissions();
        permissions.set_mode(mode);
        std::fs::set_permissions(path, permissions)
            .unwrap_or_else(|_| panic!("chmod {}", path.display()));
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

    #[cfg(unix)]
    #[test]
    fn purge_removes_previous_run_artifacts_but_keeps_semantic_files() {
        let repo = temp_dir("repo");
        let run_root = temp_dir("run-root");
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim fresh run root");
        seed_run_artifacts(&run_root);

        purge_previous_parallel_run_artifacts(&repo, &authority)
            .expect("purge secured prior run");

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

    #[cfg(unix)]
    #[test]
    fn purge_keeps_everything_when_run_state_ledger_present() {
        let repo = temp_dir("repo-unclean");
        let run_root = temp_dir("run-root-unclean");
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim fresh run root");
        seed_run_artifacts(&run_root);
        std::fs::write(run_root.join(".run-state.json"), b"{}").expect("write ledger");
        set_mode(&run_root.join(".run-state.json"), 0o644);

        purge_previous_parallel_run_artifacts(&repo, &authority)
            .expect("retain secured unfinished run");

        for sub in PURGEABLE_SUBDIRS {
            assert!(
                run_root.join(sub).exists(),
                "{sub} must survive an unclean prior run"
            );
        }

        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn purge_never_claims_or_sweeps_an_unmarked_legacy_repo_root() {
        let repo = temp_dir("repo-legacy");
        let run_root = temp_dir("run-root-legacy");
        let legacy = repo.join(".auto").join("parallel");
        std::fs::create_dir_all(&legacy).expect("create legacy root");
        set_mode(&repo.join(".auto"), 0o755);
        set_mode(&legacy, 0o755);
        seed_run_artifacts(&legacy);
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim active root only");

        purge_previous_parallel_run_artifacts(&repo, &authority)
            .expect("purge selected root only");

        for sub in PURGEABLE_SUBDIRS {
            assert!(
                legacy.join(sub).exists(),
                "unmarked legacy {sub} must not be purged"
            );
        }

        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn purge_unlinks_internal_symlink_without_touching_external_directory() {
        let root = temp_dir("internal-symlink");
        let repo = root.join("repo");
        let run_root = root.join("run");
        let outside = root.join("outside");
        std::fs::create_dir_all(&repo).expect("create repo");
        std::fs::create_dir_all(&outside).expect("create outside");
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim fresh run root");
        std::fs::create_dir_all(run_root.join("lanes/lane-1")).expect("create lane");
        std::fs::write(outside.join("sentinel.bin"), b"external").expect("write sentinel");
        std::os::unix::fs::symlink(&outside, run_root.join("lanes/lane-1/pivot"))
            .expect("create internal symlink");

        purge_previous_parallel_run_artifacts(&repo, &authority)
            .expect("descriptor-relative purge");

        assert_eq!(
            std::fs::read(outside.join("sentinel.bin")).expect("read sentinel"),
            b"external"
        );
        assert!(!run_root.join("lanes").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn purge_rejects_symlinked_managed_subdir_without_touching_external_directory() {
        let root = temp_dir("subdir-symlink");
        let repo = root.join("repo");
        let run_root = root.join("run");
        let outside = root.join("outside");
        std::fs::create_dir_all(&repo).expect("create repo");
        std::fs::create_dir_all(&outside).expect("create outside");
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim fresh run root");
        std::fs::write(outside.join("sentinel.bin"), b"external").expect("write sentinel");
        std::os::unix::fs::symlink(&outside, run_root.join("lanes"))
            .expect("create managed symlink");

        let error = purge_previous_parallel_run_artifacts(&repo, &authority)
            .expect_err("active managed symlink must abort");
        assert!(
            format!("{error:#}").contains("real no-follow directory"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(outside.join("sentinel.bin")).expect("read sentinel"),
            b"external"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn retained_run_rejects_symlinked_managed_subdir() {
        let root = temp_dir("retained-subdir-symlink");
        let repo = root.join("repo");
        let run_root = root.join("run");
        let outside = root.join("outside");
        std::fs::create_dir_all(&repo).expect("create repo");
        std::fs::create_dir_all(&outside).expect("create outside");
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim fresh run root");
        std::fs::write(run_root.join(".run-state.json"), b"{}").expect("write run state");
        set_mode(&run_root.join(".run-state.json"), 0o644);
        std::fs::write(outside.join("sentinel.bin"), b"external").expect("write sentinel");
        std::os::unix::fs::symlink(&outside, run_root.join("lane-caches"))
            .expect("create retained managed symlink");

        let error = purge_previous_parallel_run_artifacts(&repo, &authority)
            .expect_err("retained managed symlink must abort");
        assert!(
            format!("{error:#}").contains("real no-follow directory"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(outside.join("sentinel.bin")).expect("read sentinel"),
            b"external"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn purge_rejects_symlinked_run_state_before_retaining_managed_paths() {
        let root = temp_dir("run-state-symlink");
        let repo = root.join("repo");
        let run_root = root.join("run");
        let outside = root.join("outside-state.json");
        std::fs::create_dir_all(&repo).expect("create repo");
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim fresh run root");
        std::fs::create_dir_all(run_root.join("lanes/lane-1")).expect("create run root");
        std::fs::write(&outside, b"{}").expect("write external state");
        std::os::unix::fs::symlink(&outside, run_root.join(".run-state.json"))
            .expect("symlink run state");

        let error = purge_previous_parallel_run_artifacts(&repo, &authority)
            .expect_err("symlinked run state must abort");
        assert!(
            format!("{error:#}").contains("regular no-follow file"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(&outside).expect("external state survives"),
            b"{}"
        );
        assert!(run_root.join("lanes/lane-1").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn purge_rejects_symlinked_active_run_root_without_touching_external_directory() {
        let root = temp_dir("root-symlink");
        let repo = root.join("repo");
        let outside = root.join("outside");
        let run_root = root.join("run");
        std::fs::create_dir_all(&repo).expect("create repo");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::fs::write(outside.join("sentinel.bin"), b"external").expect("write sentinel");
        std::os::unix::fs::symlink(&outside, &run_root).expect("symlink active root");

        let error = ParallelRunRootAuthority::acquire(&repo, &run_root)
            .expect_err("symlinked active root must abort");
        assert!(format!("{error:#}").contains("symlink"), "{error:#}");
        assert_eq!(
            std::fs::read(outside.join("sentinel.bin")).expect("read sentinel"),
            b"external"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn startup_refuses_to_claim_a_nonempty_unmarked_explicit_run_root() {
        let root = temp_dir("home-like-explicit-root");
        let repo = root.join("repo");
        let home_like_run_root = root.join("home");
        std::fs::create_dir_all(&repo).expect("create repo");
        std::fs::create_dir_all(home_like_run_root.join("lanes"))
            .expect("create unrelated home-like lanes directory");
        std::fs::write(
            home_like_run_root.join("lanes/operator-notes.txt"),
            b"must survive\n",
        )
        .expect("write unrelated file");
        set_mode(&home_like_run_root, 0o700);

        let error = ParallelRunRootAuthority::acquire(&repo, &home_like_run_root)
            .expect_err("an unmarked nonempty explicit directory must not be claimed");

        assert!(
            format!("{error:#}").contains("ownership marker"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(home_like_run_root.join("lanes/operator-notes.txt"))
                .expect("unrelated file must survive"),
            b"must survive\n"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn startup_refuses_a_second_host_for_the_same_run_root() {
        let root = temp_dir("double-startup");
        let repo = root.join("repo");
        let run_root = root.join("run");
        std::fs::create_dir_all(&repo).expect("create repo");

        let first = ParallelRunRootAuthority::acquire(&repo, &run_root)
            .expect("first host acquires run root");
        let error = ParallelRunRootAuthority::acquire(&repo, &run_root)
            .expect_err("second host must not share the run root");

        assert!(format!("{error:#}").contains("host lease"), "{error:#}");
        drop(first);
        ParallelRunRootAuthority::acquire(&repo, &run_root)
            .expect("lease must release when the first host exits");
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn startup_rejects_a_corrupt_run_root_ownership_marker() {
        let root = temp_dir("corrupt-marker");
        let repo = root.join("repo");
        let run_root = root.join("run");
        std::fs::create_dir_all(&repo).expect("create repo");
        drop(
            ParallelRunRootAuthority::acquire(&repo, &run_root)
                .expect("initialize marked run root"),
        );
        std::fs::write(
            run_root.join(".autodev-parallel-root.json"),
            b"{\"format\":\"forged\"}\n",
        )
        .expect("corrupt marker");

        let error = ParallelRunRootAuthority::acquire(&repo, &run_root)
            .expect_err("corrupt marker must fail closed");

        assert!(
            format!("{error:#}").contains("ownership marker"),
            "{error:#}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn purge_disabled_still_rejects_a_symlinked_managed_entry() {
        let root = temp_dir("purge-disabled-managed-symlink");
        let repo = root.join("repo");
        let run_root = root.join("run");
        let outside = root.join("outside");
        std::fs::create_dir_all(&repo).expect("create repo");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::fs::write(outside.join("sentinel"), b"survives").expect("write sentinel");
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim fresh run root");
        std::os::unix::fs::symlink(&outside, run_root.join("lanes"))
            .expect("install managed symlink");

        let error =
            purge_previous_parallel_run_artifacts_with_enabled(&repo, &authority, false)
                .expect_err("disabled purge must still validate managed entries");

        assert!(
            format!("{error:#}").contains("real no-follow directory"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(outside.join("sentinel")).expect("read sentinel"),
            b"survives"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn startup_fails_closed_on_nested_symlink_in_selected_authority_tree() {
        let root = temp_dir("selected-authority-corruption");
        let repo = root.join("repo");
        let run_root = root.join("external-run");
        let outside = root.join("outside");
        std::fs::create_dir_all(&repo).expect("create repo");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::fs::write(outside.join("sentinel"), b"survives").expect("write sentinel");
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim selected root");
        std::fs::create_dir_all(run_root.join("gate-holds"))
            .expect("create selected authority directory");
        std::os::unix::fs::symlink(&outside, run_root.join("gate-holds/TASK-1.hold"))
            .expect("install nested corrupt selected authority entry");

        let error =
            purge_previous_parallel_run_artifacts_with_enabled(&repo, &authority, false)
                .expect_err("selected authority corruption must abort startup");

        assert!(
            format!("{error:#}").contains("gate-holds"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(outside.join("sentinel")).expect("read sentinel"),
            b"survives"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn startup_rejects_a_nested_special_file_in_an_authority_subtree() {
        let root = temp_dir("authority-special-file");
        let repo = root.join("repo");
        let run_root = root.join("external-run");
        std::fs::create_dir_all(&repo).expect("create repo");
        let authority =
            ParallelRunRootAuthority::acquire(&repo, &run_root).expect("claim selected root");
        let verified_source = run_root.join("verified-source");
        std::fs::create_dir_all(&verified_source).expect("create authority subtree");
        let socket_path = verified_source.join("TASK-1.socket");
        let _listener =
            std::os::unix::net::UnixListener::bind(&socket_path).expect("bind special file");

        let error =
            purge_previous_parallel_run_artifacts_with_enabled(&repo, &authority, false)
                .expect_err("special authority entry must abort startup");

        assert!(
            format!("{error:#}").contains("directory or regular file"),
            "{error:#}"
        );
        drop(_listener);
        std::fs::remove_dir_all(&root).ok();
    }
}
