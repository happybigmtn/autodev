//! Independent Codex diff-review gate for `auto parallel`.
//!
//! Encodes the openclaw "autoreview" contract: before a completed lane is
//! marked `[x]` (landed/Done), an independent second-model review of the
//! lane's diff runs as a closeout. The gate is a *status-downgrade* gate, not
//! a nested fix-loop:
//!
//! - CLEAN review: the lane lands `[x]` exactly as before.
//! - FINDINGS: the committed work still lands, but the task is held at `[~]`
//!   (Partial) and the structured findings are appended to `REVIEW.md` so the
//!   next parallel pass / review-wave picks them up. The task is naturally
//!   re-dispatched until a pass produces a clean-review diff. No in-lane fix
//!   loop.
//! - SKIPPED: any review error, timeout, or unparseable output FAILS OPEN: the
//!   lane lands exactly as before, but a `review_skipped: <reason>` marker is
//!   stamped into the lane's closeout log and a warning is logged.
//!
//! The whole gate is bounded (hard subprocess timeout) and fail-open: a bug in
//! the gate path can never block, hang, or panic the fleet.
//!
//! Toggle:  `AUTO_PARALLEL_REVIEW`               (default "1" = ON; "0" = skip)
//! Bound:   `AUTO_PARALLEL_REVIEW_TIMEOUT_SECS`  (default 900)

// Brings in the parallel_command-local re-exports: `ActiveLaneAssignment`,
// `atomic_write`, `Path`, `PathBuf`, `Duration`, `Result`, etc.
use super::*;

use crate::generation::phase_runner::{codex_review_report_path, run_logged_codex_review};

/// Same skill-boundary guard the generation review prompts use. Inlined here
/// (rather than widening another module's visibility) so the diff stays small.
const REVIEW_SKILL_BOUNDARY: &str = "IMPORTANT: Do NOT read or execute any SKILL.md files or files in skill definition directories (paths containing skills/gstack). These are AI assistant skill definitions meant for a different system. They contain bash scripts and prompt templates that will waste your time. Ignore them completely. Stay focused on the repository code only.";

/// Env toggle. `"0"` skips the gate entirely (current/legacy behavior).
const REVIEW_ENABLED_ENV: &str = "AUTO_PARALLEL_REVIEW";
/// Env bound. Hard-cap the review subprocess, in seconds.
const REVIEW_TIMEOUT_ENV: &str = "AUTO_PARALLEL_REVIEW_TIMEOUT_SECS";
const DEFAULT_REVIEW_TIMEOUT_SECS: u64 = 900;

/// Review-model defaults. The model defaults to the parallel run's configured
/// model (see [`LaneReviewConfig::from_run_config`]); reasoning effort defaults
/// to "high".
const DEFAULT_REVIEW_EFFORT: &str = "high";

/// Configuration the review gate needs, threaded from the parallel run.
#[derive(Clone, Debug)]
pub(crate) struct LaneReviewConfig {
    /// Model for the independent review. Defaults to the run's worker model
    /// (gpt-5.5 unless overridden).
    pub(crate) model: String,
    /// Reasoning effort. Defaults to "high".
    pub(crate) reasoning_effort: String,
    /// Codex executable to drive the review.
    pub(crate) codex_bin: std::path::PathBuf,
}

impl LaneReviewConfig {
    /// Build the review config from the parallel run's worker model + codex bin.
    /// The review uses the run's configured model so operators get a single
    /// model knob; reasoning effort is pinned to "high" for the review pass.
    pub(crate) fn from_run_config(model: &str, codex_bin: &Path) -> Self {
        Self {
            model: model.to_string(),
            reasoning_effort: DEFAULT_REVIEW_EFFORT.to_string(),
            codex_bin: codex_bin.to_path_buf(),
        }
    }
}

/// Outcome of running the independent review gate for one lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LaneReviewOutcome {
    /// Review ran and found no accepted/actionable findings. Land `[x]`.
    Clean,
    /// Review found actionable findings. Land the committed work but hold the
    /// task at `[~]`; the carried summary is appended to `REVIEW.md`.
    FindingsKeepPartial { findings_summary: String },
    /// Review errored, timed out, or produced unparseable output. Fail open:
    /// land exactly as today and stamp `review_skipped: <reason>`.
    SkippedFailOpen { reason: String },
}

