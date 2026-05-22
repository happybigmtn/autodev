//! Prompt builders, skill-policy text, and the per-surface gstack skill
//! classifiers for the audit pipeline.

use std::path::Path;

use crate::audit_everything::manifest::{EverythingManifest, FileState, FileQualityRatingState, GroupState};
use crate::audit_everything::run_paths::{
    final_review_markdown_path, file_quality_file_path, RunPaths,
};

pub(crate) const GSTACK_SKILL_POLICY: &str = r#"# GStack Skill Policy

This audit uses gstack skills as deterministic compact lenses unless the phase explicitly asks for live validation. Workers should not bulk-load full skill files by default.

## Always-On Audit Lenses

- review: pre-landing structural review, diff risk, SQL/data safety, LLM trust boundaries, conditional side effects, documentation staleness.
- health: project-native typecheck, lint, tests, dead-code, shell lint, and quality score evidence.
- investigate: root-cause discipline; no fixes or recommendations without evidence and a falsifiable theory.
- cso: secrets archaeology, dependency and CI/CD supply chain, auth/session boundaries, OWASP/STRIDE, LLM/AI security, production safety.
- careful: destructive-command caution, especially for deletion, force pushes, migrations, production, and shared environments.

## Planning And Context Lenses

- autoplan: complete plan gauntlet, represented here by CEO, engineering, design, and developer-experience review lenses.
- plan-ceo-review: product scope, ambition, simplification, and whether the proposed best version is worth building.
- plan-eng-review: architecture, data flow, invariants, edge cases, test plan, rollout risk, and maintainability.
- plan-design-review: UI/UX plan quality, hierarchy, interaction model, accessibility, visual system consistency.
- plan-devex-review: APIs, CLIs, SDKs, docs, onboarding, error messages, and time-to-hello-world.
- design-consultation: creation or repair of DESIGN.md/design-system docs when UI surfaces lack a coherent source of truth.

## Implementation And Remediation Lenses

- qa: test-fix-verify loop for web or interactive surfaces when remediation is allowed.
- qa-only: report-only web/app QA when source edits are disallowed.
- design-review: live visual QA and design polish for implemented UI surfaces.
- benchmark: browser-backed performance, Core Web Vitals, load time, resource and bundle regressions.
- devex-review: live developer-experience audit of docs, CLI help, onboarding, and error messages.
- document-release: post-change documentation synchronization across README, ARCHITECTURE, AGENTS, changelog, and TODOs.
- ship: pre-merge readiness, base-branch sync, validation gate, version/changelog/PR hygiene.
- land-and-deploy: merge/deploy/canary posture; use as a final-review lens, not as an automatic action inside audit workers.
- canary: post-deploy monitoring and visual/console/performance anomaly checks when deployment exists.

## State And Boundary Lenses

- checkpoint, context-save, context-restore: resumability and handoff quality.
- freeze, guard, unfreeze: write-scope control. For this audit, prefer the host runner's group boundaries over ad hoc widening.
- learn, retro: mine previous decisions or trends only when local project artifacts make them relevant.

## Browser And Artifact Tools

- browse/gstack: direct browser inspection for web/app QA, screenshots, responsive checks, forms, dialogs, and state assertions.
- connect-chrome/open-gstack-browser/setup-browser-cookies/pair-agent: direct browser setup only when authenticated or visible-browser QA is explicitly required.
- make-pdf: optional final packaging for markdown reports; never required for merge readiness.

## Usually Excluded From Audit Workers

- benchmark-models, plan-tune, gstack-upgrade, design-shotgun, design-html, office-hours: meta/tooling/ideation skills. Mention only if the file itself implements those workflows or the user explicitly requested that surface.
"#;

pub(crate) const CODEBASE_IMPROVEMENT_POLICY: &str = r#"# Codebase Improvement Policy

This audit is allowed to improve architecture, delete proved-dead code, and remove accumulated agent-written filler. It should not degrade product substance in exchange for a cleaner-looking diff.

## Default Posture

- Prefer deletion, consolidation, and simplification when repository evidence proves code is orphaned, deprecated, duplicated, transitional, or hollow.
- Improve module boundaries and dependency direction when a group report shows shallow modules, misplaced ownership, leaked invariants, or domain vocabulary drift.
- Treat "AI slop" as real debt: vague comments, generic wrappers, fake extensibility, repeated boilerplate, overexplained docs, and abstractions that do not protect an invariant or simplify a caller.
- Preserve behavior unless the synthesized report explicitly recommends changing it and the final review can explain why.

## Required Proof Before Removal

A remediation lane may delete or retire code only when it records proof in the lane report:

- reachability evidence from references, imports, exports, entrypoints, commands, config, docs, generated bindings, and tests
- public API, CLI, operator, or runtime behavior either preserved or intentionally updated
- narrow validation or characterization evidence for the affected surface
- confirmation that durable audit evidence, production/mainnet proof, generated runtime state, and operator artifacts were not removed merely because they looked unused

## Debt Register Classes

Use these classes in group reports and remediation notes:

- `safe_delete`: no live references or external contract remain
- `deprecated_remove`: an obsolete path can be removed with compatibility/docs updated
- `consolidate`: duplicated responsibilities should merge behind one owner
- `simplify`: code remains, but ceremony, wrappers, comments, or branches should shrink
- `deepen_module`: boundaries, names, invariants, or dependency direction need architectural repair
- `leave_with_reason`: suspicious code should stay because evidence shows it still carries substance

## Refactor Discipline

