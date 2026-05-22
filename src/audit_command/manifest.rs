//! Audit manifest types and the resume-aware audit queue planner.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audit_command::files::{now_iso8601, sha256_hex};
use crate::util::git_stdout;
use crate::AuditResumeMode;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) started_at: String,
    pub(crate) repo_head: String,
    pub(crate) doctrine_hash: String,
    pub(crate) rubric_hash: String,
    pub(crate) files: Vec<ManifestEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ManifestEntry {
    pub(crate) path: String,
    pub(crate) status: EntryStatus,
    pub(crate) content_hash: Option<String>,
    pub(crate) audited_doctrine_hash: Option<String>,
    pub(crate) audited_rubric_hash: Option<String>,
    pub(crate) verdict: Option<String>,
    pub(crate) audited_at: Option<String>,
    pub(crate) commit: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntryStatus {
    Pending,
    Audited,
    ApplyFailed,
    Escalated,
    Skipped,
}

pub(crate) fn initial_manifest(
    repo_root: &Path,
    tracked: &[String],
    doctrine_hash: &str,
    rubric_hash: &str,
) -> Result<Manifest> {
    let head = git_stdout(repo_root, ["rev-parse", "HEAD"]).unwrap_or_default();
    Ok(Manifest {
        started_at: now_iso8601(),
        repo_head: head.trim().to_string(),
        doctrine_hash: doctrine_hash.to_string(),
        rubric_hash: rubric_hash.to_string(),
        files: tracked
            .iter()
            .map(|path| ManifestEntry {
                path: path.clone(),
                status: EntryStatus::Pending,
                content_hash: None,
                audited_doctrine_hash: None,
                audited_rubric_hash: None,
                verdict: None,
                audited_at: None,
                commit: None,
            })
            .collect(),
    })
}

/// Reconcile an existing manifest with the current tree: add new files as
/// `Pending`, drop entries whose path no longer exists.
pub(crate) fn reconcile_manifest_with_tree(
    manifest: &mut Manifest,
    tracked: &[String],
    _repo_root: &Path,
) -> Result<()> {
    let tracked_set: std::collections::HashSet<&str> = tracked.iter().map(String::as_str).collect();
    manifest
        .files
        .retain(|entry| tracked_set.contains(entry.path.as_str()));
    let existing: std::collections::HashSet<String> = manifest
        .files
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    for path in tracked {
        if !existing.contains(path) {
            manifest.files.push(ManifestEntry {
                path: path.clone(),
                status: EntryStatus::Pending,
                content_hash: None,
                audited_doctrine_hash: None,
                audited_rubric_hash: None,
                verdict: None,
                audited_at: None,
                commit: None,
            });
        }
    }
    Ok(())
}

pub(crate) fn plan_audit_queue(
    manifest: &mut Manifest,
    mode: AuditResumeMode,
    repo_root: &Path,
    doctrine_hash: &str,
    rubric_hash: &str,
) -> Result<Vec<ManifestEntry>> {
    let mut queue = Vec::new();
    for entry in &manifest.files {
        let current_content = match std::fs::read(repo_root.join(&entry.path)) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to read {}", repo_root.join(&entry.path).display())
                });
            }
        };
        let current_content_hash = sha256_hex(&current_content);
        let content_matches_last_audit =
            entry.content_hash.as_deref() == Some(current_content_hash.as_str());
        let doctrine_matches = entry
            .audited_doctrine_hash
            .as_deref()
            .map(|h| h == doctrine_hash)
            .unwrap_or(false);
        let rubric_matches = entry
            .audited_rubric_hash
            .as_deref()
            .map(|h| h == rubric_hash)
            .unwrap_or(false);
        let is_audited = matches!(entry.status, EntryStatus::Audited);
        let is_applied_failed = matches!(entry.status, EntryStatus::ApplyFailed);
        let needs_reaudit =
            is_audited && (!content_matches_last_audit || !doctrine_matches || !rubric_matches);
        match mode {
            AuditResumeMode::Fresh => queue.push(entry.clone()),
            AuditResumeMode::Resume => {
                if !is_audited || needs_reaudit || is_applied_failed {
                    queue.push(entry.clone());
                }
            }
            AuditResumeMode::OnlyDrifted => {
                if (is_audited && needs_reaudit) || is_applied_failed {
                    queue.push(entry.clone());
                }
            }
        }
    }
    Ok(queue)
}

