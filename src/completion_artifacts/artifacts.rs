//! Declared completion-artifact resolution and content hashing.

use std::fs;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::completion_artifacts::receipt::verification_receipt_root;
use crate::task_parser::parse_tasks as parse_shared_tasks;

pub(crate) fn declared_completion_artifacts(task_markdown: &str) -> Vec<String> {
    parse_shared_tasks(task_markdown)
        .into_iter()
        .next()
        .map(|task| task.completion_artifacts)
        .unwrap_or_default()
}

pub(crate) fn current_declared_artifact_hashes(
    repo_root: &Path,
    verification_receipt_path: &Path,
    declared_artifacts: &[String],
) -> Vec<(String, String)> {
    declared_artifacts
        .iter()
        .filter_map(|relative| {
            if declared_artifact_hash_is_mutable_handoff(relative) {
                return None;
            }
            let path = declared_artifact_path(repo_root, relative)?;
            if same_path(&path, verification_receipt_path) {
                return None;
            }
            artifact_hash(&path).map(|hash| (relative.clone(), hash))
        })
        .collect()
}

fn declared_artifact_hash_is_mutable_handoff(relative: &str) -> bool {
    matches!(
        relative,
        "REVIEW.md"
            | "IMPLEMENTATION_PLAN.md"
            | "COMPLETED.md"
            | "WORKLIST.md"
            | "ARCHIVED.md"
            | "RECEIPTS-DRIFT.md"
    )
}

pub(crate) fn declared_artifact_path(repo_root: &Path, relative: &str) -> Option<PathBuf> {
    if !declared_artifact_relative_path_is_safe(relative) {
        return None;
    }
    let direct = repo_root.join(relative);
    if direct.exists() {
        return Some(direct);
    }
    relative
        .strip_prefix(".auto/symphony/verification-receipts/")
        .map(|file_name| verification_receipt_root(repo_root).join(file_name))
        .filter(|path| path.exists())
}

fn declared_artifact_relative_path_is_safe(relative: &str) -> bool {
    let path = Path::new(relative);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

pub(crate) fn artifact_hash(path: &Path) -> Option<String> {
    if path.is_file() {
        return fs::read(path).ok().map(|bytes| sha256_hex(&bytes));
    }
    if !path.is_dir() {
        return None;
    }

    let mut entries = Vec::new();
    collect_artifact_dir_entries(path, path, &mut entries).ok()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, hash) in entries {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(hash.as_bytes());
        hasher.update([0]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

fn collect_artifact_dir_entries(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<(String, String)>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_artifact_dir_entries(root, &path, entries)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let hash = sha256_hex(&fs::read(&path)?);
            entries.push((relative, hash));
        }
    }
    Ok(())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::declared_completion_artifacts;

    fn temp_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "autodev-completion-artifacts-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    #[test]
    fn declared_completion_artifacts_extracts_repo_relative_paths() {
        let markdown = r#"- [ ] `TASK-1` Example
Completion artifacts:
  - `docs/ops/proof.md`
  - .auto/local-proof.json -- emitted by helper
Dependencies: none
"#;
        assert_eq!(
            declared_completion_artifacts(markdown),
            vec![
                "docs/ops/proof.md".to_string(),
                ".auto/local-proof.json".to_string()
            ]
        );
    }

    #[test]
    fn completion_artifact_paths_reject_parent_escape() {
        let root = temp_dir("artifact-path-escape");
        fs::create_dir_all(root.join("docs")).expect("failed to create docs");
        fs::write(root.join("docs/proof.md"), "proof\n").expect("failed to write proof");
        let outside = root.parent().unwrap().join("outside-proof.md");
        fs::write(&outside, "outside\n").expect("failed to write outside proof");

        assert!(super::declared_artifact_path(&root, "docs/proof.md").is_some());
        assert!(super::declared_artifact_path(&root, "../outside-proof.md").is_none());
        assert!(super::declared_artifact_path(&root, outside.to_str().unwrap()).is_none());

        fs::remove_dir_all(root).ok();
        fs::remove_file(outside).ok();
    }

    #[test]
    fn directory_artifact_hashing_respects_documented_limit() {
        let schema = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/verification-receipt-schema.md"),
        )
        .expect("schema should exist");
        assert!(schema.contains("Directory Hash Limit"));

        let root = temp_dir("directory-artifact-hash");
        fs::create_dir_all(root.join("artifact/sub")).expect("failed to create artifact dir");
        fs::write(root.join("artifact/sub/proof.txt"), "proof\n")
            .expect("failed to write artifact");
        let first =
            super::artifact_hash(&root.join("artifact")).expect("directory hash should compute");
        fs::write(root.join("artifact/sub/proof.txt"), "proof changed\n")
            .expect("failed to update artifact");
        let second =
            super::artifact_hash(&root.join("artifact")).expect("directory hash should compute");
        assert_ne!(first, second);
        fs::remove_dir_all(root).ok();
    }
}
