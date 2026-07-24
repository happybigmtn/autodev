//! Per-task "owned inputs" fingerprint (`task_owned_inputs_v1`).
//!
//! The whole-repo drift-sweep fingerprint (HEAD + `git status`) is a coarse
//! gate: it changes whenever *anything* in the tree moves, forcing the drift
//! audit to reconsider every `[x]` row even when none of a given task's own
//! inputs moved. This module narrows the signal to exactly what a task's
//! verification can legitimately depend on:
//!
//!   (a) the task's own NORMALIZED contract (its plan-row markdown, with the
//!       status checkbox neutralized so a `[x]`/`[~]` flip is not a change),
//!   (b) the task's `Owns:` paths,
//!   (c) its direct dependency task IDs, and
//!   (d) the union of each direct dependency's `Owns:` paths plus their
//!       non-receipt completion-artifact paths.
//!
//! To keep drift detection honest the task's OWN declared completion-artifact
//! paths are content-addressed too (a superset of the design's item (b)) so a
//! declared-artifact drift can never be silently trusted.
//!
//! All paths are content-addressed via git enumeration (tracked + untracked,
//! respecting `.gitignore`) so file names, contents, executable/symlink modes,
//! deletions, untracked files, refs, and submodule gitlink commits all fold in,
//! while UNRELATED repo paths are structurally absent from the hash.
//!
//! Conservative on failure: any git/enumeration error yields `None`, which the
//! caller treats as "changed" (force re-verify), never as "unchanged".

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use super::artifacts::sha256_hex;
use super::receipt::normalize_plan_status_markers;
use crate::task_parser::parse_owns_paths;

/// Version tag folded into every fingerprint. Bump only when the hashing scheme
/// changes in a way that must invalidate previously-stamped fingerprints.
const OWNED_INPUTS_SCHEME: &str = "task-owned-inputs-v1";

/// Paths that must never contribute to a per-task fingerprint: the receipt /
/// run-state store (self-reference would make stamping change the hash) and the
/// mutable host-handoff docs whose churn is unrelated to a task's verification
/// inputs. Matched against repo-relative paths.
fn path_is_fingerprint_excluded(rel: &str) -> bool {
    matches!(
        rel,
        "IMPLEMENTATION_PLAN.md"
            | "REVIEW.md"
            | "RECEIPTS-DRIFT.md"
            | "COMPLETED.md"
            | "WORKLIST.md"
            | "ARCHIVED.md"
    ) || rel.starts_with(".auto/symphony/verification-receipts/")
        || rel.starts_with(".git/")
}

/// Compute the `task_owned_inputs_v1` fingerprint for `task_id` against the
/// current working tree. `all_tasks` is the parsed plan (used to resolve the
/// task and its direct dependencies). Returns `None` when the task is absent
/// from the plan or any git enumeration fails — the caller treats `None`
/// conservatively as "inputs changed".
pub(crate) fn compute_task_owned_inputs_fingerprint(
    repo_root: &Path,
    task_id: &str,
    all_tasks: &[crate::task_parser::PlanTask],
) -> Option<String> {
    let task = all_tasks.iter().find(|task| task.id == task_id)?;

    // (a) normalized contract: the task's own plan-row markdown, checkbox-
    // neutralized so a status flip never invalidates the fingerprint.
    let normalized_contract = normalize_plan_status_markers(&task.markdown);

    // (c) direct dependency IDs (sorted, deduped for determinism).
    let mut dep_ids: Vec<String> = task.dependencies.clone();
    dep_ids.sort();
    dep_ids.dedup();

    // Path set to content-address:
    //   task Owns + task completion-artifacts (self, to preserve declared-
    //   artifact drift detection) + each dep's Owns + dep's completion-artifacts.
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for owned in parse_owns_paths(&task.markdown) {
        paths.insert(owned);
    }
    for artifact in &task.completion_artifacts {
        paths.insert(artifact.clone());
    }
    for dep_id in &dep_ids {
        if let Some(dep) = all_tasks.iter().find(|task| &task.id == dep_id) {
            for owned in parse_owns_paths(&dep.markdown) {
                paths.insert(owned);
            }
            for artifact in &dep.completion_artifacts {
                paths.insert(artifact.clone());
            }
        }
    }

    let path_digest = content_address_paths(repo_root, &paths)?;

    let mut hasher = Sha256::new();
    hasher.update(OWNED_INPUTS_SCHEME.as_bytes());
    hasher.update([0]);
    hasher.update(task_id.as_bytes());
    hasher.update([0]);
    hasher.update(b"contract\0");
    hasher.update(normalized_contract.as_bytes());
    hasher.update([0]);
    hasher.update(b"deps\0");
    for dep_id in &dep_ids {
        hasher.update(dep_id.as_bytes());
        hasher.update([0]);
    }
    hasher.update(b"paths\0");
    hasher.update(path_digest.as_bytes());
    Some(format!("{:x}", hasher.finalize()))
}

