# Shared Task Parser And Blocked-Task Preservation

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This plan makes task status, dependencies, verification fields, and artifacts mean the same thing across generation, loop, parallel, review, completion artifacts, and Symphony. Operators gain a more reliable task lifecycle: blocked tasks stay blocked, partial tasks stay visible, and generated plans are interpreted the same way by every executor.

The user can see it working when shared parser fixtures pass across statuses `[ ]`, `[~]`, `[!]`, and `[x]`, and `auto gen` no longer risks dropping blocked tasks during root plan merge.

## Requirements Trace

- R1: Preserve blocked `[!]` tasks during generated plan merge.
- R2: Preserve partial `[~]` tasks and their completion path.
- R3: Parse dependencies, spec refs, verification commands, owned paths, and artifacts consistently.
- R4: Provide shared fixtures used by generation, loop, parallel, review, completion, and Symphony callers.
- R5: Avoid a broad rewrite of `parallel_command.rs` in this slice.

## Scope Boundaries

This plan does not redesign the root plan format. It does not change task semantics unless required to preserve existing states. It does not split `parallel_command.rs`; it introduces or extracts shared parsing helpers with targeted call-site migrations.

## Progress

- [x] 2026-04-23: Duplicate task parsing identified across command modules.
- [x] 2026-04-23: Blocked-task preservation risk identified in generation merge parsing.
- [ ] 2026-04-23: Add shared parser fixtures.
- [ ] 2026-04-23: Migrate the smallest safe call sites.

## Surprises & Discoveries

Generation, loop, parallel, review, completion, and Symphony all care about task status, but not all parse the same statuses. That is a contract bug, not merely duplication.

## Decision Log

- Mechanical: `[!]` blocked tasks are open work and must not be dropped.
- Taste: Extract the shared parser incrementally instead of moving all task logic at once.
- Mechanical: Keep public markdown format stable.

## Outcomes & Retrospective

None yet. After implementation, record which call sites use the shared parser and which remain intentionally local.

## Context and Orientation

Relevant files:

- `src/generation.rs`: generated plan merge and corpus/generated-plan validation.
- `src/loop_command.rs`: sequential queue parsing.
- `src/parallel_command.rs`: dependency-aware task parsing and lane scheduling.
- `src/review_command.rs`: completed item harvesting.
- `src/completion_artifacts.rs`: task completion evidence.
- `src/symphony_command.rs`: issue/task parsing for Linear/Symphony.

Terms:

- Blocked task: a task marked `[!]`, meaning it should remain visible but not ready.
- Partial task: a task marked `[~]`, meaning some work landed but more is required.
- Completion path: explicit notes explaining how a partial task becomes complete.

## Plan of Work

Add a small shared task parser module or helper set with fixtures first. Include only the data fields needed by the first migration: status, title/id text, body block, dependencies, verification commands, and completion artifacts. Migrate generation merge parsing first to preserve blocked tasks. Then migrate loop/review or add adapter tests that prove their existing behavior matches shared fixtures.

## Implementation Units

Unit 1 - Shared task model and fixtures:

- Goal: Define common task status and block extraction.
- Requirements advanced: R1, R2, R3, R4.
- Dependencies: Plan 006 should be complete or compatible.
- Files to create or modify: new module such as `src/task_plan.rs`; `src/main.rs` module list if needed.
- Tests to add or modify: parser fixtures covering `[ ]`, `[~]`, `[!]`, `[x]`, dependencies, spec refs, verification, and artifacts.
- Approach: keep the model plain and repo-local; avoid overfitting to one command.
- Specific test scenarios: blocked task survives parse; partial task keeps completion path; checked task remains parseable for archive/review.

Unit 2 - Generation merge migration:

- Goal: Preserve blocked tasks during root plan sync.
- Requirements advanced: R1, R2.
- Dependencies: Unit 1.
- Files to create or modify: `src/generation.rs`, shared parser module.
- Tests to add or modify: merge regression for existing `[!]` task.
- Approach: replace generation's local task header parse for merge preservation with shared status parsing.
- Specific test scenarios: generated plan merge preserves blocked existing task not present in new plan; partial existing task remains partial.

Unit 3 - Loop/review compatibility:

- Goal: Prove loop and review interpret statuses consistently.
- Requirements advanced: R2, R3, R4.
- Dependencies: Unit 1.
- Files to create or modify: `src/loop_command.rs`, `src/review_command.rs`, or tests only if migration is too large.
- Tests to add or modify: queue parsing tests against shared fixtures.
- Approach: migrate one simple call site or add adapter tests that pin behavior before later migration.
- Specific test scenarios: loop treats `[~]` as pending; review harvest ignores unfinished tasks; `[!]` remains blocked.

Unit 4 - Parallel/Symphony adapter boundary:

- Goal: Avoid breaking complex scheduling while proving compatibility.
- Requirements advanced: R3, R5.
- Dependencies: Units 1-3.
- Files to create or modify: `src/parallel_command.rs`, `src/symphony_command.rs`, or tests only.
- Tests to add or modify: existing parser tests should import shared fixtures where safe.
- Approach: do not rewrite schedulers; add translation from shared task blocks only where low risk.
- Specific test scenarios: dependency-ready tasks still sort correctly; Symphony task parsing still handles multiline dependencies.

## Concrete Steps

From the repository root:

    rg -n "parse_.*task|TaskStatus|\\[!\\]|\\[~\\]" src/generation.rs src/loop_command.rs src/parallel_command.rs src/review_command.rs src/completion_artifacts.rs src/symphony_command.rs
    cargo test generation::tests::merge_generated_plan_with_existing_open_tasks
    cargo test loop_command::tests::parse_loop_queue
    cargo test parallel_command::tests::parse_loop_plan

After edits:

    cargo test task_plan::tests::
    cargo test generation::tests::merge_generated_plan_with_existing_open_tasks
    cargo test loop_command::tests::parse_loop_queue_treats_tilde_tasks_as_pending
    cargo test parallel_command::tests::parse_loop_plan_tracks_ready_and_blocked_dependencies
    cargo test symphony_command::tests::parse_tasks_extracts_pending_items_and_dependencies

Expected observation: shared parser tests cover every status and generation merge preserves blocked tasks.

## Validation and Acceptance

Acceptance requires:

- a shared parser or shared fixtures exist;
- blocked tasks are preserved by generated plan merge;
- partial tasks remain visible;
- existing loop, parallel, review, and Symphony parser tests still pass;
- any call sites not migrated are listed with rationale.

## Idempotence and Recovery

Parser migrations are risky. Make them one call site at a time. If a migration changes scheduling behavior unexpectedly, keep the shared fixtures and revert only that call-site migration, then record the incompatible behavior as a follow-up.

## Artifacts and Notes

Record before/after examples for a blocked task and a partial task. Include the names of parser functions replaced or intentionally left local.

## Interfaces and Dependencies

Interfaces used or changed:

- task markdown format in root plans;
- generation merge helpers;
- loop queue parsing;
- parallel dependency parsing;
- review harvesting;
- Symphony task parsing;
- completion artifact evidence parsing.
