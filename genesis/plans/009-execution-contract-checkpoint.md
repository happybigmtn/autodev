# Execution Contract Checkpoint

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This checkpoint decides whether the execution contract is trustworthy enough for lifecycle, DX, performance, and release-promotion work. The operator gains a deliberate pause after evidence and schema hardening before expanding the campaign.

## Requirements Trace

- R1: Receipt/artifact containment and shared evidence inspection from Plan 006 are closed or waived.
- R2: Release gate ordering and verdict parsing from Plan 007 are closed or waived.
- R3: Shared execution-row schema from Plan 008 is closed or waived.
- R4: All changed paths pass focused tests and clippy.
- R5: Remaining risks are classified as GO, NO-GO, or explicit waiver.

## Scope Boundaries

This plan is a gate, not a feature implementation. It should not rewrite lifecycle commands or docs. It records whether later work may proceed.

## Progress

- [x] 2026-04-30: Checkpoint position established after Plans 006-008.
- [ ] Verify evidence inspector and artifact containment.
- [ ] Verify release gate and verdict parser hardening.
- [ ] Verify execution schema parity.
- [ ] Record GO/NO-GO decision.

## Surprises & Discoveries

None yet.

## Decision Log

- Mechanical: Release and scheduler work must not proceed on ambiguous evidence or schema.
- Taste: Run this checkpoint before DX because DX should teach stable behavior.
- User Challenge: Waivers are allowed only with explicit production risk ownership.

## Outcomes & Retrospective

None yet.

## Context and Orientation

This checkpoint depends on:

- Plan 006: `genesis/plans/006-receipt-artifact-and-release-evidence-binding.md`.
- Plan 007: `genesis/plans/007-release-gate-and-verdict-parser-hardening.md`.
- Plan 008: `genesis/plans/008-super-loop-review-and-schema-parity.md`.

It verifies the execution contract from root row to scheduler to receipt to release gate.

## Plan of Work

Collect test results and evidence from Plans 006-008. Confirm unsafe artifact paths are rejected, stale receipts block completion and release, mixed verdicts fail, ship gates run after sync and after model work, and all execution paths use the shared row schema. Decide whether later lifecycle and DX work can proceed.

## Implementation Units

- Unit 1: Evidence binding verification.
  - Goal: Confirm receipt/artifact contract.
  - Requirements advanced: R1.
  - Dependencies: Plan 006.
  - Files to create or modify: checkpoint evidence if promoted.
  - Tests to add or modify: none in this checkpoint.
  - Approach: Run and record focused evidence tests.
  - Test scenarios: Test expectation: none -- this unit verifies Plan 006 tests.

- Unit 2: Release/verdict verification.
  - Goal: Confirm release gates and verdict parsing fail closed.
  - Requirements advanced: R2.
  - Dependencies: Plan 007.
  - Files to create or modify: checkpoint evidence if promoted.
  - Tests to add or modify: none in this checkpoint.
  - Approach: Run and record focused ship/design/audit/book tests.
  - Test scenarios: Test expectation: none -- this unit verifies Plan 007 tests.

- Unit 3: Schema parity verification.
  - Goal: Confirm execution row contract is shared.
  - Requirements advanced: R3.
  - Dependencies: Plan 008.
  - Files to create or modify: checkpoint evidence if promoted.
  - Tests to add or modify: none in this checkpoint.
  - Approach: Run and record task/generation/scheduler parity tests.
  - Test scenarios: Test expectation: none -- this unit verifies Plan 008 tests.

- Unit 4: Gate decision.
  - Goal: Record GO/NO-GO for later lifecycle and release-promotion work.
  - Requirements advanced: R4, R5.
  - Dependencies: Units 1-3.
  - Files to create or modify: `REVIEW.md` or checkpoint artifact if promoted.
  - Tests to add or modify: none.
  - Approach: Write the decision with blockers and waivers.
  - Test scenarios: Test expectation: none -- this is an operator decision artifact.

## Concrete Steps

From the repository root:

    git status --short --branch
    cargo test completion_artifacts
    cargo test ship
    cargo test design
    cargo test audit_everything
    cargo test book
    cargo test task_parser
    cargo test generation
    cargo test parallel
    cargo test super
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: all focused suites pass; any waivers are written down before proceeding.

## Validation and Acceptance

GO requires all requirements closed or explicitly waived. NO-GO is required if release evidence, verdict parsing, or execution-row schema remains ambiguous. The checkpoint passes only if a later operator can read the evidence and understand the risk state without reconstructing the whole campaign.

## Idempotence and Recovery

This checkpoint can be rerun. If any test fails, return to the owning plan. If a waiver expires or new code changes relevant modules, rerun this checkpoint before moving forward.

## Artifacts and Notes

- Evidence to fill in: focused test outputs.
- Evidence to fill in: GO/NO-GO decision.
- Evidence to fill in: waivers, if any.

## Interfaces and Dependencies

- Depends on Plans 006-008.
- Commands: `cargo test`, `cargo clippy`, `git status`.
- Files: root ledgers, receipts, release reports, generated corpus.