/// Content-address a set of declared repo-relative paths. Deterministic: every
/// tracked and untracked-non-ignored file under each declared path contributes
/// `(path, mode, content-hash)`, plus a per-declared-path presence marker so an
/// absent<->present transition of a whole directory is also captured. Refs
/// (`refs/...`) resolve to their target commit; submodule gitlinks fold in the
/// recorded submodule commit without descending. Returns `None` on any git
/// failure (conservative "changed").
fn content_address_paths(repo_root: &Path, paths: &BTreeSet<String>) -> Option<String> {
    // (mode, path, content) tuples, deduplicated + ordered by BTreeSet.
    let mut entries: BTreeSet<(String, String, String)> = BTreeSet::new();

    for declared in paths {
        if path_is_fingerprint_excluded(declared) {
            continue;
        }
        if declared.starts_with("refs/") {
            let resolved =
                git_rev_parse_ref(repo_root, declared).unwrap_or_else(|| "absent".to_string());
            entries.insert(("ref".to_string(), declared.clone(), resolved));
            continue;
        }

        // Per-declared-path presence marker.
        let present = repo_root.join(declared).exists();
        entries.insert((
            "declared".to_string(),
            declared.clone(),
            if present { "present" } else { "absent" }.to_string(),
        ));

        // Tracked files under the declared path (index view: mode + object).
        for (mode, rel) in git_ls_tracked(repo_root, declared)? {
            if path_is_fingerprint_excluded(&rel) {
                continue;
            }
            if mode == "160000" {
                // Submodule gitlink: the ls-files object hash IS the recorded
                // submodule commit. Fold it in without descending.
                let sub_commit =
                    git_ls_tracked_object(repo_root, &rel).unwrap_or_else(|| "gitlink".to_string());
                entries.insert((mode, rel, sub_commit));
            } else {
                let content = worktree_content_hash(repo_root, &rel);
                entries.insert((mode, rel, content));
            }
        }

        // Untracked, non-ignored files under the declared path.
        for rel in git_ls_untracked(repo_root, declared)? {
            if path_is_fingerprint_excluded(&rel) {
                continue;
            }
            let mode = worktree_mode(repo_root, &rel);
            let content = worktree_content_hash(repo_root, &rel);
            entries.insert((mode, rel, content));
        }
    }

    let mut hasher = Sha256::new();
    for (mode, path, content) in &entries {
        hasher.update(mode.as_bytes());
        hasher.update([0]);
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(content.as_bytes());
        hasher.update([0]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// `git ls-files -s -z -- <path>` → `(mode, relative-path)` for tracked entries.
/// `None` on git failure.
fn git_ls_tracked(repo_root: &Path, pathspec: &str) -> Option<Vec<(String, String)>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-s", "-z", "--", pathspec])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut entries = Vec::new();
    for record in output.stdout.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(record);
        // Format: "<mode> <object> <stage>\t<path>"
        let Some((meta, path)) = text.split_once('\t') else {
            continue;
        };
        let mode = meta
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        entries.push((mode, path.to_string()));
    }
    Some(entries)
}

/// The recorded object hash (submodule commit for a gitlink) of a single
/// tracked path.
fn git_ls_tracked_object(repo_root: &Path, pathspec: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-s", "-z", "--", pathspec])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let record = output
        .stdout
        .split(|byte| *byte == 0)
        .find(|r| !r.is_empty())?;
    let text = String::from_utf8_lossy(record);
    let meta = text.split_once('\t')?.0;
    meta.split_whitespace().nth(1).map(str::to_string)
}

/// `git ls-files --others --exclude-standard -z -- <path>` → untracked,
/// non-ignored relative paths. `None` on git failure.
fn git_ls_untracked(repo_root: &Path, pathspec: &str) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            pathspec,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .map(|record| String::from_utf8_lossy(record).to_string())
            .collect(),
    )
}

