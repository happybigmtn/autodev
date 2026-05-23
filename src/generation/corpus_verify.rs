//! Validation and sanitization of `auto corpus` planning outputs.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::generation::markdown::{
    markdown_section_contains, markdown_section_has_nonempty_body, split_markdown_section,
    strip_ordered_list_marker,
};
use crate::generation::planning_root::ActivePlanSurface;
use crate::generation::print_stage;
use crate::generation::prompts::{
    CORPUS_DELETE_DEMOTE_MARKERS, CORPUS_LEGACY_EXECPLAN_REQUIRED_SECTIONS,
    CORPUS_NEXT_LEVER_MARKERS, CORPUS_PRIORITY_PLAN_REQUIRED_SECTIONS,
    CORPUS_REPORT_REQUIRED_SECTIONS,
};
use crate::util::{atomic_write, list_markdown_files};

#[derive(Debug)]
pub(crate) struct CorpusOutputSummary {
    pub(crate) assessment_path: PathBuf,
    pub(crate) spec_path: PathBuf,
    pub(crate) plans_index_path: PathBuf,
    pub(crate) report_path: PathBuf,
    pub(crate) design_path: Option<PathBuf>,
    pub(crate) focus_path: Option<PathBuf>,
    pub(crate) idea_path: Option<PathBuf>,
    pub(crate) plan_count: usize,
}

pub(crate) fn verify_corpus_outputs_read_only(
    repo_root: &Path,
    planning_root: &Path,
    focus_requested: bool,
    active_plan_surface: &ActivePlanSurface,
    run_started_at: Instant,
) -> Result<CorpusOutputSummary> {
    print_stage("verify corpus outputs", run_started_at);
    verify_corpus_outputs(
        repo_root,
        planning_root,
        focus_requested,
        active_plan_surface,
    )
}

pub(crate) fn sanitize_and_verify_corpus_outputs(
    repo_root: &Path,
    planning_root: &Path,
    focus_requested: bool,
    active_plan_surface: &ActivePlanSurface,
    run_started_at: Instant,
) -> Result<CorpusOutputSummary> {
    print_stage("sanitize corpus outputs", run_started_at);
    sanitize_corpus_outputs(repo_root, planning_root)?;

    print_stage("verify corpus outputs", run_started_at);
    let summary = verify_corpus_outputs(
        repo_root,
        planning_root,
        focus_requested,
        active_plan_surface,
    )?;
    Ok(summary)
}

pub(crate) fn verify_corpus_outputs(
    repo_root: &Path,
    planning_root: &Path,
    focus_requested: bool,
    active_plan_surface: &ActivePlanSurface,
) -> Result<CorpusOutputSummary> {
    let assessment_path = planning_root.join("ASSESSMENT.md");
    let spec_path = planning_root.join("SPEC.md");
    let plans_index_path = planning_root.join("PLANS.md");
    let report_path = planning_root.join("GENESIS-REPORT.md");
    let design_path = planning_root.join("DESIGN.md");
    let focus_path = planning_root.join("FOCUS.md");
    let plans_dir = planning_root.join("plans");

    for path in [
        &assessment_path,
        &spec_path,
        &plans_index_path,
        &report_path,
    ] {
        if !path.exists() {
            bail!("corpus generation did not write {}", path.display());
        }
    }
    let plan_files = list_markdown_files(&plans_dir)?
        .into_iter()
        .filter(|path| is_numbered_corpus_plan_file(path))
        .collect::<Vec<_>>();
    if plan_files.is_empty() {
        bail!(
            "corpus generation did not write any numbered plans under {}",
            plans_dir.display()
        );
    }
    for plan_path in &plan_files {
        verify_corpus_execplan(plan_path)?;
    }
    if focus_requested && !focus_path.exists() {
        bail!("corpus generation did not write {}", focus_path.display());
    }
    verify_corpus_semantics(
        repo_root,
        planning_root,
        &plans_index_path,
        &report_path,
        active_plan_surface,
    )?;
    Ok(CorpusOutputSummary {
        assessment_path,
        spec_path,
        plans_index_path,
        report_path,
        design_path: design_path.exists().then_some(design_path),
        focus_path: focus_path.exists().then_some(focus_path),
        idea_path: planning_root
            .join("IDEA.md")
            .exists()
            .then_some(planning_root.join("IDEA.md")),
        plan_count: plan_files.len(),
    })
}

fn is_numbered_corpus_plan_file(path: &Path) -> bool {
    let Some(filename) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let Some(stem) = filename.strip_suffix(".md") else {
        return false;
    };
    let bytes = stem.as_bytes();
    bytes.len() > 4 && bytes[..3].iter().all(|byte| byte.is_ascii_digit()) && bytes[3] == b'-'
}

fn sanitize_corpus_outputs(repo_root: &Path, planning_root: &Path) -> Result<()> {
    sanitize_corpus_numbered_plan_shapes(planning_root)?;
    sanitize_corpus_repo_root_paths(repo_root, planning_root)
}