pub(crate) fn write_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let body = serde_json::to_string_pretty(manifest)?;
    crate::util::atomic_write(path, body.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn mark_entry(
    manifest: &mut Manifest,
    path: &str,
    status: EntryStatus,
    content_hash: Option<String>,
    audited_doctrine_hash: Option<String>,
    audited_rubric_hash: Option<String>,
    verdict: Option<String>,
    commit: Option<String>,
) {
    if let Some(entry) = manifest.files.iter_mut().find(|e| e.path == path) {
        entry.status = status;
        if content_hash.is_some() {
            entry.content_hash = content_hash;
        }
        if audited_doctrine_hash.is_some() {
            entry.audited_doctrine_hash = audited_doctrine_hash;
        }
        if audited_rubric_hash.is_some() {
            entry.audited_rubric_hash = audited_rubric_hash;
        }
        if verdict.is_some() {
            entry.verdict = verdict;
        }
        if commit.is_some() {
            entry.commit = commit;
        }
        if matches!(status, EntryStatus::Audited | EntryStatus::ApplyFailed) {
            entry.audited_at = Some(now_iso8601());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{plan_audit_queue, EntryStatus, Manifest, ManifestEntry};
    use crate::audit_command::files::sha256_hex;
    use crate::AuditResumeMode;

    fn temp_repo_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "autodev-audit-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new(name: &str) -> Self {
            let path = temp_repo_path(name);
            fs::create_dir_all(&path).expect("failed to create temp dir");
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn audited_manifest(path: &str, content: &[u8]) -> Manifest {
        Manifest {
            started_at: "unix:0".to_string(),
            repo_head: "head".to_string(),
            doctrine_hash: "doctrine-new".to_string(),
            rubric_hash: "rubric-new".to_string(),
            files: vec![ManifestEntry {
                path: path.to_string(),
                status: EntryStatus::Audited,
                content_hash: Some(sha256_hex(content)),
                audited_doctrine_hash: Some("doctrine-new".to_string()),
                audited_rubric_hash: Some("rubric-new".to_string()),
                verdict: Some("CLEAN".to_string()),
                audited_at: Some("unix:0".to_string()),
                commit: None,
            }],
        }
    }

    #[test]
    fn resume_reaudits_when_file_content_hash_changes() {
        let repo = TestTempDir::new("resume-content-drift");
        fs::write(repo.path().join("README.md"), "# changed\n").expect("failed to write README");
        let mut manifest = audited_manifest("README.md", b"# old\n");

        let queue = plan_audit_queue(
            &mut manifest,
            AuditResumeMode::Resume,
            repo.path(),
            "doctrine-new",
            "rubric-new",
        )
        .expect("plan should succeed");

        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].path, "README.md");
    }

    #[test]
    fn resume_skips_when_file_content_and_prompts_match() {
        let repo = TestTempDir::new("resume-no-drift");
        fs::write(repo.path().join("README.md"), "# same\n").expect("failed to write README");
        let mut manifest = audited_manifest("README.md", b"# same\n");

        let queue = plan_audit_queue(
            &mut manifest,
            AuditResumeMode::Resume,
            repo.path(),
            "doctrine-new",
            "rubric-new",
        )
        .expect("plan should succeed");

        assert!(queue.is_empty());
    }

    #[test]
    fn resume_skips_manifest_entries_for_deleted_files() {
        let repo = TestTempDir::new("resume-deleted-file");
        let mut manifest = audited_manifest("deleted.md", b"# old\n");

        let queue = plan_audit_queue(
            &mut manifest,
            AuditResumeMode::Resume,
            repo.path(),
            "doctrine-new",
            "rubric-new",
        )
        .expect("plan should tolerate deleted manifest entries");

        assert!(queue.is_empty());
    }
}