fn git_rev_parse_ref(repo_root: &Path, reference: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--verify", "--quiet", reference])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!resolved.is_empty()).then_some(resolved)
}

/// Content hash of a working-tree file. Follows symlinks; a missing/unreadable
/// file (e.g. deleted in the worktree) hashes to a stable `deleted` marker so
/// the deletion itself is a detectable change.
fn worktree_content_hash(repo_root: &Path, rel: &str) -> String {
    let path = repo_root.join(rel);
    match std::fs::read(&path) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(_) => "deleted".to_string(),
    }
}

/// A coarse mode string for an untracked file: `100755` if any executable bit is
/// set, else `100644`. (Tracked files take their mode straight from git.)
fn worktree_mode(repo_root: &Path, rel: &str) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::symlink_metadata(repo_root.join(rel)) {
            if meta.file_type().is_symlink() {
                return "120000".to_string();
            }
            if meta.permissions().mode() & 0o111 != 0 {
                return "100755".to_string();
            }
        }
        "100644".to_string()
    }
    #[cfg(not(unix))]
    {
        let _ = (repo_root, rel);
        "100644".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::compute_task_owned_inputs_fingerprint;
    use crate::task_parser::parse_tasks;

    fn temp_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "autodev-owned-inputs-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git invocation failed");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(root: &Path) {
        git(root, &["init", "-q"]);
        git(root, &["config", "user.email", "t@example.com"]);
        git(root, &["config", "user.name", "t"]);
    }

    fn plan_markdown() -> &'static str {
        "\
- [x] `TASK-A` Producer
  Spec: build the lib
  Owns: `crates/a/src`
  Verification:
    - `cargo test -p a`
  Completion artifacts: none
  Dependencies: none

- [x] `TASK-B` Consumer
  Spec: consume the lib
  Owns: `crates/b/src`
  Verification:
    - `cargo test -p b`
  Completion artifacts: none
  Dependencies: `TASK-A`
