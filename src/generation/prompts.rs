//! Pure prompt builders and shared prompt constants for the generation
//! pipeline. Nothing in this module performs IO.

use std::path::{Path, PathBuf};

use crate::corpus::PlanningCorpus;
use crate::generation::{
    ActivePlanSurface, CorpusPromptInputs, GeneratedSpecDocument, GenerationMode,
};
use crate::prompt_ethos::PRODUCTION_ORCHESTRATION_DISCIPLINE;

pub(crate) const IMPLEMENTATION_PLAN_HEADER: &str = "# IMPLEMENTATION_PLAN";
pub(crate) const SPEC_OBJECTIVE_HEADER: &str = "## Objective";
pub(crate) const SPEC_ACCEPTANCE_CRITERIA_HEADER: &str = "## Acceptance Criteria";
pub(crate) const SPEC_VERIFICATION_HEADER: &str = "## Verification";
pub(crate) const REQUIRED_SPEC_SECTIONS: [&str; 12] = [
    SPEC_OBJECTIVE_HEADER,
    "## Source Of Truth",
    "## Evidence Status",
    "## Runtime Contract",
    "## UI Contract",
    "## Generated Artifacts",
    "## Fixture Policy",
    "## Retired / Superseded Surfaces",
    SPEC_ACCEPTANCE_CRITERIA_HEADER,
    SPEC_VERIFICATION_HEADER,
    "## Review And Closeout",
    "## Open Questions",
];
pub(crate) const REQUIRED_PLAN_SECTIONS: [&str; 3] = [
    "## Priority Work",
    "## Follow-On Work",
    "## Completed / Already Satisfied",
];
pub(crate) const CORPUS_PRIORITY_PLAN_REQUIRED_SECTIONS: [&str; 7] = [
    "## Priority Decision",
    "## User / Operator Outcome",
    "## Evidence",
    "## Scope Boundary",
    "## Implementation Slice",
    "## Verification",
    "## Deferred",
];
pub(crate) const CORPUS_REPORT_REQUIRED_SECTIONS: [&str; 3] = [
    "## Priority Focus",
    "## Next Autodev Lever",
    "## Delete Or Demote",
];
pub(crate) const CORPUS_NEXT_LEVER_MARKERS: [&str; 5] = [
    "auto design",
    "auto gen",
    "auto parallel",
    "active run",
    "human decision",
];
pub(crate) const CORPUS_DELETE_DEMOTE_MARKERS: [&str; 9] = [
    "delete",
    "demote",
    "not-doing",
    "not doing",
    "stale",
    "evidence-only",
    "docs-only",
    "lower-priority",
    "none",
];
pub(crate) const CORPUS_LEGACY_EXECPLAN_REQUIRED_SECTIONS: [&str; 15] = [
    "## Purpose / Big Picture",
    "## Requirements Trace",
    "## Scope Boundaries",
    "## Progress",
    "## Surprises & Discoveries",
    "## Decision Log",
    "## Outcomes & Retrospective",
    "## Context and Orientation",
    "## Plan of Work",
    "## Implementation Units",
    "## Concrete Steps",
    "## Validation and Acceptance",
    "## Idempotence and Recovery",
    "## Artifacts and Notes",
    "## Interfaces and Dependencies",
];
pub(crate) const CODEX_SKILL_BOUNDARY: &str = "IMPORTANT: Do NOT read or execute any SKILL.md files or files in skill definition directories (paths containing skills/gstack). These are AI assistant skill definitions meant for a different system. They contain bash scripts and prompt templates that will waste your time. Ignore them completely. Stay focused on the repository code only.";
pub(crate) const PRIORITY_FOCUS_LENS: &str = r#"Priority focus lens:
- Prioritize by product leverage, user-visible clarity, and engineering feasibility/tests. The best output is not the largest audit; it is the smallest truthful slice that improves the product or operator experience and can be proven with narrow verification.
- Treat the corpus as a priority map for the next implementation cycle, not as an audit book or documentation exercise.
- Meld the CEO, design, and engineering perspectives into one ranking:
  1. User/operator value: does this create or unblock the core product loop?
  2. Design clarity: will a user or operator know what to do, what state they are in, and how to recover?
  3. Engineering leverage: does this reduce the biggest implementation risk, source-of-truth drift, or verification gap?
  4. Evidence: is there code, tests, logs, or direct repo evidence that this matters now?
  5. Parallel executability: can one focused worker land the slice with concrete verification?
- For repos that are not production codebases yet, do not front-load large documentation, audit, governance, release, or artifact-expansion work. Capture only the smallest docs needed to keep implementation truthful.
- Documentation, audits, reports, generated snapshots, and process artifacts are support evidence, not the goal. They only outrank code, tests, or UX/runtime work when they directly unblock the next executable slice or correct stale instructions that would send workers into the wrong files.
- Prefer vertical slices that make the system run, become testable, or become understandable to its intended user/operator over broad inventory, book-writing, or report-only tasks.
- The lenses above inform RANKING only. Never write lens numbers, `Trace: lens 1/2/3`, or CEO/Design/Eng provenance labels into the output rows, and never emit one task per lens -- a lens that surfaces nothing real produces zero tasks.
- Priority classes:
  - P0: blocks using, learning from, or validating the core product loop now.
  - P1: unlocks multiple future slices, removes high-risk ambiguity, or makes tests catch real regressions.
  - P2: cleanup, polish, docs, audits, reports, or artifact hygiene that does not unlock immediate learning.
- Score candidates by user/operator pain, code leverage/reuse, risk retired, executable proof within one worker slice, and subtraction/scope reduction. Apply a penalty to docs/audit/artifact-only work unless it directly unblocks P0/P1 implementation.
- Every top priority must state why it outranks plausible alternatives using the combined product/design/engineering lens above."#;