- Characterize behavior before risky refactors or deletions.
- Prefer vertical, reviewable changes over broad cosmetic churn.
- Update `ARCHITECTURE.md`, focused docs, or ADR-like notes when ownership, vocabulary, dependency direction, or durable invariants change.
- If proof is incomplete, leave the code in place and document the missing evidence instead of guessing.
"#;

pub(crate) fn build_context_prompt(worktree_root: &Path, report_root: &Path) -> String {
    format!(
        r#"You are preparing the context layer for `auto audit --everything`.

Repository root: `{worktree_root}`
Report root: `{report_root}`
GStack skill policy: `{report_root}/GSTACK-SKILL-POLICY.md`
Codebase improvement policy: `{report_root}/CODEBASE-IMPROVEMENT-POLICY.md`

Edit only repository-local context documents and the report root:
- Create or revise root `AGENTS.md`.
- Create or revise root `ARCHITECTURE.md`.
- Write `{report_root}/CONTEXT.md` summarizing what changed and what remains inferred.

Context engineering requirements:
- Follow the OpenAI harness-engineering posture: `AGENTS.md` is a short map, not a giant manual.
- Keep `AGENTS.md` concise and operational. Point to deeper docs instead of copying them.
- Follow Matklad's `ARCHITECTURE.md` guidance: describe the problem, codemap, module boundaries, invariants, and cross-cutting concerns. Keep details stable and avoid stale links.
- If `doctrine/` exists and contains files, reference it explicitly as doctrine injected into every audit loop. If it does not exist or is empty, ignore it.
- Reference the gstack skill policy as a compact routing artifact for future audit workers. Do not paste the full policy into `AGENTS.md`; point to it and keep `AGENTS.md` short.
- Reference the codebase improvement policy as the audit's default-on debt, deletion, AI-slop, and architecture-deepening contract. Do not paste the whole policy into root docs.
- Treat gstack skills as deterministic lenses by phase. Direct tool-like invocation is reserved for remediation/final validation when the selected surface calls for browser, QA, benchmark, deploy, or documentation checks.
- Add architecture context that helps later workers find domain boundaries, deprecated surfaces, orphan-risk areas, and evidence sources for safe deletion.
- Favor evidence-backed statements. Mark inferred architecture as inferred instead of pretending certainty.
- These first target repos are Bitino and Autonomy, so make the docs useful for Rust workspace/crate-heavy systems, runtime operators, and agent workers.

Do not edit source code in this phase. Do not run formatters across the repo.
"#,
        worktree_root = worktree_root.display(),
        report_root = report_root.display(),
    )
}

pub(crate) fn build_file_prompt(file: &FileState, context: &str, file_body: &str) -> String {
    let skill_policy = selected_skill_policy_for_file(&file.path);
    format!(
        r#"You are running first-pass professional audit analysis for exactly one tracked file.

Hard boundaries:
- Analyze only the file named below.
- Do not edit repository source files.
- Do not read neighboring source files in this first pass.
- The only architectural context you may use is the injected context below.
- Write outputs only in the artifact directory.
- Apply only the selected gstack lenses below for this file's surface. Do not invoke tools in this first pass. Do not discuss unrelated lenses.
- If the target file content below says it is omitted because the file is large, you must read the entire target file from its path in ordered chunks before writing artifacts. Do not sample. Do not rely on metadata only. If you cannot inspect every line, fail this file instead of writing artifacts.

Injected context:
{context}

Selected gstack lenses:
{skill_policy}

Default-on codebase improvement policy:
- Look for orphaned, deprecated, duplicated, transitional, overabstracted, or agent-generated filler in this file.
- Apply the deletion test even in first pass: what references, exports, config, docs, generated bindings, tests, or runtime entrypoints would prove this file or part of it is still live?
- Prefer architectural depth over micro-edits: identify whether this file owns a real invariant, belongs in this module, leaks responsibilities, or should consolidate with another owner.
- Do not recommend deletion unless you can name the proof still needed or already visible from this file plus injected context.

File under audit:
- Path: `{path}`
- Group: `{group}`
- Content hash: `{hash}`
- Artifact directory: `{artifact_dir}`

Write these files:
1. `{artifact_dir}/analysis.md`
2. `{artifact_dir}/analysis.json`

`analysis.md` must include:
- `# {path}`
- What this file does.
- Important public types/functions/modules/configuration it owns.
- How it appears to fit the architecture.
- Whether it is the best version of itself it could be.
- Orphan/deprecation signals, AI-slop signals, and simplification/deletion candidates with the evidence needed before removal.
- Architecture/debt assessment: ownership, module depth, domain vocabulary, dependency direction, duplicated responsibilities, and whether this file appears to carry real substance.
- A coverage note stating whether the full file content was provided inline or reviewed from disk in chunks.
- If not 10/10, list expansions, deletions, revisions, clarifications, tests, code refactors, documentation moves, or retirement steps that would make it an idiomatic 10/10 work product.
- Cross-file questions or likely relationships surfaced by this file, without resolving them from other source files in this pass.

`analysis.json` must be valid JSON with:
`path`, `group`, `score_out_of_10`, `summary`, `best_version_assessment`, `orphaned_or_deprecated_signals`, `ai_slop_signals`, `deletion_candidates`, `architecture_smells`, `behavior_preservation_needs`, `recommended_actions`, `cross_file_questions`, `coverage`, `confidence`.

Target file content:
```text
{file_body}
```
"#,
        context = context,
        skill_policy = skill_policy,
        path = file.path,
        group = file.group,
        hash = file.content_hash,
        artifact_dir = file.artifact_dir,
        file_body = file_body,
    )
}

