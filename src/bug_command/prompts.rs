//! Prompt construction and JSON schema text for the `auto bug` phases.

use std::path::Path;

use crate::bug_command::types::RepoChunk;

pub(crate) fn build_finder_prompt(
    chunk: &RepoChunk,
    findings_json: &Path,
    findings_md: &Path,
) -> String {
    let risk_hints = if chunk.risk_notes.is_empty() {
        "No static risk hints were found for this chunk. Still perform a full audit.".to_string()
    } else {
        chunk
            .risk_notes
            .iter()
            .map(|note| format!("- {note}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        r#"You are the finder pass in a multi-pass bug pipeline.

Audit repo chunk `{chunk_id}` with primary scope `{scope}`.

Primary files in this chunk:
{files}

Static risk hints from the cheap pre-index:
{risk_hints}

Rules:
- Treat the live codebase as truth.
- You may inspect adjacent files outside this chunk only when they are required to validate an integration path.
- Do not modify code.
- Only write these files:
  - `{findings_json}`
  - `{findings_md}`
- `{findings_json}` must be a JSON array. If nothing survives your audit, write `[]`.

Scoring:
- Low impact bug: 1 point
- Medium impact bug: 5 points
- Critical impact bug: 10 points

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-{ordinal:03}-01",
  "title": "Short bug title",
  "location": "path:line or subsystem identifier",
  "impact": "low|medium|critical",
  "points": 1,
  "description": "Concrete failure mode",
  "why_plausible": "Why this is plausibly real in this repo",
  "falsification_checks": ["Specific repro or validation step"],
  "evidence": ["Code reference or observed invariant"]
}}

Requirements:
- Maximize recall, but every finding must name a concrete failure mode and at least one falsification check.
- Every finding must include direct code evidence and either a reproduction path, violated invariant, or exact validation command.
- Prefer findings with a believable reproduction path, violated invariant, and plausible root-cause region over vague smell reports.
- Cover correctness, state consistency, security, performance, and runtime behavior when the code supports them.
- Use bug IDs with prefix `BUG-{ordinal:03}-`.
- Match `points` to `impact` exactly.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
- Double-escape literal backslashes in regexes, paths, and code snippets (for example `\\d`, `C:\\tmp`, or `foo\\bar`).
- `{findings_md}` should summarize the same findings, grouped by impact, and end with a total score.
"#,
        chunk_id = chunk.id,
        scope = chunk.scope_label,
        files = render_prompt_files(&chunk.files),
        risk_hints = risk_hints,
        findings_json = findings_json.display(),
        findings_md = findings_md.display(),
        ordinal = chunk.ordinal,
    )
}

pub(crate) fn build_skeptic_prompt(
    chunk: &RepoChunk,
    findings_json: &Path,
    verdicts_json: &Path,
    verdicts_md: &Path,
) -> String {
    format!(
        r#"You are the skeptic pass in a multi-pass bug pipeline.

Review chunk `{chunk_id}` with primary scope `{scope}`.

Input findings file:
- `{findings_json}`

Rules:
- Treat the codebase as truth.
- Challenge every reported bug.
- Do not modify code.
- Only write these files:
  - `{verdicts_json}`
  - `{verdicts_md}`
- `{verdicts_json}` must be a JSON array with one verdict per input bug. If the input file is empty, write `[]`.

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-{ordinal:03}-01",
  "decision": "accepted|disproved",
  "confidence_percent": 0,
  "counter_argument": "Why it is not a bug, or why the claim still survives challenge",
  "risk_calculation": "Reasoning about the downside of dismissing it incorrectly",
  "follow_up_checks": ["Extra validation that would tighten confidence"]
}}

Requirements:
- Be aggressive about disproving weak claims.
- Challenge whether the claim identifies a real root-cause bug instead of a symptom, style issue, or speculative concern.
- Prefer discarding findings that cannot be grounded in a runnable falsification path or direct code evidence.
- Only `accepted` findings should survive to verification.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
- Double-escape literal backslashes in regexes, paths, and code snippets (for example `\\d`, `C:\\tmp`, or `foo\\bar`).
- `{verdicts_md}` should summarize disproved vs accepted findings and call out the hardest borderline decisions.
"#,
        chunk_id = chunk.id,
        scope = chunk.scope_label,
        findings_json = findings_json.display(),
        verdicts_json = verdicts_json.display(),
        verdicts_md = verdicts_md.display(),
        ordinal = chunk.ordinal,
    )
}

