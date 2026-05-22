//! Prompt construction for the `auto nemesis` phases.

use std::path::Path;

pub(crate) const DEFAULT_NEMESIS_PROMPT: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, and staging rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, and any security- or audit-related docs already present.
0c. You are running a Nemesis-style audit inspired by the upstream `nemesis-auditor` workflow. Emulate the method directly in this run:
    - Phase 0: Recon and target selection
    - Pass 1: Feynman-style deep logic audit
    - Pass 2: State inconsistency audit enriched by Pass 1 findings
    - Pass 3+: Alternate targeted Feynman and State re-passes until convergence or a maximum of 6 total passes
    - Only keep evidence-backed findings

1. Your task is to perform a deep hardening audit of the live repository and write the audit outputs only into `nemesis/`.
   - Treat the codebase as truth.
   - Use docs and existing plans as supporting context, not authority.
   - Focus on business-logic flaws, state-desync risks, broken invariants, ordering problems, missing guards, and dangerous assumptions.

2. Do not modify root `specs/` or root `IMPLEMENTATION_PLAN.md` directly.
   - Write exactly these files:
     - `nemesis/nemesis-audit.md`
     - `nemesis/IMPLEMENTATION_PLAN.md`

3. `nemesis/nemesis-audit.md` requirements:
   - Must start with `# Specification: Nemesis Audit Findings and Hardening Requirements`
   - Capture only verified findings or verified hardening requirements
   - For each major finding or requirement, include:
     - affected surfaces
     - triggering scenario or failure mode
     - invariant or assumption that breaks
     - why this matters now
     - discovery path (`Feynman`, `State`, or `Cross-feed`)

4. `nemesis/IMPLEMENTATION_PLAN.md` requirements:
   - Must start with `# IMPLEMENTATION_PLAN`
   - Use these top-level sections exactly:
     - `## Priority Work`
     - `## Follow-On Work`
     - `## Completed / Already Satisfied`
   - Each actionable task must use this exact header format:
     - `- [ ] `TASK-ID` Short title`
   - Each task must include these exact fields:
     - `Spec:`
     - `Why now:`
     - `Codebase evidence:`
     - `Owns:`
     - `Integration touchpoints:`
     - `Scope boundary:`
     - `Required tests:`
     - `Dependencies:`
     - `Completion signal:`
   - Only put unfinished work in `Priority Work` or `Follow-On Work`
   - Put already-satisfied audit items only in `Completed / Already Satisfied`
   - Use task ids prefixed with `NEM-`

5. The resulting plan must be execution-ready:
   - concrete
   - file-grounded
   - bounded
   - high signal
   - no vague “investigate further” tasks unless the uncertainty itself is the verified issue

99999. Important: this is not a generic security scan. Use the Nemesis back-and-forth method.
999999. Important: do not invent findings that you cannot support with repo evidence.
9999999. Important: write the two required files completely into `nemesis/` and stop."#;

const DEFAULT_NEMESIS_REVIEW_PROMPT: &str = r#"You are the final Nemesis synthesis pass.

Review the draft Nemesis audit outputs below, then re-check the live repository before you keep any item.

Draft inputs:
- `{draft_audit_path}`
- `{draft_plan_path}`

Rules:
- Treat the live codebase as truth.
- Treat the draft outputs as suspect until they survive your own review.
- Remove weak, duplicated, stale, or unsupported findings instead of carrying them forward.
- Tighten tasks so they are execution-ready and bounded.

{final_prompt}

Additional requirements:
- Only keep evidence-backed findings and tasks in the final outputs.
- Prefer fewer stronger findings over a longer noisy report.
- If a draft item is directionally right but over-scoped, narrow it before keeping it.
"#;

const DEFAULT_NEMESIS_IMPLEMENT_PROMPT: &str = r#"You are the final Nemesis implementation pass.

Input audit artifacts:
- `{audit_path}`
- `{plan_path}`

Rules:
- Treat the live codebase as truth and the final Nemesis plan as the execution contract.
- Implement the unchecked `NEM-` tasks in `## Priority Work` first, then `## Follow-On Work` when their dependencies are satisfied.
- Reproduce the issue, failing invariant, or strongest direct proof first when practical. If literal reproduction is not practical, document the best executable proof you used instead of pretending.
- Fix root causes, not cosmetic symptoms.
- Add or update regression coverage when the repo exposes a real test surface for the affected behavior.
- For runtime-sensitive or user-facing issues, use runtime/browser verification when available.
- Update `{plan_path}` as tasks are truly completed. Mark completed tasks as satisfied instead of leaving them open.
- Do not edit root `specs/` or root `IMPLEMENTATION_PLAN.md` directly in this pass.
- Stay on the currently checked-out branch `{branch}`.
- Commit only truthful fix increments with a message like `repo-name: nemesis fixes`.
- Push to `origin/{branch}` after each successful commit.
- Do not create or switch branches.
- Do not stage or commit unrelated pre-existing changes already present in the worktree.
- Do not stage or commit generated workflow artifacts under `.auto/`, `bug/`, or `gen-*`.
- Only write these files directly as workflow artifacts:
  - `{results_json}`
  - `{results_md}`