pub(crate) fn build_synthesis_prompt(paths: &RunPaths, group: &GroupState) -> String {
    let skill_policy = selected_skill_policy_for_group(group);
    format!(
        r#"You are the second-pass cross-file synthesis reviewer for one professional audit group.

Repository root: `{repo}`
Group: `{group}`
Report: `{report}`

Read the group report and the per-file first-pass analyses it references. You may now reason across files in this group and across the concise context docs (`AGENTS.md`, `ARCHITECTURE.md`, and `doctrine/` if present).

The authoritative input set is the report plus the exact first-pass artifact paths referenced inside it. Do not glob or enumerate `{report_root}/files`; unreferenced artifact directories may be stale leftovers from interrupted or upgraded runs.

Selected gstack lenses for this group:
{skill_policy}

Default-on codebase improvement policy:
- Build or update a debt register for this group. Use the classes `safe_delete`, `deprecated_remove`, `consolidate`, `simplify`, `deepen_module`, and `leave_with_reason`.
- Treat orphaned/deprecated code and AI-slop as first-class audit findings, not optional polish.
- Prefer cross-file architecture fixes over isolated micro-edits when duplicated responsibility, shallow modules, or weak domain boundaries are the real problem.
- Require proof before deletion: references/imports/exports, entrypoints, config, docs, generated bindings, tests, runtime paths, and behavior characterization where needed.
- If proof is missing, record exactly what evidence would be needed instead of guessing.

Revise `{report}` in place. Keep every file represented. Tighten or correct the first-pass assessments based on relationships surfaced between files:
- duplicated responsibilities
- unclear ownership or misplaced modules
- missing invariants
- dead code or files that should retire
- deprecated paths, transitional scaffolding, and orphaned exports
- AI-slop: generic wrappers, hollow abstractions, vague comments, repeated boilerplate, or docs that add words without operational value
- test gaps
- docs that should move into `AGENTS.md`, `ARCHITECTURE.md`, doctrine, or inline comments
- cross-crate/API seams

`{report}` must include a `## Debt Register` section. For each candidate, include path(s), class, recommended action, deletion/refactor proof found, proof still missing, behavior-preservation needs, and risk.

Use the selected lenses as a compact prompt injection, not as permission to bulk-load unrelated skill files. Keep the output grounded in repository evidence.

Do not edit source code in this phase. Only edit `{report}` and optional notes next to it.
"#,
        repo = paths.worktree_root.display(),
        group = group.name,
        report = group.report_path,
        report_root = paths.report_root.display(),
        skill_policy = skill_policy,
    )
}

pub(crate) fn build_final_review_synthesis_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    shard_root: Option<&Path>,
) -> String {
    let skill_policy = selected_skill_policy_for_final_review();
    let shard_instruction = shard_root
        .map(|path| {
            format!(
                "Parallel reviewer shard reports are available under `{}`. Read every `shard.md` there and synthesize them with your own full-diff review. Treat shards as advisory evidence, not as a substitute for final judgment.",
                path.display()
            )
        })
        .unwrap_or_else(|| {
            "No parallel reviewer shard reports were generated for this run.".to_string()
        });
    format!(
        r#"You are the final professional audit reviewer.

Repository root: `{repo}`
Report root: `{report_root}`
Base commit: `{base}`
Audit branch: `{branch}`
Reviewer shards: {shard_instruction}

Review all group reports under the report root and the full git diff from `{base}` to HEAD.

Selected gstack lenses for final review:
{skill_policy}

Use `gpt-5.5 xhigh` judgment standards:
- Verify changes correspond to report findings.
- Reject speculative rewrites not grounded in file reports.
- Check for broken architecture docs, stale AGENTS instructions, overbroad edits, missing tests, and merge-risk.
- Verify debt-removal and architecture-deepening work: deletion candidates are evidence-backed, deprecated paths are intentionally retired, AI-slop was removed where safe, and refactors preserve product substance.
- Run or inspect the narrowest feasible validation for the changed surfaces.

Write `{report_root}/FINAL-REVIEW.md` with:
- `# FINAL REVIEW`
- A line exactly `Verdict: GO` or `Verdict: NO-GO`
- Diff summary
- Report consistency assessment
- Validation run and result
- Evidence class checklist
- Deletion and refactor proof checklist
- Required blockers before merge
- Optional follow-ups

The evidence class checklist must classify each evidence class as `pass`, `not run`, `blocked`, or `not applicable`; cite the exact artifact, command, or report path for each non-`not applicable` row; and state what claims the evidence does and does not support. Include at least these classes:
- local static/build/unit validation
- generated contract/binding validation
- browser QA or visual/product workflow validation
- deployment/canary/health validation
- live production or mainnet/on-chain validation
- external-owner or cross-repo validation
- documentation/status artifact integrity

The deletion and refactor proof checklist must classify each debt-removal claim as `pass`, `blocked`, or `not applicable`; cite the group report, debt-register item, diff path, and validation/characterization proof. Reject the audit with `Verdict: NO-GO` if code was deleted or behavior was refactored without enough evidence to show no substance was lost.

Do not count local, fixture, regtest, or synthetic proof as live production proof. Do not merge bulky first-pass mirrors such as `audit/everything/<run-id>/files/**` unless the host explicitly requested them; they should remain transient evidence caches by default.

Also write a chaptered codebase book under `{report_root}/CODEBASE-BOOK/`. This is the final explanatory artifact for a human who wants to understand the audited codebase without rereading every source file. It must not be a single giant markdown file. Write it in a Feynman-style teaching voice: clear first principles, concrete examples, patient logical order, and plain-spoken explanations. Avoid hype and vague praise.

The book standard is intentionally higher than an executive audit summary. A smart junior developer who is otherwise unfamiliar with this repository should be able to read the book and gain a deep technical understanding of the important crates/files, runtime flows, state boundaries, validation posture, and production risks before opening the source code.

`CODEBASE-BOOK/` must include:
- `README.md` with `# CODEBASE BOOK`, the table of contents, the recommended reading path, and links to all chapter files.
- Numbered chapter files organized by the repository's logical architecture and conceptual flow, not by incidental filesystem order. Example shape: `01-problem-and-mental-model.md`, `02-runtime-or-control-flow.md`, `03-data-model-and-storage.md`, followed by subsystem chapters that match this repo.
- File-catalog chapters split by subsystem/group so the catalog is readable as a book appendix, not as one enormous list.
- A documentation and architecture chapter that says what changed in `ARCHITECTURE.md`, `AGENTS.md`, and focused docs, or explicitly says no changes were made.
- A validation and residual-risk chapter.
- Pointers back to the group reports and first-pass artifacts for readers who want evidence.

The book must cover every tracked file included in this audit. Every file needs a reasonably detailed explanation of what it does, why it exists, what owns it or calls it, and how it fits into the surrounding subsystem. Do not use empty boilerplate like "utility file" without explanation. For files changed by this audit, include `changed:` in that file's entry and summarize the substance of the change.

For key files and key sections, include narrative code walkthroughs: name the important modules, functions, types, tests, configuration, and command paths; explain why each matters and how control or data moves through it. If the first draft becomes too high-level, prefer fewer but deeper narrative chapters plus appendix links over shallow coverage everywhere. The standalone `auto book` command can later rewrite these narrative chapters from the completed audit corpus using Codex's maximum context window while preserving appendix/catalog files.

Do not merge. The host runner handles merge only after this file says `Verdict: GO`.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        base = manifest.base_commit,
        branch = manifest.audit_branch,
        shard_instruction = shard_instruction,
        skill_policy = skill_policy,
    )
}