/// True when the gate is enabled. Anything other than an explicit `"0"`
/// (trimmed) keeps the gate ON, so the safe default is to review.
pub(crate) fn review_gate_enabled() -> bool {
    match std::env::var(REVIEW_ENABLED_ENV) {
        Ok(value) => value.trim() != "0",
        Err(_) => true,
    }
}

/// Resolve the hard subprocess timeout, honoring the env override. Invalid or
/// zero values fall back to the default rather than disabling the bound.
fn review_timeout() -> Duration {
    let secs = std::env::var(REVIEW_TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_REVIEW_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Build the independent review prompt encoding the autoreview contract. The
/// prompt mirrors the generation review's structured-report convention: the
/// reviewer writes a markdown report to `report_path` whose first non-blank
/// line is a machine-parseable `VERDICT:` line the caller classifies.
pub(crate) fn build_lane_review_prompt(
    repo_root: &Path,
    target_branch: &str,
    task_id: &str,
    changed_files: &[String],
    report_path: &Path,
) -> String {
    let files = if changed_files.is_empty() {
        "(host recorded no changed files; inspect the diff against the target branch directly)"
            .to_string()
    } else {
        changed_files
            .iter()
            .map(|path| format!("- `{path}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        r#"{skill_boundary}

You are an INDEPENDENT second-model code reviewer acting as the closeout gate for one landed lane of `auto parallel`. This is the openclaw "autoreview" contract: an independent code review of THIS lane's diff only, run before the host marks the task complete.

Repository: `{repo_root}`
Task under review: `{task_id}`
Target branch: `{target_branch}`

Lane diff surface (changed files the host recorded for this task):
{files}

Inspect the diff for this lane against the target branch (for example: `git diff {target_branch}...HEAD -- <files>`, or `git show` on the landed commits). Review ONLY this lane's changes.

Your job and its strict boundaries:
- This is an ADVISORY review. Report ONLY concrete, actionable problems that are clearly attributable to THIS diff: real correctness bugs, real security risks, or clear regressions that the diff introduces.
- REJECT speculative findings, hypothetical edge cases, "what if" concerns, style nits, and anything you are not confident is a real defect in this diff.
- Do NOT request refactors, rewrites, broad redesigns, added abstractions, or scope expansion. Those are not findings.
- Do NOT spawn or request any nested reviewers, sub-agents, or further review passes.
- Do NOT edit any source files, queue files, or `REVIEW.md`/`IMPLEMENTATION_PLAN.md`. You only WRITE the report at the path below. The host owns all queue and review state.
- Do not ask the user questions. Make conservative, code-grounded decisions.

Verdict rule:
- If the diff has NO accepted actionable findings under the bar above, the verdict is CLEAN.
- If and only if you find at least one concrete actionable bug, security risk, or clear regression introduced by this diff, the verdict is FINDINGS.
- When uncertain whether something clears the bar, prefer CLEAN. A false FINDINGS needlessly re-dispatches the task.

Write your report to `{report_path}` as markdown with EXACTLY this shape:
- The FIRST non-blank line MUST be either `VERDICT: CLEAN` or `VERDICT: FINDINGS` (uppercase, no other text on that line).
- Then a `## Summary` section: one or two sentences on what you reviewed and the verdict rationale.
- If and only if the verdict is FINDINGS, a `## Findings` section listing each accepted finding as a numbered item: the file/location, what is wrong, and why it is a real defect introduced by this diff. Keep each finding concrete and actionable.
- If the verdict is CLEAN, omit `## Findings` (or leave it empty) and briefly note in `## Summary` what you checked.

Use only lightweight local inspection (`git diff`, `git show`, `rg`, targeted file reads). Do not run integration suites or production-affecting commands.
"#,
        skill_boundary = REVIEW_SKILL_BOUNDARY,
        repo_root = repo_root.display(),
        target_branch = target_branch,
        task_id = task_id,
        files = files,
        report_path = report_path.display(),
    )
}

/// Classify a review report's text into a [`LaneReviewOutcome`]. The first
/// non-blank line is the authoritative verdict. Unparseable output (no verdict
/// line, empty report) fails open as `SkippedFailOpen` per the contract.
pub(crate) fn classify_review_report(report_text: &str) -> LaneReviewOutcome {
    let mut verdict_line: Option<&str> = None;
    for line in report_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        verdict_line = Some(trimmed);
        break;
    }
    let Some(first) = verdict_line else {
        return LaneReviewOutcome::SkippedFailOpen {
            reason: "review report was empty".to_string(),
        };
    };
    let upper = first.to_ascii_uppercase();
    if upper.starts_with("VERDICT: CLEAN") || upper.starts_with("VERDICT:CLEAN") {
        return LaneReviewOutcome::Clean;
    }
    if upper.starts_with("VERDICT: FINDINGS") || upper.starts_with("VERDICT:FINDINGS") {
        let findings_summary = extract_findings_summary(report_text);
        return LaneReviewOutcome::FindingsKeepPartial { findings_summary };
    }
    LaneReviewOutcome::SkippedFailOpen {
        reason: format!("review report verdict line was unparseable: {first:?}"),
    }
}

/// Pull the human-readable findings body out of the report for REVIEW.md. Falls
/// back to the whole report when no `## Findings` / `## Summary` section is
/// present.
fn extract_findings_summary(report_text: &str) -> String {
    if let Some(section) = extract_markdown_section(report_text, "## Findings") {
        if !section.trim().is_empty() {
            return section.trim().to_string();
        }
    }
    if let Some(section) = extract_markdown_section(report_text, "## Summary") {
        if !section.trim().is_empty() {
            return section.trim().to_string();
        }
    }
    report_text.trim().to_string()
}

/// Return the body of the named `##` heading up to the next `##` heading.
fn extract_markdown_section(text: &str, heading: &str) -> Option<String> {
    let mut lines = text.lines();
    let mut body = String::new();
    let mut in_section = false;
    for line in lines.by_ref() {
        if in_section {
            if line.trim_start().starts_with("## ") {
                break;
            }
            body.push_str(line);
            body.push('\n');
        } else if line.trim() == heading {
            in_section = true;
        }
    }
    if in_section {
        Some(body)
    } else {
        None
    }
}

/// Append independent-review findings to `REVIEW.md` under a clearly marked
/// block so the next parallel/review pass picks them up. The host's existing
/// `ensure_host_review_handoff` may already have written a `## `<task>`` entry;
/// this appends a distinct findings block rather than collide with it. Best
/// effort — a write failure must not block landing.
pub(crate) fn append_lane_review_findings(
    repo_root: &Path,
    task_id: &str,
    findings_summary: &str,
) -> Result<()> {
    let review_path = repo_root.join("REVIEW.md");
    let mut review_text = if review_path.exists() {
        std::fs::read_to_string(&review_path)?
    } else {
        "# REVIEW\n\nAwaiting auto review:\n".to_string()
    };
    if !review_text.ends_with('\n') {
        review_text.push('\n');
    }
    review_text.push_str(&render_lane_review_findings_entry(task_id, findings_summary));
    atomic_write(&review_path, review_text.as_bytes())?;
    Ok(())
}

fn render_lane_review_findings_entry(task_id: &str, findings_summary: &str) -> String {
    let body = if findings_summary.trim().is_empty() {
        "Independent review reported actionable findings but recorded no detail.".to_string()
    } else {
        findings_summary.trim().to_string()
    };
    format!(
        "\n## `{task_id}`: independent review findings\n\
- Source: auto parallel independent diff-review gate (held at `[~]`).\n\
- These findings were raised against the landed diff. Address them, then the task\n  re-dispatches until an independent review of the diff is clean.\n\n\
{body}\n"
    )
}

/// Run the bounded, fail-open independent review gate for one lane.
///
/// This is the public entry wired into the landing seam. It runs a real Codex
/// review via [`run_logged_codex_review`], bounded by the configured timeout,
/// and classifies the resulting report. Every error path degrades to
/// `SkippedFailOpen` — this function never returns `Err` and never panics.
pub(crate) async fn run_lane_review_gate(
    repo_root: &Path,
    target_branch: &str,
    assignment: &ActiveLaneAssignment,
    changed_files: &[String],
    config: &LaneReviewConfig,
) -> LaneReviewOutcome {
    let report_path = codex_review_report_path(repo_root, "parallel-lane-review");
    let prompt = build_lane_review_prompt(
        repo_root,
        target_branch,
        &assignment.task.id,
        changed_files,
        &report_path,
    );
    let runner = run_logged_codex_review(
        repo_root,
        "parallel-lane-review",
        &prompt,
        &config.model,
        &config.reasoning_effort,
        &config.codex_bin,
        &report_path,
    );
    match tokio::time::timeout(review_timeout(), runner).await {
        Ok(Ok(summary)) => match std::fs::read_to_string(&summary.report_path) {
            Ok(report_text) => classify_review_report(&report_text),
            Err(err) => LaneReviewOutcome::SkippedFailOpen {
                reason: format!("could not read review report: {err}"),
            },
        },
        Ok(Err(err)) => LaneReviewOutcome::SkippedFailOpen {
            reason: format!("review subprocess failed: {err:#}"),
        },
        Err(_) => LaneReviewOutcome::SkippedFailOpen {
            reason: format!("review timed out after {}s", review_timeout().as_secs()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "autodev-review-gate-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    // --- toggle: AUTO_PARALLEL_REVIEW=0 bypasses the gate -------------------

    #[test]
    fn review_gate_disabled_when_env_is_zero() {
        // Env-mutating test: run serially-safe via a saved/restored guard.
        let prev = std::env::var(REVIEW_ENABLED_ENV).ok();
        std::env::set_var(REVIEW_ENABLED_ENV, "0");
        assert!(!review_gate_enabled());
        std::env::set_var(REVIEW_ENABLED_ENV, " 0 ");
        assert!(!review_gate_enabled(), "trimmed 0 still disables");
        std::env::set_var(REVIEW_ENABLED_ENV, "1");
        assert!(review_gate_enabled());
        std::env::remove_var(REVIEW_ENABLED_ENV);
        assert!(review_gate_enabled(), "default-on when unset");
        match prev {
            Some(value) => std::env::set_var(REVIEW_ENABLED_ENV, value),
            None => std::env::remove_var(REVIEW_ENABLED_ENV),
        }
    }

    // --- clean review -> Clean outcome -------------------------------------

    #[test]
    fn clean_verdict_classifies_as_clean() {
        let report = "VERDICT: CLEAN\n\n## Summary\nChecked the diff; no real defects.\n";
        assert_eq!(classify_review_report(report), LaneReviewOutcome::Clean);
    }

    #[test]
    fn clean_verdict_tolerates_leading_blank_lines_and_no_space() {
        let report = "\n\n  VERDICT:CLEAN\n## Summary\nfine\n";
        assert_eq!(classify_review_report(report), LaneReviewOutcome::Clean);
    }

    // --- findings -> FindingsKeepPartial + REVIEW.md append ----------------

    #[test]
    fn findings_verdict_classifies_and_carries_summary() {
        let report = "VERDICT: FINDINGS\n\n## Summary\nFound a bug.\n\n## Findings\n1. `src/x.rs`: off-by-one in the loop bound introduced by this diff.\n";
        let outcome = classify_review_report(report);
        match outcome {
            LaneReviewOutcome::FindingsKeepPartial { findings_summary } => {
                assert!(findings_summary.contains("off-by-one"));
                assert!(findings_summary.contains("src/x.rs"));
            }
            other => panic!("expected FindingsKeepPartial, got {other:?}"),
        }
    }

    #[test]
    fn findings_append_writes_marked_block_to_review_md() {
        let dir = temp_dir("findings-append");
        // Pre-existing REVIEW.md with a host handoff entry for the same task.
        fs::write(
            dir.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n\n## `TASK-001`\n- Source: host handoff.\n",
        )
        .unwrap();
        append_lane_review_findings(&dir, "TASK-001", "1. `src/x.rs`: real bug here.")
            .expect("append should succeed");
        let review = fs::read_to_string(dir.join("REVIEW.md")).unwrap();
        assert!(review.contains("## `TASK-001`: independent review findings"));
        assert!(review.contains("real bug here"));
        // Host handoff entry is preserved, not clobbered.
        assert!(review.contains("- Source: host handoff."));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn findings_append_creates_review_md_when_missing() {
        let dir = temp_dir("findings-create");
        append_lane_review_findings(&dir, "TASK-002", "1. `src/y.rs`: regression.")
            .expect("append should succeed");
        let review = fs::read_to_string(dir.join("REVIEW.md")).unwrap();
        assert!(review.starts_with("# REVIEW"));
        assert!(review.contains("## `TASK-002`: independent review findings"));
        assert!(review.contains("regression"));
        let _ = fs::remove_dir_all(&dir);
    }

    // --- review error / unparseable -> SkippedFailOpen ---------------------

    #[test]
    fn empty_report_fails_open() {
        let outcome = classify_review_report("   \n\n  \n");
        match outcome {
            LaneReviewOutcome::SkippedFailOpen { reason } => {
                assert!(reason.contains("empty"));
            }
            other => panic!("expected SkippedFailOpen, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_verdict_fails_open() {
        let outcome = classify_review_report("I think this is fine, no verdict line here.\n");
        match outcome {
            LaneReviewOutcome::SkippedFailOpen { reason } => {
                assert!(reason.contains("unparseable"));
            }
            other => panic!("expected SkippedFailOpen, got {other:?}"),
        }
    }

    // --- prompt encodes the autoreview contract ----------------------------

    #[test]
    fn prompt_encodes_autoreview_contract() {
        let prompt = build_lane_review_prompt(
            Path::new("/repo"),
            "main",
            "TASK-007",
            &["src/a.rs".to_string(), "src/b.rs".to_string()],
            Path::new("/repo/.auto/logs/report.md"),
        );
        assert!(prompt.contains("INDEPENDENT"));
        assert!(prompt.contains("ADVISORY"));
        assert!(prompt.contains("THIS lane's diff only") || prompt.contains("THIS lane's changes"));
        assert!(prompt.contains("Do NOT request refactors"));
        assert!(prompt.contains("nested reviewers"));
        assert!(prompt.contains("VERDICT: CLEAN"));
        assert!(prompt.contains("VERDICT: FINDINGS"));
        assert!(prompt.contains("TASK-007"));
        assert!(prompt.contains("src/a.rs"));
        assert!(prompt.contains("prefer CLEAN"));
    }

    // --- timeout resolution ------------------------------------------------

    #[test]
    fn timeout_honors_env_and_falls_back() {
        let prev = std::env::var(REVIEW_TIMEOUT_ENV).ok();
        std::env::set_var(REVIEW_TIMEOUT_ENV, "42");
        assert_eq!(review_timeout(), Duration::from_secs(42));
        std::env::set_var(REVIEW_TIMEOUT_ENV, "0");
        assert_eq!(
            review_timeout(),
            Duration::from_secs(DEFAULT_REVIEW_TIMEOUT_SECS),
            "zero falls back to default"
        );
        std::env::set_var(REVIEW_TIMEOUT_ENV, "not-a-number");
        assert_eq!(
            review_timeout(),
            Duration::from_secs(DEFAULT_REVIEW_TIMEOUT_SECS),
            "garbage falls back to default"
        );
        std::env::remove_var(REVIEW_TIMEOUT_ENV);
        assert_eq!(
            review_timeout(),
            Duration::from_secs(DEFAULT_REVIEW_TIMEOUT_SECS)
        );
        match prev {
            Some(value) => std::env::set_var(REVIEW_TIMEOUT_ENV, value),
            None => std::env::remove_var(REVIEW_TIMEOUT_ENV),
        }
    }
}
