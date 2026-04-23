# Execution Contract Checkpoint Gate

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This checkpoint decides whether verification, task parsing, and backend-policy work are coherent enough to move into first-run DX and CI changes. Users gain a pause point where evidence contracts are tested before onboarding teaches them as reliable.

The gate is visible when known false-proof examples are covered, blocked tasks are preserved, parser behavior is consistent, and backend invocation risks are mapped.

## Requirements Trace

- R1: Verification command hardening covers known false-proof cases.
- R2: Shared task parsing or fixtures cover all task statuses.
- R3: Blocked tasks are preserved during generated plan merge.
- R4: Backend invocation policy research is complete before runner refactors.
- R5: Remaining execution-contract risks are documented before DX/CI work.

## Scope Boundaries

This gate does not implement new parser or backend changes. It verifies Plans 006 through 008 and decides whether Plan 010 may proceed.

## Progress

- [x] 2026-04-23: Gate created after execution-contract phase.
- [ ] 2026-04-23: Run after Plans 006, 007, and 008.
- [ ] 2026-04-23: Record go/no-go decision.

## Surprises & Discoveries

None yet. Use this section to record compatibility issues between shared parsing and existing parallel/Symphony behavior.

## Decision Log

- Mechanical: Do not document a first-run path around unproved evidence semantics.
- Taste: Allow backend unification to remain research if parser and verification fixes already reduce risk.
- User Challenge: If the backend policy research recommends dangerous-mode default changes, pause for operator approval.

## Outcomes & Retrospective

None yet. At gate time, record whether Phase 2 passed and which execution-contract risks remain.

## Context and Orientation

This gate depends on:

- Plan 006: verification command and receipt hardening.
- Plan 007: shared task parser and blocked-task preservation.
- Plan 008: backend invocation policy research.

Relevant files:

- `src/completion_artifacts.rs`;
- `src/generation.rs`;
- `src/loop_command.rs`;
- `src/parallel_command.rs`;
- `src/review_command.rs`;
- `src/symphony_command.rs`;
- backend modules and command spawn paths.

## Plan of Work

Run targeted tests for completion artifacts and task parsing. Inspect the backend policy research artifact. Confirm root docs record any default-changing proposals as user challenges. Only then begin first-run and CI work.

## Implementation Units

Unit 1 - Verify receipt hardening:

- Goal: Confirm known false-proof cases are no longer accepted as routine proof.
- Requirements advanced: R1.
- Dependencies: Plan 006.
- Files to create or modify: none unless evidence is missing.
- Tests to add or modify: none at gate time.
- Approach: run targeted completion artifact tests.
- Specific test scenarios: malformed cargo command, zero-test filter, directory grep, and shell interpreter examples are covered.

Unit 2 - Verify task parser behavior:

- Goal: Confirm statuses and dependencies are interpreted consistently.
- Requirements advanced: R2, R3.
- Dependencies: Plan 007.
- Files to create or modify: none unless evidence is missing.
- Tests to add or modify: none at gate time.
- Approach: run shared parser, generation, loop, parallel, review, and Symphony parser tests.
- Specific test scenarios: `[!]` blocked task survives generation merge; `[~]` task remains pending; dependencies still schedule correctly.

Unit 3 - Review backend policy research:

- Goal: Confirm future backend refactors have a safe implementation path.
- Requirements advanced: R4, R5.
- Dependencies: Plan 008.
- Files to create or modify: root planning docs if decisions need promotion.
- Tests to add or modify: Test expectation: none -- research review only.
- Approach: inspect spawn inventory and classify default changes.
- Specific test scenarios: every direct provider spawn path is accounted for.

## Concrete Steps

From the repository root:

    cargo test completion_artifacts::tests::
    cargo test generation::tests::merge_generated_plan_with_existing_open_tasks
    cargo test loop_command::tests::parse_loop_queue
    cargo test parallel_command::tests::parse_loop_plan
    cargo test review_command::tests::extracts_completed_plan_items_and_leaves_unfinished_tasks
    cargo test symphony_command::tests::parse_tasks_extracts_pending_items_and_dependencies
    rg -n "dangerous|sandbox|approval|Command::new|codex exec|claude -p" IMPLEMENTATION_PLAN.md specs genesis/plans/008-backend-invocation-policy-research.md

Expected observation: targeted parser and evidence tests pass, and backend default-change questions are explicit.

## Validation and Acceptance

Gate passes only if:

- verification false-proof fixtures pass;
- blocked task preservation is covered;
- task parsing changes did not break existing loop/parallel/review/Symphony tests;
- backend invocation research maps all current paths;
- any operator-sensitive default changes are listed as user challenges.

## Idempotence and Recovery

This gate can be rerun after any Phase 2 fix. If a parser test fails, return to Plan 007. If a receipt fixture fails, return to Plan 006. If backend inventory is incomplete, keep Plan 008 open and do not start runner refactors.

## Artifacts and Notes

Record concise test output and the path to the backend policy artifact. Note any call sites intentionally left local.

## Interfaces and Dependencies

This checkpoint depends on completion artifact parsing, task parser behavior, backend spawn inventory, and root planning docs.