pub(crate) fn build_final_review_shard_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    shard_index: usize,
    groups: &[GroupState],
    artifact_dir: &Path,
) -> String {
    let skill_policy = selected_skill_policy_for_final_review();
    let group_list = groups
        .iter()
        .map(|group| {
            format!(
                "- `{}` report `{}` ({} file(s))",
                group.name,
                group.report_path,
                group.files.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"You are a parallel final-review shard for `auto audit --everything`.

Repository root: `{repo}`
Report root: `{report_root}`
Base commit: `{base}`
Audit branch: `{branch}`
Shard index: {shard_index}
Output file: `{artifact_dir}/shard.md`

Assigned group reports:
{group_list}

Selected gstack lenses for final review:
{skill_policy}

Review the assigned group reports, their debt registers, first-pass evidence when needed, and the git diff from `{base}` to HEAD for paths owned by these groups. You are not the final synthesizer; do not write `FINAL-REVIEW.md` and do not write `CODEBASE-BOOK/`.

Write `{artifact_dir}/shard.md` with:
- `# FINAL REVIEW SHARD {shard_index}`
- Assigned groups covered.
- GO/NO-GO recommendation for this shard only.
- Highest-risk blockers with exact report paths and diff paths.
- Evidence checklist gaps relevant to these groups.
- Deletion/refactor proof gaps relevant to these groups.
- Validation you inspected or ran, if any.
- Optional follow-ups that should not block.

Be concise but evidence-backed. Do not merge. Do not edit source code.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        base = manifest.base_commit,
        branch = manifest.audit_branch,
        shard_index = shard_index,
        artifact_dir = artifact_dir.display(),
        group_list = group_list,
        skill_policy = skill_policy,
    )
}

pub(crate) fn build_final_review_repair_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    attempt: usize,
    archived_review_path: &Path,
) -> String {
    format!(
        r#"You are repairing actionable blockers from a professional `auto audit --everything` final review.

Repository root: `{repo}`
Report root: `{report_root}`
Base commit: `{base}`
Audit branch: `{branch}`
NO-GO review to repair: `{review}`
Repair attempt: {attempt}

Read the archived NO-GO review and identify only concrete, actionable blockers under `Required blockers before merge`. Apply the smallest grounded repair that clears those blockers.

Rules:
- Do not broaden the audit or invent new remediation scope.
- Do not hide or delete evidence. If a blocker is invalid, document why in the active group report or `FINAL-REVIEW-REPAIR-{attempt}.md`.
- Update source, tests, docs, group reports, `REMEDIATION-PLAN.md`, and `CODEBASE-BOOK/` only when the blocker requires it.
- Run or inspect the narrowest meaningful verification for the repaired surface.
- Write `{report_root}/FINAL-REVIEW-REPAIR-{attempt}.md` with blockers addressed, changed files, validation, and any blockers that remain.
- Do not write `Verdict: GO`; the host will rerun final review after this repair pass.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        base = manifest.base_commit,
        branch = manifest.audit_branch,
        review = archived_review_path.display(),
        attempt = attempt,
    )
}

pub(crate) fn build_file_quality_rerate_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    file: &FileState,
    pass_index: usize,
) -> String {
    let first_pass = Path::new(&file.artifact_dir).join("analysis.json");
    let first_pass_markdown = Path::new(&file.artifact_dir).join("analysis.md");
    let group_report = manifest
        .groups
        .iter()
        .find(|group| group.name == file.group)
        .map(|group| group.report_path.as_str())
        .unwrap_or("");
    let artifact_dir = file_quality_file_path(paths, pass_index, file);
    format!(
        r#"You are the file-quality rerating reviewer for `auto audit --everything`.

Repository root: `{repo}`
Report root: `{report_root}`
Pass: {pass_index}
File under rerating: `{path}`
First-pass JSON: `{first_pass}`
First-pass markdown: `{first_pass_markdown}`
Group report: `{group_report}`
Final review: `{final_review}`
Output directory: `{artifact_dir}`

Regrade the first-pass rating for exactly this file against the current repository state. Read the file itself, the first-pass artifacts, the group report, and the final review. Do not edit source code in this rerating step.

Apply strict professional standards. The target is {target:.0}/10. A score below {accept:.0}/10 means this file still needs another deliverable pass before merge. Regrade independently; do not rubber-stamp the original first-pass score.

Penalize the file if it still contains unnecessary code, orphaned/deprecated surfaces, duplicated responsibility, AI-slop, shallow ownership, vague comments, fake extensibility, or missed consolidation opportunities. A file should not reach {accept:.0}/10 if it obviously needs deletion, retirement, or architectural relocation and the audit failed to handle or document that.

Write:
1. `{artifact_dir}/rating.md`
2. `{artifact_dir}/rating.json`

`rating.md` must include:
- Current score out of 10.
- Whether the first-pass score was too high, too low, or accurate.
- Concrete deliverables needed to make the file a {target:.0}/10 work product.
- Any remaining deletion, consolidation, simplification, AI-slop, or architecture-deepening deliverables.
- What would be acceptable evidence that the file is at least {accept:.0}/10.

`rating.json` must be valid JSON with:
`path`, `pass_index`, `score_out_of_10`, `previous_score_out_of_10`, `first_pass_grade_was`, `debt_or_architecture_findings`, `deliverables_to_reach_10`, `minimum_evidence_for_9`, `confidence`.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        pass_index = pass_index,
        path = file.path,
        first_pass = first_pass.display(),
        first_pass_markdown = first_pass_markdown.display(),
        group_report = group_report,
        final_review = final_review_markdown_path(paths).display(),
        artifact_dir = artifact_dir.display(),
        target = crate::audit_everything::file_quality::FILE_QUALITY_TARGET_SCORE,
        accept = crate::audit_everything::file_quality::FILE_QUALITY_ACCEPT_SCORE,
    )
}

