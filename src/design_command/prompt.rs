use std::path::Path;

use crate::prompt_ethos::PRODUCTION_ORCHESTRATION_DISCIPLINE;

pub(crate) const DESIGN_ARTIFACTS: [&str; 6] = [
    "DESIGN-AUDIT.md",
    "DESIGN-SYSTEM-PROPOSAL.md",
    "ENGINE-UI-CONTRACT.md",
    "FRONTEND-QA.md",
    "DESIGN-PLAN-ITEMS.md",
    "DESIGN-REPORT.md",
];

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum DesignRunKind {
    Standalone,
    Resolve,
    Super,
    SuperResolve,
}

pub(crate) fn build_design_parallel_prompt(output_root: &Path, pass: usize) -> String {
    format!(
        r#"You are an `auto design --resolve` implementation worker.

Read repo-local instructions first, then read `IMPLEMENTATION_PLAN.md` and the latest design artifacts for this pass:
- Design output root: `{output_root}`
- Current design pass: `pass-{pass:02}`
- Required context: `{output_root}/pass-{pass:02}/DESIGN-REPORT.md`, `{output_root}/pass-{pass:02}/DESIGN-PLAN-ITEMS.md`, and `{output_root}/pass-{pass:02}/ENGINE-UI-CONTRACT.md` when present.

{production_orchestration_discipline}

Worker selection rules:
- Prefer dependency-ready `DESIGN-*` tasks and design/runtime/UI tasks promoted by `auto design`.
- If multiple tasks are available, pick the one that repairs a shared runtime-backed component, interaction contract, source-of-truth drift, or user-facing blocker before isolated polish.
- Do not choose docs-only, report-only, screenshot-only, evidence-only, or artifact-only work unless the assigned task explicitly proves it unlocks source/runtime/UI implementation.
- Implement exactly one bounded task with code, tests, and narrow verification. Do not edit `DESIGN-AUDIT.md`, `DESIGN-SYSTEM-PROPOSAL.md`, `ENGINE-UI-CONTRACT.md`, `FRONTEND-QA.md`, `DESIGN-PLAN-ITEMS.md`, or `DESIGN-REPORT.md` from the lane unless the assigned task explicitly owns that artifact update.
- Design implementation should land reusable components/contracts and runtime-backed presentation behavior, not one-off presentations for a single screen or game.
"#,
        output_root = output_root.display(),
        pass = pass,
        production_orchestration_discipline = PRODUCTION_ORCHESTRATION_DISCIPLINE,
    )
}

