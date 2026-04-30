# Execution Contract Checkpoint Gate

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

This checkpoint verifies that receipts, reconciliation, and task contracts now agree before later audit, DX, and release work proceeds. Operators gain a second go/no-go point focused on execution truth. They can see it working when stale receipts fail, partial rows remain well formed, and schema-invalid work is blocked consistently.

## Requirements Trace

- R1: Verify Plan 006 receipt freshness and release proof binding.
- R2: Verify Plan 007 Symphony/review reconciliation safety.
- R3: Verify Plan 008 schema parity across execution surfaces.
- R4: Confirm root queue state is accurate enough for final readiness planning.
- R5: Record blockers before audit/DX/release polish.

## Scope Boundaries

This is an evidence and decision plan. It does not implement Plans 006-008, rewrite documentation, or launch parallel lanes.

## Progress

- [x] 2026-04-30: Checkpoint plan created after execution-contract risks were identified.
- [ ] 2026-04-30: Run after Plans 006-008 are implemented.
- [ ] 2026-04-30: Record gate outcome.

## Surprises & Discoveries

None yet. Fill this in after running the checkpoint.

## Decision Log

- Mechanical: Later release and DX work should not proceed if evidence contracts still lie.
- Taste: Separate execution-contract verification from the first security/state checkpoint to keep failure domains clear.

## Outcomes & Retrospective

None yet. Record go/no-go and waivers after execution.

## Context and Orientation

Relevant files:

- Plans 006, 007, and 008.
- `scripts/run-task-verification.sh`, `scripts/verification_receipt.py`, `src/completion_artifacts.rs`, `src/ship_command.rs`.
- `src/review_command.rs`, `src/symphony_command.rs`, `src/super_command.rs`, `src/loop_command.rs`, `src/generation.rs`.
- `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `WORKLIST.md`.

Non-obvious term:

- Execution contract: the combined task schema, receipt proof, and reconciliation behavior that determines whether autonomous work can be trusted.

## Plan of Work

Run targeted tests for receipt metadata, completion evidence, ship gate, Symphony/review reconciliation, and schema parity. Inspect active root plan rows for stale tasks. Confirm that no stale receipt, malformed partial row, or schema-invalid task can satisfy a completion or release path. Record the decision in the active handoff if promoted.

## Implementation Units

- Unit 1: Test execution. Goal: gather proof for Plans 006-008. Requirements advanced: R1, R2, R3. Dependencies: Plans 006-008 complete. Files to create or modify: none unless promoted. Tests: plan-specific command suite. Approach: run exact tests and inspect failures. Scenarios: stale receipt rejected; branch mismatch no-write; super schema rejects invalid row.
- Unit 2: Queue truth review. Goal: decide readiness for final work. Requirements advanced: R4, R5. Dependencies: Unit 1. Artifact: checkpoint note. Test expectation: none -- this is a decision artifact. Approach: compare root plan, worklist, review, and receipts. Scenarios: all rows reconciled; blockers listed.

## Concrete Steps

From the repository root:

    cargo test completion_artifacts::tests
    cargo test ship_command::tests
    cargo test review_command::tests
    cargo test symphony_command::tests
    cargo test generation::tests
    cargo test super_command::tests
    cargo test loop_command::tests
    auto parallel status
    rg -n "AD-014|TASK-016|WORKLIST|receipt|zero-test|Dependencies:" IMPLEMENTATION_PLAN.md WORKLIST.md REVIEW.md

Expected observations: no stale proof is accepted, and active root truth is either ready or explicitly blocked.

## Validation and Acceptance

Acceptance:

- Plan 006-008 tests pass or blockers are documented.
- Current root queue has no hidden dependency, receipt, or schema inconsistencies.
- The checkpoint produces a go/no-go decision before Plans 010-012 proceed.

## Idempotence and Recovery

Rerun this checkpoint after any execution-contract fix. If it fails, return to the failed plan and update only the relevant code/docs. Do not proceed by weakening the gate.

## Artifacts and Notes

Fill in:

- Receipt validation result.
- Reconciliation validation result.
- Schema parity result.
- Queue truth decision.

## Interfaces and Dependencies

Interfaces: Cargo tests, receipt scripts, root planning docs, `auto parallel status`. Dependencies: completed Plans 006-008.