pub(crate) fn build_file_quality_deliverables_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    file: &FileState,
    rating: &FileQualityRatingState,
    pass_index: usize,
) -> String {
    let group_report = manifest
        .groups
        .iter()
        .find(|group| group.name == file.group)
        .map(|group| group.report_path.as_str())
        .unwrap_or("");
    let artifact_dir = std::path::PathBuf::from(&rating.artifact_dir);
    format!(
        r#"You are running a per-file quality deliverable pass for `auto audit --everything`.

Repository root: `{repo}`
Report root: `{report_root}`
Pass: {pass_index}
Owned file: `{path}`
Current rerating score: {score}
Rating artifact: `{rating_json}`
Rating notes: `{rating_md}`
Group report: `{group_report}`
Final review: `{final_review}`

Raise this file toward a {target:.0}/10 work product. The immediate acceptance floor is {accept:.0}/10 on the next rerating pass, but the deliverables should aim at {target:.0}/10.

Rules:
- Keep the primary edit scope to `{path}`.
- You may update the nearest tests or focused docs only when that is necessary to prove or explain this file's change; list those as scope exceptions in your output.
- You may delete, simplify, consolidate, or relocate code when the rating artifact and group report provide evidence that this is the right way to raise quality.
- Before deleting or retiring code, prove the removal with references/imports/exports, entrypoints, config/docs/generated bindings, and narrow validation or behavior characterization where practical.
- Do not broaden into unrelated cleanup.
- Preserve evidence. Do not delete first-pass, group, final-review, or previous file-quality artifacts.
- Run or inspect the narrowest meaningful validation for this file when practical.
- Commit nothing manually; the host runner owns commits.

Write `{artifact_dir}/deliverables.md` with:
- Deliverables applied.
- Changed files.
- Deletion/refactor proof, or `not applicable`.
- Validation command/result or the reason validation was not practical.
- Remaining work, if any, to reach {target:.0}/10.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        pass_index = pass_index,
        path = file.path,
        score = rating
            .score_out_of_10
            .map(|score| format!("{score:.1}/10"))
            .unwrap_or_else(|| "unknown".to_string()),
        rating_json = artifact_dir.join("rating.json").display(),
        rating_md = artifact_dir.join("rating.md").display(),
        group_report = group_report,
        final_review = final_review_markdown_path(paths).display(),
        target = crate::audit_everything::file_quality::FILE_QUALITY_TARGET_SCORE,
        accept = crate::audit_everything::file_quality::FILE_QUALITY_ACCEPT_SCORE,
        artifact_dir = artifact_dir.display(),
    )
}

pub(crate) fn selected_skill_policy_for_file(path: &str) -> String {
    render_skill_policy(&selected_skill_names_for_file(path))
}

fn selected_skill_policy_for_group(group: &GroupState) -> String {
    let mut selected = Vec::new();
    push_unique(&mut selected, "review");
    push_unique(&mut selected, "health");
    push_unique(&mut selected, "investigate");
    push_unique(&mut selected, "plan-eng-review");
    for path in &group.files {
        for skill in selected_skill_names_for_file(path) {
            push_unique(&mut selected, skill);
        }
    }
    render_skill_policy(&selected)
}

fn selected_skill_policy_for_final_review() -> String {
    render_skill_policy(&[
        "review",
        "cso",
        "health",
        "investigate",
        "careful",
        "qa-only",
        "benchmark",
        "devex-review",
        "document-release",
        "ship",
        "land-and-deploy",
        "canary",
        "checkpoint",
    ])
}

