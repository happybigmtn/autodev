# Specification: Parallel Loop And Lane Recovery

## Objective
Define the execution contract for `auto loop` and `auto parallel` so serial and tmux-backed workers claim dependency-ready tasks, preserve host-owned queue files, recover from partial completion, and produce truthful handoff evidence.

## Evidence Status

### Verified Facts

- `auto parallel` handles `status` without starting work in `src/parallel_command.rs:57-59`.
- Multi-worker parallel runs auto-launch tmux, default run state to `.auto/parallel`, checkpoint and sync before launch, and then run host orchestration in `src/parallel_command.rs:57-214` and `src/parallel_command.rs:223-251`.
- Parallel startup computes cargo build jobs from available parallelism and worker count, clamped from 1 to 4, in `src/parallel_command.rs:489-494`.
- Parallel worker environment may set lane-local `CARGO_TARGET_DIR`, and prompts tell workers not to override the host-provided target directory in `src/parallel_command.rs:401-475`.
- `run_parallel_status` prints repo root, branch, run root, tmux state, host processes, lanes, repo status, logs, frontier, and health summary in `src/parallel_command.rs:1510-1648`.
- Parallel worker prompts state that queue and review files are host-owned, workers must not push to the remote, must finish with local commits and clean worktrees, must use verification wrappers when available, and must not hand-edit receipts in `src/parallel_command.rs:5314`.
- Parallel reconciliation inspects task completion evidence after landing and marks tasks done only when evidence is complete; otherwise it keeps them partial in `src/parallel_command.rs:5476-5550`.
- `audit_parallel_completion_drift` demotes already-done tasks when repo-local completion evidence is missing in `src/parallel_command.rs:3453-3496`.
- `auto loop` has prompt-level rules to select the first actionable `[ ]` or `[~]`, skip `[!]`, mark incomplete evidence as `[~]`, and avoid stale background processes in `src/loop_command.rs:17-93`.
- `auto loop` parses pending and blocked task state in `src/loop_command.rs:260-323` and performs progress or commit checks in `src/loop_command.rs:351-380`.
- The planning corpus says verification evidence is stronger in parallel than loop and that loop receipt policy is still prompt-oriented in `genesis/ASSESSMENT.md:75`.

### Recommendations

- Keep `auto parallel` as the strongest completion-enforcement path and make loop converge toward the same `completion_artifacts` checks.
- Keep host-owned queue reconciliation centralized; workers should commit task changes and evidence, not edit queue state directly.
- Treat `auto parallel status` as the operator's first recovery command for tmux, lane, queue, and repo health.
- Preserve lane-local cargo target behavior until a better compile-lock strategy is proven.

### Hypotheses / Unresolved Questions

- It is unresolved whether `auto loop` should mechanically require receipts before marking tasks done or only when tasks contain executable verification commands.
- It is unresolved whether parallel should support a non-tmux execution mode for CI or headless environments.
- It is unresolved whether lane salvage and recovery should promote partial tasks to `[!]` when the blocker is external.

## Acceptance Criteria

- `auto parallel status` can be run on an existing checkout without launching new worker lanes.
- Parallel workers are never instructed to edit `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, or receipt JSON files directly.
- Host reconciliation marks a landed task `[x]` only when review handoff, verification evidence, and declared completion artifacts are all present.
- Host reconciliation marks a landed task `[~]` when code landed but completion evidence is incomplete.
- Already-completed tasks are demoted to `[~]` when later evidence drift proves review handoff, receipts, or artifacts are missing.
- Parallel lane prompts preserve narrative verification guidance separately from executable verification commands.
- Loop and parallel both skip `[!]` blocked tasks and do not schedule partial completion-path placeholders as primary work.

## Verification

- `cargo test parallel_command::tests::parse_loop_plan_tracks_ready_and_blocked_dependencies`
- `cargo test parallel_command::tests::audit_parallel_completion_drift_demotes_done_without_review_handoff`
- `cargo test parallel_command::tests::update_task_completion_in_plan_text_marks_partial_instead_of_dropping_block`
- `cargo test parallel_command::tests::parse_loop_plan_skips_partial_completion_path_placeholders`
- `cargo test -- --list | rg "(parallel_command|loop_command|completion_artifacts)"`
- Add loop regressions for receipt-aware completion once loop adopts shared completion evidence.

## Open Questions

- Should `auto loop` and `auto parallel` share one executor contract document emitted at runtime?
- Should partial tasks with external blockers be represented as `[~]` or `[!]` after host reconciliation?
- Should `auto parallel status` include receipt and review-handoff drift counts by default?
