# Master Plan For Operator Trust

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This master plan coordinates the generated corpus. A user gains a clear, sequenced path from current repo reality to a safer `auto` CLI: reconciled planning truth, fixed credential handling, hardened workflow rendering, stronger verification evidence, shared task parsing, and better first-run proof.

The work is visible when root planning docs stop contradicting code, quota credential swaps restore exactly, Symphony workflows quote hostile inputs safely, verification receipts reject false proof, and operators can run a no-model smoke path before trusting live agents.

## Requirements Trace

- R1: Keep `genesis/` subordinate to active root planning docs until work is promoted.
- R2: Fix credential restore/copy safety before broader execution refactors.
- R3: Harden generated executable text before expanding Symphony usage.
- R4: Make verification evidence explicit and risk-classed.
- R5: Converge duplicated task parsing.
- R6: Add first-run and CI proof after safety gates pass.

## Scope Boundaries

This plan does not directly edit Rust code, root docs, CI, or provider credentials. It coordinates the numbered plans and defines phase gates. It does not add commands, change defaults, or decide encryption-at-rest.

## Progress

- [x] 2026-04-23: Current repo review completed across code, docs, tests, CI, git history, and archived corpus.
- [x] 2026-04-23: Plan sequence selected with checkpoint gates after security and execution-contract phases.
- [ ] 2026-04-23: Promote selected slices into root `IMPLEMENTATION_PLAN.md` before execution.

## Surprises & Discoveries

The archived previous corpus is materially stale. Several high-priority old findings, including CI absence and older command inventory drift, have been addressed. Current `cargo test` is green, but urgent issues remain in credential restore, planning drift, Symphony rendering, and verification proof.

## Decision Log

- Mechanical: No root `PLANS.md` or root `plans/` exists, so generated ExecPlans are subordinate corpus artifacts.
- Taste: Planning truth reconciliation comes before security implementation so later work is tracked in the correct root queue.
- Mechanical: Security checkpoint Plan 005 blocks execution-contract work until credential and workflow risks are addressed.
- User Challenge: Dangerous-mode defaults and encryption-at-rest remain operator decisions rather than silent changes.

## Outcomes & Retrospective

Not implemented yet. The intended outcome is a promoted, evidence-backed subset of this corpus in the root plan, followed by phase-by-phase execution. Update this section after each checkpoint.

## Context and Orientation

Relevant files:

- `AGENTS.md` documents build and validation commands.
- `src/main.rs` defines the public CLI surface and defaults.
- `src/generation.rs` writes and verifies `genesis/` and `gen-*` planning artifacts.
- `IMPLEMENTATION_PLAN.md`, `ARCHIVED.md`, `WORKLIST.md`, and `specs/` are active root planning materials.
- `src/quota_exec.rs`, `src/quota_config.rs`, and `src/quota_usage.rs` own quota-account behavior.
- `src/symphony_command.rs` renders Symphony workflow text.
- `src/completion_artifacts.rs` interprets verification evidence.
- `src/parallel_command.rs`, `src/loop_command.rs`, and `src/review_command.rs` execute or review task queues.

## Plan of Work

First reconcile planning truth. Then fix credential and generated workflow security issues. Add a security gate. Then harden verification receipts, shared task parsing, and backend policy design. Add an execution-contract gate. Finally improve first-run DX, CI, and release readiness.

## Implementation Units

Unit 1 - Promote the chosen plan subset:

- Goal: Copy the selected next work into the active root plan.
- Requirements advanced: R1.
- Dependencies: this corpus.
- Files to create or modify: `IMPLEMENTATION_PLAN.md`.
- Tests to add or modify: none.
- Approach: add only the chosen near-term slice rows, preserving existing root plan style.
- Specific test scenarios: `rg -n "Quota Credential Restore|Symphony Workflow Rendering|Verification Command" IMPLEMENTATION_PLAN.md` should find promoted work when those slices are selected.

Unit 2 - Execute phase gates:

- Goal: Stop later phases until the previous phase has evidence.
- Requirements advanced: R2, R3, R4, R5, R6.
- Dependencies: Plans 005, 009, and 012.
- Files to create or modify: gate plans and root status docs.
- Tests to add or modify: none for this coordination unit.
- Approach: update root status only after targeted commands pass.
- Specific test scenarios: each checkpoint plan lists its own acceptance commands.

Unit 3 - Maintain the corpus:

- Goal: Keep generated plans aligned with actual execution.
- Requirements advanced: R1.
- Dependencies: all plans.
- Files to create or modify: `genesis/GENESIS-REPORT.md`, this plan, and affected numbered plans.
- Tests to add or modify: Test expectation: none -- this is a documentation maintenance unit, not code behavior.
- Approach: update Progress and Outcomes sections as execution proceeds.
- Specific test scenarios: `rg -n "None yet" genesis/plans` should shrink as plans move from proposed to executed.

## Concrete Steps

From the repository root:

    rg -n "Root Planning Truth|Quota Credential|Symphony Workflow" genesis/plans
    sed -n '1,220p' genesis/PLANS.md
    sed -n '1,220p' IMPLEMENTATION_PLAN.md

Expected observation: `genesis/PLANS.md` indexes the full sequence, while root `IMPLEMENTATION_PLAN.md` remains the active queue until explicitly updated.

## Validation and Acceptance

Acceptance for this master plan is documentation-level:

- the generated corpus contains plans 001 through 012;
- the index states that root docs remain active;
- checkpoint plans exist after meaningful phase boundaries;
- no generated markdown contains the absolute repository-root path.

## Idempotence and Recovery

Rerun the corpus review safely by archiving current `genesis/` first if using `auto corpus`. If a generated plan is partially promoted, compare root `IMPLEMENTATION_PLAN.md` with this file and only keep the selected slices. Do not mass-copy every generated plan into active work without operator choice.

## Artifacts and Notes

Evidence captured in this corpus:

- targeted review reran the previously reported quota usage failure and full `cargo test`; both passed;
- no root `PLANS.md` or root `plans/` directory exists;
- previous corpus snapshot is historical and stale in several areas.

## Interfaces and Dependencies

This plan depends on markdown docs only. It coordinates the interfaces owned by later plans: quota credentials, Symphony workflow rendering, completion artifacts, task parsing, backend execution, first-run smoke tests, and CI.
