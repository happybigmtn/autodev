# Security State And Scheduler Checkpoint

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This checkpoint prevents the campaign from moving into broader execution work until the highest-risk state, credential, and scheduler truth blockers are closed. The operator gains a concrete go/no-go decision before trusting `auto gen`, `auto super`, or `auto parallel` with new production work.

## Requirements Trace

- R1: Plan 002 account/profile/failover safety is closed or explicitly waived.
- R2: Plan 003 corpus/state safety is closed or explicitly waived.
- R3: Plan 004 scheduler/completion truth is closed or explicitly waived.
- R4: Current root queue and generated corpus agree on active planning primacy.
- R5: CI and focused tests for changed modules pass.

## Scope Boundaries

This is a decision-gate plan. It should not introduce unrelated product features. It may add a small script or checklist if needed, but its main output is evidence and a go/no-go decision.

## Progress

- [x] 2026-04-30: Checkpoint position established after Plans 002-004.
- [ ] Verify Plan 002 closure evidence.
- [ ] Verify Plan 003 closure evidence.
- [ ] Verify Plan 004 closure evidence.
- [ ] Record gate decision in root review or ship notes if promoted.

## Surprises & Discoveries

None yet.

## Decision Log

- Mechanical: State, credential, and scheduler blockers must be resolved before evidence/release work can be trusted.
- Taste: Use a standalone checkpoint plan rather than burying this in Plan 004 so later parallel work has a clear dependency.
- User Challenge: If the operator chooses to waive a blocker, the waiver must name the production risk and rescue path.

## Outcomes & Retrospective

None yet.

## Context and Orientation

This checkpoint depends on:

- Plan 002: `genesis/plans/002-quota-backend-and-credential-safety.md`.
- Plan 003: `genesis/plans/003-corpus-state-and-planning-root-safety.md`.
- Plan 004: `genesis/plans/004-scheduler-completion-truth-and-lane-resume.md`.

Relevant runtime evidence includes quota tests, corpus/generation tests, completion/scheduler tests, `git status`, and the presence of a complete `genesis/` corpus.

## Plan of Work

Collect evidence from the completed implementation slices. Confirm unsafe quota account names fail, corpus roots are contained and non-empty, current ledger conventions do not trigger false demotion, and scheduler dispatch fails closed on stale plan truth. Confirm the active planning surface remains root ledgers and that this generated corpus is still subordinate unless promoted.

If all requirements pass, record a GO decision and proceed to Plans 006-008. If any requirement fails, record NO-GO, list blockers, and return to the relevant implementation plan.

## Implementation Units

- Unit 1: Quota safety evidence.
  - Goal: Verify account/profile/failover safety.
  - Requirements advanced: R1.
  - Dependencies: Plan 002.
  - Files to create or modify: gate report location if promoted, otherwise none.
  - Tests to add or modify: none in this checkpoint; consume Plan 002 tests.
  - Approach: Run focused quota/backend tests and inspect changed behavior.
  - Test scenarios: Test expectation: none -- this unit verifies existing Plan 002 tests.

- Unit 2: Corpus/state evidence.
  - Goal: Verify planning-root containment and empty-corpus rejection.
  - Requirements advanced: R2, R4.
  - Dependencies: Plan 003.
  - Files to create or modify: gate report location if promoted, otherwise none.
  - Tests to add or modify: none in this checkpoint; consume Plan 003 tests.
  - Approach: Run corpus/generation tests and inspect `genesis/`.
  - Test scenarios: Test expectation: none -- this unit verifies existing Plan 003 tests.

- Unit 3: Scheduler truth evidence.
  - Goal: Verify completion policy, fail-closed refresh, and lane resume hashing.
  - Requirements advanced: R3.
  - Dependencies: Plan 004.
  - Files to create or modify: gate report location if promoted, otherwise none.
  - Tests to add or modify: none in this checkpoint; consume Plan 004 tests.
  - Approach: Run completion/scheduler tests and inspect status output.
  - Test scenarios: Test expectation: none -- this unit verifies existing Plan 004 tests.

- Unit 4: Gate decision.
  - Goal: Record GO/NO-GO and next dependency state.
  - Requirements advanced: R5.
  - Dependencies: Units 1-3.
  - Files to create or modify: `REVIEW.md` or a checkpoint artifact if promoted.
  - Tests to add or modify: none.
  - Approach: Write concise evidence with commands and results.
  - Test scenarios: Test expectation: none -- this is an operator decision artifact.

## Concrete Steps

From the repository root:

    git status --short --branch
    find genesis -maxdepth 2 -type f | sort
    rg -n "^- \\[( |~|!)\\]" IMPLEMENTATION_PLAN.md REVIEW.md

Run the focused tests created by Plans 002-004:

    cargo test quota
    cargo test corpus
    cargo test generation
    cargo test completion_artifacts
    cargo test parallel_status

Then run the common Rust gate:

    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: all focused tests and clippy pass, with no unexpected root queue rows.

## Validation and Acceptance

GO requires passing evidence for all requirements or explicit operator waivers. NO-GO is required if any high-severity security, state, or scheduler truth issue remains unaddressed. A waiver must include affected command, risk, rescue path, and expiration condition.

## Idempotence and Recovery

The checkpoint can be rerun safely. If tests fail, return to the owning plan and rerun after fixes. If root ledgers changed during checkpointing, inspect `git diff` and keep only deliberate evidence updates.

## Artifacts and Notes

- Evidence to fill in: command outputs from focused tests.
- Evidence to fill in: GO/NO-GO decision.
- Evidence to fill in: any waivers and expiration conditions.

## Interfaces and Dependencies

- Depends on Plans 002-004.
- Commands: `cargo test`, `cargo clippy`, `git status`, `find`, `rg`.
- Files: `genesis/`, `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, optional checkpoint evidence.