pub(crate) fn build_design_prompt(
    repo_root: &Path,
    planning_root: Option<&Path>,
    output_dir: &Path,
    operator_prompt: Option<&str>,
    apply: bool,
    skip_qa: bool,
    kind: DesignRunKind,
) -> String {
    let planning_clause = planning_root
        .map(|path| {
            format!(
                "- Planning corpus root: `{}`. If present, treat its `DESIGN.md` as planning input, not automatically as live product truth.",
                path.display()
            )
        })
        .unwrap_or_else(|| "- Planning corpus root: none detected.".to_string());
    let prompt_clause = operator_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\nOperator focus:\n{value}\n"))
        .unwrap_or_default();
    let edit_clause = if apply {
        match kind {
            DesignRunKind::Standalone => {
                "- You may make bounded edits to root `DESIGN.md`, design-relevant `specs/*.md`, and `IMPLEMENTATION_PLAN.md` when they are necessary to encode design/runtime truth. Do not edit application source code."
            }
            DesignRunKind::Resolve => {
                "- You may make bounded edits to root `DESIGN.md`, design-relevant `specs/*.md`, and `IMPLEMENTATION_PLAN.md` when they are necessary to encode design/runtime truth. Do not edit application source code in this design pass; unresolved source/runtime work must become executable implementation-plan tasks for the following `auto parallel` pass."
            }
            DesignRunKind::Super => {
                "- You may amend root `DESIGN.md` and the planning corpus design files so `auto gen` inherits the design contract. Do not edit source code, root specs, or root `IMPLEMENTATION_PLAN.md` in this pre-generation super module."
            }
            DesignRunKind::SuperResolve => {
                "- You may make bounded edits to root `DESIGN.md`, design-relevant `specs/*.md`, planning corpus design files, and `IMPLEMENTATION_PLAN.md` when needed to encode design/runtime truth. Do not edit application source code in this design pass; unresolved source/runtime work must become executable implementation-plan tasks for the following `auto parallel` pass."
            }
        }
    } else {
        "- Report-only mode: do not edit repo files outside the output directory. Put proposed patches and plan items in the artifacts."
    };
    let qa_clause = if skip_qa {
        "- Browser/runtime QA is explicitly skipped. Still inspect code-level UI/runtime contracts and list the skipped QA as a blocker where it matters."
    } else {
        "- Run the narrowest truthful frontend QA available: existing browser/Playwright/gstack/agent-browser tooling, local dev server smoke, route/API probes, console-error checks, and responsive checks. If no app can run, record the exact blocker and still audit static frontend/runtime bindings."
    };
    let stage_clause = match kind {
        DesignRunKind::Standalone => "You are running standalone `auto design`.",
        DesignRunKind::Resolve => {
            "You are running `auto design --resolve`: diagnose design/runtime drift, encode durable doctrine and queue-ready implementation tasks, then let implementation lanes repair source code before you re-verify."
        }
        DesignRunKind::Super => {
            "You are the `auto super` design perfection gate running after corpus and before generation. Design is first-class and blocking: do not subordinate, soften, or defer design/runtime integrity findings into a later generic review."
        }
        DesignRunKind::SuperResolve => {
            "You are the `auto super` design repair gate running after corpus and before generation. Design is first-class and blocking: diagnose design/runtime drift, encode executable repair work, let implementation lanes fix it, and only allow the CEO production campaign to continue after `Verdict: GO`."
        }
    };

    let apply_instructions = if apply {
        r#"If applying edits is enabled:
- Update `DESIGN.md` only with durable doctrine grounded in the live product and existing frontend.
- In standalone mode, add or amend plan/spec items only for real unresolved work. In super mode, prefer amending the planning corpus so `auto gen` emits the queue unless this is a resolve pass.
- In resolve mode, every unresolved NO-GO issue that requires source/runtime/UI changes must also be inserted into root `IMPLEMENTATION_PLAN.md` as an unchecked, dependency-ready task unless it has a concrete dependency. Use stable `DESIGN-*` task IDs, machine-readable `Dependencies:`, narrow `Owns:`, runtime owner, UI consumer, generated artifact, fixture boundary, and executable verification fields so `auto parallel` can pick it up immediately.
- In resolve mode, do not leave the only actionable repair work inside `DESIGN-PLAN-ITEMS.md`; that file is an audit artifact, while `IMPLEMENTATION_PLAN.md` is the executor queue.
- Do not mark any implementation item complete."#
    } else {
        r#"If report-only mode is enabled:
- Do not edit root `DESIGN.md`, specs, root implementation plans, frontend/source files, or generated bindings.
- Put all proposed doctrine changes, contract changes, and queue-ready implementation items in the output artifacts.
- Make `DESIGN-REPORT.md` name the exact next command or promotion path needed before implementation."#
    };

    format!(
        r#"{stage_clause}

Repository: `{repo_root}`
{planning_clause}
Output directory: `{output_dir}`
{prompt_clause}

Your job is to synthesize expert design review, design-system consultation, web interface guidelines, frontend design craft, and QA into a repo-native design contract. This is not a fake mockup generator. This is a design/runtime integrity pass that must be perfected before broader functional lanes proceed.

{production_orchestration_discipline}

Use these lenses together:
- Plan design review: rate and close gaps in information architecture, interaction states, journey, AI-slop risk, design-system alignment, responsive behavior, accessibility, and unresolved design decisions.
- Design consultation: infer or improve a coherent product-specific system: aesthetic direction, safe category conventions, deliberate creative risks, typography, color, spacing, layout, motion, and component vocabulary.
- Web interface guidelines: fetch or recall current web UI/a11y best practices and apply them to actual frontend files, not generic screenshots.
- Frontend design craft: avoid generic AI aesthetics, overused fonts, purple-gradient defaults, meaningless cards, generic dashboard widgets, and product-copy fog. Existing design tokens and component patterns outrank generic advice.
- TUI/Ratatui/TachyonFX craft: treat terminal UI as a first-class frontend. Design around stable rectangles, sparse hierarchy, deliberate foreground/background tokens, readable command affordances, breakpoint plates such as 80x24/120x32/160x48, direct buffer/cell assertions, and post-render effects that enhance state transitions without becoming visual noise.
- QA discipline: test what a real user can do, check console/runtime errors after interactions, verify responsive states, and capture evidence or exact blockers.
- Additional skills.sh design synthesis: use product-frontend critique for message clarity, frontend-ui-ux engineering for accessible polish and micro-interactions, and design-token extraction discipline from design-system skills. Do not require external paid design tools or infinite-canvas mockup systems.

Required first reads:
- `AGENTS.md` or repo-local agent instructions.
- Product doctrine: `README.md`, `DESIGN.md`, GDD/OS/invariant docs when present.
- Planning truth: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, active `specs/`, and `{planning_root_display}` when present.
- Frontend code: app/routes/components/styles/design tokens/tests/build scripts.
- TUI code: Ratatui widgets, terminal init/restore, event loops, layout helpers, style/theme tokens, buffer snapshot tests, headless render/export paths, and TachyonFX or other animation managers when present.
- Runtime/engine/API code that owns facts displayed by UI.
- Generated bindings/schemas/client code and their regeneration commands when present.

Hard rules:
- Do not create fake mockups as acceptance evidence. Preview pages are allowed only as proposals and must be labeled non-authoritative.
- Do not invent frontend bindings, constants, catalogs, balances, settlement math, eligibility rules, risk classes, or status derivations. UI must consume runtime/API/generated truth.
- If the design calls for new data, name the runtime owner, API/schema change, generator, consumer, and test/readback proof.
- Prefer existing helpers, generated clients, hooks, stores, route loaders, and design tokens over new manual glue.
- Production code must not import fixture/demo/sample data as fallback truth.
- Retired or superseded screens/specs must be deleted, archived, tombstoned, or explicitly blocked from active implementation.
- A design improvement is not complete unless it names the engine/API contract and the proof that would fail if UI drifts again.
- Existing queues matter: if root `IMPLEMENTATION_PLAN.md` or another active queue already exists, reconcile design findings into that queue and promote the highest-leverage shared UI/runtime work. Do not create a parallel design backlog that leaves executable runtime or user-facing blockers untouched.
- Design tasks must prefer reusable component systems, shared layout/state primitives, and runtime-backed interaction contracts over one-off presentations for a single screen, route, game, or fixture.
- For Ratatui or other TUIs, acceptance must include terminal-size plates, deterministic headless rendering or buffer snapshots, cell-level assertions for critical geometry/style, keyboard/input-state coverage, and animation-frame proof for TachyonFX effects. Effects must run after base widgets render and must be bounded, stateful, and scoped to meaningful regions.

{edit_clause}
{qa_clause}

Write these non-empty artifacts under `{output_dir}`:
1. `DESIGN-AUDIT.md`
   - Current UI/design-system inventory.
   - Existing frontend design signals and reusable components/tokens.
   - For TUI surfaces, current layout geometry, breakpoint behavior, command density, typography/color token discipline, and motion/animation usage.
   - 0-10 ratings for the seven plan-design-review dimensions.
   - AI-slop risks and modern/stunning UI opportunities specific to this product.
2. `DESIGN-SYSTEM-PROPOSAL.md`
   - Proposed or revised `DESIGN.md` doctrine.
   - Aesthetic thesis, safe choices, deliberate risks, typography, color, spacing, layout, motion, components, empty/error/loading states, responsive and accessibility rules.
   - For TUIs, terminal-native component contracts: viewport grid, panel hierarchy, card/table/list geometry, color roles, focus/selection states, command bar, animation policy, and fallback behavior for small terminals.
   - Explicitly explain what belongs in real product UI versus non-authoritative concept previews.
3. `ENGINE-UI-CONTRACT.md`
   - Table of UI surfaces, runtime/API source of truth, existing helpers/bindings, generated artifacts, fixture boundary, and required drift guard.
   - Call out every manual binding or duplicated frontend derivation found.
4. `FRONTEND-QA.md`
   - Commands/URLs/tools used, screenshots or artifact paths if produced, console/runtime findings, responsive findings, and exact blockers.
   - For TUIs, include terminal dimensions tested, headless exports or buffer snapshots, interaction transcript, animation-frame evidence, and any terminal capability assumptions.
   - Separate confirmed breaks from hypotheses and from skipped/unavailable checks.
5. `DESIGN-PLAN-ITEMS.md`
   - Queue-ready plan items for unresolved design/runtime gaps using the repo's implementation-plan field style.
   - Every item must include runtime owner, UI consumers, generated artifacts, contract generation, cross-surface proof, and closeout review.
6. `DESIGN-REPORT.md`
   - Executive summary, files changed if any, recommended next workflow step, and GO/NO-GO for design-aware implementation.
   - In the `auto super` flow, `Verdict: NO-GO` blocks the CEO production campaign until design/runtime integrity is repaired.

{apply_instructions}

Final line of `DESIGN-REPORT.md` must be exactly one of:
- `Verdict: GO`
- `Verdict: NO-GO`
"#,
        stage_clause = stage_clause,
        repo_root = repo_root.display(),
        planning_clause = planning_clause,
        output_dir = output_dir.display(),
        prompt_clause = prompt_clause,
        planning_root_display = planning_root
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "no planning corpus".to_string()),
        edit_clause = edit_clause,
        qa_clause = qa_clause,
        production_orchestration_discipline = PRODUCTION_ORCHESTRATION_DISCIPLINE,
        apply_instructions = apply_instructions,
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{build_design_parallel_prompt, build_design_prompt, DesignRunKind};

    #[test]
    fn design_prompt_rejects_fake_mockup_and_requires_runtime_truth() {
        let prompt = build_design_prompt(
            &PathBuf::from("/repo"),
            Some(&PathBuf::from("/repo/genesis")),
            &PathBuf::from("/repo/.auto/design/run"),
            Some("make the UI better"),
            true,
            false,
            DesignRunKind::Standalone,
        );

        assert!(prompt.contains("not a fake mockup generator"));
        assert!(prompt.contains("Do not create fake mockups as acceptance evidence"));
        assert!(prompt.contains("UI must consume runtime/API/generated truth"));
        assert!(prompt.contains("live production playground"));
        assert!(prompt.contains("reusable component systems"));
        assert!(prompt.contains("ENGINE-UI-CONTRACT.md"));
        assert!(prompt.contains("FRONTEND-QA.md"));
    }

    #[test]
    fn design_prompt_includes_terminal_ui_quality_gate() {
        let prompt = build_design_prompt(
            &PathBuf::from("/repo"),
            None,
            &PathBuf::from("/repo/.auto/design/run"),
            Some("make the TUI beautiful"),
            true,
            false,
            DesignRunKind::Standalone,
        );

        assert!(prompt.contains("TUI/Ratatui/TachyonFX craft"));
        assert!(prompt.contains("breakpoint plates such as 80x24/120x32/160x48"));
        assert!(prompt.contains("buffer/cell assertions"));
        assert!(prompt.contains("TachyonFX effects"));
        assert!(prompt.contains("Effects must run after base widgets render"));
        assert!(prompt.contains("animation-frame evidence"));
    }

    #[test]
    fn design_prompt_report_only_does_not_request_root_edits() {
        let prompt = build_design_prompt(
            &PathBuf::from("/repo"),
            Some(&PathBuf::from("/repo/genesis")),
            &PathBuf::from("/repo/.auto/design/run"),
            Some("make the UI better"),
            false,
            false,
            DesignRunKind::Standalone,
        );

        assert!(prompt.contains("If report-only mode is enabled:"));
        assert!(prompt.contains("Do not edit root `DESIGN.md`"));
        assert!(prompt.contains("Put all proposed doctrine changes"));
        assert!(!prompt.contains("If `report-only mode is enabled`:"));
    }

    #[test]
    fn design_parallel_prompt_steers_workers_to_shared_executable_design_repairs() {
        let prompt = build_design_parallel_prompt(&PathBuf::from("/repo/.auto/design/run"), 2);

        assert!(prompt.contains("auto design --resolve"));
        assert!(prompt.contains("pass-02"));
        assert!(prompt.contains("DESIGN-PLAN-ITEMS.md"));
        assert!(prompt.contains("Prefer dependency-ready `DESIGN-*` tasks"));
        assert!(prompt.contains("shared runtime-backed component"));
        assert!(prompt.contains("not one-off presentations"));
        assert!(prompt.contains("docs-only, report-only, screenshot-only, evidence-only"));
    }

    #[test]
    fn super_design_prompt_keeps_pre_generation_edit_boundary() {
        let prompt = build_design_prompt(
            &PathBuf::from("/repo"),
            Some(&PathBuf::from("/repo/genesis")),
            &PathBuf::from("/repo/.auto/super/run/design"),
            None,
            true,
            false,
            DesignRunKind::Super,
        );

        assert!(prompt.contains("auto super` design perfection gate"));
        assert!(prompt.contains("Design is first-class and blocking"));
        assert!(prompt
            .contains("Do not edit source code, root specs, or root `IMPLEMENTATION_PLAN.md`"));
    }
}