pub(crate) fn selected_skill_names_for_file(path: &str) -> Vec<&'static str> {
    let lower = path.to_ascii_lowercase();
    let mut selected = Vec::new();
    push_unique(&mut selected, "review");
    push_unique(&mut selected, "health");
    push_unique(&mut selected, "investigate");

    if is_context_path(&lower) {
        push_unique(&mut selected, "plan-ceo-review");
        push_unique(&mut selected, "plan-eng-review");
        push_unique(&mut selected, "plan-devex-review");
        push_unique(&mut selected, "plan-design-review");
        push_unique(&mut selected, "document-release");
        push_unique(&mut selected, "checkpoint");
        push_unique(&mut selected, "context-save");
        push_unique(&mut selected, "context-restore");
    }
    if is_rust_or_backend_path(&lower) {
        push_unique(&mut selected, "plan-eng-review");
        push_unique(&mut selected, "cso");
    }
    if is_security_or_ops_path(&lower) {
        push_unique(&mut selected, "cso");
        push_unique(&mut selected, "careful");
        push_unique(&mut selected, "ship");
    }
    if is_ui_path(&lower) {
        push_unique(&mut selected, "plan-design-review");
        push_unique(&mut selected, "design-review");
        push_unique(&mut selected, "qa");
        push_unique(&mut selected, "qa-only");
        push_unique(&mut selected, "browse");
        push_unique(&mut selected, "benchmark");
    }
    if is_docs_or_devex_path(&lower) {
        push_unique(&mut selected, "plan-devex-review");
        push_unique(&mut selected, "devex-review");
        push_unique(&mut selected, "document-release");
    }
    if is_test_or_perf_path(&lower) {
        push_unique(&mut selected, "qa");
        push_unique(&mut selected, "qa-only");
        push_unique(&mut selected, "benchmark");
    }
    if is_release_or_deploy_path(&lower) {
        push_unique(&mut selected, "ship");
        push_unique(&mut selected, "land-and-deploy");
        push_unique(&mut selected, "canary");
        push_unique(&mut selected, "setup-deploy");
    }

    selected
}

pub(crate) fn push_unique<'a>(items: &mut Vec<&'a str>, item: &'a str) {
    if !items.contains(&item) {
        items.push(item);
    }
}

pub(crate) fn render_skill_policy(skills: &[&str]) -> String {
    skills
        .iter()
        .map(|skill| format!("- `{skill}`: {}", skill_summary(skill)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn skill_summary(skill: &str) -> &'static str {
    match skill {
        "autoplan" => "run the CEO, engineering, design, and DX review gauntlet as one planning lens.",
        "benchmark" => "check page speed, Core Web Vitals, resource size, and bundle/performance regressions.",
        "browse" => "use browser evidence for UI state, screenshots, responsive behavior, forms, dialogs, and flows.",
        "canary" => "use post-deploy health, console, screenshot, and performance anomaly checks as release criteria.",
        "careful" => "treat destructive commands, deletions, force pushes, production, and shared resources as gated risks.",
        "checkpoint" => "preserve resumability: decisions, git state, remaining work, and handoff clarity.",
        "context-restore" => "verify restored context is sufficient before resuming interrupted work.",
        "context-save" => "capture progress and remaining work in durable, resume-friendly artifacts.",
        "cso" => "audit secrets, auth boundaries, supply chain, CI/CD, LLM trust boundaries, OWASP, and STRIDE risks.",
        "design-consultation" => "create or repair design-system source-of-truth docs when UI lacks coherent direction.",
        "design-review" => "judge implemented UI for visual hierarchy, spacing, consistency, accessibility, and interaction polish.",
        "devex-review" => "test docs, CLI/API ergonomics, onboarding, error messages, and time-to-hello-world.",
        "document-release" => "keep README, AGENTS, ARCHITECTURE, changelog, specs, and TODOs aligned with shipped behavior.",
        "freeze" => "hold remediation to the intended directory or module boundary.",
        "guard" => "combine destructive-command caution with strict write-scope discipline.",
        "health" => "prefer project-native check, lint, test, dead-code, and shell-lint evidence over guesswork.",
        "investigate" => "insist on root cause, falsifiable hypotheses, and direct evidence before proposing fixes.",
        "land-and-deploy" => "judge merge/deploy/canary readiness; do not perform deployment from an audit worker.",
        "plan-ceo-review" => "challenge scope, ambition, product value, and whether the best-version recommendation is worthwhile.",
        "plan-design-review" => "score UI/UX plans for interaction model, accessibility, visual system, hierarchy, and polish.",
        "plan-devex-review" => "score developer-facing APIs, CLIs, docs, onboarding, and friction before implementation.",
        "plan-eng-review" => "review architecture, invariants, data flow, edge cases, test plan, performance, and rollout risk.",
        "qa" => "when edits are allowed, run a test-fix-verify loop for app and browser-facing behavior.",
        "qa-only" => "when edits are disallowed, produce report-only QA evidence with repro steps and health score.",
        "review" => "pre-landing code-review lens for structural bugs, behavioral regressions, and stale documentation.",
        "setup-deploy" => "verify deployment configuration, production URL, health checks, and status commands exist and are current.",
        "ship" => "evaluate base-branch sync, validation, version/changelog, diff hygiene, and PR readiness.",
        _ => "use only when the audited surface directly implements or depends on this skill.",
    }
}

fn is_context_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path == "agents.md"
        || path == "architecture.md"
        || path == "claude.md"
        || path.starts_with("doctrine/")
        || path.starts_with("specs/")
        || path.starts_with("plans/")
        || path.contains("architecture")
}

pub(crate) fn is_rust_or_backend_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.ends_with(".rs")
        || path.ends_with(".toml")
        || path.starts_with("src/")
        || path.starts_with("crates/")
        || path.starts_with("packages/")
        || path.contains("/server/")
        || path.contains("/backend/")
        || path.contains("/api/")
}