`{results_json}` must be a JSON array using exactly this schema:
{{
  "task_id": "NEM-001",
  "status": "fixed|deferred|blocked",
  "summary": "What changed and why",
  "validation_commands": ["Command actually run"],
  "touched_files": ["path/to/file"],
  "residual_risks": ["Anything still not fully closed"]
}}

Requirements:
- Cover every unchecked `NEM-` task in the plan with one result entry unless the final plan already marks it satisfied.
- `fixed` means the root cause was addressed and re-verified.
- If the live repo already satisfies a `fixed` task without edits, keep `touched_files` as `[]` and say plainly in `summary` that no file changes were needed because the requirement was already satisfied.
- `deferred` means the task remains valid but was intentionally left open with a truthful reason.
- `blocked` means an external dependency, ambiguity, or repo limitation prevented a truthful close.
- `{results_md}` should summarize proof-before-fix, root cause, changes made, validation, and any deferred or blocked tasks.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
- Double-escape literal backslashes in regexes, paths, and code snippets (for example `\\d`, `C:\\tmp`, or `foo\\bar`).
"#;

pub(crate) fn build_audit_prompt(
    prompt_template: &str,
    audit_path: &Path,
    plan_path: &Path,
) -> String {
    let prompt = render_prompt_outputs(prompt_template, audit_path, plan_path);
    format!(
        "You are the initial Nemesis audit pass.\n\n{prompt}\n\nAdditional requirements:\n- This pass should maximize useful recall while staying grounded in evidence.\n- Treat these outputs as draft artifacts that will be challenged by a second-stage review.\n"
    )
}

pub(crate) fn build_review_prompt(
    prompt_template: &str,
    draft_audit_path: &Path,
    draft_plan_path: &Path,
    final_audit_path: &Path,
    final_plan_path: &Path,
) -> String {
    let final_prompt = render_prompt_outputs(prompt_template, final_audit_path, final_plan_path);
    DEFAULT_NEMESIS_REVIEW_PROMPT
        .replace(
            "{draft_audit_path}",
            &draft_audit_path.display().to_string(),
        )
        .replace("{draft_plan_path}", &draft_plan_path.display().to_string())
        .replace("{final_prompt}", &final_prompt)
}

pub(crate) fn build_implementation_prompt(
    audit_path: &Path,
    plan_path: &Path,
    results_json: &Path,
    results_md: &Path,
    branch: &str,
) -> String {
    DEFAULT_NEMESIS_IMPLEMENT_PROMPT
        .replace("{audit_path}", &audit_path.display().to_string())
        .replace("{plan_path}", &plan_path.display().to_string())
        .replace("{results_json}", &results_json.display().to_string())
        .replace("{results_md}", &results_md.display().to_string())
        .replace("{branch}", branch)
}

/// Codex finalizer prompt. Reads the produced spec + plan + implementation
/// results and produces an independent review of the landed diff. Fails the
/// audit if the reviewer finds regressions, missing test coverage, or a fix
/// that claims `status: fixed` without touching the cited files.
pub(crate) fn build_finalizer_prompt(
    audit_path: &Path,
    plan_path: &Path,
    results_json: &Path,
    results_md: &Path,
    branch: &str,
) -> String {
    format!(
        r#"You are the Codex finalizer for an `auto nemesis` run.

The audit, synthesis, and implementation passes have just produced the landed
diff. Your job is to give that diff an independent code review and decide
whether the run is safe to ship as-is.

## Inputs

- Audit: `{audit}`
- Plan: `{plan}`
- Implementation results: `{results_json}`
- Implementation summary: `{results_md}`
- Branch: `{branch}`

## What to verify

1. For every task marked `status: fixed` in `{results_json}`:
   - Re-read each cited path in `touched_files` and confirm the code change
     actually addresses the root cause the audit + plan describe.
   - Run the listed `validation_commands` and record pass/fail.
   - Surface any regression, missing test coverage, or silent scope creep.
2. For every task marked `deferred` or `blocked`, verify the stated reason is
   truthful against the code.
3. Flag any fix that claims `touched_files: []` but the codebase still contains
   the documented failure mode.
4. Look for usual agent failure modes: over-wide refactors, speculative cleanup,
   silent suppression of warnings, hard-coded test fixtures.

## Deliverables

Write your review to a markdown file at `nemesis/final-review.md`. Structure:

```
# Final Review — auto nemesis

## Verdict
PASS | CONCERNS | FAIL

## Per-task verdicts
- TASK_ID: PASS | CONCERNS | FAIL — rationale in 2-3 lines

## Regressions observed
(if any)

## Validation commands rerun
(which ones you executed; outcomes)
```

If you find `FAIL`-severity issues, fix them in place with a minimal diff and
record them under `## Regressions observed`. Do not rewrite passing work.
Do not touch `nemesis/` artifacts other than `nemesis/final-review.md`.

Stay on branch `{branch}`. Commit any remediation with the message
`codex-finalizer: address nemesis regressions`.
"#,
        audit = audit_path.display(),
        plan = plan_path.display(),
        results_json = results_json.display(),
        results_md = results_md.display(),
        branch = branch,
    )
}

