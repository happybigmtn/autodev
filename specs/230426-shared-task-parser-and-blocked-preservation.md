# Specification: Shared Task Parser And Blocked Preservation

## Objective
Unify implementation-plan task parsing across generation, loop, parallel, review, completion evidence, and Symphony so task status, dependencies, verification commands, completion artifacts, and blocked work mean the same thing in every executor.

## Evidence Status

### Verified Facts

- The planning corpus explicitly says task status, dependencies, verification fields, and artifacts should mean the same thing across generation, loop, parallel, review, completion artifacts, and Symphony in `genesis/plans/007-shared-task-parser-and-blocked-task-preservation.md:7`.
- The same plan identifies generation, loop, parallel, review, completion, and Symphony as consumers that currently care about task status but do not parse every status the same way in `genesis/plans/007-shared-task-parser-and-blocked-task-preservation.md:32-53`.
- Generation plan merge preserves parsed unchecked tasks by ID in `src/generation.rs:3335-3356`.
- Generation task-header parsing recognizes `[ ]`, `[~]`, `[x]`, and `[X]`, but not `[!]`, in `src/generation.rs:3494-3509`.
- Parallel task status is represented as `Pending`, `Blocked`, `Partial`, and `Done` in `src/parallel_command.rs:513-518`.
- Parallel task snapshots include pending tasks, non-placeholder partial tasks, and blocked tasks in `src/parallel_command.rs:541-555`.
- Parallel readiness excludes unresolved dependencies, in-flight tasks, blocked tasks, partial tasks, and completion-path placeholders in `src/parallel_command.rs:558-624`.
- Parallel parser coverage includes blocked dependencies, deferred/not-shipped rows, `none` dependencies, parallelism-note filtering, partial tasks as unfinished dependencies, and completion-path placeholders in `src/parallel_command.rs:7296-7538`.
- Symphony task parsing recognizes `[ ]`, `[!]`, `[~]`, `[x]`, and `[X]` in `src/symphony_command.rs:1796-1825`.
- Completion-artifact parsing extracts `Verification:` and `Completion artifacts:` sections from task markdown in `src/completion_artifacts.rs:242-310`.
- Review harvest moves completed `IMPLEMENTATION_PLAN.md` rows into `REVIEW.md` before review work starts in `src/review_command.rs:78-120` and `src/review_command.rs:447-591`.

### Recommendations

- Introduce a shared parser module with one task struct for ID, title, status, markdown body, dependencies, verification plan, completion artifacts, spec refs, and completion-path target.
- Migrate generation merge first because the current `[!]` gap can lose blocked tasks during plan regeneration.
- Migrate loop, parallel, completion artifacts, review harvest, and Symphony adapters incrementally behind shared fixtures.
- Preserve command-specific behavior in adapters instead of forcing every executor to run the same scheduling algorithm.

### Hypotheses / Unresolved Questions

- It is unresolved whether `Completion path:` should remain parser-level metadata or parallel-only scheduling metadata.
- It is unresolved whether review harvest should parse the same dependency and artifact fields or only completed task rows.
- It is unresolved whether task IDs must be globally unique across all plan sections or only unique among unfinished tasks.

## Acceptance Criteria

- A shared parser fixture containing `[ ]`, `[~]`, `[!]`, `[x]`, and `[X]` tasks yields the same status values for generation, loop, parallel, review, completion evidence, and Symphony adapters.
- Existing blocked `[!]` tasks survive generated plan merge when no generated task with the same ID replaces them.
- Partial `[~]` tasks remain unfinished dependencies for ready-task selection.
- `Dependencies: none` parses as an empty dependency set in every adapter.
- Multiline dependencies and narrative parallelism notes do not create false dependency IDs.
- Completion-path placeholder tasks are visible in parsed output but are not scheduled as ready implementation tasks.
- Existing targeted parser tests for parallel and Symphony still pass after the shared parser migration.

## Verification

- `cargo test parallel_command::tests::parse_loop_plan_tracks_ready_and_blocked_dependencies`
- `cargo test parallel_command::tests::parse_loop_plan_treats_partial_tasks_as_unfinished_dependencies`
- `cargo test parallel_command::tests::parse_loop_plan_treats_none_dependencies_as_empty`
- `cargo test symphony_command::tests::parse_tasks_extracts_pending_items_and_dependencies`
- `cargo test symphony_command::tests::parse_tasks_recognizes_partial_items`
- `cargo test review_command::tests::harvest_completed_plan_items_flows_through_completed_queue`
- Add and run a generation regression for `[!]` preservation during plan merge.

## Open Questions

- Should blocked task preservation include blocked tasks without stable backtick IDs?
- Should the shared parser be public within the crate or private with command-specific wrapper functions?
- Should parser errors be strict and stop execution, or should malformed tasks be preserved as blocked work with diagnostics?