pub(crate) fn build_corpus_prompt(
    repo_root: &Path,
    planning_root: &Path,
    inputs: CorpusPromptInputs<'_>,
) -> String {
    let CorpusPromptInputs {
        previous_planning_snapshot,
        parallelism,
        idea,
        focus,
        reference_repos,
        active_plan_surface,
        gbrain_context_path,
    } = inputs;
    let planning_root = planning_root
        .strip_prefix(repo_root)
        .unwrap_or(planning_root)
        .display()
        .to_string();
    let previous_snapshot_clause = previous_planning_snapshot
        .map(|path| {
            format!(
                "- Archived previous planning snapshot for optional historical context: `{}`\n",
                path.display()
            )
        })
        .unwrap_or_default();
    let idea_output_clause = if idea.is_some() {
        format!("- `{planning_root}/IDEA.md`\n")
    } else {
        String::new()
    };
    let focus_output_clause = if focus.is_some() {
        format!("- `{planning_root}/FOCUS.md`\n")
    } else {
        String::new()
    };
    let reference_repo_clause = if reference_repos.is_empty() {
        String::new()
    } else {
        let listing = reference_repos
            .iter()
            .map(|path| format!("- Mandatory reference repo: `{}`", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Reference repositories to inspect as required input:\n{listing}\n\nWhen reference repos are listed:\n- Inspect them directly; do not treat them as optional background.\n- Use them to distinguish reusable code, architectural inspiration, and non-reusable coupling.\n- Be explicit about which conclusions came from the target repo vs the reference repos.\n\n"
        )
    };
    let gbrain_context_clause = gbrain_context_clause(repo_root, gbrain_context_path);
    let active_plan_clause = if active_plan_surface.root_plan_standard_path.is_none()
        && !active_plan_surface.has_active_plans()
    {
        String::new()
    } else if active_plan_surface.has_active_plans() {
        let root_standard = active_plan_surface
            .root_plan_standard_path
            .as_deref()
            .map(|path| format!("- Root ExecPlan standard: `{path}`\n"))
            .unwrap_or_default();
        let active_plans = active_plan_surface
            .active_plan_paths
            .iter()
            .map(|path| format!("- Active root plan: `{path}`"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Existing root planning surfaces to inspect as first-class inputs:\n{root_standard}{active_plans}\nWhen root plans already exist:\n- Treat them as strong evidence about current planning intent and sequencing.\n- Before calling them the active control plane, inspect repo-root instruction files such as `AGENTS.md` or `CLAUDE.md` and any control docs that may explicitly designate a different active planning root.\n- Do not create a second active master plan or competing queue inside `{planning_root}` unless the repo's own instructions explicitly say `{planning_root}` is the active planning corpus.\n- If repo instructions say another planning root is active, preserve that relationship explicitly instead of forcing subordination to the root plans.\n- If you disagree with an active plan, record that disagreement explicitly as `Mechanical`, `Taste`, or `User Challenge`; do not silently replace the plan hierarchy.\n- Reuse shared interface names and planning vocabulary from the established planning surface unless code evidence proves they are wrong.\n\n"
        )
    } else {
        let root_standard = active_plan_surface
            .root_plan_standard_path
            .as_deref()
            .unwrap_or("PLANS.md");
        format!(
            "Existing planning-standard input to inspect:\n- Root ExecPlan standard: `{root_standard}`\nWhen only a root planning standard exists:\n- Read it fully and follow its ExecPlan shape for any generated numbered plans.\n- Do not infer from `{root_standard}` alone that root backlog files own the active control plane.\n- Inspect repo-root instruction files such as `AGENTS.md` or `CLAUDE.md` to determine whether a different planning root such as `{planning_root}` is explicitly designated as active.\n- If repo instructions designate `{planning_root}` or another planning root as active, preserve that relationship explicitly in the generated corpus.\n\n"
        )
    };
    let idea_context_clause = idea
        .map(|idea| {
            format!(
                r#"- Idea seed from the operator: `{idea}`

Run a non-interactive office-hours shaping pass first:
- Treat the idea seed as the intended future state.
- Do not ask follow-up questions. Infer the strongest truthful framing from the idea, the repo, and the surrounding code reality.
- Pressure-test the idea the way office-hours would: demand reality, status quo, desperate specificity, narrowest wedge, observation risk, and future-fit.
- If evidence is missing because the idea is early, label those sections as hypotheses or open questions instead of pretending certainty.
- Infer whether this is closer to startup mode or builder mode and say why.
- Write the result to `{planning_root}/IDEA.md` as a durable seed brief before expanding the rest of the corpus.

`IDEA.md` must include:
- the raw idea in normalized form
- inferred mode: startup or builder, with a short rationale
- problem statement
- target user or audience
- strongest demand evidence currently available vs what is still hypothetical
- status quo / current workaround
- narrowest wedge
- success criteria
- constraints
- assumptions and open questions
- key assumptions to validate next, with the fastest credible validation path for each
- candidate approaches
- alternatives considered and why they were rejected
- risks
- explicit non-goals
- one recommended direction
"#
            )
        })
        .unwrap_or_default();
    let focus_context_clause = focus
        .map(|focus| {
            format!(
                r#"- Focus steering from the operator: `{focus}`

Treat this as an attention and prioritization signal, not a blinders command:
- Still perform a wide repo sweep and do not ignore critical issues outside the focus
- Spend extra review budget on the focused surfaces, likely failure modes, and next-priority decisions
- Use the focus to rank recommendations and plans, not to invent scope unsupported by the codebase
- Write the normalized focus brief to `{planning_root}/FOCUS.md`

`FOCUS.md` must include:
- the raw focus string
- the normalized focus themes
- the likely code, product, and operational surfaces this implies
- what still requires repo-wide review despite the focus
- the main questions the focus should answer
- how the focus changed priority ordering, if it did
"#
            )
        })
        .unwrap_or_default();
    format!(
        r#"You are the interim CEO/CTO of this repository at `{target_repo}`. Your job is to perform a deep repo review and author a detailed planning corpus.

Overarching objective (the default north-star when no idea seed overrides it): advance the UNIFIED TWO-CURRENCY ISLAND ECONOMY -- agents building a real economy using rBTC (a boring literal Bitcoin fork; stable money) and rNG (an experimental mined currency that may fork/mutate freely), both live on networks we own 100% (build on our own live mainnet chains; no external value, no real BTC/wallet/recovery-key exposure). Name THIS repo's role in that economy and the exact integration surface to the hub (a two-currency wallet/ledger and game settlement in rBTC|rNG, mineable by anyone via trustlessminer, played/settled via bitino, transacted by agents via autonomy). Rank every focus area by how directly it advances a real, user/operator/agent-exercisable capability in that unified economy. Guard scripts, receipts, audits, evidence rollups, CI plumbing, and process bookkeeping are NEVER a top focus area unless they directly unblock a named economy capability.

Write all output files with tools into `{planning_root}/`; do not print the corpus to stdout.

Use up to {parallelism} parallel subagents when helpful for code review, repo-history analysis, and topic decomposition.

Additional operator-provided context:
{previous_snapshot_clause}
{gbrain_context_clause}
{reference_repo_clause}
{active_plan_clause}

{priority_focus_lens}

{production_orchestration_discipline}

Mandatory output files:
- `{planning_root}/ASSESSMENT.md`
- `{planning_root}/SPEC.md`
- `{planning_root}/PLANS.md`
- `{planning_root}/GENESIS-REPORT.md`
- `{planning_root}/DESIGN.md` if the repo has meaningful user-facing surfaces
{idea_output_clause}{focus_output_clause}- `{planning_root}/plans/001-master-plan.md`
- `{planning_root}/plans/002-*.md` through `plans/NNN-*.md`

Review the actual codebase first, not just docs:
- Read the main entry points, state definitions, and user-facing routes
- Review security boundaries, input validation, observability, tests, CI, and git history
- Treat completed docs and plans as claims that must be verified against code
- If an archived previous planning snapshot exists, use it only as historical context, not truth
- If an idea seed is present, use it as intentional product direction, then reconcile it against repo reality, reusable assets, and the actual gaps.
- If a focus seed is present, use it to bias depth and plan ordering while still preserving full-repo coverage.
- If root plans already exist under `plans/`, reconcile to them explicitly unless repo-root instructions clearly designate `{planning_root}` or another planning root as the active control corpus.
- The current codebase is still the truth for current state, constraints, and what can be reused.
- Read repo-root instruction files such as `AGENTS.md` or `CLAUDE.md` before deciding which planning surface is active.
- Never emit the absolute repository-root path in generated markdown. In prose say "the repository root"; in shell examples either assume the command starts at repo root or use `cd "$(git rev-parse --show-toplevel)"` when a directory change is required.
- When the repo needs an agent-instruction file, prefer the repo's actual primary convention.
  - In Codex-first repos, prefer `AGENTS.md`.
  - Do not choose the instruction filename based on which planning model ran the corpus pass.
- Start by framing the repo as a real product/system:
  - write a crisp "How Might We" style problem statement grounded in the current code reality
  - identify the primary users/operators and what success should look like for them
  - surface the biggest constraints, hidden assumptions, and trade-offs
  - consider 2-3 plausible future directions before choosing the recommended one
  - make a clear "Not Doing" list so the corpus reflects focus, not wishful scope
  - if the repo is developer-facing, also assess the first-run developer experience: zero friction at T0, learn-by-doing, uncertainty reduction, and whether the onboarding examples are honest about the real work
- Every exact version, dependency tag, timeout, threshold, benchmark target, chain choice, or protocol detail must be handled explicitly as one of:
  - verified from code or a primary source
  - recommendation for the new system
  - hypothesis / open question
- Do not present guessed values as settled requirements.
- For future phases with unresolved feasibility, keep the artifacts at research/design level instead of pretending the implementation details are already locked.
- Apply the current gstack `/autoplan` review discipline while authoring the corpus:
  - Run the review in the sequence CEO -> Design when UI/UX is in scope -> Eng -> DX when the repo is developer-facing or has meaningful setup/API/operator experience.
  - CEO review must challenge the premise, map existing code leverage before proposing new work, compare plausible future states, state alternatives considered, preserve a real Not Doing list, and capture major failure modes and rescue paths.
  - Design review must cover information architecture, state coverage, user journeys, accessibility, responsive behavior, and AI-slop risk when the repo has user-facing surfaces; if it does not, say why the design pass is not applicable.
  - Eng review must cover architecture, dependency order, data flow, integration seams, persistence/migrations, error handling, observability, performance, and testing; every no-issue conclusion must still say what was examined and why it is acceptable.
  - DX review must cover first-run developer/operator experience, learn-by-doing paths, error clarity, time-to-hello-world, honest examples, and uncertainty-reducing docs or tooling when applicable.
  - Classify important planning decisions as `Mechanical`, `Taste`, or `User Challenge`. Treat model disagreements and close alternatives as taste decisions that need a short rationale. Treat any point that would change the operator's stated direction as a user challenge instead of silently auto-deciding it.
  - Use these decision principles: choose completeness, inspect broadly when the problem requires it, stay pragmatic, avoid redundant artifacts, prefer explicit contracts over clever prose, and bias toward action when evidence is sufficient.
- Apply the priority focus lens when ranking outputs: a small implementation slice that proves the core loop, fixes a runtime/source-of-truth gap, or makes a user/operator path clear should outrank a large documentation, audit, or artifact-only exercise unless the artifact directly unblocks that slice.
- Apply the production orchestration discipline when the repo already has an active queue: the corpus should steer, reconcile, and reorder the next executor cycle, not expand lower-priority evidence work ahead of executable blockers.
- Before finishing, make an explicit autodev lever decision: whether the operator should run `auto design`, run `auto gen`, run `auto parallel`, continue/supervise an existing run, or stop for a human decision. Tie that recommendation to repo evidence, conflict risk, and the top dependency that must be true first.

{idea_context_clause}
{focus_context_clause}

ASSESSMENT.md must include:
- what the project says it is vs what the code shows it is
- what works, what is broken, what is half-built
- tech debt inventory
- security risks
- test gaps
- documentation staleness
- implementation-status table for prior claims and plans
- code-review coverage list proving which source files were actually read
- target users, success criteria, and repo constraints
- assumption ledger: what seems true, what is verified, and what still needs proof
- focus-response section: what the operator focus emphasized, what the code says about it, and any non-focused risks that still outrank it
- opportunity framing: strongest direction, rejected directions, and why they were rejected
- priority focus map: the top 3-5 focus areas, each scored qualitatively against user/operator value, design clarity, engineering leverage, evidence, and parallel executability
- autodev lever decision: the recommended next command path across `auto design`, `auto gen`, `auto parallel`, and any active run, including restart/conflict risk
- for developer-facing repos: a short DX assessment covering first-run friction, copy-paste onboarding honesty, error clarity, and whether the fastest path produces a meaningful success moment

SPEC.md must summarize the repo as a product/system with concrete behaviors grounded in the code and near-term direction.

`{planning_root}/PLANS.md` must index the generated plan set and explain sequencing, dependency order, and why the chosen slice order is preferable to obvious alternatives. This file is an index, not the ExecPlan authoring standard. If the target repo has a root `PLANS.md`, read the entire file before writing numbered plans, treat it as the governing ExecPlan standard, and make the generated index say that numbered plans follow the root `PLANS.md` standard. Determine the active planning surface from the repo's own instructions and control docs rather than assuming it from filename alone. If the target repo already has active root plans under `plans/` and no repo instruction overrides that, the generated index must say those root plans remain the active planning surface and that the generated corpus is subordinate to them. If repo instructions designate `{planning_root}` as the active planning corpus, the generated index must say that explicitly instead of inventing root-level primacy.

GENESIS-REPORT.md must start with `## Priority Focus` before general findings. That section must list the top 3-5 focus areas in priority order and explain why each outranks plausible documentation, audit, artifact, or process work right now. Every focus area MUST name an observable product, runtime, or protocol capability that moves this repo toward its production-grade testnet milestone, and state the user/operator-visible change it unlocks. A focus area whose payload is only a guard, receipt, audit, evidence rollup, CI trigger, or process/bookkeeping change may never appear in the top 3-5 unless it is the single unavoidable unblocker for a named product capability listed alongside it. Keep the "Not Doing" list real: park lower-leverage audit/doc/process work there explicitly rather than smuggling it into Priority Focus.
GENESIS-REPORT.md must summarize the corpus refresh, major findings, recommended direction, top next priorities, and the explicit "Not Doing" list.
GENESIS-REPORT.md must include a `## Next Autodev Lever` section that recommends exactly one immediate lever path from `auto design`, `auto gen`, `auto parallel`, continuing/supervising the active run, or human decision. If the repo has meaningful UI/TUI/frontend surfaces, explicitly decide whether `auto design` should run before generation or parallel execution.
GENESIS-REPORT.md must include a `## Delete Or Demote` section naming stale, evidence-only, docs-only, or lower-priority tracks that should not consume the next executor cycle unless they directly unlock a named implementation slice.
If a focus seed exists, GENESIS-REPORT.md must also say how it changed the recommended priority order and call out any higher-priority issues that escaped the requested focus.
GENESIS-REPORT.md must also include a concise decision audit trail with `Mechanical`, `Taste`, and `User Challenge` classifications for major scope and sequencing choices.

Each numbered plan under `{planning_root}/plans/` must be a compact priority plan, not a high-level task stub and not a 15-section audit document. The generated plan file itself is the plan, so omit surrounding triple-backtick fences and do not nest fenced code blocks inside it; use indented command blocks when examples are needed.

Priority-plan requirements for every numbered plan:
- start with a markdown H1 title
- do not include YAML front matter or metadata blocks before the H1
- be fully self-contained for a novice who has only the current working tree and that single plan file
- define every non-obvious term in plain language and tie it to concrete repo files or commands
- describe one concrete vertical slice or research gate, not a vague epic
- prefer code/test/UX/runtime-contract slices over docs-only or audit-only work; keep docs-only and report-only plans out of the first priority group unless they unblock implementation or prevent workers from following stale instructions
- if a slice feels larger than one focused implementation session, split it into additional numbered plans
- keep future-phase plans with unresolved feasibility research-shaped, with explicit decision gates before implementation promises
- after every 2-3 numbered plans or at meaningful phase boundaries, include an explicit checkpoint or decision-gate plan only when later work truly depends on unresolved evidence

Every numbered plan under `{planning_root}/plans/` must include these non-empty sections, using these exact headings:
- `## Priority Decision`
- `## User / Operator Outcome`
- `## Evidence`
- `## Scope Boundary`
- `## Implementation Slice`
- `## Verification`
- `## Deferred`

Section requirements for numbered priority plans:
- `## Priority Decision` states P0/P1/P2, the score/rationale, and why this outranks plausible alternatives.
- `## Priority Decision` also states which autodev lever this plan feeds (`auto design`, `auto gen`, `auto parallel`, active-run supervision, or human decision) and why that lever is the right next control point.
- `## User / Operator Outcome` explains what a user or operator gains and how they can see it working.
- `## Evidence` names repository-relative code, tests, logs, commands, or docs that prove this priority is real.
- `## Scope Boundary` states what the plan intentionally does not change, especially docs/audits/artifacts that are not needed now.
- `## Implementation Slice` names the goal, dependencies, files to create or modify, tests to add or modify, and the approach. For code or UX/runtime implementation slices, include literal lines or labels containing `Goal`, `Files`, and `Tests` inside `## Implementation Slice`; commands in `## Verification` do not satisfy the slice contract by themselves. For research/checkpoint plans, name the decision artifact and write `Test expectation: none -- <reason>` only when no code behavior changes. For a master/index plan whose deliverable is the generated plan set rather than code, write an explicit dispatch phrase inside `## Implementation Slice`, such as `Plan dispatch`, `Dispatch index`, or `The master plan delivers`.
- `## Verification` gives exact commands or checks from the repository root and the expected observation.
- `## Deferred` lists follow-on work that is intentionally not part of this slice.

Do not use the short `## Objective` / `## Description` / `## Acceptance Criteria` / `## Verification` / `## Dependencies` shape for numbered plans. That shape is too high-level for this corpus. Do not use the old 15-section ExecPlan envelope unless a repo-local instruction explicitly requires it.

Never trust docs over code. If docs claim something the code does not support, say so clearly."#,
        target_repo = repo_root.display(),
        planning_root = planning_root,
        parallelism = parallelism,
        previous_snapshot_clause = previous_snapshot_clause,
        reference_repo_clause = reference_repo_clause,
        active_plan_clause = active_plan_clause,
        idea_output_clause = idea_output_clause,
        focus_output_clause = focus_output_clause,
        idea_context_clause = idea_context_clause,
        focus_context_clause = focus_context_clause,
        priority_focus_lens = PRIORITY_FOCUS_LENS,
        production_orchestration_discipline = PRODUCTION_ORCHESTRATION_DISCIPLINE,
    )
}

pub(crate) fn build_corpus_codex_review_prompt(
    repo_root: &Path,
    planning_root: &Path,
    report_path: &Path,
    reference_repos: &[PathBuf],
    active_plan_surface: &ActivePlanSurface,
) -> String {
    let reference_repo_clause = if reference_repos.is_empty() {
        String::new()
    } else {
        let listing = reference_repos
            .iter()
            .map(|path| {
                format!(
                    "- Reference repo available to inspect: `{}`",
                    path.display()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Reference repositories already supplied to the corpus run:\n{listing}\n- Inspect them directly before calling cross-repo work ungrounded.\n- Be explicit about which findings came from the target repo vs a reference repo.\n\n"
        )
    };
    let active_plan_clause = if active_plan_surface.root_plan_standard_path.is_none()
        && !active_plan_surface.has_active_plans()
    {
        String::new()
    } else if active_plan_surface.has_active_plans() {
        let root_standard = active_plan_surface
            .root_plan_standard_path
            .as_deref()
            .map(|path| format!("- Root ExecPlan standard: `{path}`\n"))
            .unwrap_or_default();
        let active_plans = active_plan_surface
            .active_plan_paths
            .iter()
            .map(|path| format!("- Active root plan: `{path}`"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Root planning inputs already exist:\n{root_standard}{active_plans}\n- The generated corpus must reconcile to these surfaces explicitly unless repo-root instructions designate another planning root as active.\n- Reject any corpus that creates a second active master plan or competing control plane without an explicit repo-instruction basis.\n- Require `GENESIS-REPORT.md` or `{planning_root}/PLANS.md` to explain the actual planning relationship the repo declares, whether that means subordination to root plans or an explicitly active `{planning_root}` corpus.\n\n",
            planning_root = planning_root.display(),
        )
    } else {
        let root_standard = active_plan_surface
            .root_plan_standard_path
            .as_deref()
            .unwrap_or("PLANS.md");
        format!(
            "A root ExecPlan standard already exists:\n- Root ExecPlan standard: `{root_standard}`\n- The review must enforce that generated numbered plans follow this format.\n- The review must not assume from `{root_standard}` alone that root backlog files own the active control plane; inspect repo-root instruction files such as `AGENTS.md` or `CLAUDE.md` first.\n\n"
        )
    };
    format!(
        r#"{skill_boundary}

You are the mandatory GPT-5.5 xhigh Codex outside-voice review step for `auto corpus`.

A GPT-5.5 xhigh Codex authoring pass has already produced the initial planning corpus under `{planning_root}` for the repository at `{repo_root}`. Your job is to conduct an independent review and validation pass, then amend the generated corpus in place when the documents fall short.

Edit boundary:
- You may read the repository at `{repo_root}` and the generated corpus at `{planning_root}`.
- You may edit only markdown files under `{planning_root}` and the review report at `{report_path}`.
- Do not edit source code, root specs, root implementation plans, generated output dirs outside `{planning_root}`, or any skill definition directory.
- Do not ask the user questions. Make conservative, code-grounded decisions and record uncertainty.

{reference_repo_clause}{active_plan_clause}

{priority_focus_lens}

{production_orchestration_discipline}

Review method adapted from the latest gstack `/autoplan` workflow:
- Run review phases in order: CEO, Design when user-facing UI or UX is in scope, Eng, and DX when the repo is developer-facing or has a meaningful setup/API/operator experience.
- Use these decision principles: choose completeness over shortcuts; be willing to inspect broadly when needed; be pragmatic; avoid duplicate/redundant artifacts; prefer explicit contracts over clever prose; bias toward action when evidence is sufficient.
- Enforce the priority focus lens: if the corpus ranks a documentation, audit, report, or generated-artifact exercise above executable product/code/test/UX progress, require code-grounded evidence that it directly unblocks the next slice. Otherwise move it behind higher-leverage implementation focus.
- Enforce the production orchestration discipline: if an active root queue exists, amend the corpus so it reconciles to the highest-priority executable queue work instead of producing duplicate evidence, audit, or checkpoint lanes.
- Classify important review decisions in the report as `Mechanical`, `Taste`, or `User Challenge`.
- Treat a `User Challenge` as any point where both the authoring pass and your independent review would recommend changing the user's stated direction. Do not silently auto-decide those; preserve the challenge explicitly in `GENESIS-REPORT.md`, `ASSESSMENT.md`, or `{report_path}`.
- Treat author-vs-review disagreements that are not mechanical as `Taste` decisions, explain why you chose one direction, and amend the corpus only when the repository evidence supports the change.

CEO review pass:
- Re-test the premise, product direction, opportunity cost, and "Not Doing" list against the actual code.
- Map existing code leverage before recommending new work.
- Check that alternatives were considered and rejected for concrete reasons.
- Look for hidden assumptions, failure modes, rescue paths, and unclear scope boundaries.

Design review pass, when applicable:
- Check information architecture, user journeys, empty/loading/error/success states, accessibility, responsive behavior, and AI-slop risk.
- If the repo has no meaningful UI, say that in the report and skip UI-specific rewrites.

Eng review pass:
- Check architecture, data flow, dependency order, integration points, migrations/persistence, error handling, observability, performance risks, and test strategy.
- Verify current-state claims against files, commands, or code structure. Docs are claims, not truth.

DX review pass, when applicable:
- Check first-run developer/operator experience, learn-by-doing path, error clarity, time-to-hello-world, honest examples, and uncertainty-reducing docs or tooling.
- If the repo is not developer-facing, say that in the report and skip DX-specific rewrites.

Corpus-specific validation:
- `ASSESSMENT.md` must say what was actually inspected, separate verified facts from assumptions, and call out stale doc claims.
- `ASSESSMENT.md` must include a priority focus map with the top 3-5 focus areas ranked by user/operator value, design clarity, engineering leverage, evidence, and parallel executability.
- `SPEC.md` must describe concrete current behavior and intended near-term direction without presenting guesses as settled facts.
- `PLANS.md` under `{planning_root}` must be an index to the generated plan set, not a substitute for the repo root ExecPlan standard.
- Determine the active planning surface from repo instructions and control docs, not from filenames alone.
- If active root plans already exist under `plans/` and the repo's own instructions do not designate another active planning root, the generated corpus must explicitly reconcile to them and must not present itself as a second active planning surface.
- If repo-root instructions explicitly designate `{planning_root}` as the active planning corpus, the generated corpus should say that plainly and should not invent root-level primacy.
- `GENESIS-REPORT.md` must start with `## Priority Focus` and explain why the chosen top focus areas outrank plausible documentation, audit, artifact, or process work right now.
- `GENESIS-REPORT.md` must include `## Next Autodev Lever` with one immediate command-path recommendation across `auto design`, `auto gen`, `auto parallel`, continuing/supervising an active run, or human decision. For repos with meaningful UI/TUI/frontend surfaces, the recommendation must explicitly decide whether `auto design` should run before generation or parallel execution.
- `GENESIS-REPORT.md` must include `## Delete Or Demote` naming stale, evidence-only, docs-only, or lower-priority tracks that should not consume the next executor cycle unless they directly unlock a named implementation slice.
- Every numbered plan under `{planning_root}/plans/` must use the compact priority-plan shape rather than the old high-level `Objective` / `Description` / `Acceptance Criteria` / `Verification` / `Dependencies` stub shape or the bulky 15-section audit-style ExecPlan shape.
- Numbered priority plans must be self-contained, novice-readable, vertically sliced where possible, and grounded in repository-relative files and commands.
- Reject or rewrite any absolute repo-root path that appears in the corpus. Use repository-relative references, "the repository root" in prose, or `cd "$(git rev-parse --show-toplevel)"` in shell examples instead.
- Every numbered priority plan must include non-empty sections for `Priority Decision`, `User / Operator Outcome`, `Evidence`, `Scope Boundary`, `Implementation Slice`, `Verification`, and `Deferred`.
- `Priority Decision` must state P0/P1/P2 and why the slice outranks plausible docs/audit/artifact work. `Implementation Slice` must name goal, dependencies, files to create or modify, tests to add or modify, and approach. For code or UX/runtime implementation slices, include literal lines or labels containing `Goal`, `Files`, and `Tests` inside `## Implementation Slice`; commands in `## Verification` do not satisfy the slice contract by themselves. For research-only or checkpoint work, name the decision artifact and explain why no code test is expected. For a master/index plan whose deliverable is the generated plan set rather than code, write an explicit dispatch phrase inside `## Implementation Slice`, such as `Plan dispatch`, `Dispatch index`, or `The master plan delivers`.
- `Priority Decision` must state which autodev lever the plan feeds and why that lever is the right next control point.
- Add checkpoint or decision-gate plans only when later work depends on unresolved evidence.

Validation expectations:
- Use lightweight local inspection commands as needed, such as `rg`, `ls`, and targeted file reads. Do not run long integration suites or production-affecting commands for this document review pass.
- After edits, re-check the generated corpus shape yourself before finishing.
- Write `{report_path}` with these sections: `# Codex Corpus Review`, `## Summary`, `## Files Reviewed`, `## Changes Made`, `## Decision Audit Trail`, `## User Challenges`, `## Taste Decisions`, `## Validation`, and `## Remaining Risks`.
- If no corpus edits are needed, still write the report and explain what you checked.
"#,
        skill_boundary = CODEX_SKILL_BOUNDARY,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        report_path = report_path.display(),
        reference_repo_clause = reference_repo_clause,
        active_plan_clause = active_plan_clause,
        priority_focus_lens = PRIORITY_FOCUS_LENS,
        production_orchestration_discipline = PRODUCTION_ORCHESTRATION_DISCIPLINE,
    )
}

pub(crate) fn build_generation_codex_review_prompt(
    mode: GenerationMode,
    repo_root: &Path,
    planning_root: &Path,
    output_dir: &Path,
    report_path: &Path,
) -> String {
    let mode_clause = match mode {
        GenerationMode::Gen => {
            "This is an `auto gen` review. The corpus represents intended future direction, but current code remains authoritative for every current-state fact. Preserve future intent only when it is labeled as a recommendation, hypothesis, or decision gate until evidence proves it."
        }
        GenerationMode::Reverse => {
            "This is an `auto reverse` review. The live codebase is the source of truth, and the corpus is supporting context only."
        }
    };
    format!(
        r#"{skill_boundary}

You are the mandatory GPT-5.5 xhigh Codex outside-voice review step for `{command_label}`.

A GPT-5.5 xhigh Codex authoring pass has already produced initial generated specs and an implementation plan in `{output_dir}` for the repository at `{repo_root}`.

{mode_clause}

Edit boundary:
- You may read the repository at `{repo_root}`, the planning corpus at `{planning_root}`, and generated outputs at `{output_dir}`.
- You may edit only `{output_dir}/specs/*.md`, `{output_dir}/IMPLEMENTATION_PLAN.md`, and the review report at `{report_path}`.
- Do not edit root `specs/`, root `IMPLEMENTATION_PLAN.md`, source code, the planning corpus, or any skill definition directory. The generator will sync reviewed outputs to the root after your pass.
- Do not ask the user questions. Make conservative, code-grounded decisions and record uncertainty.

{priority_focus_lens}

{production_orchestration_discipline}

Review method adapted from the latest gstack `/autoplan` workflow:
- Run review phases in order: CEO, Design when user-facing UI or UX is in scope, Eng, and DX when the repo is developer-facing or has a meaningful setup/API/operator experience.
- Use these decision principles: choose completeness over shortcuts; be willing to inspect broadly when needed; be pragmatic; avoid duplicate/redundant artifacts; prefer explicit contracts over clever prose; bias toward action when evidence is sufficient.
- Enforce the priority focus lens: specs and tasks should identify the next highest-leverage implementation focus areas. Move docs-only, audit-only, report-only, and artifact-only work behind executable source/test/UX/runtime work unless it directly unblocks that work.
- Enforce the production orchestration discipline: active production blockers, runtime/source-of-truth gaps, reusable UI/runtime contracts, and executable proofs must outrank evidence expansion unless the evidence names the exact implementation decision it unlocks.
- Classify important review decisions in the report as `Mechanical`, `Taste`, or `User Challenge`.
- Treat a `User Challenge` as any point where both the authoring pass and your independent review would recommend changing the user's stated direction. Do not silently auto-decide those; preserve the challenge explicitly in the generated docs or `{report_path}`.
- Treat author-vs-review disagreements that are not mechanical as `Taste` decisions, explain why you chose one direction, and amend generated docs only when repository evidence supports the change.

CEO review pass:
- Check whether the generated specs and plan preserve the right product/system direction, scope boundaries, non-goals, alternatives, and hidden assumptions.
- Ensure future-facing recommendations do not outrun evidence or dependency order.

Design review pass, when applicable:
- Check whether specs and plan tasks account for information architecture, user journeys, empty/loading/error/success states, accessibility, responsive behavior, and AI-slop risk.
- If the repo has no meaningful UI, say that in the report and skip UI-specific rewrites.

Eng review pass:
- Check architecture, data flow, dependency order, integration points, persistence/migrations, error handling, observability, performance risks, and test strategy.
- Verify exact current-state claims against files, commands, or code structure. Docs are claims, not truth.
- Ensure implementation tasks are dependency-ordered, small enough for one focused worker session where possible, and include explicit checkpoint tasks after risky clusters or every 2-3 priority tasks.

DX review pass, when applicable:
- Check first-run developer/operator experience, learn-by-doing path, error clarity, time-to-hello-world, honest examples, and uncertainty-reducing docs or tooling.
- If the repo is not developer-facing, say that in the report and skip DX-specific rewrites.

Generated spec validation:
- Every spec under `{output_dir}/specs/` must start with `# Specification:`.
- Every spec must include non-empty `## Objective`, `## Evidence Status`, `## Acceptance Criteria`, `## Verification`, and `## Open Questions`.
- Every spec must also include non-empty `## Source Of Truth`, `## Runtime Contract`, `## UI Contract`, `## Generated Artifacts`, `## Fixture Policy`, `## Retired / Superseded Surfaces`, and `## Review And Closeout`.
- `## Source Of Truth` must name runtime owners, UI consumers, generated artifacts, and retired/superseded surfaces.
- `## UI Contract` must prohibit production UI from duplicating runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth.
- `## Fixture Policy` must quarantine sample/demo/test data away from production components.
- `## Evidence Status` must separate verified code facts from recommendations, hypotheses, and unresolved questions.
- Acceptance criteria must be observable, testable outcomes, not vague capability prose.
- Specs must cite concrete files, commands, APIs, or primary-source documentation for exact current-state claims.

Generated implementation plan validation:
- `{output_dir}/IMPLEMENTATION_PLAN.md` must start with `# IMPLEMENTATION_PLAN`.
- It must include `## Priority Work`, `## Follow-On Work`, and `## Completed / Already Satisfied`.
- Every unfinished task must include `Spec:`, `Why now:`, `Codebase evidence:`, `Owns:`, `Integration touchpoints:`, `Scope boundary:`, `Acceptance criteria:`, `Verification:`, `Required tests:`, `Completion artifacts:`, `Dependencies:`, `Estimated scope:`, and `Completion signal:`.
- Every unfinished task must also include `Source of truth:`, `Runtime owner:`, `UI consumers:`, `Generated artifacts:`, `Fixture boundary:`, `Retired surfaces:`, `Contract generation:`, `Cross-surface tests:`, and `Review/closeout:`.
- Runtime-impacting tasks should implement runtime/API truth before UI consumers, regenerate contracts before consumer adaptation, and include an independent closeout proof that catches the original drift.
- Priority work should be code/test/UX/runtime-contract forward. Documentation, audit, report, or generated-artifact-only tasks need explicit `Why now:` evidence that they unblock implementation; otherwise demote them to follow-on work.
- Every `Spec:` reference must point to a spec file that exists under `{output_dir}/specs/`.
- `Dependencies:` is scheduler input, not prose. When editing `{output_dir}/IMPLEMENTATION_PLAN.md`, keep each `Dependencies:` field exactly `none` or only comma-separated/backticked task IDs such as ``Dependencies: `TASK-001`, `TASK-002` ``. Put readiness notes, completed-task context, and sequencing explanations in `Why now:`, `Codebase evidence:`, `Integration touchpoints:`, or `Review/closeout:` instead.
- Behavior-changing tasks should prefer a prove-it validation path: failing test or repro first, green proof, then broader regression check.
- Research or design tasks must name the closing artifact or decision and must not promise implementation details before the prerequisite evidence exists.

Validation expectations:
- Use lightweight local inspection commands as needed, such as `rg`, `ls`, and targeted file reads. Do not run long integration suites or production-affecting commands for this document review pass.
- After edits, re-check the generated docs' shape yourself before finishing.
- Write `{report_path}` with these sections: `# Codex Generation Review`, `## Summary`, `## Files Reviewed`, `## Changes Made`, `## Decision Audit Trail`, `## User Challenges`, `## Taste Decisions`, `## Validation`, and `## Remaining Risks`.
- If no generated-doc edits are needed, still write the report and explain what you checked.
"#,
        skill_boundary = CODEX_SKILL_BOUNDARY,
        command_label = mode.command_label(),
        mode_clause = mode_clause,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        output_dir = output_dir.display(),
        report_path = report_path.display(),
        priority_focus_lens = PRIORITY_FOCUS_LENS,
        production_orchestration_discipline = PRODUCTION_ORCHESTRATION_DISCIPLINE,
    )
}

pub(crate) fn build_spec_generation_prompt(
    mode: GenerationMode,
    repo_root: &Path,
    planning_root: &Path,
    output_dir: &Path,
    corpus: &PlanningCorpus,
    parallelism: usize,
    gbrain_context_path: Option<&Path>,
) -> String {
    let mode_clause = match mode {
        GenerationMode::Gen => {
            "This is a generation pass guided by the planning corpus. Use the corpus for intended future direction, but treat the live codebase as authoritative for every current-state fact, concrete filename, metric name, command, count, API shape, and behavior claim."
        }
        GenerationMode::Reverse => {
            "This is a reverse-engineering pass. The live codebase is the source of truth. Use the planning corpus only as supporting context."
        }
    };
    let spec_listing = corpus
        .spec_documents
        .iter()
        .map(|spec| format!("- `{}` — {}", spec.path, spec.title))
        .collect::<Vec<_>>()
        .join("\n");
    let plan_listing = corpus
        .primary_plans
        .iter()
        .map(|plan| format!("- `{}` — {}", plan.path, plan.title))
        .collect::<Vec<_>>()
        .join("\n");
    let idea_clause = corpus
        .idea_path
        .as_deref()
        .map(|path| {
            format!(
                "If `{path}` exists in the corpus snapshot, treat it as the office-hours-style seed brief for intended future direction. Preserve its product framing unless later corpus evidence or code reality clearly overrides it."
            )
        })
        .unwrap_or_else(|| "No IDEA.md seed is present for this corpus.".to_string());
    let focus_clause = corpus
        .focus_path
        .as_deref()
        .map(|path| {
            format!(
                "If `{path}` exists in the corpus snapshot, treat it as operator steering for what deserved extra attention in the planning pass. Preserve the full-system view, but use the focus brief to understand why certain priorities may have been ranked ahead of equally plausible alternatives."
            )
        })
        .unwrap_or_else(|| "No FOCUS.md steering brief is present for this corpus.".to_string());
    let gbrain_context_clause = gbrain_context_clause(repo_root, gbrain_context_path);
    format!(
        r#"You are generating a new spec snapshot for `{repo_root}`.

{mode_clause}

Write all generated specs under `{output_dir}/specs/`. Do not print the specs to stdout.
Use `{planning_root}` as supporting planning context for this generation pass.

Use up to {parallelism} parallel subagents where helpful.

Existing corpus spec documents:
{spec_listing}

Existing corpus plans:
{plan_listing}

Idea-seed context:
{idea_clause}

Focus context:
{focus_clause}

{gbrain_context_clause}

{priority_focus_lens}

{production_orchestration_discipline}

Required output contract:
- Write one markdown file per generated spec into `{output_dir}/specs/`
- Filenames must use `ddmmyy-topic-slug.md`
- Each file must start with `# Specification: ...`
- Each file must include `## Objective`
- Each file must include `## Source Of Truth`
- Each file must include `## Evidence Status`
- Each file must include `## Runtime Contract`
- Each file must include `## UI Contract`
- Each file must include `## Generated Artifacts`
- Each file must include `## Fixture Policy`
- Each file must include `## Retired / Superseded Surfaces`
- Each file must include a `## Acceptance Criteria` section
- Each file must include a `## Verification` section
- Each file must include `## Review And Closeout`
- Each file must include `## Open Questions`
- `## Source Of Truth` must name runtime owner modules/APIs, UI consumers, generated artifacts, and retired/superseded surfaces; use `none` only after checking
- Acceptance criteria must be concrete, testable, and phrased as truthful observable outcomes
- Acceptance criteria should use flat bullet points, not prose paragraphs
- Specs must be concrete, file-grounded, and implementation-oriented
- Avoid placeholders and abstract framework prose
- Surface important assumptions or spec/code conflicts explicitly instead of smoothing them over
- Include commands, boundaries, or open questions when they materially affect implementation or verification
- `## Runtime Contract` must say which engine/runtime/API owns canonical facts and what must fail closed when that data is absent
- `## UI Contract` must say how UI or presentation consumers avoid duplicating runtime constants, catalogs, eligibility rules, risk classifications, settlement math, or sample fallback truth
- `## Generated Artifacts` must name bindings, schemas, docs, snapshots, or generation commands to refresh; write `none` only when there are no generated contracts
- `## Fixture Policy` must quarantine fixture/demo/sample data to test-only or dev-only surfaces and say what production code must not import
- `## Retired / Superseded Surfaces` must name stale specs/files/contracts to delete, archive, or tombstone, or `none`
- Every exact current-state fact should be backed by a file path, command, or primary-source documentation citation in `## Evidence Status`
- `## Evidence Status` must separate:
  - verified facts grounded in code or primary-source documentation
  - recommendations for the intended system
  - hypotheses / unresolved questions
- `## Review And Closeout` must explain how a reviewer independently proves each original requirement was satisfied, including grep/assertion proof when normal tests would not catch the drift
- Treat the live codebase as authoritative for current-state facts in every mode
- Any exact version, timeout, threshold, dependency tag, benchmark target, chain choice, or protocol step that is not verified must be labeled as a recommendation or hypothesis instead of stated as settled fact
- If a spec describes a future phase or unresolved surface, keep it at research/design level and avoid implementation detail that the evidence does not yet support
- If the repo is developer-facing, capture onboarding, error handling, and first-success expectations truthfully enough that a future worker can improve the DX without guessing
- Preserve proven current behavior in reverse mode
- In gen mode, preserve intended future direction from the corpus, but keep future intent under recommendations or hypotheses until code or primary-source evidence proves otherwise
- Generate specs for the highest-priority product/system focus areas first. Do not multiply specs for docs, audits, reports, or artifacts unless they directly unblock an executable implementation slice.
- In live partially complete repos, generate specs that tighten the next executable implementation queue. Do not spend the spec pass inventing new evidence-only tracks while runtime, user/operator workflow, or reusable UI/runtime contract blockers remain.
- If the corpus recommends `auto design` or the repo has meaningful UI/TUI/frontend gaps, preserve that as a reusable runtime-backed design contract. Do not reduce it to one-off screen polish, screenshot acceptance, or fixture-specific presentation work.

Cover the main product and system surfaces represented in the repo. Use the codebase and the planning corpus to decide the right spec set."#,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        mode_clause = mode_clause,
        output_dir = output_dir.display(),
        parallelism = parallelism.max(1),
        spec_listing = if spec_listing.is_empty() {
            "- none".to_string()
        } else {
            spec_listing
        },
        idea_clause = idea_clause,
        focus_clause = focus_clause,
        plan_listing = if plan_listing.is_empty() {
            "- none".to_string()
        } else {
            plan_listing
        },
        priority_focus_lens = PRIORITY_FOCUS_LENS,
        production_orchestration_discipline = PRODUCTION_ORCHESTRATION_DISCIPLINE,
    )
}

pub(crate) fn build_implementation_plan_prompt(
    mode: GenerationMode,
    repo_root: &Path,
    output_dir: &Path,
    generated_specs: &[GeneratedSpecDocument],
    parallelism: usize,
    gbrain_context_path: Option<&Path>,
) -> String {
    let mode_clause = match mode {
        GenerationMode::Gen => {
            "This is a planning pass grounded in the generated specs plus current code review. Use the specs to preserve intended direction, but treat the live codebase as authoritative for current-state facts, repo shape, counts, commands, metric names, and existing coverage."
        }
        GenerationMode::Reverse => {
            "This is a reverse-engineering planning pass. Use the generated specs and current code reality to identify the next actionable work."
        }
    };
    let spec_listing = generated_specs
        .iter()
        .map(|path| {
            format!(
                "- `{}`",
                path.path
                    .strip_prefix(output_dir)
                    .unwrap_or(&path.path)
                    .display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let gbrain_context_clause = gbrain_context_clause(repo_root, gbrain_context_path);
    format!(
        r#"You are writing `{output_dir}/IMPLEMENTATION_PLAN.md` for `{repo_root}`.

{mode_clause}

Use up to {parallelism} parallel subagents where helpful.

Generated specs for this run:
{spec_listing}

{gbrain_context_clause}

{priority_focus_lens}

{production_orchestration_discipline}

Before writing the plan, do the real planning work:
- operate in read-only planning mode first
- map dependency order and existing code patterns
- identify the highest-risk unknowns
- rank candidate tasks through the priority focus lens before assigning them to `## Priority Work`
- prefer vertical slices over horizontal layer dumps
- keep tasks small enough for one focused worker session
- do not hide ambiguity; encode real blockers and assumptions in the task contracts
- if the repo is developer-facing, explicitly consider zero-friction onboarding, learn-by-doing examples, error clarity, and uncertainty-reducing docs or tooling as first-class planning concerns
- treat spec statements labeled as hypotheses, recommendations, design-phase, or research-required as non-binding until the plan closes the corresponding decision gate
- do not create implementation tasks whose contract depends on unverified future-phase details; write a research, validation, or decision task first
- verify every exact current-state fact in the plan from code, tests, or concrete commands before you write it down
- add explicit checkpoint tasks after each risky cluster or every 2-3 priority tasks so a future worker knows when to stop and re-evaluate before widening scope
- In `## Priority Work`, prefer tasks that change source code, tests, runtime contracts, user/operator flows, or executable verification. Put docs-only, audit-only, report-only, and artifact-only tasks in `## Follow-On Work` unless they directly unblock implementation or correct stale guidance that would otherwise make workers implement the wrong thing.
- Do not turn the priority queue into a documentation or audit campaign for a repo that is not production-ready yet. Make the next `auto parallel` run land the highest-leverage executable improvements.
- Substance bar (HARD): every `## Priority Work` task must add or change runtime/product behavior a user or operator can directly observe, and state that observable change in `Why now:`. Rows whose only product is a guard script, receipt, evidence rollup, CI check, audit, snapshot, stamp-flip, or a verifier of a prior task are capped at ONE total across the entire plan, and only when that one unblocks a named product task; any others go in `## Follow-On Work` or are dropped. Never emit a task whose spec or title duplicates a prior or already-completed row; re-promoting a row byte-for-byte is forbidden -- if real work remains, write a new row naming the specific remaining delta. Do not tag rows with lens numbers or `Trace: lens 1/2/3` / CEO-Design-Eng provenance labels.
- For active repos, preserve useful existing queue intent but rewrite ordering and task contracts so the next `auto parallel` run can select production blockers first. Evidence, receipts, checkpoints, and report generation may appear only when they name the exact implementation decision they unlock.
- Encode the immediate autodev lever in the first priority tasks: `auto design` for reusable UI/runtime contract gaps, `auto gen` only when root doctrine or queue shape must be regenerated, `auto parallel` when dependency-ready implementation can start, and active-run supervision when a running executor is already attacking the top blockers.

Output requirements:
- Write exactly one file: `{output_dir}/IMPLEMENTATION_PLAN.md`
- The first non-empty line must be exactly `{IMPLEMENTATION_PLAN_HEADER}`
- Use these top-level sections:
  - `## Priority Work`
  - `## Follow-On Work`
  - `## Completed / Already Satisfied`
- Every unfinished task in `## Priority Work` and `## Follow-On Work` must use this exact header format:
  - `- [ ] `TASK-ID` Short title`
- Each task field below must appear on its own line, indented under the task header, with the field name flush against the start (no `- ` bullet prefix on field lines). Example shape:
  ```
  - [ ] `TASK-001` Short title

    Spec: `specs/...md`
    Why now: ...
    Estimated scope: S
  ```
- Every unfinished task in `## Priority Work` and `## Follow-On Work` must include these CORE fields, even when it is deferred, gated, research-shaped, or lower priority:
  - `Spec:`
  - `Why now:`
  - `Codebase evidence:`
  - `Owns:`
  - `Acceptance criteria:`
  - `Verification:`
  - `Required tests:` (name AT MOST 5 concrete test names or commands; if a task needs more than five, split it into separate tasks)
  - `Completion artifacts:`
  - `Dependencies:`
  - `Estimated scope:`
  - `Completion signal:`
- Include these CONDITIONAL fields ONLY when the task actually touches that surface; omit the line entirely otherwise. Never invent a value to satisfy a contract -- an omitted line is correct when the surface is untouched, and stamping `none` on every row is exactly the ceremony to avoid:
  - `Source of truth:` / `Runtime owner:` -- only when the task changes runtime/engine/API-owned facts.
  - `UI consumers:` / `Cross-surface tests:` -- only when the task changes a UI/presentation surface that reads runtime facts.
  - `Generated artifacts:` / `Contract generation:` -- only when the task changes a codegen/bindings/schema shape.
  - `Fixture boundary:` -- only when the task touches fixtures/demo/sample data.
  - `Retired surfaces:` -- only when the task deletes or supersedes an existing surface.
  - `Integration touchpoints:` / `Scope boundary:` -- only when the task spans multiple crates/services or needs an explicit non-goal.
  - `Review/closeout:` -- only when the task needs a closeout proof beyond its `Verification:` command.
- `## Follow-On Work` is not a shorthand backlog. If you list a follow-on item with `- [ ]`, give it the same full task contract as priority work. Do not create compact follow-on rows with only `Spec:`, `Why now:`, and `Dependencies:`.
- `Spec:` values must point to `specs/*.md`
- Every `Spec:` reference must exactly match one of the generated spec paths listed for this run; do not invent alternate dates or filenames
- Keep the plan concrete, file-grounded, and executable
- `Source of truth:` must name the canonical runtime/API/spec/doc owner for facts changed by the task
- `Runtime owner:` must name the engine/runtime path or `none`
- `UI consumers:` must name concrete UI/presentation paths/routes or `none`
- `Generated artifacts:` must name bindings, schemas, docs, snapshots, or `none`; this is proof metadata, not a reason to create artifact-only work
- `Fixture boundary:` must state production cannot import fixture/demo/sample data, or explain why not applicable
- `Retired surfaces:` must name stale specs/files/contracts to delete/archive/tombstone, or `none`
- `Owns:` must name concrete path-like owners such as `crates/foo/src/lib.rs`, `crates/foo/`, `docker-compose.yml`, `docs`, or a root crate/directory; do not put shell commands, broad prose, `missing`, `TBD`, or `unspecified` there. Tasks whose only output is a git ref (annotated tag, branch) MUST write the ref path directly, e.g. `Owns: refs/tags/v0.2.0` or `Owns: refs/heads/release/0.3` — prose like `git tags only` is rejected
- `Integration touchpoints:` should name concrete adjacent modules, route prefixes, commands, or config files; if none exist, write `none`
- Do not include lane prose, staffing prose, or meta commentary
- Keep tasks dependency-ordered and bounded; if a task feels bigger than one focused implementation session, break it down again
- `Why now:` must explain the task's blended product/design/engineering priority, not just that an artifact is missing
- Any prerequisite, expansion gate, or "after P-..." constraint mentioned in prose must also be encoded in the task's `Dependencies:` field; never rely on prose-only gates
- Front-load risk where practical, but never at the cost of violating dependency order
- `Acceptance criteria:` must be specific, testable, and truthful
- `Verification:` must name the concrete commands or runtime checks a worker should run
- For behavior-changing tasks, `Verification:` should prefer a prove-it path: failing test or repro first, then green proof, then broader regression checks
- `Estimated scope:` for every unfinished task must be exactly `XS`, `S`, or `M`
- Do not emit `Estimated scope: L`; if the underlying spec implies larger work, decompose it into dependency-ordered child tasks yourself
- Do not write `decomposition required`, `split before implementation`, or similar placeholders; the generated plan is responsible for doing that decomposition now
- `Required tests:` must list concrete test names or an explicit `none` for docs-only tasks; never write `See spec`, `TBD`, or a broad module name
- No unfinished task may list more than five required tests; split the task if it needs more
- `Contract generation:` must name the generation/check command for affected generated artifacts, or `none -- no generated contract`
- `Cross-surface tests:` must name a runtime-output-to-UI/readback proof when UI is affected, or `none -- no UI/runtime boundary`
- `Review/closeout:` must describe independent proof for the original requirement. It cannot be only `cargo check`; include test, grep/assertion, artifact, or reviewer checklist proof that would catch the original drift returning
- `Completion artifacts:` must list concrete repo-relative evidence files or directories that must exist before the task can truthfully become done; write `none` only when the task has no durable artifact beyond code/tests/review handoff. This field records proof for the implementation slice, not permission to create reports as the slice.
- `Dependencies:` is scheduler input, not prose. It must be exactly `none` or only comma-separated/backticked task IDs such as ``Dependencies: `TASK-001`, `TASK-002` ``. Do not include completed-task adjectives, path names, readiness notes, "parallel", "after", "blocked by", "depends on", "coordinate with", parentheticals, semicolons, or explanatory text in this field. Put readiness, existing crate/path, and coordination context in `Why now:`, `Codebase evidence:`, `Integration touchpoints:`, or `Review/closeout:` instead.
- `Verification:` must stay narrow: prefer exact test-name filters and affected-crate checks; do not use `cargo check --workspace`, `cargo test --workspace`, `cargo test --all`, or equivalent broad workspace sweeps as the primary item verification
- Every `cargo test` verification command must include a concrete test-name/filter token after package or target flags; reject package-wide commands such as `cargo test -p crate`, `cargo test -p crate --lib`, or `cargo test -p crate --test integration_file`
- Put only unfinished work in the unchecked queue sections
- Put already-satisfied items only in `## Completed / Already Satisfied`
- Future-phase work with unresolved feasibility must stay in research-shaped tasks until the prerequisite evidence exists

The goal is a truthful, execution-ready implementation queue."#,
        repo_root = repo_root.display(),
        output_dir = output_dir.display(),
        IMPLEMENTATION_PLAN_HEADER = IMPLEMENTATION_PLAN_HEADER,
        mode_clause = mode_clause,
        parallelism = parallelism.max(1),
        spec_listing = if spec_listing.is_empty() {
            "- none".to_string()
        } else {
            spec_listing
        },
        gbrain_context_clause = gbrain_context_clause,
        priority_focus_lens = PRIORITY_FOCUS_LENS,
        production_orchestration_discipline = PRODUCTION_ORCHESTRATION_DISCIPLINE,
    )
}

fn gbrain_context_clause(repo_root: &Path, gbrain_context_path: Option<&Path>) -> String {
    let Some(path) = gbrain_context_path else {
        return String::new();
    };
    let display_path = path
        .strip_prefix(repo_root)
        .unwrap_or(path)
        .display()
        .to_string();
    format!(
        r#"Shared gbrain context:
- Auto-collected file: `{display_path}`
- Read it as durable operator and project memory before ranking priorities.
- Treat it as advisory memory only; verify current code facts against the live checkout.
- Use it to avoid recreating stale branch-memory work and to preserve recent strategic decisions.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_corpus_codex_review_prompt, build_corpus_prompt,
        build_generation_codex_review_prompt, build_implementation_plan_prompt,
    };
    use crate::generation::tests::generated_spec;
    use crate::generation::{ActivePlanSurface, CorpusPromptInputs, GenerationMode};
    use std::path::{Path, PathBuf};

    #[test]
    fn corpus_prompt_requires_assumption_validation_and_checkpoint_plans() {
        let prompt = build_corpus_prompt(
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            CorpusPromptInputs {
                previous_planning_snapshot: None,
                parallelism: 4,
                idea: Some("build a thing"),
                focus: None,
                reference_repos: &[],
                active_plan_surface: &ActivePlanSurface::default(),
                gbrain_context_path: None,
            },
        );

        assert!(prompt.contains("key assumptions to validate next"));
        assert!(prompt.contains("alternatives considered"));
        assert!(prompt.contains("explicit checkpoint or decision-gate plan only when"));
        assert!(prompt.contains("prefer `AGENTS.md`"));
        assert!(prompt.contains("must be a compact priority plan"));
        assert!(prompt.contains("## Priority Decision"));
        assert!(prompt.contains("## User / Operator Outcome"));
        assert!(prompt.contains("## Implementation Slice"));
        assert!(prompt.contains("literal lines or labels containing `Goal`, `Files`, and `Tests`"));
        assert!(prompt.contains("commands in `## Verification` do not satisfy the slice contract"));
        assert!(prompt.contains("`Plan dispatch`, `Dispatch index`, or `The master plan delivers`"));
        assert!(prompt.contains("Do not use the short `## Objective`"));
        assert!(prompt.contains("current gstack `/autoplan` review discipline"));
        assert!(prompt.contains("Priority focus lens"));
        assert!(prompt.contains("Production orchestration discipline"));
        assert!(prompt.contains("live production playground"));
        assert!(prompt.contains("Next Autodev Lever"));
        assert!(prompt.contains("Delete Or Demote"));
        assert!(prompt.contains("whether the operator should run `auto design`"));
        assert!(prompt
            .contains("product leverage, user-visible clarity, and engineering feasibility/tests"));
        assert!(prompt.contains("Never emit the absolute repository-root path"));
        assert!(prompt.contains("cd \"$(git rev-parse --show-toplevel)\""));
        assert!(prompt.contains(
            "Classify important planning decisions as `Mechanical`, `Taste`, or `User Challenge`"
        ));
        assert!(prompt.contains("concise decision audit trail"));
    }

    #[test]
    fn corpus_prompt_can_require_focus_brief_without_losing_repo_wide_sweep() {
        let prompt = build_corpus_prompt(
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            CorpusPromptInputs {
                previous_planning_snapshot: None,
                parallelism: 4,
                idea: None,
                focus: Some("wire reconnects, TLS failures, session-token handling"),
                reference_repos: &[],
                active_plan_surface: &ActivePlanSurface::default(),
                gbrain_context_path: None,
            },
        );

        assert!(prompt.contains("`genesis/FOCUS.md`"));
        assert!(prompt.contains("Still perform a wide repo sweep"));
        assert!(prompt.contains("attention and prioritization signal"));
    }

    #[test]
    fn codex_review_prompts_encode_autoplan_boundary_and_edit_scope() {
        let corpus_prompt = build_corpus_codex_review_prompt(
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            std::path::Path::new("/tmp/repo/.auto/logs/corpus-report.md"),
            &[],
            &ActivePlanSurface::default(),
        );

        assert!(corpus_prompt.contains("GPT-5.5 xhigh Codex outside-voice review"));
        assert!(corpus_prompt.contains("Do NOT read or execute any SKILL.md files"));
        assert!(
            corpus_prompt.contains("You may edit only markdown files under `/tmp/repo/genesis`")
        );
        assert!(corpus_prompt.contains("Priority focus lens"));
        assert!(corpus_prompt.contains("production orchestration discipline"));
        assert!(corpus_prompt.contains("`## Next Autodev Lever`"));
        assert!(corpus_prompt.contains("`## Delete Or Demote`"));
        assert!(corpus_prompt.contains("`auto design` should run before generation"));
        assert!(corpus_prompt.contains("`Mechanical`, `Taste`, or `User Challenge`"));
        assert!(corpus_prompt.contains(
            "Every numbered plan under `/tmp/repo/genesis/plans/` must use the compact priority-plan shape"
        ));
        assert!(corpus_prompt
            .contains("literal lines or labels containing `Goal`, `Files`, and `Tests`"));
        assert!(corpus_prompt
            .contains("commands in `## Verification` do not satisfy the slice contract"));
        assert!(corpus_prompt
            .contains("`Plan dispatch`, `Dispatch index`, or `The master plan delivers`"));
        assert!(corpus_prompt.contains("Reject or rewrite any absolute repo-root path"));
        assert!(corpus_prompt.contains("cd \"$(git rev-parse --show-toplevel)\""));
        assert!(corpus_prompt.contains("# Codex Corpus Review"));

        let generation_prompt = build_generation_codex_review_prompt(
            GenerationMode::Gen,
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            std::path::Path::new("/tmp/repo/gen-010203"),
            std::path::Path::new("/tmp/repo/.auto/logs/gen-report.md"),
        );

        assert!(generation_prompt.contains("outside-voice review step for `auto gen`"));
        assert!(generation_prompt.contains("Do NOT read or execute any SKILL.md files"));
        assert!(generation_prompt.contains("You may edit only `/tmp/repo/gen-010203/specs/*.md`"));
        assert!(generation_prompt
            .contains("The generator will sync reviewed outputs to the root after your pass"));
        assert!(generation_prompt.contains("Priority focus lens"));
        assert!(generation_prompt.contains("active production blockers"));
        assert!(generation_prompt.contains("Move docs-only, audit-only, report-only"));
        assert!(generation_prompt.contains("`auto design`"));
        assert!(generation_prompt.contains("continuing or supervising an existing run"));
        assert!(generation_prompt.contains("`Dependencies:` is scheduler input, not prose"));
        assert!(generation_prompt
            .contains("Put readiness notes, completed-task context, and sequencing explanations"));
        assert!(generation_prompt.contains("# Codex Generation Review"));
    }

    #[test]
    fn corpus_prompt_reconciles_to_existing_active_root_plans() {
        let prompt = build_corpus_prompt(
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            CorpusPromptInputs {
                previous_planning_snapshot: None,
                parallelism: 4,
                idea: None,
                focus: None,
                reference_repos: &[PathBuf::from("/tmp/bitino")],
                active_plan_surface: &ActivePlanSurface {
                    root_plan_standard_path: Some("PLANS.md".to_string()),
                    active_plan_paths: vec!["plans/001-master-plan.md".to_string()],
                },
                gbrain_context_path: None,
            },
        );

        assert!(prompt.contains("Existing root planning surfaces"));
        assert!(prompt.contains("Do not create a second active master plan"));
        assert!(prompt.contains("repo-root instruction files such as `AGENTS.md`"));
        assert!(prompt.contains("Reference repositories to inspect as required input"));
        assert!(prompt.contains("Mandatory reference repo: `/tmp/bitino`"));
    }

    #[test]
    fn corpus_prompt_with_only_root_standard_does_not_force_root_control_plane() {
        let prompt = build_corpus_prompt(
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            CorpusPromptInputs {
                previous_planning_snapshot: None,
                parallelism: 4,
                idea: None,
                focus: None,
                reference_repos: &[],
                active_plan_surface: &ActivePlanSurface {
                    root_plan_standard_path: Some("PLANS.md".to_string()),
                    active_plan_paths: vec![],
                },
                gbrain_context_path: None,
            },
        );

        assert!(prompt.contains("Root ExecPlan standard: `PLANS.md`"));
        assert!(prompt.contains("Do not infer from `PLANS.md` alone"));
        assert!(prompt.contains(
            "determine whether a different planning root such as `genesis` is explicitly designated as active"
        ));
    }

    #[test]
    fn codex_review_prompt_inherits_reference_repos_and_active_plan_surface() {
        let corpus_prompt = build_corpus_codex_review_prompt(
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            std::path::Path::new("/tmp/repo/.auto/logs/corpus-report.md"),
            &[PathBuf::from("/tmp/bitino")],
            &ActivePlanSurface {
                root_plan_standard_path: Some("PLANS.md".to_string()),
                active_plan_paths: vec!["plans/001-master-plan.md".to_string()],
            },
        );

        assert!(corpus_prompt.contains("Reference repo available to inspect"));
        assert!(corpus_prompt.contains("before calling cross-repo work ungrounded"));
        assert!(corpus_prompt.contains("must reconcile to these surfaces explicitly"));
        assert!(corpus_prompt.contains("second active master plan"));
    }

    #[test]
    fn implementation_plan_prompt_requires_checkpoint_tasks_and_prove_it_verification() {
        let prompt = build_implementation_plan_prompt(
            GenerationMode::Gen,
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/gen-123"),
            &[generated_spec(
                "workspace-build-system",
                "# Specification: Workspace Build System\n",
            )],
            4,
            None,
        );

        assert!(prompt.contains("checkpoint tasks"));
        assert!(prompt.contains("failing test or repro first"));
        assert!(prompt.contains("generated spec paths listed for this run"));
        assert!(prompt.contains("verify every exact current-state fact"));
        assert!(prompt.contains("must be exactly `XS`, `S`, or `M`"));
        assert!(prompt.contains("decompose it into dependency-ordered child tasks yourself"));
        assert!(prompt.contains("No unfinished task may list more than five required tests"));
        assert!(prompt.contains("must include a concrete test-name/filter token"));
        assert!(prompt.contains("must name concrete path-like owners"));
        assert!(prompt.contains("must also be encoded in the task's `Dependencies:` field"));
        assert!(prompt.contains("rank candidate tasks through the priority focus lens"));
        assert!(prompt.contains("Production orchestration discipline"));
        assert!(prompt.contains("Evidence, receipts, checkpoints, and report generation"));
        assert!(prompt.contains(
            "Put docs-only, audit-only, report-only, and artifact-only tasks in `## Follow-On Work`"
        ));
        assert!(prompt.contains("Generated artifacts:` must name bindings, schemas, docs, snapshots, or `none`; this is proof metadata"));
    }

    #[test]
    fn implementation_plan_prompt_requires_full_follow_on_task_contracts() {
        let prompt = build_implementation_plan_prompt(
            GenerationMode::Gen,
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/gen-123"),
            &[generated_spec(
                "workspace-build-system",
                "# Specification: Workspace Build System\n",
            )],
            4,
            None,
        );

        assert!(
            prompt.contains("Every unfinished task in `## Priority Work` and `## Follow-On Work`")
        );
        assert!(
            prompt.contains("even when it is deferred, gated, research-shaped, or lower priority")
        );
        assert!(prompt.contains("`## Follow-On Work` is not a shorthand backlog"));
        assert!(prompt.contains("Do not create compact follow-on rows"));
    }

    #[test]
    fn generation_prompt_makes_code_authoritative_for_current_state_facts() {
        let prompt = build_implementation_plan_prompt(
            GenerationMode::Gen,
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/gen-123"),
            &[generated_spec(
                "workspace-build-system",
                "# Specification: Workspace Build System\n",
            )],
            4,
            None,
        );

        assert!(prompt.contains("authoritative for current-state facts"));
        assert!(prompt.contains("metric names"));
        assert!(prompt.contains("do not invent alternate dates or filenames"));
    }

    #[test]
    fn gbrain_context_path_is_referenced_as_advisory_shared_memory() {
        let prompt = build_corpus_prompt(
            Path::new("/tmp/repo"),
            Path::new("/tmp/repo/genesis"),
            CorpusPromptInputs {
                previous_planning_snapshot: None,
                parallelism: 4,
                idea: None,
                focus: None,
                reference_repos: &[],
                active_plan_surface: &ActivePlanSurface::default(),
                gbrain_context_path: Some(Path::new("/tmp/repo/genesis/GBRAIN-CONTEXT.md")),
            },
        );

        assert!(prompt.contains("Shared gbrain context"));
        assert!(prompt.contains("`genesis/GBRAIN-CONTEXT.md`"));
        assert!(prompt.contains("advisory memory only"));

        let plan_prompt = build_implementation_plan_prompt(
            GenerationMode::Gen,
            Path::new("/tmp/repo"),
            Path::new("/tmp/repo/gen-123"),
            &[generated_spec(
                "workspace-build-system",
                "# Specification: Workspace Build System\n",
            )],
            4,
            Some(Path::new("/tmp/repo/gen-123/GBRAIN-CONTEXT.md")),
        );

        assert!(plan_prompt.contains("`gen-123/GBRAIN-CONTEXT.md`"));
        assert!(plan_prompt.contains("durable operator and project memory"));
    }
}