fn render_prompt_outputs(prompt_template: &str, audit_path: &Path, plan_path: &Path) -> String {
    prompt_template
        .replace(
            "nemesis/nemesis-audit.md",
            &audit_path.display().to_string(),
        )
        .replace(
            "nemesis/IMPLEMENTATION_PLAN.md",
            &plan_path.display().to_string(),
        )
}

pub(crate) fn build_nemesis_results_repair_prompt(
    audit_path: &Path,
    plan_path: &Path,
    results_json_path: &Path,
    results_md_path: &Path,
) -> String {
    format!(
        r#"You are repairing malformed implementation artifacts for auto nemesis.

Input context:
- Audit: `{audit_path}`
- Plan: `{plan_path}`

Artifacts to repair:
- `{results_json_path}`
- `{results_md_path}`

Rules:
- Do not modify code, tests, git state, or any workflow artifacts other than the two files above.
- Read the audit, the plan, and the current repository state to recover the truthful implementation summary.
- Rewrite `{results_json_path}` as valid JSON only. No markdown fences. No commentary.
- Rewrite `{results_md_path}` as a concise markdown summary of the same results.
- Preserve every recoverable task result. Do not invent work that did not happen.
- `{results_json_path}` must be a JSON array using exactly this schema:
[
  {{
    "task_id": "NEM-001",
    "status": "fixed|deferred|blocked",
    "summary": "What changed and why",
    "validation_commands": ["Command actually run"],
    "touched_files": ["path/to/file"],
    "residual_risks": ["Anything still not fully closed"]
  }}
]
- If a `fixed` task was already satisfied before this pass and required no edits, keep `touched_files` as `[]` and state explicitly in `summary` that no file changes were needed because the live repo already satisfied the requirement.
- JSON strings must stay valid JSON. Escape embedded quotes when needed.
- Double-escape literal backslashes in regexes, paths, and code snippets.
"#,
        audit_path = audit_path.display(),
        plan_path = plan_path.display(),
        results_json_path = results_json_path.display(),
        results_md_path = results_md_path.display(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{build_implementation_prompt, build_nemesis_results_repair_prompt};

    #[test]
    fn implementation_prompt_requires_commit_and_push_on_current_branch() {
        let prompt = build_implementation_prompt(
            Path::new("nemesis/nemesis-audit.md"),
            Path::new("nemesis/IMPLEMENTATION_PLAN.md"),
            Path::new("nemesis/implementation-results.json"),
            Path::new("nemesis/implementation-results.md"),
            "main",
        );

        assert!(prompt.contains("Commit only truthful fix increments"));
        assert!(prompt.contains("Push to `origin/main`"));
        assert!(
            prompt.contains("Do not edit root `specs/` or root `IMPLEMENTATION_PLAN.md` directly")
        );
    }

    #[test]
    fn nemesis_results_repair_prompt_is_file_scoped() {
        let prompt = build_nemesis_results_repair_prompt(
            Path::new("nemesis/nemesis-audit.md"),
            Path::new("nemesis/IMPLEMENTATION_PLAN.md"),
            Path::new("nemesis/implementation-results.json"),
            Path::new("nemesis/implementation-results.md"),
        );

        assert!(prompt.contains("Do not modify code, tests, git state"));
        assert!(prompt.contains("implementation-results.json"));
        assert!(prompt.contains("implementation-results.md"));
        assert!(prompt.contains("\"task_id\": \"NEM-001\""));
    }
}
