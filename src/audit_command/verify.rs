//! `auto audit --verify-findings`: independent closure verification for every
//! flagged file in the manifest.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::audit_command::files::{file_artifact_dir, now_iso8601, sha256_hex};
use crate::audit_command::manifest::{EntryStatus, Manifest, ManifestEntry};
use crate::audit_command::FileVerdict;
use crate::util::atomic_write;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FindingVerificationReport {
    generated_at: String,
    manifest_path: String,
    pub(crate) total_flagged: usize,
    pub(crate) resolved_removed: usize,
    resolved_clean_artifact: usize,
    pub(crate) still_open: usize,
    pub(crate) needs_reaudit: usize,
    pub(crate) findings: Vec<FindingVerificationEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FindingVerificationEntry {
    pub(crate) path: String,
    verdict: String,
    status: EntryStatus,
    pub(crate) result: FindingVerificationResult,
    manifest_content_hash: Option<String>,
    current_content_hash: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FindingVerificationResult {
    ResolvedRemoved,
    ResolvedCleanArtifact,
    NeedsReaudit,
    StillOpen,
}

pub(crate) fn verify_audit_findings(repo_root: &Path, output_dir: &Path) -> Result<()> {
    let manifest_path = output_dir.join("MANIFEST.json");
    let report = build_finding_verification_report(repo_root, output_dir)?;
    write_finding_verification_report(output_dir, &report)?;

    println!("auto audit finding verification");
    println!("manifest:         {}", manifest_path.display());
    println!("flagged findings: {}", report.total_flagged);
    println!("resolved removed: {}", report.resolved_removed);
    println!("needs re-audit:   {}", report.needs_reaudit);
    println!("still open:       {}", report.still_open);
    println!(
        "report:           {}",
        output_dir.join("FINDING-VERIFY.md").display()
    );

    if report.needs_reaudit > 0 || report.still_open > 0 {
        bail!(
            "audit findings are not fully closed: {} need re-audit, {} are still open",
            report.needs_reaudit,
            report.still_open
        );
    }

    Ok(())
}

pub(crate) fn build_finding_verification_report(
    repo_root: &Path,
    output_dir: &Path,
) -> Result<FindingVerificationReport> {
    let manifest_path = output_dir.join("MANIFEST.json");
    if !manifest_path.exists() {
        bail!(
            "audit finding verification requires an existing manifest at {}",
            manifest_path.display()
        );
    }

    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let mut findings = Vec::new();
    for entry in manifest.files.iter().filter(audit_entry_requires_closure) {
        let path = repo_root.join(&entry.path);
        let current_content_hash = match fs::read(&path) {
            Ok(content) => Some(sha256_hex(&content)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let clean_artifact_closes_finding =
            clean_artifact_verdict_for_current_source(output_dir, repo_root, &entry.path)?;
        let result = match current_content_hash.as_deref() {
            None => FindingVerificationResult::ResolvedRemoved,
            Some(_) if clean_artifact_closes_finding => {
                FindingVerificationResult::ResolvedCleanArtifact
            }
            Some(current_hash) if entry.content_hash.as_deref() != Some(current_hash) => {
                FindingVerificationResult::NeedsReaudit
            }
            Some(_) => FindingVerificationResult::StillOpen,
        };
        findings.push(FindingVerificationEntry {
            path: entry.path.clone(),
            verdict: entry
                .verdict
                .clone()
                .unwrap_or_else(|| "UNKNOWN".to_string()),
            status: entry.status,
            result,
            manifest_content_hash: entry.content_hash.clone(),
            current_content_hash,
        });
    }

    findings.sort_by(|left, right| left.path.cmp(&right.path));
    let report = FindingVerificationReport {
        generated_at: now_iso8601(),
        manifest_path: manifest_path.display().to_string(),
        total_flagged: findings.len(),
        resolved_removed: findings
            .iter()
            .filter(|finding| finding.result == FindingVerificationResult::ResolvedRemoved)
            .count(),
        resolved_clean_artifact: findings
            .iter()
            .filter(|finding| finding.result == FindingVerificationResult::ResolvedCleanArtifact)
            .count(),
        needs_reaudit: findings
            .iter()
            .filter(|finding| finding.result == FindingVerificationResult::NeedsReaudit)
            .count(),
        still_open: findings
            .iter()
            .filter(|finding| finding.result == FindingVerificationResult::StillOpen)
            .count(),
        findings,
    };
    Ok(report)
}

pub(crate) fn audit_entry_requires_closure(entry: &&ManifestEntry) -> bool {
    manifest_entry_requires_closure(entry)
}

fn manifest_entry_requires_closure(entry: &ManifestEntry) -> bool {
    matches!(
        entry.verdict.as_deref(),
        Some("DRIFT-LARGE" | "DRIFT-SMALL" | "REFACTOR" | "RETIRE")
    ) || matches!(
        entry.status,
        EntryStatus::ApplyFailed | EntryStatus::Escalated
    )
}

fn clean_artifact_verdict(output_dir: &Path, rel_path: &str) -> Result<bool> {
    let verdict_path = file_artifact_dir(output_dir, rel_path).join("verdict.json");
    if !verdict_path.exists() {
        return Ok(false);
    }
    let raw = fs::read_to_string(&verdict_path)
        .with_context(|| format!("failed to read {}", verdict_path.display()))?;
    let verdict: FileVerdict = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", verdict_path.display()))?;
    Ok(verdict.verdict == "CLEAN")
}

fn clean_artifact_verdict_for_current_source(
    output_dir: &Path,
    repo_root: &Path,
    rel_path: &str,
) -> Result<bool> {
    let verdict_path = file_artifact_dir(output_dir, rel_path).join("verdict.json");
    if !clean_artifact_verdict(output_dir, rel_path)? {
        return Ok(false);
    }
    let source_path = repo_root.join(rel_path);
    let verdict_mtime = fs::metadata(&verdict_path)
        .and_then(|metadata| metadata.modified())
        .with_context(|| format!("failed to stat {}", verdict_path.display()))?;
    let source_mtime = fs::metadata(&source_path)
        .and_then(|metadata| metadata.modified())
        .with_context(|| format!("failed to stat {}", source_path.display()))?;
    Ok(verdict_mtime >= source_mtime)
}

pub(crate) fn write_finding_verification_report(
    output_dir: &Path,
    report: &FindingVerificationReport,
) -> Result<()> {
    let json = serde_json::to_vec_pretty(report)?;
    atomic_write(&output_dir.join("FINDING-VERIFY.json"), &json)?;

    let mut markdown = String::new();
    markdown.push_str("# Audit Finding Verification\n\n");
    markdown.push_str(&format!("- Generated: `{}`\n", report.generated_at));
    markdown.push_str(&format!("- Manifest: `{}`\n", report.manifest_path));
    markdown.push_str(&format!("- Flagged findings: `{}`\n", report.total_flagged));
    markdown.push_str(&format!(
        "- Resolved by removal: `{}`\n",
        report.resolved_removed
    ));
    markdown.push_str(&format!(
        "- Resolved by clean artifact: `{}`\n",
        report.resolved_clean_artifact
    ));
    markdown.push_str(&format!("- Needs re-audit: `{}`\n", report.needs_reaudit));
    markdown.push_str(&format!("- Still open: `{}`\n\n", report.still_open));

    if report.needs_reaudit == 0 && report.still_open == 0 {
        markdown.push_str("Verdict: GO. Every flagged finding has independent closure evidence.\n");
    } else {
        markdown.push_str(
            "Verdict: NO-GO. Re-run `auto audit --resume-mode only-drifted` after remediation, \
             then run `auto audit --verify-findings` again.\n\n",
        );
        markdown.push_str("| Result | Verdict | Status | Path |\n");
        markdown.push_str("|---|---|---|---|\n");
        for finding in &report.findings {
            if matches!(
                finding.result,
                FindingVerificationResult::ResolvedRemoved
                    | FindingVerificationResult::ResolvedCleanArtifact
            ) {
                continue;
            }
            markdown.push_str(&format!(
                "| `{:?}` | `{}` | `{:?}` | `{}` |\n",
                finding.result, finding.verdict, finding.status, finding.path
            ));
        }
    }
    atomic_write(&output_dir.join("FINDING-VERIFY.md"), markdown.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::verify_audit_findings;
    use crate::audit_command::files::{file_artifact_dir, sha256_hex};
    use crate::audit_command::manifest::{EntryStatus, Manifest, ManifestEntry};

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

    #[test]
    fn verify_audit_findings_requires_reaudit_for_changed_flagged_files() {
        let repo = TestTempDir::new("verify-findings-reaudit");
        let audit_dir = repo.path().join("audit");
        fs::create_dir_all(&audit_dir).unwrap();
        fs::write(repo.path().join("a.rs"), "fn old() {}\n").unwrap();
        let manifest = Manifest {
            started_at: "2026-04-28T00:00:00Z".to_string(),
            repo_head: "HEAD".to_string(),
            doctrine_hash: "doctrine".to_string(),
            rubric_hash: "rubric".to_string(),
            files: vec![ManifestEntry {
                path: "a.rs".to_string(),
                status: EntryStatus::Audited,
                content_hash: Some(sha256_hex(b"fn old() {}\n")),
                audited_doctrine_hash: Some("doctrine".to_string()),
                audited_rubric_hash: Some("rubric".to_string()),
                verdict: Some("DRIFT-LARGE".to_string()),
                audited_at: Some("2026-04-28T00:00:00Z".to_string()),
                commit: None,
            }],
        };
        fs::write(
            audit_dir.join("MANIFEST.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(repo.path().join("a.rs"), "fn fixed() {}\n").unwrap();

        let err = verify_audit_findings(repo.path(), &audit_dir)
            .expect_err("changed flagged files must be re-audited before closure");
        assert!(err.to_string().contains("need re-audit"), "{err}");
        let report = fs::read_to_string(audit_dir.join("FINDING-VERIFY.md")).unwrap();
        assert!(report.contains("NeedsReaudit"), "{report}");
    }

    #[test]
    fn verify_audit_findings_accepts_removed_flagged_files() {
        let repo = TestTempDir::new("verify-findings-removed");
        let audit_dir = repo.path().join("audit");
        fs::create_dir_all(&audit_dir).unwrap();
        let manifest = Manifest {
            started_at: "2026-04-28T00:00:00Z".to_string(),
            repo_head: "HEAD".to_string(),
            doctrine_hash: "doctrine".to_string(),
            rubric_hash: "rubric".to_string(),
            files: vec![ManifestEntry {
                path: "retire-me.rs".to_string(),
                status: EntryStatus::Audited,
                content_hash: Some(sha256_hex(b"obsolete\n")),
                audited_doctrine_hash: Some("doctrine".to_string()),
                audited_rubric_hash: Some("rubric".to_string()),
                verdict: Some("RETIRE".to_string()),
                audited_at: Some("2026-04-28T00:00:00Z".to_string()),
                commit: None,
            }],
        };
        fs::write(
            audit_dir.join("MANIFEST.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        verify_audit_findings(repo.path(), &audit_dir).unwrap();
        let report = fs::read_to_string(audit_dir.join("FINDING-VERIFY.md")).unwrap();
        assert!(report.contains("Verdict: GO"), "{report}");
    }

    #[test]
    fn verify_audit_findings_accepts_clean_artifact_for_stale_manifest_verdict() {
        let repo = TestTempDir::new("verify-findings-clean-artifact");
        let audit_dir = repo.path().join("audit");
        fs::create_dir_all(&audit_dir).unwrap();
        fs::write(repo.path().join("a.rs"), "fn fixed() {}\n").unwrap();
        let manifest = Manifest {
            started_at: "2026-04-28T00:00:00Z".to_string(),
            repo_head: "HEAD".to_string(),
            doctrine_hash: "doctrine".to_string(),
            rubric_hash: "rubric".to_string(),
            files: vec![ManifestEntry {
                path: "a.rs".to_string(),
                status: EntryStatus::Audited,
                content_hash: Some(sha256_hex(b"fn fixed() {}\n")),
                audited_doctrine_hash: Some("doctrine".to_string()),
                audited_rubric_hash: Some("rubric".to_string()),
                verdict: Some("DRIFT-SMALL".to_string()),
                audited_at: Some("2026-04-28T00:00:00Z".to_string()),
                commit: None,
            }],
        };
        fs::write(
            audit_dir.join("MANIFEST.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let artifact_dir = file_artifact_dir(&audit_dir, "a.rs");
        fs::create_dir_all(&artifact_dir).unwrap();
        fs::write(
            artifact_dir.join("verdict.json"),
            serde_json::json!({
                "verdict": "CLEAN",
                "rationale": "re-audited clean",
                "touched_paths": [],
                "escalate": false
            })
            .to_string(),
        )
        .unwrap();

        verify_audit_findings(repo.path(), &audit_dir).unwrap();
        let report = fs::read_to_string(audit_dir.join("FINDING-VERIFY.md")).unwrap();
        assert!(report.contains("Verdict: GO"), "{report}");
        assert!(
            report.contains("Resolved by clean artifact: `1`"),
            "{report}"
        );
        let report_json = fs::read_to_string(audit_dir.join("FINDING-VERIFY.json")).unwrap();
        assert!(
            report_json.contains("resolved_clean_artifact"),
            "{report_json}"
        );
    }

    #[test]
    fn verify_audit_findings_accepts_newer_clean_artifact_for_drifted_pending_entry() {
        let repo = TestTempDir::new("verify-findings-clean-artifact-drifted-pending");
        let audit_dir = repo.path().join("audit");
        fs::create_dir_all(&audit_dir).unwrap();
        fs::write(repo.path().join("a.rs"), "fn fixed() {}\n").unwrap();
        let manifest = Manifest {
            started_at: "2026-04-28T00:00:00Z".to_string(),
            repo_head: "HEAD".to_string(),
            doctrine_hash: "doctrine".to_string(),
            rubric_hash: "rubric".to_string(),
            files: vec![ManifestEntry {
                path: "a.rs".to_string(),
                status: EntryStatus::Pending,
                content_hash: Some(sha256_hex(b"fn old() {}\n")),
                audited_doctrine_hash: Some("doctrine".to_string()),
                audited_rubric_hash: Some("rubric".to_string()),
                verdict: Some("DRIFT-SMALL".to_string()),
                audited_at: Some("2026-04-28T00:00:00Z".to_string()),
                commit: None,
            }],
        };
        fs::write(
            audit_dir.join("MANIFEST.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let artifact_dir = file_artifact_dir(&audit_dir, "a.rs");
        fs::create_dir_all(&artifact_dir).unwrap();
        fs::write(
            artifact_dir.join("verdict.json"),
            serde_json::json!({
                "verdict": "CLEAN",
                "rationale": "current source re-audited clean",
                "touched_paths": [],
                "escalate": false
            })
            .to_string(),
        )
        .unwrap();

        verify_audit_findings(repo.path(), &audit_dir).unwrap();
        let report = fs::read_to_string(audit_dir.join("FINDING-VERIFY.md")).unwrap();
        assert!(
            report.contains("Resolved by clean artifact: `1`"),
            "{report}"
        );
        assert!(report.contains("Verdict: GO"), "{report}");
    }
}
