use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use shlex::split as shell_split;

use crate::task_parser::{
    parse_tasks as parse_shared_tasks, task_field_body_until_any, TASK_FIELD_BOUNDARIES,
};
use crate::util::atomic_write;

const REVIEW_HEADER: &str = "# REVIEW\n\nAwaiting auto review:\n";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TaskCompletionEvidence {
    pub(crate) has_review_handoff: bool,
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct VerificationPlan {
    pub(crate) steps: Vec<String>,
    pub(crate) executable_commands: Vec<String>,
    pub(crate) narrative_guidance: Vec<String>,
}

impl TaskCompletionEvidence {
    pub(crate) fn is_fully_evidenced(&self) -> bool {
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
    let (verification_receipt_present, verification_receipt_status) = inspect_verification_receipt(
        verification_receipt_required,
        verification_wrapper_present,
        &verification_receipt_path,
        &verification.executable_commands,
    );
    let declared_completion_artifacts = declared_completion_artifacts(task_markdown);
    let missing_completion_artifacts = declared_completion_artifacts
        .iter()
        .filter(|relative| !repo_root.join(relative.as_str()).exists())
        .cloned()
        .collect::<Vec<_>>();
    let unresolved_audit_findings =
        unresolved_owned_audit_findings(repo_root, task_id, task_markdown);

    TaskCompletionEvidence {
        has_review_handoff: review_contains_task(&review_text, task_id),
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

fn declared_completion_artifacts(task_markdown: &str) -> Vec<String> {
    parse_shared_tasks(task_markdown)
        .into_iter()
        .next()
        .map(|task| task.completion_artifacts)
        .unwrap_or_default()
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct AuditManifest {
    #[serde(default)]
    files: Vec<AuditManifestFile>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct AuditManifestFile {
    path: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    verdict: String,
}

fn unresolved_owned_audit_findings(
    repo_root: &Path,
    task_id: &str,
    task_markdown: &str,
) -> Vec<String> {
    if !task_id.starts_with("AUD-") {
        return Vec::new();
    }
    let manifest_path = repo_root.join("audit/MANIFEST.json");
    if !manifest_path.exists() {
        return Vec::new();
    }
    let owned_patterns = audit_owned_path_patterns(task_markdown);
    if owned_patterns.is_empty() {
        return Vec::new();
    }

    let manifest_text = match fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(err) => {
            return vec![format!(
                "failed to read `{}`: {err}",
                manifest_path.display()
            )]
        }
    };
    let manifest = match serde_json::from_str::<AuditManifest>(&manifest_text) {
        Ok(manifest) => manifest,
        Err(err) => return vec![format!("invalid `{}`: {err}", manifest_path.display())],
    };

    let mut unresolved = manifest
        .files
        .into_iter()
        .filter(audit_manifest_file_is_unresolved)
        .filter(|file| {
            owned_patterns
                .iter()
                .any(|pattern| audit_owned_pattern_matches(pattern, &file.path))
        })
        .map(|file| format!("{} {} ({})", file.verdict, file.path, file.status))
        .collect::<Vec<_>>();
    unresolved.sort();
    unresolved
}

fn audit_manifest_file_is_unresolved(file: &AuditManifestFile) -> bool {
    matches!(
        file.verdict.as_str(),
        "DRIFT-LARGE" | "DRIFT-SMALL" | "REFACTOR" | "RETIRE"
    ) || matches!(file.status.as_str(), "ApplyFailed" | "Escalated")
}

fn audit_owned_path_patterns(task_markdown: &str) -> Vec<String> {
    let Some(body) = task_field_body_until_any(task_markdown, "Owns:", TASK_FIELD_BOUNDARIES)
    else {
        return Vec::new();
    };

    let mut patterns = Vec::new();
    for fragment in body.lines().flat_map(backtick_fragments) {
        if audit_owned_token_looks_path_like(&fragment) {
            patterns.push(normalize_audit_owned_pattern(&fragment));
        }
    }
    if patterns.is_empty() {
        for token in body
            .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == ';')
            .map(|token| token.trim_matches(|ch: char| "`:.()[]".contains(ch)))
            .filter(|token| audit_owned_token_looks_path_like(token))
        {
            patterns.push(normalize_audit_owned_pattern(token));
        }
    }
    patterns.sort();
    patterns.dedup();
    patterns
}

fn audit_owned_token_looks_path_like(token: &str) -> bool {
    let token = token.trim();
    !token.is_empty()
        && (token.contains('/')
            || token.contains('*')
            || token.ends_with(".md")
            || token.ends_with(".rs")
            || token.ends_with(".ts")
            || token.ends_with(".tsx")
            || token == "AGENTS.md"
            || token == "WORKLIST.md"
            || token == "IMPLEMENTATION_PLAN.md"
            || token == "REVIEW.md")
}

fn normalize_audit_owned_pattern(pattern: &str) -> String {
    pattern
        .trim()
        .trim_matches('`')
        .trim_start_matches("./")
        .trim_matches(|ch: char| ch == ',' || ch == ';')
        .to_string()
}

fn audit_owned_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./");
    let path = path.trim_start_matches("./");
    if pattern == path {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if let Some(prefix) = pattern.strip_suffix("/**/*") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if let Some(prefix) = pattern.strip_suffix('/') {
        return path.starts_with(&format!("{prefix}/"));
    }
    if !pattern.contains('*') {
        return false;
    }
    wildcard_match(pattern.as_bytes(), path.as_bytes())
}

fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star = None::<usize>;
    let mut match_after_star = 0usize;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            match_after_star = t;
            p += 1;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            match_after_star += 1;
            t = match_after_star;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn summarize_unresolved_audit_findings(findings: &[String]) -> String {
    const MAX_RENDERED: usize = 8;
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

fn verification_receipt_path(repo_root: &Path, task_id: &str) -> PathBuf {
    verification_receipt_root(repo_root).join(format!("{task_id}.json"))
}

fn verification_receipt_root(repo_root: &Path) -> PathBuf {
    if repo_root.file_name().and_then(|name| name.to_str()) == Some("repo") {
        let ancestors = repo_root.ancestors().collect::<Vec<_>>();
        if ancestors
            .iter()
            .any(|ancestor| ancestor.file_name().and_then(|name| name.to_str()) == Some("lanes"))
        {
            if let Some(auto_root) = ancestors.iter().find(|ancestor| {
                ancestor.file_name().and_then(|name| name.to_str()) == Some(".auto")
            }) {
                return auto_root.join("symphony/verification-receipts");
            }
        }
    }

    repo_root.join(".auto/symphony/verification-receipts")
}

pub(crate) fn verification_plan(task_markdown: &str) -> VerificationPlan {
    let Some(body) = parse_shared_tasks(task_markdown)
        .into_iter()
        .next()
        .and_then(|task| task.verification_text)
    else {
        return VerificationPlan::default();
    };

    let steps = body
        .lines()
        .map(strip_list_bullet)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let executable_commands = steps
        .iter()
        .flat_map(|step| executable_commands_from_verification_step(step))
        .collect::<Vec<_>>();
    let narrative_guidance = steps
        .iter()
        .filter(|step| executable_commands_from_verification_step(step).is_empty())
        .cloned()
        .collect::<Vec<_>>();
    VerificationPlan {
        steps,
        executable_commands,
        narrative_guidance,
    }
}

fn verification_step_looks_external(step: &str) -> bool {
    let step = step.to_ascii_lowercase();
    [
        "http://",
        "https://",
        "ssh ",
        "kubectl",
        "hcloud",
        "github ui",
        "grafana import",
        "reference host",
        "loom host",
        "staging alertmanager",
        "external dogfood",
        "deploy_house.sh deploy",
    ]
    .into_iter()
    .any(|marker| step.contains(marker))
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct VerificationReceipt {
    #[serde(default)]
    commands: Vec<VerificationReceiptCommand>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct VerificationReceiptCommand {
    command: String,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    supersedes: Vec<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    runner_summary: Option<VerificationRunnerSummary>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct VerificationRunnerSummary {
    #[serde(default)]
    zero_test_detected: bool,
}

fn inspect_verification_receipt(
    verification_receipt_required: bool,
    verification_wrapper_present: bool,
    verification_receipt_path: &Path,
    expected_commands: &[String],
) -> (bool, Option<String>) {
    if !verification_receipt_required {
        return (true, None);
    }
    if !verification_wrapper_present {
        return (
            false,
            Some(format!(
                "missing scripts/run-task-verification.sh; executable Verification command(s) need receipt-backed proof: {}",
                expected_commands
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }
    if !verification_receipt_path.exists() {
        return (false, None);
    }

    let receipt_text = match fs::read_to_string(verification_receipt_path) {
        Ok(text) => text,
        Err(err) => {
            return (
                false,
                Some(format!(
                    "failed to read verification receipt `{}`: {err}",
                    verification_receipt_path.display()
                )),
            );
        }
    };
    let receipt = match serde_json::from_str::<VerificationReceipt>(&receipt_text) {
        Ok(receipt) => receipt,
        Err(err) => {
            return (
                false,
                Some(format!(
                    "invalid verification receipt `{}`: {err}",
                    verification_receipt_path.display()
                )),
            );
        }
    };

    let mut missing = expected_commands
        .iter()
        .filter(|command| {
            !receipt
                .commands
                .iter()
                .any(|entry| verification_receipt_command_matches(entry, command))
        })
        .cloned()
        .collect::<Vec<_>>();
    missing.sort();
    if !missing.is_empty() {
        return (
            false,
            Some(format!(
                "verification receipt `{}` is missing command(s): {}",
                verification_receipt_path.display(),
                missing
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }

    let mut failed = expected_commands
        .iter()
        .filter(|command| {
            let matching_entries = receipt
                .commands
                .iter()
                .filter(|entry| verification_receipt_command_matches(entry, command))
                .collect::<Vec<_>>();
            !matching_entries.is_empty()
                && matching_entries.iter().all(|entry| {
                    entry.status.as_deref() != Some("passed") || entry.exit_code != Some(0)
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    failed.sort();
    if !failed.is_empty() {
        return (
            false,
            Some(format!(
                "verification receipt `{}` has failed command(s): {}",
                verification_receipt_path.display(),
                failed
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }

    let mut zero_test = expected_commands
        .iter()
        .filter(|command| {
            receipt
                .commands
                .iter()
                .filter(|entry| verification_receipt_command_matches(entry, command))
                .any(verification_receipt_reports_zero_tests)
        })
        .cloned()
        .collect::<Vec<_>>();
    zero_test.sort();
    if !zero_test.is_empty() {
        return (
            false,
            Some(format!(
                "verification receipt `{}` reported zero-test run(s): {}",
                verification_receipt_path.display(),
                zero_test
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }

    let mut unsuperseded_failed = receipt
        .commands
        .iter()
        .filter(|entry| !verification_receipt_command_passed(entry))
        .filter(|entry| {
            !verification_receipt_failed_entry_is_superseded(
                entry,
                &receipt.commands,
                expected_commands,
            )
        })
        .map(|entry| entry.command.clone())
        .collect::<Vec<_>>();
    unsuperseded_failed.sort();
    unsuperseded_failed.dedup();
    if !unsuperseded_failed.is_empty() {
        return (
            false,
            Some(format!(
                "verification receipt `{}` has unsuperseded failed command(s): {}",
                verification_receipt_path.display(),
                unsuperseded_failed
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }

    (true, None)
}

fn verification_receipt_command_passed(entry: &VerificationReceiptCommand) -> bool {
    entry.status.as_deref() == Some("passed") && entry.exit_code == Some(0)
}

fn verification_receipt_failed_entry_is_superseded(
    failed_entry: &VerificationReceiptCommand,
    all_entries: &[VerificationReceiptCommand],
    expected_commands: &[String],
) -> bool {
    all_entries.iter().any(|entry| {
        verification_receipt_command_passed(entry)
            && expected_commands
                .iter()
                .any(|expected| verification_receipt_command_matches(entry, expected))
            && entry
                .supersedes
                .iter()
                .any(|superseded| superseded == &failed_entry.command)
    })
}

fn verification_receipt_reports_zero_tests(entry: &VerificationReceiptCommand) -> bool {
    entry.status.as_deref() == Some("passed")
        && entry.exit_code == Some(0)
        && entry
            .runner_summary
            .as_ref()
            .is_some_and(|summary| summary.zero_test_detected)
}

fn verification_receipt_command_matches(
    entry: &VerificationReceiptCommand,
    expected_command: &str,
) -> bool {
    if entry.command == expected_command {
        return true;
    }

    if entry.argv.is_empty() {
        return false;
    }

    shell_split(expected_command)
        .map(|expected_argv| expected_argv == entry.argv)
        .unwrap_or(false)
}

fn executable_commands_from_verification_step(step: &str) -> Vec<String> {
    let step = step.trim();
    if step.is_empty() {
        return Vec::new();
    }

    let backtick_commands = backtick_fragments(step)
        .into_iter()
        .filter(|candidate| looks_like_executable_command(candidate))
        .collect::<Vec<_>>();
    if !backtick_commands.is_empty() {
        return backtick_commands;
    }

    let candidate = truncate_verification_narrative(step);
    if looks_like_executable_command(candidate) {
        vec![candidate.to_string()]
    } else {
        Vec::new()
    }
}

fn backtick_fragments(line: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else {
            break;
        };
        let candidate = rest[..end].trim();
        if !candidate.is_empty() {
            fragments.push(candidate.to_string());
        }
        rest = &rest[end + 1..];
    }
    fragments
}

fn truncate_verification_narrative(step: &str) -> &str {
    let narrative_markers = [
        "; same command",
        "; production",
        "; glossary",
        "; privacy audit",
        " exits ",
        " returns ",
        " starts ",
        " succeeds ",
        " fails ",
        " within ",
        " without ",
    ];
    let lower = step.to_ascii_lowercase();
    let cut = narrative_markers
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
        .unwrap_or(step.len());
    step[..cut].trim()
}

fn looks_like_executable_command(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.starts_with('-') || candidate.contains('→') {
        return false;
    }

    let first = candidate.split_whitespace().next().unwrap_or_default();
    if first.is_empty() {
        return false;
    }

    if is_env_assignment(first) {
        return candidate.split_whitespace().nth(1).is_some();
    }

    let shell_prefixes = [
        "./", "cargo", "bash", "sh", "python", "python3", "node", "pnpm", "npm", "yarn", "rg",
        "grep", "curl", "ssh", "docker", "kubectl", "git", "make", "just", "uv", "go", "pytest",
        "scripts/",
    ];
    shell_prefixes
        .iter()
        .any(|prefix| first == *prefix || first.starts_with(prefix))
        && candidate.split_whitespace().nth(1).is_some()
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, value)) = token.split_once('=') else {
        return false;
    };
    !value.is_empty()
        || token.ends_with('=')
            && !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn strip_list_bullet(line: &str) -> &str {
    let trimmed = line.trim_start();
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return rest;
        }
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{
        assess_task_completion_gap, declared_completion_artifacts, ensure_host_review_handoff,
        inspect_task_completion_evidence, review_contains_task, verification_plan,
        CompletionGapKind, TaskCompletionEvidence,
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
            r#"{"commands":[{"command":"cargo test -p demo receipt_example","exit_code":0,"status":"passed"}]}"#,
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
    fn verification_plan_preserves_narrative_without_treating_it_as_shell() {
        let plan = verification_plan(
            "- [~] `TASK-5` Dashboard task\nVerification:\n  - Grafana import on reference host succeeds; glossary cross-links resolve.\nRequired tests: none\nDependencies: none\n",
        );
        assert!(plan.executable_commands.is_empty());
        assert_eq!(plan.narrative_guidance.len(), 1);
    }

    #[test]
    fn verification_plan_extracts_backtick_commands_without_bare_flags() {
        let plan = verification_plan(
            "- [~] `TASK-6` Fail fast\nVerification:\n  - `BITINO_HOUSE_SESSION_SECRET= cargo run -p bitino-house` exits non-zero; same command with `--dev` starts + warns; production container with `--dev` fails CI.\nRequired tests: none\nDependencies: none\n",
        );
        assert_eq!(
            plan.executable_commands,
            vec!["BITINO_HOUSE_SESSION_SECRET= cargo run -p bitino-house".to_string()]
        );
        assert!(plan.narrative_guidance.is_empty());
    }

    #[test]
    fn verification_plan_stops_at_dependencies_before_completion_notes() {
        let plan = verification_plan(
            "- [x] `TASK-9` Completed\nVerification: `rg -n 'Thing' src`\nDependencies: none\nEstimated scope: S\n- Completed 2026-04-21: added proof.\n- Verification 2026-04-21: `cargo test -p demo hidden`\n",
        );
        assert_eq!(
            plan.executable_commands,
            vec!["rg -n 'Thing' src".to_string()]
        );
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