pub(crate) fn build_fix_prompt(
    verified_json: &Path,
    results_json: &Path,
    results_md: &Path,
    branch: &str,
) -> String {
    format!(
        r#"You are the implementation pass in a multi-pass bug pipeline.

Implement every verified bug in the repository-wide findings set.

Input verified findings file:
- `{verified_json}`

Rules:
- Modify code only as needed to address the verified findings plus the minimum adjacent integration surfaces.
- Reproduce each bug with a failing test, failing command, or other executable proof first when practical. If that is truly not practical, document the best direct evidence you used instead of pretending.
- Fix root causes, not cosmetic symptoms.
- Add or update regression coverage for every `fixed` result when the repo has a real test surface for that behavior.
- Run validation commands that honestly support your changes.
- Stay on the currently checked-out branch `{branch}`.
- Commit only truthful fix increments with a message like `repo-name: bug fixes`.
- Push to `origin/{branch}` after each successful commit.
- Do not create or switch branches.
- Do not stage or commit unrelated pre-existing changes already present in the worktree.
- Do not stage or commit generated workflow artifacts under `bug/`, `.auto/`, `nemesis/`, or `gen-*`.
- Only write these files:
  - `{results_json}`
  - `{results_md}`
- `{results_json}` must be a JSON array with one entry per verified bug. If there are no verified bugs, write `[]`.

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-001-01",
  "status": "fixed|deferred|not_reproduced",
  "summary": "What changed and why",
  "validation_commands": ["Command actually run"],
  "touched_files": ["path/to/file"],
  "residual_risks": ["Anything still not fully closed"]
}}

Requirements:
- Treat verified findings as the contract; do not widen scope into unrelated cleanup.
- For browser-facing or runtime-sensitive bugs, use runtime/browser verification when available.
- `{results_md}` should summarize proof-before-fix, root cause, fix, validation, and any deferred items.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
- Double-escape literal backslashes in regexes, paths, and code snippets (for example `\\d`, `C:\\tmp`, or `foo\\bar`).
"#,
        verified_json = verified_json.display(),
        results_json = results_json.display(),
        results_md = results_md.display(),
        branch = branch,
    )
}

pub(crate) fn build_final_review_prompt(
    verified_json: &Path,
    implementation_json: &Path,
    results_json: &Path,
    results_md: &Path,
    branch: &str,
) -> String {
    format!(
        r#"You are the final Codex review pass in a multi-pass bug pipeline.

Review the repository-wide verified findings and the implementation pass results.

Input files:
- `{verified_json}`
- `{implementation_json}`

Rules:
- Treat the live repo state as truth.
- Re-check every verified bug against the implementation results before you trust them.
- Make any final code, test, or validation changes needed to close real remaining gaps.
- Keep scope tight: finish or truthfully defer verified bugs; do not widen into unrelated cleanup.
- Stay on the currently checked-out branch `{branch}`.
- Commit only truthful review refinements with a message like `repo-name: bug review fixes`.
- Push to `origin/{branch}` after each successful commit.
- Do not create or switch branches.
- Do not stage or commit unrelated pre-existing changes already present in the worktree.
- Do not stage or commit generated workflow artifacts under `bug/`, `.auto/`, `nemesis/`, or `gen-*`.
- Only write these files:
  - `{results_json}`
  - `{results_md}`
- `{results_json}` must be a JSON array with one entry per verified bug. If there are no verified bugs, write `[]`.

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-001-01",
  "status": "confirmed|amended|deferred",
  "summary": "What the final review concluded and what changed",
  "validation_commands": ["Command actually run"],
  "touched_files": ["path/to/file"],
  "residual_risks": ["Anything still not fully closed"]
}}

Requirements:
- `confirmed` means the implementation pass already fixed the bug and your review required no further code changes.
- `amended` means you made additional code, test, or validation changes to finish the fix.
- `deferred` means the bug remains real but you could not close it safely in this run.
- `{results_md}` should summarize what the implementation pass got right, what you had to tighten, and any truthful remaining gaps.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
- Double-escape literal backslashes in regexes, paths, and code snippets (for example `\\d`, `C:\\tmp`, or `foo\\bar`).
"#,
        verified_json = verified_json.display(),
        implementation_json = implementation_json.display(),
        results_json = results_json.display(),
        results_md = results_md.display(),
        branch = branch,
    )
}

pub(crate) fn build_review_prompt(
    chunk: &RepoChunk,
    accepted_json: &Path,
    results_json: &Path,
    results_md: &Path,
) -> String {
    format!(
        r#"You are the verification review pass in a multi-pass bug pipeline.

Review the skeptic-approved bugs for chunk `{chunk_id}` with primary scope `{scope}`.

Input accepted findings file:
- `{accepted_json}`

Rules:
- Treat the codebase as truth.
- Verify that each accepted bug is strong enough to survive to the final implementation pass.
- Do not modify code.
- Only write these files:
  - `{results_json}`
  - `{results_md}`
- `{results_json}` must be a JSON array with one entry per accepted bug. If there are no accepted bugs, write `[]`.

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-{ordinal:03}-01",
  "verdict": "verified|discarded",
  "confidence": "high|medium|low",
  "notes": "Why this finding should or should not survive to implementation",
  "follow_up": ["Concrete follow-up validation or scoping note"]
}}