fn is_security_or_ops_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("auth")
        || path.contains("secret")
        || path.contains("credential")
        || path.contains("token")
        || path.contains("session")
        || path.contains("cookie")
        || path.contains("tls")
        || path.contains("security")
        || path.contains("policy")
        || path.starts_with(".github/")
        || path.starts_with("infra/")
        || path.starts_with("ops/")
        || path.starts_with("deploy/")
        || path.contains("docker")
}

fn is_ui_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.ends_with(".tsx")
        || path.ends_with(".jsx")
        || path.ends_with(".css")
        || path.ends_with(".scss")
        || path.ends_with(".html")
        || path.contains("/ui/")
        || path.contains("/frontend/")
        || path.contains("/client/")
        || path.contains("/web/")
        || path.contains("/tui/")
        || path.contains("component")
        || path.contains("screen")
        || path.contains("view")
}

pub(crate) fn is_docs_or_devex_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.ends_with(".md")
        || path.starts_with("docs/")
        || path.starts_with("examples/")
        || path.starts_with("scripts/")
        || path.contains("readme")
        || path.contains("cli")
        || path.contains("help")
        || path.contains("onboard")
}

pub(crate) fn is_test_or_perf_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.contains("test")
        || path.contains("spec")
        || path.contains("bench")
        || path.contains("perf")
        || path.contains("playwright")
}

pub(crate) fn is_release_or_deploy_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("release")
        || path.contains("deploy")
        || path.contains("ship")
        || path.contains("version")
        || path.contains("changelog")
        || path.contains("canary")
        || path.starts_with(".github/workflows/")
}

#[cfg(test)]
mod tests {
    use super::{
        build_file_prompt, build_final_review_repair_prompt,
        build_final_review_synthesis_prompt, build_synthesis_prompt,
        build_file_quality_deliverables_prompt, build_file_quality_rerate_prompt,
        selected_skill_names_for_file, selected_skill_policy_for_final_review,
    };
    use crate::audit_everything::manifest::{
        FileQualityRatingState, FileState, GroupState, StageStatus,
    };
    use crate::audit_everything::prompts::CODEBASE_IMPROVEMENT_POLICY;
    use crate::audit_everything::run_paths::{RunPaths, PAUSE_REQUEST_FILE};
    use crate::audit_everything::tests::{group_for_test, manifest_with_groups};
    use std::path::{Path, PathBuf};

