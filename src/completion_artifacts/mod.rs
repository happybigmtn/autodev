//! Task completion-evidence inspection: review handoff, receipts, artifacts, audit gaps.

mod artifacts;
mod audit;
mod receipt;
mod verification;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::util::atomic_write;

use crate::completion_artifacts::artifacts::{
    declared_artifact_path, declared_completion_artifacts,
};
use crate::completion_artifacts::audit::{
    summarize_unresolved_audit_findings, unresolved_owned_audit_findings,
};
use crate::completion_artifacts::receipt::{
    inspect_verification_receipt, verification_receipt_path,
};
pub(crate) use crate::completion_artifacts::verification::verification_step_looks_external;

pub(crate) use receipt::{
    git_verification_receipt_footers, legacy_verification_receipt_backfill_footer,
    normalized_plan_hash_bytes, shared_footer_receipt_freshness_problem,
    shared_receipt_freshness_problem, verification_receipt_commit_footer,
};
pub(crate) use verification::verification_plan;

const REVIEW_HEADER: &str = "# REVIEW\n\nAwaiting auto review:\n";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TaskCompletionEvidence {
    pub(crate) has_review_handoff: bool,
    pub(crate) unresolved_review_findings: Vec<String>,
    pub(crate) verification_receipt_path: Option<PathBuf>,
    pub(crate) verification_receipt_present: bool,
    pub(crate) verification_receipt_status: Option<String>,
    pub(crate) declared_completion_artifacts: Vec<String>,
    pub(crate) missing_completion_artifacts: Vec<String>,
    pub(crate) unresolved_audit_findings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompletionGapKind {
    None,
    LocalRepairable,
    ExternalOrLiveFollowUp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompletionGapAssessment {
    pub(crate) kind: CompletionGapKind,
    pub(crate) missing_reasons: Vec<String>,
    pub(crate) verification_steps: Vec<String>,
    pub(crate) verification_commands: Vec<String>,
    pub(crate) verification_guidance: Vec<String>,
}

impl TaskCompletionEvidence {
    pub(crate) fn is_fully_evidenced(&self) -> bool {
        self.is_ready_for_definition_of_done_gates() && self.unresolved_review_findings.is_empty()
    }

    pub(crate) fn is_ready_for_definition_of_done_gates(&self) -> bool {
        self.has_review_handoff
            && self.verification_receipt_present
            && self.missing_completion_artifacts.is_empty()
            && self.unresolved_audit_findings.is_empty()
    }

    pub(crate) fn missing_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if !self.has_review_handoff {
            reasons.push("missing REVIEW.md handoff".to_string());
        }
        if !self.unresolved_review_findings.is_empty() {
            reasons.push(format!(
                "unresolved REVIEW.md finding(s): {}",
                summarize_review_findings(&self.unresolved_review_findings)
            ));
        }
        if !self.verification_receipt_present {
            reasons.push(self.verification_receipt_status.clone().unwrap_or_else(|| {
                if let Some(path) = &self.verification_receipt_path {
                    format!("missing verification receipt `{}`", path.display())
                } else {
                    "missing verification receipt".to_string()
                }
            }));
        }
        if !self.missing_completion_artifacts.is_empty() {
            reasons.push(format!(
                "missing completion artifact(s): {}",
                self.missing_completion_artifacts
                    .iter()
                    .map(|path| format!("`{path}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.unresolved_audit_findings.is_empty() {
            reasons.push(format!(
                "unresolved audit finding(s) still in owned scope: {}",
                summarize_unresolved_audit_findings(&self.unresolved_audit_findings)
            ));
        }
        reasons
    }
}

pub(crate) fn assess_task_completion_gap(
    task_markdown: &str,
    evidence: &TaskCompletionEvidence,
) -> CompletionGapAssessment {
    let missing_reasons = evidence.missing_reasons();
    let verification = verification_plan(task_markdown);
    if missing_reasons.is_empty() {
        return CompletionGapAssessment {
            kind: CompletionGapKind::None,
            missing_reasons,
            verification_steps: verification.steps,
            verification_commands: verification.executable_commands,
            verification_guidance: verification.narrative_guidance,
        };
    }

    let kind = if verification
        .steps
        .iter()
        .any(|step| verification_step_looks_external(step))
    {
        CompletionGapKind::ExternalOrLiveFollowUp
    } else {
        CompletionGapKind::LocalRepairable
    };

    CompletionGapAssessment {
        kind,
        missing_reasons,
        verification_steps: verification.steps,
        verification_commands: verification.executable_commands,
        verification_guidance: verification.narrative_guidance,
    }
}

pub(crate) fn inspect_task_completion_evidence(
    repo_root: &Path,
    task_id: &str,
    task_markdown: &str,
) -> TaskCompletionEvidence {
    let review_path = repo_root.join("REVIEW.md");
    let review_text = fs::read_to_string(&review_path).unwrap_or_default();
    let verification_receipt_path = verification_receipt_path(repo_root, task_id);
    let verification = verification_plan(task_markdown);
    let verification_receipt_required = !verification.executable_commands.is_empty();
    let verification_wrapper_present = repo_root.join("scripts/run-task-verification.sh").exists();
    let declared_completion_artifacts = declared_completion_artifacts(task_markdown);
    let (verification_receipt_present, verification_receipt_status) = inspect_verification_receipt(
        repo_root,
        verification_receipt_required,
        verification_wrapper_present,
        &verification_receipt_path,
        &verification.executable_commands,
        &declared_completion_artifacts,
    );
    let missing_completion_artifacts = declared_completion_artifacts
        .iter()
        .filter(|relative| declared_artifact_path(repo_root, relative).is_none())
        .cloned()
        .collect::<Vec<_>>();
    let unresolved_audit_findings =
        unresolved_owned_audit_findings(repo_root, task_id, task_markdown);

    TaskCompletionEvidence {
        has_review_handoff: review_contains_task(&review_text, task_id),
        unresolved_review_findings: unresolved_review_findings_for_task(&review_text, task_id),
        verification_receipt_path: verification_receipt_required
            .then_some(verification_receipt_path),
        verification_receipt_present,
        verification_receipt_status,
        declared_completion_artifacts,
        missing_completion_artifacts,
        unresolved_audit_findings,
    }
}

pub(crate) fn ensure_host_review_handoff(
    repo_root: &Path,
    task_id: &str,
    changed_files: &[String],
    evidence: &TaskCompletionEvidence,
) -> Result<bool> {
    let review_path = repo_root.join("REVIEW.md");
    let mut review_text = if review_path.exists() {
        fs::read_to_string(&review_path)
            .with_context(|| format!("failed to read {}", review_path.display()))?
    } else {
        default_review_doc()
    };
    if review_contains_task(&review_text, task_id) {
        return Ok(false);
    }

    review_text.push_str(&render_host_review_entry(task_id, changed_files, evidence));
    atomic_write(&review_path, review_text.as_bytes())
        .with_context(|| format!("failed to write {}", review_path.display()))?;
    Ok(true)
}

pub(crate) fn default_review_doc() -> String {
    REVIEW_HEADER.to_string()
}

pub(crate) fn review_contains_task(review_text: &str, task_id: &str) -> bool {
    let needle = format!("`{task_id}`");
    review_text.lines().any(|line| {
        line.contains(&format!("{needle}:"))
            || line.contains(&format!("## {needle}"))
            || line.trim() == needle
    })
}

pub(crate) fn unresolved_review_findings_for_task(review_text: &str, task_id: &str) -> Vec<String> {
    let mut unresolved = Vec::new();
    for item in extract_review_items(review_text) {
        if !review_item_mentions_task(&item, task_id) {
            continue;
        }
        if review_item_clears_task(&item, task_id) {
            unresolved.clear();
            continue;
        }
        if review_item_has_unresolved_finding(&item) {
            unresolved.push(review_item_summary(&item));
        }
    }
    unresolved
}

fn extract_review_items(content: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = Vec::new();
    let mut in_item = false;

    let flush = |items: &mut Vec<String>, current: &mut Vec<String>, in_item: &mut bool| {
        if !current.is_empty() {
            items.push(current.join("\n").trim_end().to_string());
            current.clear();
        }
        *in_item = false;
    };

    for raw_line in content.lines() {
        let line = raw_line.trim_end();
        if line.starts_with("## ") || is_bullet_review_item_start(line) {
            flush(&mut items, &mut current, &mut in_item);
            current.push(line.to_string());
            in_item = true;
            continue;
        }
        if in_item {
            current.push(line.to_string());
        }
    }
    flush(&mut items, &mut current, &mut in_item);
    items
}

fn is_bullet_review_item_start(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("- `") else {
        return false;
    };
    let Some(identity) = rest.split('`').next() else {
        return false;
    };
    !identity.trim().is_empty()
        && identity
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':'))
        && identity
            .chars()
            .any(|ch| ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

fn review_item_mentions_task(item: &str, task_id: &str) -> bool {
    item.contains(&format!("`{task_id}`"))
}

fn review_item_clears_task(item: &str, task_id: &str) -> bool {
    review_item_mentions_task(item, task_id)
        && item
            .to_ascii_lowercase()
            .contains("auto parallel standing-review gate cleared")
}

fn review_item_has_unresolved_finding(item: &str) -> bool {
    let lower = item.to_ascii_lowercase();
    if lower.contains("independent review findings")
        || lower.contains("host re-execution verification failed")
        || lower.contains("workspace cargo test failed")
        || lower.contains("review gate skipped")
        || lower.contains("held at `[~]`")
        || lower.contains("verdict: findings")
        || lower.contains("[~]")
    {
        return true;
    }

    item.lines().any(|line| {
        let lower_line = line.to_ascii_lowercase();
        let Some((_, blockers)) = lower_line.split_once("remaining blockers:") else {
            return false;
        };
        let blockers = blockers.trim();
        !blockers.is_empty()
            && !blockers.starts_with("none")
            && !blockers.starts_with("no ")
            && blockers != "n/a"
    })
}

fn review_item_summary(item: &str) -> String {
    item.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("REVIEW.md item")
        .to_string()
}

fn summarize_review_findings(findings: &[String]) -> String {
    const MAX_RENDERED: usize = 5;
    let mut rendered = findings
        .iter()
        .take(MAX_RENDERED)
        .map(|finding| format!("`{finding}`"))
        .collect::<Vec<_>>();
    if findings.len() > MAX_RENDERED {
        rendered.push(format!("... and {} more", findings.len() - MAX_RENDERED));
    }
    rendered.join(", ")
}

fn render_host_review_entry(
    task_id: &str,
    changed_files: &[String],
    evidence: &TaskCompletionEvidence,
) -> String {
    let files = if changed_files.is_empty() {
        "none recorded by host".to_string()
    } else {
        changed_files
            .iter()
            .map(|path| format!("`{path}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let verification = if let Some(path) = &evidence.verification_receipt_path {
        if evidence.verification_receipt_present {
            format!("host observed verification receipt at `{}`", path.display())
        } else {
            evidence
                .verification_receipt_status
                .clone()
                .unwrap_or_else(|| {
                    format!("verification receipt still missing at `{}`", path.display())
                })
        }
    } else {
        "repo does not require a verification receipt wrapper for this task".to_string()
    };
    let remaining = if evidence.missing_reasons().is_empty() {
        "none".to_string()
    } else {
        evidence.missing_reasons().join("; ")
    };
    let completion_artifacts = if evidence.declared_completion_artifacts.is_empty() {
        "none".to_string()
    } else {
        evidence
            .declared_completion_artifacts
            .iter()
            .map(|path| format!("`{path}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "\n## `{task_id}`\n\
- Source: auto parallel host handoff synthesized after lane landing.\n\
- Files: {files}\n\
- Scope exceptions: none recorded by host.\n\
- Validation: {verification}\n\
- Completion artifacts: {completion_artifacts}\n\
- Remaining blockers: {remaining}\n"
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use crate::completion_artifacts::receipt::latest_verification_receipt_footer;

    use super::{
        assess_task_completion_gap, ensure_host_review_handoff, inspect_task_completion_evidence,
        review_contains_task, verification_receipt_commit_footer, CompletionGapKind,
        TaskCompletionEvidence,
    };

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

    fn init_git_repo(root: &std::path::Path) {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("init")
            .output()
            .expect("git init failed");
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "user.email", "test@example.com"])
            .output()
            .expect("git config email failed");
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "user.name", "Test User"])
            .output()
            .expect("git config name failed");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), "# plan\n").expect("failed to write plan");
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "IMPLEMENTATION_PLAN.md"])
            .output()
            .expect("git add failed");
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["commit", "-m", "initial"])
            .output()
            .expect("git commit failed");
    }

    fn git_head(root: &std::path::Path) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse failed");
        String::from_utf8(output.stdout)
            .expect("head should be utf8")
            .trim()
            .to_string()
    }

    fn git_ok(root: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git command failed to launch");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn inspect_task_completion_evidence_requires_review_and_receipts() {
        let root = temp_dir("evidence");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-1`\n",
        )
        .expect("failed to write review");
        fs::create_dir_all(root.join("docs/ops")).expect("failed to create docs dir");
        fs::write(root.join("docs/ops/proof.md"), "proof\n").expect("failed to write proof");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-1.json"),
            r#"{"declared_artifacts":[{"path":"docs/ops/proof.md","sha256":"f6ed42a9d765eeb230a069bbc3d5dc346b2669594bb0b83cc6d14d5d967b8961"}],"commands":[{"command":"cargo test -p demo receipt_example","exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-1",
            "- [ ] `TASK-1` Example\nVerification:\n  - `cargo test -p demo receipt_example`\nCompletion artifacts:\n  - `docs/ops/proof.md`\nDependencies: none\n",
        );
        assert!(evidence.is_fully_evidenced());
        assert!(evidence.missing_reasons().is_empty());
    }

    #[test]
    fn inspect_task_completion_evidence_accepts_commit_footer_receipts() {
        let root = temp_dir("footer-evidence");
        init_git_repo(&root);
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-FOOTER`\n",
        )
        .expect("failed to write review");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-FOOTER.json"),
            r#"{"task_id":"TASK-FOOTER","commands":[{"command":"cargo test footer_receipt","exit_code":0,"status":"passed","output_summary":{"stdout_tail":"large transient output","stderr_tail":"","stdout_bytes":22,"stderr_bytes":0}}]}"#,
        )
        .expect("failed to write receipt");
        let footer = verification_receipt_commit_footer(&root, "TASK-FOOTER")
            .expect("footer generation should succeed")
            .expect("footer should be present");
        assert!(footer.contains("Auto-Verification-Receipt-Task: TASK-FOOTER"));
        assert!(!footer.contains("large transient output"));
        git_ok(
            &root,
            &[
                "commit",
                "--allow-empty",
                "-m",
                "footer evidence",
                "-m",
                &footer,
            ],
        );
        fs::remove_file(root.join(".auto/symphony/verification-receipts/TASK-FOOTER.json"))
            .expect("failed to remove json receipt");

        let footer = latest_verification_receipt_footer(&root, "TASK-FOOTER")
            .expect("footer receipt should be discoverable");
        assert_eq!(footer.task_id, "TASK-FOOTER");
        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-FOOTER",
            "- [ ] `TASK-FOOTER` Example\nVerification:\n  - `cargo test footer_receipt`\nDependencies: none\n",
        );

        assert!(evidence.verification_receipt_present);
        assert!(evidence.missing_reasons().is_empty());
    }

    #[test]
    fn inspect_task_completion_evidence_reads_parallel_lane_receipts() {
        let base = temp_dir("parallel-lane-receipts");
        let root = base.join(".auto/parallel/lanes/lane-3/repo");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-LANE`\n",
        )
        .expect("failed to write review");
        fs::create_dir_all(base.join(".auto/symphony/verification-receipts"))
            .expect("failed to create host receipt dir");
        fs::write(
            base.join(".auto/symphony/verification-receipts/TASK-LANE.json"),
            r#"{"commands":[{"command":"cargo test completion_artifacts::tests::lane_receipt","exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write host receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-LANE",
            "- [ ] `TASK-LANE` Example\nVerification:\n  - `cargo test completion_artifacts::tests::lane_receipt`\nDependencies: none\n",
        );

        assert!(evidence.verification_receipt_present);
        assert!(evidence.missing_reasons().is_empty());
    }

    #[test]
    fn inspect_task_completion_evidence_reads_nested_parallel_lane_receipts() {
        let base = temp_dir("nested-parallel-lane-receipts");
        let root =
            base.join(".auto/super/20260430-133225/design/parallel/pass-01/lanes/lane-1/repo");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-NESTED-LANE`\n",
        )
        .expect("failed to write review");
        fs::create_dir_all(base.join(".auto/symphony/verification-receipts"))
            .expect("failed to create host receipt dir");
        fs::write(
            base.join(".auto/symphony/verification-receipts/TASK-NESTED-LANE.json"),
            r#"{"commands":[{"command":"cargo test completion_artifacts::tests::nested_lane_receipt","exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write host receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-NESTED-LANE",
            "- [ ] `TASK-NESTED-LANE` Example\nVerification:\n  - `cargo test completion_artifacts::tests::nested_lane_receipt`\nDependencies: none\n",
        );

        assert!(evidence.verification_receipt_present);
        assert!(evidence.missing_reasons().is_empty());
    }

    #[test]
    fn inspect_task_completion_evidence_requires_wrapper_for_executable_verification() {
        let root = temp_dir("missing-wrapper");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-2`\n",
        )
        .expect("failed to write review");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-2",
            "- [ ] `TASK-2` Example\nVerification:\n  - `cargo test -p demo proof`\nDependencies: none\n",
        );

        assert!(!evidence.is_fully_evidenced());
        assert!(!evidence.verification_receipt_present);
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("missing scripts/run-task-verification.sh"));
    }

    #[test]
    fn inspect_task_completion_evidence_allows_narrative_verification_without_receipt() {
        let root = temp_dir("narrative-verification");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-2B`\n",
        )
        .expect("failed to write review");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-2B",
            "- [ ] `TASK-2B` Example\nVerification:\n  - Operator confirms the dashboard import on the reference host.\nDependencies: none\n",
        );

        assert!(evidence.is_fully_evidenced());
        assert!(evidence.verification_receipt_present);
        assert!(evidence.verification_receipt_path.is_none());
    }

    #[test]
    fn inspect_task_completion_evidence_rejects_audit_rows_with_owned_unresolved_manifest_findings()
    {
        let root = temp_dir("audit-owned-unresolved");
        fs::create_dir_all(root.join("audit")).expect("failed to create audit dir");
        fs::write(
            root.join("audit/MANIFEST.json"),
            r#"{"files":[
                {"path":"crates/demo/src/lib.rs","status":"audited","verdict":"DRIFT-LARGE"},
                {"path":"crates/other/src/lib.rs","status":"audited","verdict":"DRIFT-LARGE"},
                {"path":"crates/demo/src/clean.rs","status":"audited","verdict":"CLEAN"}
            ]}"#,
        )
        .expect("failed to write manifest");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `AUD-DEMO-01`\n",
        )
        .expect("failed to write review");

        let evidence = inspect_task_completion_evidence(
            &root,
            "AUD-DEMO-01",
            "- [ ] `AUD-DEMO-01` Resolve demo audit findings\nOwns: `crates/demo/**`\nVerification:\n  - Operator review only.\nCompletion artifacts: none\nDependencies: none\n",
        );

        assert!(!evidence.is_fully_evidenced());
        assert_eq!(evidence.unresolved_audit_findings.len(), 1);
        assert!(evidence.unresolved_audit_findings[0].contains("crates/demo/src/lib.rs"));
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("unresolved audit finding(s)"));
    }

    #[test]
    fn inspect_task_completion_evidence_accepts_audit_rows_when_owned_manifest_scope_is_clean() {
        let root = temp_dir("audit-owned-clean");
        fs::create_dir_all(root.join("audit")).expect("failed to create audit dir");
        fs::write(
            root.join("audit/MANIFEST.json"),
            r#"{"files":[
                {"path":"crates/demo/src/lib.rs","status":"audited","verdict":"CLEAN"},
                {"path":"crates/other/src/lib.rs","status":"audited","verdict":"DRIFT-LARGE"}
            ]}"#,
        )
        .expect("failed to write manifest");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `AUD-DEMO-02`\n",
        )
        .expect("failed to write review");

        let evidence = inspect_task_completion_evidence(
            &root,
            "AUD-DEMO-02",
            "- [ ] `AUD-DEMO-02` Resolve demo audit findings\nOwns: `crates/demo/**`\nVerification:\n  - Operator review only.\nCompletion artifacts: none\nDependencies: none\n",
        );

        assert!(evidence.is_fully_evidenced());
        assert!(evidence.unresolved_audit_findings.is_empty());
    }

    #[test]
    fn ensure_host_review_handoff_is_idempotent() {
        let root = temp_dir("review");
        let evidence = TaskCompletionEvidence {
            has_review_handoff: false,
            unresolved_review_findings: Vec::new(),
            verification_receipt_path: None,
            verification_receipt_present: true,
            verification_receipt_status: None,
            declared_completion_artifacts: Vec::new(),
            missing_completion_artifacts: Vec::new(),
            unresolved_audit_findings: Vec::new(),
        };
        assert!(ensure_host_review_handoff(
            &root,
            "TASK-2",
            &["src/lib.rs".to_string()],
            &evidence
        )
        .expect("first write should succeed"));
        let review = fs::read_to_string(root.join("REVIEW.md")).expect("review should exist");
        assert!(review_contains_task(&review, "TASK-2"));
        assert!(!ensure_host_review_handoff(
            &root,
            "TASK-2",
            &["src/lib.rs".to_string()],
            &evidence
        )
        .expect("second write should be skipped"));
    }

    #[test]
    fn assess_task_completion_gap_marks_local_verification_repairs() {
        let evidence = TaskCompletionEvidence {
            has_review_handoff: true,
            unresolved_review_findings: Vec::new(),
            verification_receipt_path: Some(PathBuf::from(
                ".auto/symphony/verification-receipts/TASK-3.json",
            )),
            verification_receipt_present: false,
            verification_receipt_status: None,
            declared_completion_artifacts: vec!["docs/agent/quickstart.md".to_string()],
            missing_completion_artifacts: vec!["docs/agent/quickstart.md".to_string()],
            unresolved_audit_findings: Vec::new(),
        };
        let assessment = assess_task_completion_gap(
            "- [~] `TASK-3` Agent quickstart\nVerification:\n  - `cargo test -p bitino-mcp channel_tool_openclose`\nRequired tests: integration test\nCompletion artifacts:\n  - `docs/agent/quickstart.md`\nDependencies: none\n",
            &evidence,
        );
        assert_eq!(assessment.kind, CompletionGapKind::LocalRepairable);
        assert_eq!(assessment.verification_steps.len(), 1);
        assert_eq!(assessment.verification_commands.len(), 1);
    }

    #[test]
    fn assess_task_completion_gap_marks_external_live_followups() {
        let evidence = TaskCompletionEvidence {
            has_review_handoff: true,
            unresolved_review_findings: Vec::new(),
            verification_receipt_path: None,
            verification_receipt_present: true,
            verification_receipt_status: None,
            declared_completion_artifacts: vec![
                "docs/ops/operator-evidence/loom-cluster-recovery-2026-04-18.md".to_string(),
            ],
            missing_completion_artifacts: vec![
                "docs/ops/operator-evidence/loom-cluster-recovery-2026-04-18.md".to_string(),
            ],
            unresolved_audit_findings: Vec::new(),
        };
        let assessment = assess_task_completion_gap(
            "- [~] `TASK-4` Loom cluster health\nVerification:\n  - `curl -I https://loom.rsociety.org:30443/health`\n  - `ssh root@loom kubectl get pods`\nRequired tests: none\nCompletion artifacts:\n  - `docs/ops/operator-evidence/loom-cluster-recovery-2026-04-18.md`\nDependencies: none\n",
            &evidence,
        );
        assert_eq!(assessment.kind, CompletionGapKind::ExternalOrLiveFollowUp);
        assert_eq!(assessment.verification_steps.len(), 2);
        assert_eq!(assessment.verification_commands.len(), 2);
    }

    #[test]
    fn inspect_task_completion_evidence_rejects_failed_receipts() {
        let root = temp_dir("failed-receipt");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-7.json"),
            r#"{"commands":[{"command":"cargo test -p demo failed_receipt","exit_code":101,"status":"failed"}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-7",
            "- [ ] `TASK-7` Example\nVerification:\n  - `cargo test -p demo failed_receipt`\nDependencies: none\n",
        );
        assert!(!evidence.verification_receipt_present);
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("has failed command(s)"));
    }

    #[test]
    fn inspect_task_completion_evidence_rejects_zero_cargo_tests() {
        let root = temp_dir("zero-cargo-tests");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-ZERO-CARGO.json"),
            r#"{"commands":[{"command":"cargo test completion_artifacts::tests::missing_filter","exit_code":0,"status":"passed","runner_summary":{"kind":"cargo-test","tests_discovered":0,"tests_run":0,"zero_test_detected":true}}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-ZERO-CARGO",
            "- [ ] `TASK-ZERO-CARGO` Example\nVerification:\n  - `cargo test completion_artifacts::tests::missing_filter`\nDependencies: none\n",
        );

        assert!(!evidence.verification_receipt_present);
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("reported zero-test run(s)"));
    }

    #[test]
    fn inspect_task_completion_evidence_accepts_historical_ancestor_json_receipt() {
        let root = temp_dir("historical-ancestor-json-receipt");
        init_git_repo(&root);
        let historical_commit = git_head(&root);
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), "# plan changed\n")
            .expect("failed to change plan");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "IMPLEMENTATION_PLAN.md"])
            .output()
            .expect("git add failed");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "plan changed"])
            .output()
            .expect("git commit failed");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-HISTORICAL.json"),
            format!(
                r#"{{"commit":"{historical_commit}","commands":[{{"command":"cargo test completion_artifacts::tests::some_filter","exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-HISTORICAL",
            "- [ ] `TASK-HISTORICAL` Example\nVerification:\n  - `cargo test completion_artifacts::tests::some_filter`\nDependencies: none\n",
        );

        assert!(evidence.verification_receipt_present);
    }

    #[test]
    fn inspect_task_completion_evidence_rejects_non_ancestor_json_receipt() {
        let root = temp_dir("non-ancestor-json-receipt");
        init_git_repo(&root);
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-STALE.json"),
            r#"{"commit":"1111111111111111111111111111111111111111","commands":[{"command":"cargo test completion_artifacts::tests::some_filter","exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-STALE",
            "- [ ] `TASK-STALE` Example\nVerification:\n  - `cargo test completion_artifacts::tests::some_filter`\nDependencies: none\n",
        );

        assert!(!evidence.verification_receipt_present);
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("stale verification receipt"));
    }

    #[test]
    fn checked_row_empty_review_uses_explicit_evidence_class() {
        let evidence = TaskCompletionEvidence {
            has_review_handoff: false,
            verification_receipt_present: true,
            ..TaskCompletionEvidence::default()
        };
        let assessment = assess_task_completion_gap(
            "- [x] `TASK-EXT` External proof\nVerification: inspect live deploy\n",
            &evidence,
        );
        assert_eq!(assessment.kind, CompletionGapKind::ExternalOrLiveFollowUp);
    }

    #[test]
    fn archive_backed_checked_row_is_fully_evidenced() {
        let evidence = TaskCompletionEvidence {
            has_review_handoff: true,
            verification_receipt_present: true,
            declared_completion_artifacts: vec!["audit/archive/TASK.md".to_string()],
            ..TaskCompletionEvidence::default()
        };
        assert!(evidence.is_fully_evidenced());
    }

    #[test]
    fn unresolved_review_findings_block_completion_until_cleared() {
        let review = r#"# REVIEW

## `TASK-REVIEW`: independent review findings
- Source: auto parallel independent diff-review gate (held at `[~]`).

1. `src/lib.rs`: real bug.

## `OTHER-TASK`
- Remaining blockers: missing proof.
"#;
        let findings = super::unresolved_review_findings_for_task(review, "TASK-REVIEW");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("TASK-REVIEW"));

        let evidence = TaskCompletionEvidence {
            has_review_handoff: true,
            unresolved_review_findings: findings.clone(),
            verification_receipt_present: true,
            ..TaskCompletionEvidence::default()
        };
        assert!(
            evidence.is_ready_for_definition_of_done_gates(),
            "standing review findings should not block entry into the review gate"
        );
        assert!(
            !evidence.is_fully_evidenced(),
            "standing review findings must block final completion"
        );

        let cleared = format!(
            "{review}\n## `TASK-REVIEW`: standing review cleared\n- Source: auto parallel standing-review gate cleared this task after current-tree verification and review gates passed.\n- Remaining blockers: none.\n"
        );
        assert!(
            super::unresolved_review_findings_for_task(&cleared, "TASK-REVIEW").is_empty(),
            "clear marker should supersede earlier standing findings"
        );
    }

    #[test]
    fn inspect_task_completion_evidence_rejects_zero_pytest_tests() {
        let root = temp_dir("zero-pytest-tests");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-ZERO-PYTEST.json"),
            r#"{"commands":[{"command":"python -m pytest tests/missing.py","argv":["python","-m","pytest","tests/missing.py"],"exit_code":0,"status":"passed","runner_summary":{"kind":"pytest","tests_discovered":0,"tests_run":0,"zero_test_detected":true}}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-ZERO-PYTEST",
            "- [ ] `TASK-ZERO-PYTEST` Example\nVerification:\n  - `python -m pytest tests/missing.py`\nDependencies: none\n",
        );

        assert!(!evidence.verification_receipt_present);
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("reported zero-test run(s)"));
    }

    #[test]
    fn inspect_task_completion_evidence_rejects_corrupted_receipts() {
        let root = temp_dir("corrupted-receipt");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-6.json"),
            "{\"commands\":[",
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-6",
            "- [ ] `TASK-6` Example\nVerification:\n  - `cargo test -p demo corrupted`\nDependencies: none\n",
        );
        assert!(!evidence.verification_receipt_present);
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("invalid verification receipt"));
    }

    #[test]
    fn inspect_task_completion_evidence_rejects_mixed_failed_receipts() {
        let root = temp_dir("mixed-failed-receipt");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-11.json"),
            r#"{"commands":[{"command":"cargo test -p demo first","exit_code":0,"status":"passed"},{"command":"cargo test -p demo second","exit_code":101,"status":"failed"}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-11",
            "- [ ] `TASK-11` Example\nVerification:\n  - `cargo test -p demo first`\n  - `cargo test -p demo second`\nDependencies: none\n",
        );
        assert!(!evidence.verification_receipt_present);
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("has failed command(s)"));
    }

    #[test]
    fn inspect_task_completion_evidence_accepts_explicitly_superseded_failed_attempt() {
        let root = temp_dir("superseded-failed-receipt");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-SUPERSEDED`\n",
        )
        .expect("failed to write review");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-SUPERSEDED.json"),
            r#"{"commands":[{"command":"rg -n multi-filter WORKLIST.md src","exit_code":2,"status":"failed"},{"command":"rg -n \"multi-filter\" WORKLIST.md src/generation.rs","exit_code":0,"status":"passed","supersedes":["rg -n multi-filter WORKLIST.md src"]}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-SUPERSEDED",
            "- [ ] `TASK-SUPERSEDED` Example\nVerification:\n  - `rg -n \"multi-filter\" WORKLIST.md src/generation.rs`\nDependencies: none\n",
        );

        assert!(evidence.verification_receipt_present);
        assert!(evidence.missing_reasons().is_empty());
    }

    #[test]
    fn inspect_task_completion_evidence_accepts_later_pass_for_same_failed_command() {
        let root = temp_dir("later-pass-same-command-receipt");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-LATER-PASS`\n",
        )
        .expect("failed to write review");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-LATER-PASS.json"),
            r#"{"commands":[{"command":"rg -n HotRoller|crapsHotRollerTotalNumerator contracts/src contracts/tests","exit_code":127,"status":"failed"},{"command":"rg -n \"HotRoller|crapsHotRollerTotalNumerator\" contracts/src contracts/tests","exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-LATER-PASS",
            "- [ ] `TASK-LATER-PASS` Example\nVerification:\n  - `rg -n \"HotRoller|crapsHotRollerTotalNumerator\" contracts/src contracts/tests`\nDependencies: none\n",
        );

        assert!(evidence.verification_receipt_present);
        assert!(evidence.missing_reasons().is_empty());
    }

    #[test]
    fn inspect_task_completion_evidence_rejects_incomplete_receipts() {
        let root = temp_dir("partial-receipt");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-8.json"),
            r#"{"commands":[{"command":"cargo test -p demo first","exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-8",
            "- [ ] `TASK-8` Example\nVerification:\n  - `cargo test -p demo first`\n  - `cargo test -p demo second`\nDependencies: none\n",
        );
        assert!(!evidence.verification_receipt_present);
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("is missing command(s)"));
    }

    #[test]
    fn inspect_task_completion_evidence_accepts_quoted_command_receipts_with_argv() {
        let root = temp_dir("quoted-receipt");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-12`\n",
        )
        .expect("failed to write review");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-12.json"),
            r#"{"commands":[{"command":"sh -c echo \"hello world\"","argv":["sh","-c","echo \"hello world\""],"exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-12",
            "- [ ] `TASK-12` Example\nVerification:\n  - `sh -c 'echo \"hello world\"'`\nDependencies: none\n",
        );

        assert!(evidence.verification_receipt_present);
        assert!(evidence.missing_reasons().is_empty());
    }

    #[test]
    fn inspect_task_completion_evidence_rejects_unsuperseded_extra_failed_receipts() {
        let root = temp_dir("unsuperseded-extra-receipts");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-10`\n",
        )
        .expect("failed to write review");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-10.json"),
            r#"{"commands":[{"command":"cargo test -p demo current","exit_code":0,"status":"passed"},{"command":"cargo test -p demo old","exit_code":101,"status":"failed"}]}"#,
        )
        .expect("failed to write receipt");

        let evidence = inspect_task_completion_evidence(
            &root,
            "TASK-10",
            "- [ ] `TASK-10` Example\nVerification:\n  - `cargo test -p demo current`\nDependencies: none\n",
        );

        assert!(!evidence.verification_receipt_present);
        assert!(evidence
            .missing_reasons()
            .join("\n")
            .contains("unsuperseded failed command(s)"));
    }
}
