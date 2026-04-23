# Root Planning Truth Reconciliation

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This plan makes the active root planning surface truthful again. Operators should be able to open `IMPLEMENTATION_PLAN.md`, `ARCHIVED.md`, `WORKLIST.md`, and `specs/` and see the same current state that code and tests show.

The user can see it working when stale completed tasks are no longer listed as active, known open work remains visible, validation evidence is labeled as passing, failing, stale, or not-run, and old corpus claims are not repeated as active facts.

## Requirements Trace

- R1: Root planning docs remain the active control surface.
- R2: Current validation status must distinguish passing, failing, stale, and not-run evidence.
- R3: Prior claims must be reconciled against code, not copied from historical docs.
- R4: `genesis/` must be described as generated corpus, not root queue authority.

## Scope Boundaries

This plan edits planning/docs only. It does not change Rust source, CLI defaults, CI, tests, or generated code. It does not decide whether `genesis/` should be ignored or tracked; it documents current authority and opens a follow-up only if needed.

## Progress

- [x] 2026-04-23: Current root planning drift identified.
- [x] 2026-04-23: Authoring-pass quota failure corrected by targeted rerun and full `cargo test` pass.
- [ ] 2026-04-23: Reconcile root planning docs and stale specs.
- [ ] 2026-04-23: Record the exact remaining open work after reconciliation.

## Surprises & Discoveries

The archived corpus still contains findings that were true earlier, such as missing CI, but those findings are now stale. Conversely, the active root queue still carries old validation and task-status claims even though the code and history moved on.

## Decision Log

- Mechanical: Trust code and current command output over historical docs.
- Taste: Keep this as a docs-only slice so it can land before security code changes.
- Mechanical: Do not promote every generated `genesis/` plan automatically.

## Outcomes & Retrospective

None yet. After implementation, record which tasks were moved, which specs were corrected, and whether any stale claims were intentionally left as historical context.

## Context and Orientation

Start with:

- `IMPLEMENTATION_PLAN.md`: active queue, currently stale.
- `ARCHIVED.md`: completion ledger, useful but not proof by itself.
- `WORKLIST.md`: current follow-ups around verification command synthesis and false proof.
- `specs/220426-*.md`: dated specs with mixed freshness.
- `README.md` and `AGENTS.md`: operator docs.
- `.github/workflows/ci.yml`: current CI truth.
- `Cargo.toml`: package version and binary truth.

Current validation truth from this review pass: `cargo test -- --list` enumerated 377 tests, `cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error -- --exact` passed, and full `cargo test` passed with 377 tests. Root docs should replace older `333`-test claims with current evidence.

## Plan of Work

Read the root planning docs and mark stale claims in place. Move completed task claims out of active sections or annotate them as archived. Keep still-open `WORKLIST.md` items visible. Update specs that claim no CI, old version state, old command inventory, or old backend behavior. Add a short note explaining that `genesis/` is generated corpus and root planning remains active in root docs.

## Implementation Units

Unit 1 - Reconcile active task statuses:

- Goal: Ensure active root tasks describe only remaining work.
- Requirements advanced: R1, R2, R3.
- Dependencies: current code review and test result.
- Files to create or modify: `IMPLEMENTATION_PLAN.md`, optionally `ARCHIVED.md`.
- Tests to add or modify: none.
- Approach: compare open task IDs against `ARCHIVED.md`, `WORKLIST.md`, code, and current validation.
- Specific test scenarios: `rg -n "\\[~\\]|\\[!\\]|\\[ \\]" IMPLEMENTATION_PLAN.md` should show only genuinely active or blocked tasks after the update.

Unit 2 - Correct stale specs:

- Goal: Remove or qualify stale claims in dated specs.
- Requirements advanced: R2, R3.
- Dependencies: Unit 1.
- Files to create or modify: affected `specs/220426-*.md` files.
- Tests to add or modify: none.
- Approach: update status/evidence sections without rewriting historical intent.
- Specific test scenarios: `rg -n "no \\.github|333 tests|0\\.1\\.0|thirteen commands" specs IMPLEMENTATION_PLAN.md README.md` should not report active-current claims after the update.

Unit 3 - Document corpus authority:

- Goal: Prevent `genesis/` from becoming a competing queue by accident.
- Requirements advanced: R1, R4.
- Dependencies: Unit 1.
- Files to create or modify: `IMPLEMENTATION_PLAN.md` or a short root note in the existing planning section.
- Tests to add or modify: Test expectation: none -- this is documentation authority, not code behavior.
- Approach: state that generated `genesis/` plans must be promoted into the root plan before execution.
- Specific test scenarios: `rg -n "genesis.*subordinate|generated corpus|promoted" IMPLEMENTATION_PLAN.md` should find the note.

## Concrete Steps

From the repository root:

    git status --short
    rg -n "333 tests|no \\.github|0\\.1\\.0|thirteen commands|claude-opus|gpt-5\\.4" IMPLEMENTATION_PLAN.md ARCHIVED.md README.md specs
    rg -n "\\[ \\]|\\[~\\]|\\[!\\]" IMPLEMENTATION_PLAN.md
    sed -n '1,220p' WORKLIST.md

Expected observation: stale claims are concentrated in root plans/specs, while `WORKLIST.md` still has open verification-proof work.

After edits:

    rg -n "333 tests|no \\.github|0\\.1\\.0|thirteen commands" IMPLEMENTATION_PLAN.md specs README.md
    rg -n "quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error" IMPLEMENTATION_PLAN.md specs || true

Expected observation: stale active claims are gone or explicitly historical; validation status records the targeted quota rerun and honestly says whether full validation was rerun.

## Validation and Acceptance

Acceptance is observable in docs:

- active root plan status matches current code and test evidence;
- completed tasks are not still marked active unless they have a concrete remaining follow-up;
- stale CI/version/command-count claims are corrected or marked historical;
- still-open `WORKLIST.md` verification items remain visible;
- `genesis/` is documented as generated corpus and subordinate to root planning.

## Idempotence and Recovery

This plan is safe to rerun. Re-run the `rg` checks and only adjust claims that still appear as active-current facts. If a task status is ambiguous, leave it open with an evidence note rather than moving it to archived.

## Artifacts and Notes

Record the before/after grep output in the implementation commit or root plan note. Include exact failing test names only if they still fail at implementation time.

## Interfaces and Dependencies

Planning docs only: `IMPLEMENTATION_PLAN.md`, `ARCHIVED.md`, `WORKLIST.md`, `README.md`, `AGENTS.md`, `.github/workflows/ci.yml`, `Cargo.toml`, and `specs/`.