"
    }

    fn fingerprint(root: &Path, task_id: &str) -> String {
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        let tasks = parse_tasks(&plan);
        compute_task_owned_inputs_fingerprint(root, task_id, &tasks)
            .expect("fingerprint should compute")
    }

    fn seed_repo(name: &str) -> PathBuf {
        let root = temp_dir(name);
        init_repo(&root);
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), plan_markdown()).unwrap();
        fs::create_dir_all(root.join("crates/a/src")).unwrap();
        fs::create_dir_all(root.join("crates/b/src")).unwrap();
        fs::write(root.join("crates/a/src/lib.rs"), "pub fn a() {}\n").unwrap();
        fs::write(root.join("crates/b/src/lib.rs"), "pub fn b() {}\n").unwrap();
        fs::write(root.join("unrelated.rs"), "// noise\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "seed"]);
        root
    }

    #[test]
    fn fingerprint_stable_when_unrelated_file_changes() {
        let root = seed_repo("unrelated");
        let before = fingerprint(&root, "TASK-B");
        // Touch a file OUTSIDE TASK-B's owned/dep inputs.
        fs::write(root.join("unrelated.rs"), "// noise changed\n").unwrap();
        fs::create_dir_all(root.join("crates/c/src")).unwrap();
        fs::write(root.join("crates/c/src/lib.rs"), "pub fn c() {}\n").unwrap();
        let after = fingerprint(&root, "TASK-B");
        assert_eq!(
            before, after,
            "unrelated changes must not move the fingerprint"
        );
    }

    #[test]
    fn fingerprint_changes_when_owned_file_changes() {
        let root = seed_repo("owned-change");
        let before = fingerprint(&root, "TASK-B");
        fs::write(
            root.join("crates/b/src/lib.rs"),
            "pub fn b() { /* edit */ }\n",
        )
        .unwrap();
        let after = fingerprint(&root, "TASK-B");
        assert_ne!(
            before, after,
            "an owned-file change must move the fingerprint"
        );
    }

    #[test]
    fn fingerprint_changes_on_untracked_and_deleted_owned_files() {
        let root = seed_repo("untracked-deleted");
        let before = fingerprint(&root, "TASK-B");
        fs::write(root.join("crates/b/src/extra.rs"), "pub fn extra() {}\n").unwrap();
        let with_untracked = fingerprint(&root, "TASK-B");
        assert_ne!(
            before, with_untracked,
            "untracked owned file must move fingerprint"
        );
        fs::remove_file(root.join("crates/b/src/extra.rs")).unwrap();
        fs::remove_file(root.join("crates/b/src/lib.rs")).unwrap();
        let with_deletion = fingerprint(&root, "TASK-B");
        assert_ne!(
            before, with_deletion,
            "deleting an owned file must move fingerprint"
        );
    }

    #[test]
    fn dependency_output_change_invalidates_dependent() {
        let root = seed_repo("dep-output");
        let before = fingerprint(&root, "TASK-B");
        // Change TASK-A's owned file — TASK-B depends on TASK-A.
        fs::write(
            root.join("crates/a/src/lib.rs"),
            "pub fn a() { /* v2 */ }\n",
        )
        .unwrap();
        let after = fingerprint(&root, "TASK-B");
        assert_ne!(
            before, after,
            "a dependency's owned-output change must move the dependent's fingerprint"
        );
    }

    #[test]
    fn contract_edit_changes_fingerprint_but_checkbox_flip_does_not() {
        let root = seed_repo("contract-edit");
        let done = fingerprint(&root, "TASK-B");
        // Flip TASK-B's checkbox to [~]; the fingerprint must be stable.
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        let flipped = plan.replace("- [x] `TASK-B`", "- [~] `TASK-B`");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), &flipped).unwrap();
        let partial = fingerprint(&root, "TASK-B");
        assert_eq!(
            done, partial,
            "a checkbox flip must not move the fingerprint"
        );
        // A genuine spec edit must move it.
        let edited = flipped.replace("consume the lib", "consume the lib carefully");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), &edited).unwrap();
        let after_edit = fingerprint(&root, "TASK-B");
        assert_ne!(
            partial, after_edit,
            "a contract edit must move the fingerprint"
        );
    }

    #[test]
    fn unknown_task_yields_none() {
        let root = seed_repo("unknown");
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        let tasks = parse_tasks(&plan);
        assert!(compute_task_owned_inputs_fingerprint(&root, "TASK-MISSING", &tasks).is_none());
    }

    #[test]
    fn non_git_dir_yields_none() {
        let root = temp_dir("no-git");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), plan_markdown()).unwrap();
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        let tasks = parse_tasks(&plan);
        assert!(
            compute_task_owned_inputs_fingerprint(&root, "TASK-B", &tasks).is_none(),
            "a git-enumeration failure must yield None (conservative changed)"
        );
    }
}
