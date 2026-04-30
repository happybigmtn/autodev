# Security and State Checkpoint Gate

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

This checkpoint prevents the program from moving into broader execution work until the first safety tranche is actually proven. Operators gain an explicit go/no-go decision after quota, corpus, and dependency fixes. They can see it working through a short evidence bundle that says which risks are closed, which remain, and whether `auto parallel` can be considered for later phases.

## Requirements Trace

- R1: Verify Plan 002 quota path and lease safety.
- R2: Verify Plan 003 corpus atomicity and non-empty planning-root safety.
- R3: Verify Plan 004 dependency truth and scheduler eligibility.
- R4: Record remaining blockers and rescue path before later plans proceed.
- R5: Avoid implementation promises if any safety evidence is missing.

## Scope Boundaries

This plan is a checkpoint gate, not a code-change plan. It does not implement fixes from Plans 002-004 and does not launch model workers. It may update a checkpoint note or root handoff only after evidence is collected.

## Progress

- [x] 2026-04-30: Checkpoint plan created after initial repo review.
- [ ] 2026-04-30: Run after Plans 002-004 are implemented.
- [ ] 2026-04-30: Record go/no-go outcome and blockers.

## Surprises & Discoveries

None yet. Fill this section with any failed assumptions discovered while validating the first tranche.

## Decision Log

- Mechanical: Checkpoint is required because later work depends on safe credentials, corpus state, and scheduling truth.
- Taste: Keep this as a separate plan so operators can stop the race if core trust is still broken.

## Outcomes & Retrospective

None yet. Record whether the gate passed, failed, or passed with explicit waivers.

## Context and Orientation

Relevant files:

- Plans 002, 003, and 004.
- `src/quota_config.rs`, `src/quota_exec.rs`, `src/corpus.rs`, `src/generation.rs`, `src/task_parser.rs`, `src/parallel_command.rs`, `src/loop_command.rs`, `src/audit_everything.rs`.
- `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `REVIEW.md`.

Non-obvious term:

- Checkpoint gate: an evidence-only decision point where later work is blocked unless prerequisites pass.

## Plan of Work

Run the validation commands named in Plans 002-004. Review the diffs and test output. Inspect `auto parallel status` for dependency blockers and stale state. Write a short checkpoint note in the active handoff surface when promoted. Decide go, no-go, or go-with-waivers.

## Implementation Units

- Unit 1: Evidence collection. Goal: gather validation proof for Plans 002-004. Requirements advanced: R1, R2, R3. Dependencies: Plans 002-004 complete. Files to create or modify: none unless promoted into `REVIEW.md`. Tests: plan-specific tests. Approach: run exact commands and inspect output. Specific scenarios: unsafe account rejected; empty corpus rejected; missing dependency blocks status.
- Unit 2: Gate decision. Goal: produce go/no-go. Requirements advanced: R4, R5. Dependencies: Unit 1. Artifact: checkpoint note in `REVIEW.md` or active plan handoff. Test expectation: none -- this is a decision artifact with no code behavior changes. Approach: summarize blockers and next action. Specific scenarios: pass, fail, waived with rationale.

## Concrete Steps

From the repository root:

    cargo test quota
    cargo test generation::tests
    cargo test corpus::tests
    cargo test task_parser::tests
    cargo test parallel_command::tests
    cargo test loop_command::tests
    cargo test audit_everything::tests
    auto parallel status
    git status --short

Expected observations: tests pass, status does not mark blocked rows ready, and worktree changes are scoped to the promoted slices.

## Validation and Acceptance

Acceptance:

- All Plan 002-004 validation commands pass or failures are documented as blockers.
- `auto parallel status` does not hide missing dependencies.
- No empty `genesis/` root is accepted.
- Quota path and lease safety have direct tests.
- The checkpoint note names go/no-go and any waivers.

## Idempotence and Recovery

The checkpoint can be rerun after any fix. If it fails, do not continue to later implementation plans except to repair the failed prerequisite. Preserve failed command output in the handoff.

## Artifacts and Notes

Fill in:

- Quota validation command and result.
- Corpus validation command and result.
- Scheduler validation command and result.
- Gate decision.

## Interfaces and Dependencies

Interfaces: Cargo test suite, `auto parallel status`, git status, active handoff docs. Dependencies: completed Plans 002-004.