pub(crate) fn sanitize_corpus_numbered_plan_shapes(planning_root: &Path) -> Result<()> {
    let plans_dir = planning_root.join("plans");
    if !plans_dir.is_dir() {
        return Ok(());
    }
    for path in list_markdown_files(&plans_dir)?
        .into_iter()
        .filter(|path| is_numbered_corpus_plan_file(path))
    {
        let markdown = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let without_front_matter = strip_leading_yaml_front_matter_before_title(&markdown)
            .unwrap_or_else(|| markdown.clone());
        let sanitized = normalize_corpus_execplan_headings(&without_front_matter);
        if sanitized != markdown {
            atomic_write(&path, sanitized.as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
    }
    Ok(())
}

fn strip_leading_yaml_front_matter_before_title(markdown: &str) -> Option<String> {
    let mut offset = 0;
    let mut lines = markdown.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end_matches(&['\r', '\n'][..]) != "---" {
        return None;
    }
    offset += first.len();

    for line in lines {
        let line_end = offset + line.len();
        if line.trim_end_matches(&['\r', '\n'][..]) == "---" {
            let rest = markdown[line_end..].trim_start_matches(&['\r', '\n'][..]);
            if rest.starts_with("# ") {
                return Some(rest.to_string());
            }
            return None;
        }
        offset = line_end;
    }
    None
}

fn normalize_corpus_execplan_headings(markdown: &str) -> String {
    let mut normalized = markdown
        .lines()
        .map(|line| {
            normalize_corpus_execplan_heading_line(line).unwrap_or_else(|| line.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");
    if markdown.ends_with('\n') {
        normalized.push('\n');
    }
    normalized
}

fn normalize_corpus_execplan_heading_line(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("## ")?;
    let unnumbered = strip_ordered_list_marker(rest).unwrap_or(rest).trim();
    let canonical = match unnumbered.to_ascii_lowercase().as_str() {
        "purpose and big picture" | "purpose / big picture" => "## Purpose / Big Picture",
        "priority decision" => "## Priority Decision",
        "user / operator outcome" | "user/operator outcome" | "user and operator outcome" => {
            "## User / Operator Outcome"
        }
        "evidence" => "## Evidence",
        "scope boundary" => "## Scope Boundary",
        "implementation slice" => "## Implementation Slice",
        "deferred" => "## Deferred",
        "requirements trace" => "## Requirements Trace",
        "scope boundaries" => "## Scope Boundaries",
        "progress" => "## Progress",
        "surprises and discoveries" | "surprises & discoveries" => "## Surprises & Discoveries",
        "decision log" => "## Decision Log",
        "outcomes and retrospective" | "outcomes & retrospective" => "## Outcomes & Retrospective",
        "context and orientation" => "## Context and Orientation",
        "plan of work" => "## Plan of Work",
        "implementation units" => "## Implementation Units",
        "concrete steps" => "## Concrete Steps",
        "validation and acceptance" => "## Validation and Acceptance",
        "idempotence and recovery" => "## Idempotence and Recovery",
        "artifacts and notes" => "## Artifacts and Notes",
        "interfaces and dependencies" => "## Interfaces and Dependencies",
        _ => return None,
    };
    Some(canonical.to_string())
}

pub(crate) fn sanitize_corpus_repo_root_paths(
    repo_root: &Path,
    planning_root: &Path,
) -> Result<()> {
    let repo_root_literal = repo_root.display().to_string();
    for path in list_markdown_files(planning_root)? {
        let markdown = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let sanitized = sanitize_markdown_repo_root_paths(&markdown, &repo_root_literal);
        if sanitized != markdown {
            atomic_write(&path, sanitized.as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
    }
    Ok(())
}

fn sanitize_markdown_repo_root_paths(markdown: &str, repo_root_literal: &str) -> String {
    let repo_root_shell = "\"$(git rev-parse --show-toplevel)\"";
    let mut sanitized = markdown.replace(
        &format!("cd {repo_root_literal}"),
        &format!("cd {repo_root_shell}"),
    );
    sanitized = sanitized.replace(
        &format!("cd `{repo_root_literal}`"),
        &format!("cd `{repo_root_shell}`"),
    );
    sanitized = sanitized.replace(&format!("`{repo_root_literal}`"), "the repository root");
    sanitized = sanitized.replace(&format!("{repo_root_literal}/"), "<repo-root>/");
    sanitized.replace(repo_root_literal, "<repo-root>")
}

fn verify_corpus_semantics(
    repo_root: &Path,
    planning_root: &Path,
    plans_index_path: &Path,
    report_path: &Path,
    active_plan_surface: &ActivePlanSurface,
) -> Result<()> {
    let repo_root_literal = repo_root.display().to_string();
    for path in list_markdown_files(planning_root)? {
        let markdown = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if markdown.contains(&repo_root_literal) {
            bail!(
                "corpus document {} contains absolute repo-root path {}; use repo-relative paths or links",
                path.display(),
                repo_root_literal
            );
        }
    }

    let report = fs::read_to_string(report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    verify_corpus_report_sections(report_path, &report)?;

    if !active_plan_surface.has_active_plans() {
        return Ok(());
    }

    let plans_index = fs::read_to_string(plans_index_path)
        .with_context(|| format!("failed to read {}", plans_index_path.display()))?;
    let primary_plan_path = active_plan_surface.primary_plan_path().unwrap_or("plans/");

    if !plans_index.contains(primary_plan_path) && !report.contains(primary_plan_path) {
        bail!(
            "corpus must explicitly reference the active root planning surface at `{}` when the repo already has active plans",
            primary_plan_path
        );
    }

    let combined = format!("{plans_index}\n{report}").to_ascii_lowercase();
    let acknowledges_subordination = [
        "subordinate",
        "reconcile",
        "reconciled",
        "active planning surface",
        "active master plan",
        "not a parallel control plane",
    ]
    .iter()
    .any(|needle| combined.contains(needle));

    if !acknowledges_subordination {
        bail!(
            "corpus must explicitly state that generated plans reconcile to the active root planning surface instead of creating a parallel plan universe"
        );
    }

    Ok(())
}

fn verify_corpus_report_sections(report_path: &Path, report: &str) -> Result<()> {
    let missing = missing_corpus_sections(report, &CORPUS_REPORT_REQUIRED_SECTIONS);
    if !missing.is_empty() {
        bail!(
            "GENESIS-REPORT.md must include non-empty production-steering sections: {}",
            missing.join(", ")
        );
    }

    let (_, next_lever_body) =
        split_markdown_section(report, "## Next Autodev Lever").ok_or_else(|| {
            anyhow::anyhow!(
                "{} is missing `## Next Autodev Lever`",
                report_path.display()
            )
        })?;
    let next_lever_lower = next_lever_body.to_ascii_lowercase();
    if !CORPUS_NEXT_LEVER_MARKERS
        .iter()
        .any(|marker| next_lever_lower.contains(marker))
    {
        bail!(
            "GENESIS-REPORT.md `## Next Autodev Lever` must recommend one immediate path across auto design, auto gen, auto parallel, active-run supervision, or human decision"
        );
    }

    let (_, delete_demote_body) = split_markdown_section(report, "## Delete Or Demote")
        .ok_or_else(|| {
            anyhow::anyhow!("{} is missing `## Delete Or Demote`", report_path.display())
        })?;
    let delete_demote_lower = delete_demote_body.to_ascii_lowercase();
    if !CORPUS_DELETE_DEMOTE_MARKERS
        .iter()
        .any(|marker| delete_demote_lower.contains(marker))
    {
        bail!(
            "GENESIS-REPORT.md `## Delete Or Demote` must explicitly name stale, evidence-only, docs-only, lower-priority, or no-current demotion tracks"
        );
    }

    Ok(())
}

pub(crate) fn verify_corpus_execplan(plan_path: &Path) -> Result<()> {
    let markdown = fs::read_to_string(plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let trimmed = markdown.trim_start();
    if trimmed.starts_with("```") {
        bail!(
            "corpus plan {} must be a markdown file containing the ExecPlan directly, not a fenced code block",
            plan_path.display()
        );
    }
    if !trimmed.starts_with("# ") {
        bail!(
            "corpus plan {} must start with a markdown title",
            plan_path.display()
        );
    }
    if corpus_plan_has_sections(&markdown, &CORPUS_PRIORITY_PLAN_REQUIRED_SECTIONS) {
        verify_corpus_priority_plan(plan_path, &markdown)?;
        return Ok(());
    }
    if !corpus_plan_has_sections(&markdown, &CORPUS_LEGACY_EXECPLAN_REQUIRED_SECTIONS) {
        let missing_priority =
            missing_corpus_sections(&markdown, &CORPUS_PRIORITY_PLAN_REQUIRED_SECTIONS).join(", ");
        let missing_legacy =
            missing_corpus_sections(&markdown, &CORPUS_LEGACY_EXECPLAN_REQUIRED_SECTIONS)
                .join(", ");
        bail!(
            "corpus plan {} must use either compact priority-plan sections (missing: {}) or legacy ExecPlan sections (missing: {})",
            plan_path.display(),
            missing_priority,
            missing_legacy
        );
    }
    verify_corpus_legacy_execplan(plan_path, &markdown)
}

fn corpus_plan_has_sections(markdown: &str, sections: &[&str]) -> bool {
    sections
        .iter()
        .all(|section| markdown_section_has_nonempty_body(markdown, section))
}

fn missing_corpus_sections(markdown: &str, sections: &[&str]) -> Vec<String> {
    sections
        .iter()
        .copied()
        .filter(|section| !markdown_section_has_nonempty_body(markdown, section))
        .map(str::to_string)
        .collect()
}

/// True when the line marks a research/decision/checkpoint plan with no test
/// expectation. Accepts the ASCII (`--`), em-dash (`—`, U+2014), and en-dash
/// (`–`, U+2013) typographies the corpus authoring model emits interchangeably.
fn line_marks_test_expectation_none(line: &str) -> bool {
    let normalized = line.to_ascii_lowercase().replace(['—', '–'], "--");
    normalized.contains("test expectation: none --")
}

/// True when the `## Implementation Slice` reads as a master-plan or dispatch
/// index — a plan whose deliverable is the list of slice plans below it, not a
/// code change or single decision. Master plans are produced as `001-*.md` by
/// the corpus authoring model and have no code-slice or decision-slice shape.
fn impl_slice_is_dispatch_index(markdown: &str) -> bool {
    markdown_section_contains(markdown, "## Implementation Slice", |line| {
        let lowered = line.to_ascii_lowercase();
        lowered.contains("plan dispatch")
            || lowered.contains("master plan delivers")
            || lowered.contains("dispatch index")
            || lowered.contains("master-plan index")
    })
}

fn verify_corpus_priority_plan(plan_path: &Path, markdown: &str) -> Result<()> {
    let has_code_slice = ["goal", "files", "test"].into_iter().all(|fragment| {
        markdown_section_contains(markdown, "## Implementation Slice", |line| {
            line.to_ascii_lowercase().contains(fragment)
        })
    });
    let has_decision_slice =
        markdown_section_contains(markdown, "## Implementation Slice", |line| {
            line_marks_test_expectation_none(line)
        }) && markdown_section_contains(markdown, "## Implementation Slice", |line| {
            let lowered = line.to_ascii_lowercase();
            ["decision", "checkpoint", "research", "validation", "gate"]
                .into_iter()
                .any(|fragment| lowered.contains(fragment))
        });
    let has_index_slice = impl_slice_is_dispatch_index(markdown);
    if !has_code_slice && !has_decision_slice && !has_index_slice {
        bail!(
            "corpus plan {} must describe an implementation-slice goal/files/tests, an explicit decision/checkpoint slice, or a master-plan dispatch index in `## Implementation Slice`",
            plan_path.display()
        );
    }
    Ok(())
}

fn verify_corpus_legacy_execplan(plan_path: &Path, markdown: &str) -> Result<()> {
    if !markdown_section_contains(markdown, "## Progress", |line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- [ ]") || trimmed.starts_with("- [x]")
    }) {
        bail!(
            "corpus plan {} must include at least one checkbox item in `## Progress`",
            plan_path.display()
        );
    }
    let has_standard_unit_shape = ["goal", "files", "test"].into_iter().all(|fragment| {
        markdown_section_contains(markdown, "## Implementation Units", |line| {
            line.to_ascii_lowercase().contains(fragment)
        })
    });
    let has_artifact_only_unit_shape =
        markdown_section_contains(markdown, "## Implementation Units", |line| {
            line_marks_test_expectation_none(line)
        }) && markdown_section_contains(markdown, "## Implementation Units", |line| {
            let lowered = line.to_ascii_lowercase();
            ["artifact", "index", "checkpoint", "report", "note", "file"]
                .into_iter()
                .any(|fragment| lowered.contains(fragment))
        }) && markdown_section_contains(markdown, "## Implementation Units", |line| {
            let lowered = line.to_ascii_lowercase();
            [
                "goal",
                "emit",
                "document",
                "capture",
                "produce",
                "record",
                "delegated to",
                "no direct implementation units",
            ]
            .into_iter()
            .any(|fragment| lowered.contains(fragment))
        });
    if !has_standard_unit_shape && !has_artifact_only_unit_shape {
        bail!(
            "corpus plan {} must describe implementation-unit goal/files/tests or an explicit artifact-only unit in `## Implementation Units`",
            plan_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        impl_slice_is_dispatch_index, line_marks_test_expectation_none,
        sanitize_corpus_numbered_plan_shapes, sanitize_corpus_repo_root_paths,
        verify_corpus_execplan, verify_corpus_outputs, verify_corpus_outputs_read_only,
    };
    use crate::generation::planning_root::ActivePlanSurface;
    use crate::generation::tests::{temp_dir, valid_corpus_report, write_valid_corpus};
    use std::fs;
    use std::time::Instant;

    #[test]
    fn test_expectation_none_accepts_ascii_em_dash_and_en_dash() {
        // The corpus authoring model emits em-dash (`—`) and en-dash (`–`)
        // typography interchangeably with ASCII `--`; verification must accept
        // all three so a decision-only plan does not crash the race over
        // typography alone.
        assert!(line_marks_test_expectation_none(
            "**Test expectation: none -- index-only file, no code behavior change.**"
        ));
        assert!(line_marks_test_expectation_none(
            "**Test expectation: none — research/decision artifact, no code behavior changes.**"
        ));
        assert!(line_marks_test_expectation_none(
            "**Test expectation: none – research/decision artifact, no code behavior changes.**"
        ));
        assert!(line_marks_test_expectation_none(
            "TEST EXPECTATION: NONE — case-insensitive matching."
        ));
        assert!(!line_marks_test_expectation_none(
            "Test expectation: add a focused regression test."
        ));
        assert!(!line_marks_test_expectation_none(
            "Test expectation: none, single hyphen here is not enough."
        ));
    }

    #[test]
    fn impl_slice_is_dispatch_index_recognizes_master_plan_shapes() {
        // The 001-master-plan.md emitted by the corpus authoring model is a
        // dispatch index, not a code-slice or decision-slice. It must verify
        // without forcing the model to fake a code-slice shape.
        let master = r#"
## Implementation Slice

The master plan delivers one artifact: this file. The slice plans below are the actual implementation work.

**Plan dispatch (numbered priority plans in this corpus):**

1. **002 — first slice.**
"#;
        assert!(impl_slice_is_dispatch_index(master));

        let code_slice = r#"
## Implementation Slice

**Goal:** ship the foo.
**Files to modify:** src/foo.rs.
**Tests:** add bar_test.
"#;
        assert!(!impl_slice_is_dispatch_index(code_slice));

        let decision_slice = r#"
## Implementation Slice

**Test expectation: none -- research/decision artifact, no code behavior changes.**
Decision G1, G2, G3 documented in artifact.
"#;
        assert!(!impl_slice_is_dispatch_index(decision_slice));
    }

    #[test]
    fn corpus_execplan_validator_accepts_full_plans_md_shape() {
        let root = temp_dir("corpus-execplan-ok");
        let plan_path = root.join("001-example.md");
        fs::write(
            &plan_path,
            r#"# Example Slice

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

After this change, an operator can run a concrete proof and observe the generated artifact.

## Requirements Trace

R1: The proof artifact is generated from the live repo state.

## Scope Boundaries

This plan does not change production runtime behavior.

## Progress

- [ ] (2026-04-10 00:00Z) Implement the proof artifact.

## Surprises & Discoveries

None yet.

## Decision Log

- Decision: Keep the first slice bounded to one artifact.
  Rationale: It gives a reviewer a concrete proof before runtime changes.
  Date/Author: 2026-04-10 / auto corpus

## Outcomes & Retrospective

None yet.

## Context and Orientation

The relevant files are `docs/example.md` and `crates/example/src/lib.rs`.

## Plan of Work

Update `docs/example.md`, then add a focused regression test in `crates/example/src/lib.rs`.

## Implementation Units

Unit 1: Proof artifact.
Goal: Create the proof artifact.
Requirements advanced: R1.
Dependencies: none.
Files to create or modify: `docs/example.md`, `crates/example/src/lib.rs`.
Tests to add or modify: add `example_proof_is_generated`.
Approach: write the artifact first, then cover it with the focused test.
Specific test scenarios: invoke the proof function and expect the artifact path to be returned.

## Concrete Steps

From the repository root, run:

    cargo test -p example example_proof_is_generated -- --nocapture

## Validation and Acceptance

The focused test passes and prints the generated artifact path.

## Idempotence and Recovery

Rerunning the test overwrites the same deterministic artifact.

## Artifacts and Notes

Add the final test transcript here after implementation.

## Interfaces and Dependencies

Use the existing `example::proof` module; no new external service is required.
"#,
        )
        .unwrap();

        verify_corpus_execplan(&plan_path).unwrap();
    }

    #[test]
    fn corpus_priority_plan_validator_accepts_compact_focus_shape() {
        let root = temp_dir("corpus-priority-plan-ok");
        let plan_path = root.join("001-core-loop.md");
        fs::write(
            &plan_path,
            r#"# Core Loop Proof

## Priority Decision

P0: prove the core operator loop before expanding docs or audits. Score: user pain 3, code leverage 3, risk retired 2, proof 2, subtraction 2.

## User / Operator Outcome

An operator can run the command, see the expected state, and know whether the loop works.

## Evidence

`src/main.rs`, `src/generation.rs`, and the focused test command show this is the current bottleneck.

## Scope Boundary

This does not write broad audit reports, new governance docs, or release artifacts.

## Implementation Slice

Goal: make the core loop produce a verifiable result.
Dependencies: none.
Files to create or modify: `src/generation.rs`, `tests/planning_primacy.rs`.
Tests to add or modify: add `core_loop_is_prioritized`.
Approach: change the prompt and prove the generated contract names the priority.

## Verification

    cargo test generation::tests::corpus_priority_plan_validator_accepts_compact_focus_shape

## Deferred

Large documentation refreshes remain follow-on until the loop proof exists.
"#,
        )
        .unwrap();

        verify_corpus_execplan(&plan_path).unwrap();
    }

    #[test]
    fn corpus_sanitizer_strips_numbered_plan_front_matter_before_verify() {
        let repo_root = temp_dir("corpus-frontmatter");
        let planning_root = repo_root.join("genesis");
        let plans_dir = planning_root.join("plans");
        fs::create_dir_all(&plans_dir).unwrap();
        let plan_path = plans_dir.join("001-example.md");
        fs::write(
            &plan_path,
            r#"---
id: GENESIS-001
title: Example Slice
status: active
---

# Example Slice

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan must be maintained in accordance with `PLANS.md` at the repository root.

## 1. Purpose and Big Picture

Create one proof artifact.

## 2. Requirements Trace

R1: The artifact exists.

## 3. Scope Boundaries

No runtime behavior changes.

## 4. Progress

- [ ] Start.

## 5. Surprises and Discoveries

None yet.

## 6. Decision Log

- Decision: Keep it small.
  Rationale: Easier to verify.
  Date/Author: 2026-04-22 / test

## 7. Outcomes and Retrospective

None yet.

## 8. Context and Orientation

Read `docs/example.md`.

## 9. Plan of Work

Update the example document.

## 10. Implementation Units

Unit 1.
Goal: Update the example.
Requirements advanced: R1.
Dependencies: none.
Files to create or modify: `docs/example.md`.
Tests to add or modify: add one focused test.
Approach: edit the file.
Specific test scenarios: run the focused test.

## 11. Concrete Steps

    cargo test -p example example_test

## 12. Validation and Acceptance

The focused test passes.

## 13. Idempotence and Recovery

Rerun the same command safely.

## 14. Artifacts and Notes

No notes.

## 15. Interfaces and Dependencies

No new dependencies.
"#,
        )
        .unwrap();

        sanitize_corpus_numbered_plan_shapes(&planning_root).unwrap();

        let sanitized = fs::read_to_string(&plan_path).unwrap();
        assert!(sanitized.starts_with("# Example Slice\n"));
        assert!(!sanitized.contains("id: GENESIS-001"));
        assert!(sanitized.contains("## Purpose / Big Picture\n"));
        assert!(sanitized.contains("## Surprises & Discoveries\n"));
        assert!(sanitized.contains("## Outcomes & Retrospective\n"));
        verify_corpus_execplan(&plan_path).unwrap();
    }

    #[test]
    fn corpus_execplan_validator_accepts_index_only_artifact_unit() {
        let root = temp_dir("corpus-execplan-index");
        let plan_path = root.join("001-master-plan.md");
        fs::write(
            &plan_path,
            r#"# 001 - Master Index

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Emit the index file that ties the subordinate plan set together.

## Requirements Trace

R1: The generated plan set stays navigable for a novice operator.

## Scope Boundaries

This plan only emits the index and does not change runtime code.

## Progress

- [ ] Emit the index file.

## Surprises & Discoveries

None yet.

## Decision Log

- Decision: Keep the master plan index-only.
  Rationale: Downstream work lives in the subordinate plans.
  Date/Author: 2026-04-18 / auto corpus

## Outcomes & Retrospective

None yet.

## Context and Orientation

Look at `genesis/PLANS.md` and the numbered files under `genesis/plans/`.

## Plan of Work

Write the index, then delegate implementation to the downstream plans.

## Implementation Units

This master plan has no direct implementation units; every work item is delegated to plans 002 through 010.
Artifact: `genesis/plans/001-master-plan.md`.
Approach: emit the index file and keep downstream ownership explicit.
Test expectation: none -- index-only file with no code behavior change.

## Concrete Steps

    ls genesis/plans/

## Validation and Acceptance

The numbered plan set is indexed and navigable.

## Idempotence and Recovery

Rewriting the index file is safe and deterministic.

## Artifacts and Notes

Capture downstream evidence in the subordinate plans.

## Interfaces and Dependencies

Depends on `genesis/PLANS.md` and the numbered subordinate plan files.
"#,
        )
        .unwrap();

        verify_corpus_execplan(&plan_path).unwrap();
    }

    #[test]
    fn corpus_execplan_validator_rejects_old_task_stub_shape() {
        let root = temp_dir("corpus-execplan-stub");
        let plan_path = root.join("004-autonomous-evidence-retention-dr.md");
        fs::write(
            &plan_path,
            r#"# 004 - Autonomous Evidence Retention And DR

## Objective

Add backup, retention, and disaster-recovery treatment.

## Description

This is too high level to guide a novice implementation.

## Acceptance Criteria

- Backup is documented.

## Verification

    cargo test -p bitino-house ops_event -- --nocapture

## Dependencies

- 002 local validation baseline.
"#,
        )
        .unwrap();

        let error = verify_corpus_execplan(&plan_path)
            .expect_err("expected old high-level plan shape to be rejected");

        assert!(error.to_string().contains("Purpose / Big Picture"));
    }

    #[test]
    fn corpus_output_validator_ignores_non_numbered_plan_markdown() {
        let repo_root = temp_dir("corpus-plan-readme");
        let planning_root = repo_root.join("genesis");
        let plans_dir = planning_root.join("plans");
        fs::create_dir_all(&plans_dir).unwrap();
        fs::write(planning_root.join("ASSESSMENT.md"), "# Assessment\n").unwrap();
        fs::write(planning_root.join("SPEC.md"), "# Spec\n").unwrap();
        fs::write(
            planning_root.join("PLANS.md"),
            "# Genesis Plan Index\n\nThis index points to generated numbered plans.\n",
        )
        .unwrap();
        fs::write(
            planning_root.join("GENESIS-REPORT.md"),
            valid_corpus_report(),
        )
        .unwrap();
        fs::write(
            plans_dir.join("README.md"),
            "# Genesis Plans Directory\n\nThis directory indexes numbered execution plans.\n",
        )
        .unwrap();
        fs::write(
            plans_dir.join("001-example.md"),
            r#"# Example Slice

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Do a thing.

## Requirements Trace

R1: Do a thing.

## Scope Boundaries

No runtime behavior changes.

## Progress

- [ ] Start.

## Surprises & Discoveries

None yet.

## Decision Log

- Decision: Keep it small.
  Rationale: Easier to verify.
  Date/Author: 2026-04-13 / test

## Outcomes & Retrospective

None yet.

## Context and Orientation

Look at `docs/example.md`.

## Plan of Work

Edit one file.

## Implementation Units

Unit 1.
Goal: Do the thing.
Requirements advanced: R1.
Dependencies: none.
Files to create or modify: `docs/example.md`.
Tests to add or modify: add one focused test.
Approach: change the file.
Specific test scenarios: test the thing.

## Concrete Steps

    cargo test

## Validation and Acceptance

The test passes.

## Idempotence and Recovery

Rerun safely.

## Artifacts and Notes

No notes.

## Interfaces and Dependencies

No external dependencies.
"#,
        )
        .unwrap();

        let summary = verify_corpus_outputs(
            &repo_root,
            &planning_root,
            false,
            &ActivePlanSurface::default(),
        )
        .expect("directory README should not be validated as an ExecPlan");

        assert_eq!(summary.plan_count, 1);
    }

    #[test]
    fn corpus_output_validator_requires_lever_and_demote_sections() {
        let repo_root = temp_dir("corpus-lever-sections");
        let planning_root = repo_root.join("genesis");
        write_valid_corpus(&planning_root);

        fs::write(
            planning_root.join("GENESIS-REPORT.md"),
            "# Report\n\n## Priority Focus\n\nRuntime first.\n",
        )
        .unwrap();
        let error = verify_corpus_outputs(
            &repo_root,
            &planning_root,
            false,
            &ActivePlanSurface::default(),
        )
        .expect_err("expected missing lever sections to fail");
        assert!(error.to_string().contains("Next Autodev Lever"));
        assert!(error.to_string().contains("Delete Or Demote"));

        fs::write(
            planning_root.join("GENESIS-REPORT.md"),
            "# Report\n\n## Priority Focus\n\nRuntime first.\n\n## Next Autodev Lever\n\nKeep thinking about priorities.\n\n## Delete Or Demote\n\nMove stale evidence-only work aside.\n",
        )
        .unwrap();
        let error = verify_corpus_outputs(
            &repo_root,
            &planning_root,
            false,
            &ActivePlanSurface::default(),
        )
        .expect_err("expected vague lever recommendation to fail");
        assert!(error
            .to_string()
            .contains("must recommend one immediate path"));

        fs::write(
            planning_root.join("GENESIS-REPORT.md"),
            valid_corpus_report(),
        )
        .unwrap();
        verify_corpus_outputs(
            &repo_root,
            &planning_root,
            false,
            &ActivePlanSurface::default(),
        )
        .expect("valid lever and demotion sections should pass");
    }

    #[test]
    fn corpus_output_validator_rejects_parallel_plan_universe_and_absolute_paths() {
        let repo_root = temp_dir("corpus-semantic-guard");
        fs::write(repo_root.join("PLANS.md"), "# root plans\n").unwrap();
        let root_plans_dir = repo_root.join("plans");
        fs::create_dir_all(&root_plans_dir).unwrap();
        fs::write(
            root_plans_dir.join("001-master-plan.md"),
            "# Active Root Plan\n",
        )
        .unwrap();

        let planning_root = repo_root.join("genesis");
        let plans_dir = planning_root.join("plans");
        fs::create_dir_all(&plans_dir).unwrap();
        fs::write(planning_root.join("ASSESSMENT.md"), "# Assessment\n").unwrap();
        fs::write(planning_root.join("SPEC.md"), "# Spec\n").unwrap();
        fs::write(
            planning_root.join("PLANS.md"),
            "# Genesis Plan Index\n\nThis index points to generated plans only.\n",
        )
        .unwrap();
        fs::write(
            planning_root.join("GENESIS-REPORT.md"),
            valid_corpus_report(),
        )
        .unwrap();
        fs::write(
            plans_dir.join("001-example.md"),
            r#"# Example Slice

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Do a thing.

## Requirements Trace

R1: Do a thing.

## Scope Boundaries

No runtime behavior changes.

## Progress

- [ ] Start.

## Surprises & Discoveries

None yet.

## Decision Log

- Decision: Keep it small.
  Rationale: Easier to verify.
  Date/Author: 2026-04-11 / test

## Outcomes & Retrospective

None yet.

## Context and Orientation

Look at `docs/example.md`.

## Plan of Work

Edit one file.

## Implementation Units

Unit 1.
Goal: Do the thing.
Requirements advanced: R1.
Dependencies: none.
Files to create or modify: `docs/example.md`.
Tests to add or modify: add one focused test.
Approach: change the file.
Specific test scenarios: test the thing.

## Concrete Steps

    cargo test

## Validation and Acceptance

The test passes.

## Idempotence and Recovery

Rerun safely.

## Artifacts and Notes

No notes.

## Interfaces and Dependencies

No external dependencies.
"#,
        )
        .unwrap();

        let error = verify_corpus_outputs(
            &repo_root,
            &planning_root,
            false,
            &ActivePlanSurface {
                root_plan_standard_path: Some("PLANS.md".to_string()),
                active_plan_paths: vec!["plans/001-master-plan.md".to_string()],
            },
        )
        .expect_err("expected active-plan semantic guard to fail");

        assert!(error.to_string().contains("active root planning surface"));

        fs::write(
            planning_root.join("PLANS.md"),
            "# Genesis Plan Index\n\nThis index is subordinate to `plans/001-master-plan.md` and not a parallel control plane.\n",
        )
        .unwrap();
        fs::write(
            planning_root.join("GENESIS-REPORT.md"),
            format!(
                "{}\nThe corpus is reconciled against `plans/001-master-plan.md`.\n\nBad link: {}\n",
                valid_corpus_report(),
                repo_root.display(),
            ),
        )
        .unwrap();

        let error = verify_corpus_outputs(
            &repo_root,
            &planning_root,
            false,
            &ActivePlanSurface {
                root_plan_standard_path: Some("PLANS.md".to_string()),
                active_plan_paths: vec!["plans/001-master-plan.md".to_string()],
            },
        )
        .expect_err("expected absolute path semantic guard to fail");

        assert!(error.to_string().contains("absolute repo-root path"));
    }

    #[test]
    fn corpus_repo_root_sanitizer_rewrites_absolute_repo_paths_before_verify() {
        let repo_root = temp_dir("corpus-sanitize");
        let planning_root = repo_root.join("genesis");
        let plans_dir = planning_root.join("plans");
        fs::create_dir_all(&plans_dir).unwrap();

        fs::write(planning_root.join("ASSESSMENT.md"), "# Assessment\n").unwrap();
        fs::write(planning_root.join("SPEC.md"), "# Spec\n").unwrap();
        fs::write(
            planning_root.join("PLANS.md"),
            "# Genesis Plan Index\n\nThis index is the active planning surface.\n",
        )
        .unwrap();
        fs::write(
            planning_root.join("GENESIS-REPORT.md"),
            format!(
                "{}\nWork from `{}` starts here.\n",
                valid_corpus_report(),
                repo_root.display(),
            ),
        )
        .unwrap();
        fs::write(
            plans_dir.join("001-example.md"),
            format!(
                r#"# Example Slice

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Do a thing from `{repo_root}`.

## Requirements Trace

R1: Do a thing.

## Scope Boundaries

No runtime behavior changes.

## Progress

- [ ] Start.

## Surprises & Discoveries

None yet.

## Decision Log

- Decision: Keep it small.
  Rationale: Easier to verify.
  Date/Author: 2026-04-11 / test

## Outcomes & Retrospective

None yet.

## Context and Orientation

Look at `<repo-root>/docs/example.md`.

## Plan of Work

Edit one file.

## Implementation Units

Unit 1.
Goal: Do the thing.
Requirements advanced: R1.
Dependencies: none.
Files to create or modify: `docs/example.md`.
Tests to add or modify: add one focused test.
Approach: change the file.
Specific test scenarios: test the thing.

## Concrete Steps

    cd {repo_root}
    cargo test

## Validation and Acceptance

The test passes.

## Idempotence and Recovery

Rerun safely.

## Artifacts and Notes

No notes.

## Interfaces and Dependencies

No external dependencies.
"#,
                repo_root = repo_root.display()
            ),
        )
        .unwrap();

        sanitize_corpus_repo_root_paths(&repo_root, &planning_root).unwrap();

        let plan = fs::read_to_string(plans_dir.join("001-example.md")).unwrap();
        assert!(plan.contains("cd \"$(git rev-parse --show-toplevel)\""));
        assert!(!plan.contains(&repo_root.display().to_string()));

        let report = fs::read_to_string(planning_root.join("GENESIS-REPORT.md")).unwrap();
        assert!(!report.contains(&repo_root.display().to_string()));
        assert!(report.contains("the repository root"));

        verify_corpus_outputs(
            &repo_root,
            &planning_root,
            false,
            &ActivePlanSurface::default(),
        )
        .expect("sanitized corpus should verify successfully");
    }

    #[test]
    fn corpus_verify_only_does_not_rewrite_corpus_files() {
        let repo_root = temp_dir("corpus-verify-read-only");
        let planning_root = repo_root.join("genesis");
        write_valid_corpus(&planning_root);
        let state_path = repo_root.join(".auto/state.json");
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        fs::write(&state_path, "{\"planning_root\":\"old\"}\n").unwrap();
        let plan_path = planning_root.join("plans/001-build.md");
        let before_plan = fs::read_to_string(&plan_path).unwrap();
        let before_state = fs::read_to_string(&state_path).unwrap();

        verify_corpus_outputs_read_only(
            &repo_root,
            &planning_root,
            false,
            &ActivePlanSurface {
                root_plan_standard_path: None,
                active_plan_paths: Vec::new(),
            },
            Instant::now(),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&plan_path).unwrap(), before_plan);
        assert_eq!(fs::read_to_string(&state_path).unwrap(), before_state);
    }
}