Requirements:
- `verified` means the finding should survive into the repository-wide implementation pass.
- `discarded` means the finding is too weak, duplicated, or insufficiently supported to implement.
- Prefer `verified` only when the bug is concrete enough to justify a reproduce-first/root-cause fix workflow.
- Call out missing regression coverage, missing runtime proof, or suspiciously broad scope in `follow_up`.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
- Double-escape literal backslashes in regexes, paths, and code snippets (for example `\\d`, `C:\\tmp`, or `foo\\bar`).
- `{results_md}` should summarize what survived to implementation and what was discarded.
"#,
        chunk_id = chunk.id,
        scope = chunk.scope_label,
        accepted_json = accepted_json.display(),
        results_json = results_json.display(),
        results_md = results_md.display(),
        ordinal = chunk.ordinal,
    )
}

fn render_prompt_files(files: &[String]) -> String {
    files
        .iter()
        .map(|file| format!("- `{file}`"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn build_bug_json_repair_prompt(
    target_path: &Path,
    raw_response_path: &Path,
    artifact_label: &str,
    schema_hint: &str,
) -> String {
    format!(
        r#"You are repairing a malformed JSON workflow artifact for auto bug.

Artifact type:
- `{artifact_label}`

Target artifact:
- `{target_path}`

Raw backend response log:
- `{raw_response_path}`

Rules:
- Do not modify code.
- Do not edit any workflow artifact other than `{target_path}`.
- Read the target artifact if it exists and the raw backend response log to recover the intended content.
- Rewrite `{target_path}` so it contains valid JSON only. No markdown fences. No commentary.
- Preserve every recoverable entry and field value. If wording is ambiguous, prefer the most literal faithful reconstruction instead of inventing new findings.
- Keep the artifact as a JSON array using exactly this schema:
{schema_hint}
- JSON strings must stay valid JSON. Escape embedded quotes when needed.
- Double-escape literal backslashes in regexes, paths, and code snippets.
"#,
        artifact_label = artifact_label,
        target_path = target_path.display(),
        raw_response_path = raw_response_path.display(),
    )
}

pub(crate) fn finder_json_schema() -> &'static str {
    r#"[
  {
    "bug_id": "BUG-001-01",
    "title": "Short finding title",
    "location": "path/to/file:line",
    "impact": "critical|high|medium|low",
    "points": 0,
    "description": "Concrete failure mode",
    "why_plausible": "Why the code suggests this is real",
    "falsification_checks": ["Runnable checks"],
    "evidence": ["Direct code evidence"]
  }
]"#
}

pub(crate) fn skeptic_verdict_json_schema() -> &'static str {
    r#"[
  {
    "bug_id": "BUG-001-01",
    "decision": "accepted|disproved",
    "confidence_percent": 0,
    "counter_argument": "Why it fails or survives challenge",
    "risk_calculation": "Downside of dismissing it incorrectly",
    "follow_up_checks": ["Extra validation that would tighten confidence"]
  }
]"#
}

pub(crate) fn fix_result_json_schema() -> &'static str {
    r#"[
  {
    "bug_id": "BUG-001-01",
    "status": "fixed|deferred|not_reproduced",
    "summary": "What changed and why",
    "validation_commands": ["Command actually run"],
    "touched_files": ["path/to/file"],
    "residual_risks": ["Anything still not fully closed"]
  }
]"#
}

pub(crate) fn review_result_json_schema() -> &'static str {
    r#"[
  {
    "bug_id": "BUG-001-01",
    "verdict": "verified|discarded",
    "confidence": "high|medium|low",
    "notes": "Why this should or should not be implemented",
    "follow_up": ["Missing proof, scope risk, or test gaps"]
  }
]"#
}

pub(crate) fn final_review_result_json_schema() -> &'static str {
    r#"[
  {
    "bug_id": "BUG-001-01",
    "status": "confirmed|amended|deferred",
    "summary": "What the final review concluded and what changed",
    "validation_commands": ["Command actually run"],
    "touched_files": ["path/to/file"],
    "residual_risks": ["Anything still not fully closed"]
  }
]"#
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{build_final_review_prompt, build_fix_prompt};

    #[test]
    fn fix_prompt_requires_commit_and_push_to_current_branch() {
        let prompt = build_fix_prompt(
            Path::new("bug/verified-findings.json"),
            Path::new("bug/implementation-results.json"),
            Path::new("bug/implementation-results.md"),
            "main",
        );

        assert!(prompt.contains("Commit only truthful fix increments"));
        assert!(prompt.contains("Push to `origin/main` after each successful commit."));
        assert!(prompt.contains(
            "Do not stage or commit unrelated pre-existing changes already present in the worktree."
        ));
        assert!(prompt.contains(
            "Do not stage or commit generated workflow artifacts under `bug/`, `.auto/`, `nemesis/`, or `gen-*`."
        ));
    }

    #[test]
    fn final_review_prompt_requires_review_of_fix_results() {
        let prompt = build_final_review_prompt(
            Path::new("bug/verified-findings.json"),
            Path::new("bug/implementation-results.json"),
            Path::new("bug/final-review-results.json"),
            Path::new("bug/final-review-results.md"),
            "main",
        );

        assert!(prompt.contains(
            "Review the repository-wide verified findings and the implementation pass results."
        ));
        assert!(prompt.contains("`confirmed` means the implementation pass already fixed the bug"));
        assert!(prompt.contains("Push to `origin/main` after each successful commit."));
    }
}