    #[test]
    fn codebase_improvement_policy_defines_deletion_contract() {
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("proved-dead code"));
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("AI slop"));
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("Required Proof Before Removal"));
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("safe_delete"));
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("leave_with_reason"));
    }

    #[test]
    fn first_pass_prompt_collects_debt_and_architecture_fields() {
        let file = FileState {
            path: "src/lib.rs".to_string(),
            group: "src".to_string(),
            content_hash: "hash".to_string(),
            artifact_dir: "/tmp/run/worktree/audit/everything/test-run/files/src-lib".to_string(),
            status: StageStatus::Pending,
        };
        let prompt = build_file_prompt(&file, "# Context\n", "pub fn live() {}\n");

        assert!(prompt.contains("orphaned, deprecated, duplicated"));
        assert!(prompt.contains("AI-slop signals"));
        assert!(prompt.contains("deletion_candidates"));
        assert!(prompt.contains("architecture_smells"));
        assert!(prompt.contains("behavior_preservation_needs"));
    }

    #[test]
    fn synthesis_prompt_warns_against_unreferenced_artifact_globs() {
        let paths = RunPaths {
            host_root: PathBuf::from("/tmp/run"),
            manifest_path: PathBuf::from("/tmp/run/manifest.json"),
            latest_path: PathBuf::from("/tmp/run/latest"),
            worktree_root: PathBuf::from("/tmp/run/worktree"),
            report_root: PathBuf::from("/tmp/run/worktree/audit/everything/test-run"),
            pause_path: PathBuf::from("/tmp/run/PAUSE"),
            in_place: false,
        };
        let group = GroupState {
            name: "src".to_string(),
            slug: "src".to_string(),
            files: vec!["src/lib.rs".to_string()],
            report_path: "/tmp/run/worktree/audit/everything/test-run/reports/src.md".to_string(),
            synthesis_status: StageStatus::Pending,
            remediation_status: StageStatus::Pending,
        };

        let prompt = build_synthesis_prompt(&paths, &group);

        assert!(prompt.contains("exact first-pass artifact paths referenced inside it"));
        assert!(prompt.contains("Do not glob or enumerate"));
        assert!(prompt.contains("/tmp/run/worktree/audit/everything/test-run/files"));
        assert!(prompt.contains("Build or update a debt register"));
        assert!(prompt.contains("AI-slop"));
        assert!(prompt.contains("## Debt Register"));
    }

    #[test]
    fn selected_skill_policy_matches_ui_surface() {
        let skills = selected_skill_names_for_file("web/client/src/components/Board.tsx");
        assert!(skills.contains(&"plan-design-review"));
        assert!(skills.contains(&"design-review"));
        assert!(skills.contains(&"qa"));
        assert!(skills.contains(&"browse"));
        assert!(skills.contains(&"benchmark"));
    }

    #[test]
    fn selected_skill_policy_matches_security_and_deploy_surface() {
        let skills = selected_skill_names_for_file(".github/workflows/deploy-auth.yml");
        assert!(skills.contains(&"cso"));
        assert!(skills.contains(&"careful"));
        assert!(skills.contains(&"ship"));
        assert!(skills.contains(&"land-and-deploy"));
        assert!(skills.contains(&"setup-deploy"));
    }

    #[test]
    fn selected_skill_policy_matches_docs_and_context_surface() {
        let skills = selected_skill_names_for_file("ARCHITECTURE.md");
        assert!(skills.contains(&"plan-ceo-review"));
        assert!(skills.contains(&"plan-eng-review"));
        assert!(skills.contains(&"plan-devex-review"));
        assert!(skills.contains(&"document-release"));
        assert!(skills.contains(&"checkpoint"));
    }

    #[test]
    fn final_review_policy_is_merge_readiness_oriented() {
        let policy = selected_skill_policy_for_final_review();
        assert!(policy.contains("`review`"));
        assert!(policy.contains("`ship`"));
        assert!(policy.contains("`land-and-deploy`"));
        assert!(policy.contains("`canary`"));
    }

    #[test]
    fn final_review_prompt_requires_codebase_book() {
        let manifest = manifest_with_groups(Vec::new());
        let paths = RunPaths {
            host_root: PathBuf::from("/tmp/run"),
            manifest_path: PathBuf::from("/tmp/run/MANIFEST.json"),
            latest_path: PathBuf::from("/tmp/run/latest-run"),
            worktree_root: PathBuf::from("/tmp/run/worktree"),
            report_root: PathBuf::from("/tmp/run/worktree/audit/everything/test-run"),
            pause_path: PathBuf::from("/tmp/run/PAUSE"),
            in_place: false,
        };
        let prompt = build_final_review_synthesis_prompt(&paths, &manifest, None);
        assert!(prompt.contains("CODEBASE-BOOK/"));
        assert!(prompt.contains("must not be a single giant markdown file"));
        assert!(prompt.contains("Numbered chapter files"));
        assert!(prompt.contains("File-catalog chapters split by subsystem/group"));
        assert!(prompt.contains("cover every tracked file"));
        assert!(prompt.contains("changed:"));
        assert!(prompt.contains("Evidence class checklist"));
        assert!(prompt.contains("Deletion and refactor proof checklist"));
        assert!(prompt.contains("live production or mainnet/on-chain validation"));
        assert!(prompt.contains("Do not merge bulky first-pass mirrors"));
    }

    #[test]
    fn final_review_repair_prompt_is_bounded_to_actionable_blockers() {
        let manifest = manifest_with_groups(Vec::new());
        let paths = RunPaths {
            host_root: PathBuf::from("/tmp/run"),
            manifest_path: PathBuf::from("/tmp/run/MANIFEST.json"),
            latest_path: PathBuf::from("/tmp/run/latest-run"),
            worktree_root: PathBuf::from("/tmp/run/worktree"),
            report_root: PathBuf::from("/tmp/run/worktree/audit/everything/test-run"),
            pause_path: PathBuf::from("/tmp/run/PAUSE"),
            in_place: false,
        };
        let prompt = build_final_review_repair_prompt(
            &paths,
            &manifest,
            1,
            Path::new(
                "/tmp/run/worktree/audit/everything/test-run/FINAL-REVIEW.no-go-attempt-1.md",
            ),
        );
        assert!(prompt.contains("only concrete, actionable blockers"));
        assert!(prompt.contains("Do not broaden the audit"));
        assert!(prompt.contains("host will rerun final review"));
    }

    #[test]
    fn file_quality_prompts_target_ten_and_accept_nine() {
        let file = FileState {
            path: "src/lib.rs".to_string(),
            group: "src".to_string(),
            content_hash: "hash".to_string(),
            artifact_dir: "/tmp/run/worktree/audit/everything/test-run/files/src-lib".to_string(),
            status: StageStatus::Complete,
        };
        let mut manifest = manifest_with_groups(vec![group_for_test("src", &["src/lib.rs"])]);
        manifest.files = vec![file.clone()];
        let paths = RunPaths {
            host_root: PathBuf::from("/tmp/run"),
            manifest_path: PathBuf::from("/tmp/run/MANIFEST.json"),
            latest_path: PathBuf::from("/tmp/run/latest-run"),
            worktree_root: PathBuf::from("/tmp/run/worktree"),
            report_root: PathBuf::from("/tmp/run/worktree/audit/everything/test-run"),
            pause_path: PathBuf::from("/tmp/run/PAUSE"),
            in_place: false,
        };

        let rerate = build_file_quality_rerate_prompt(&paths, &manifest, &file, 3);
        assert!(rerate.contains("Regrade the first-pass rating"));
        assert!(rerate.contains("target is 10/10"));
        assert!(rerate.contains("below 9/10"));
        assert!(rerate.contains("rating.json"));
        assert!(rerate.contains("AI-slop"));
        assert!(rerate.contains("debt_or_architecture_findings"));

        let rating = FileQualityRatingState {
            path: file.path.clone(),
            score_out_of_10: Some(8.0),
            status: StageStatus::Complete,
            artifact_dir:
                "/tmp/run/worktree/audit/everything/test-run/FILE-QUALITY/pass-03/src-lib"
                    .to_string(),
            note: None,
        };
        let deliverables =
            build_file_quality_deliverables_prompt(&paths, &manifest, &file, &rating, 3);
        assert!(deliverables.contains("Owned file: `src/lib.rs`"));
        assert!(deliverables.contains("acceptance floor is 9/10"));
        assert!(deliverables.contains("aim at 10/10"));
        assert!(deliverables.contains("deliverables.md"));
        assert!(deliverables.contains("delete, simplify, consolidate, or relocate code"));
        assert!(deliverables.contains("Deletion/refactor proof"));
    }
}
