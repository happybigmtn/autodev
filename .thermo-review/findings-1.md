# Thermo-Nuclear Findings — `src/parallel_command.rs`

Reviewed 10,653 lines. Counts: 1 P1, 6 P2, 6 P3, 5 P4, 3 P5, 2 P6, plus 3 bugs.

## P1 — File sprawl

One 10.6k-line file holds eleven concerns -> split into `parallel_command/`:

```
src/parallel_command/
  mod.rs            ~220   run_parallel, run_parallel_inline, ParallelStartupPrep, parallel_run_root, re-exports
  plan.rs           ~520   LoopTask(+Status), LoopPlanSnapshot, LoopQueueSnapshot, parse_loop_plan,
                           finalize_task, infer_lane_kind, update_task_completion_in_plan{,_text}
  worker_env.rs     ~220   LoopWorkerEnv, ParallelCargoTargetLayout, resolve_loop_worker_env, cargo-jobs helpers
  preflight.rs      ~320   ParallelPreflightReport/Check/Status/Needs, run_parallel_preflight, classify_*
  tmux.rs           ~330   TmuxLaunchStatus, launch/exists/session_name, setup_parallel_tmux_windows
  lane_repo.rs      ~430   clone_loop_lane_repo, inspect_lane_repo_progress, cherry_pick_lane_range, git_* primitives
  assignment.rs     ~620   ActiveLaneAssignment, LaneResumeCandidate, LaneRunConfig, metadata read/write/validate
  recovery_notes.rs ~280   *_recovery_note builders, environment_blocker_*, salvage record write/read
  landing.rs        ~640   land_parallel_lane_result, reconcile_*, propagate_lane_receipts, checkpoint_parallel_*
  prompt.rs         ~210   build_parallel_lane_prompt, render_default_parallel_prompt, embedded prompt constants
  status.rs         ~480   run_parallel_status, parallel_status_safety_verdict, receipt_drift_status_summary
  scheduling.rs     ~420   ready/prioritize task selection, ParallelBlockerDetail/Kind, ParallelUnblockCandidate
  orchestrator.rs   ~900   run_serial_loop, run_parallel_loop, spawn_parallel_lane_attempt, ParallelEventLogger
  tests/                   split the 2.8k-line test module to mirror the above
```

Only shared types: `LoopTask`/`LoopPlanSnapshot`/`LoopQueueSnapshot` (plan.rs) and `ActiveLaneAssignment`/`LaneRunConfig` (assignment.rs). Cross-module coupling low.

## P2 — Spaghetti

- `run_parallel_loop` (2221-3256) is a single ~1,035-line function, cyclomatic complexity >50. Extract `dispatch_ready_lanes`, `dispatch_unblock_candidate`, `handle_lane_attempt_result`, `handle_nonzero_exit`/`handle_clean_exit`.
- 2752-3252: the `LaneLandingOutcome` match is written twice (per exit path), 5 levels deep -> one `land_and_record(...)`.
- 6069-6193: `nudge_lingering_committed_lanes` resets the same two fields in seven places -> extract `nudge_one_lane`.
- 1472-1657: `run_parallel_status` ~185 lines mixing fs scan + classification + 4 render kinds.
- 4399-4491: `refresh_parallel_plan` 7-deep Linear-drift ladder -> extract `maybe_auto_sync_linear`.
- 6824-6936: `land_parallel_lane_result` hides a `loop {...break}` state machine with two mutable flags.

## P3 — Code-judo

- 2754-2812 vs 3131-3188: `LaneLandingOutcome::Landed` handler duplicated verbatim -> `record_landed(...)`, deletes ~110 lines.
- 5846-5889: `harvest_resumable_lane_results` rebuilds `LaneResumeCandidate` from `ActiveLaneAssignment` twice -> extract converter.
- 2425-2492: unblock path re-parks tasks in three spots -> extract `re_park`.
- 6768-6822: `LaneScopeBudget` + `render_lane_scope_budget` + `lane_scope_budget` all `#[allow(dead_code)]` -> DELETE (or wire in). Do not keep the lint shim.
- 1428/4810/4824: `command_stdout`/`tmux_stdout`/`run_tmux` are the same checked-output pattern -> one `checked_output` helper.
- 1968-1998: `last_parallel_stop_state` regex-scrapes a log line the host itself formatted -> use a JSON sidecar.

## P4 — Types & boundaries

- 938-944: `LaneWorkerMetadata.harness: String` only ever "claude"/"codex" -> enum. `command: Vec<String>` is redundant (derivable).
- 7199-7213: `write_clean_no_commit_verdict` takes `verdict: &str` with literal callers -> 2-variant enum.
- 6695 & 5179: env reads buried in leaf functions -> read at `run_parallel` boundary, thread as config.
- `anyhow` errors used for control flow: `landing_error_suggests_dirty_canonical_worktree`, `is_linear_usage_limit_error`, `environment_blocker_reason` all string-match error text -> typed `LandingError`.
- 1013-1056: two logging mechanisms (`ParallelEventLogger` vs raw `println!`) coexist.

## P5 — Layering

- Orchestration reaches straight into raw git (`Command::new("git")` at 6832/7000). `lane_repo.rs` should own all git invocation.
- `build_parallel_lane_prompt` inlines ~2,000 chars of product doctrine -> `prompt.rs` constants.
- Filesystem layout knowledge duplicated across ~8 sites -> a `LaneLayout` type.

## P6 — Cosmetic

- 7832-10653: `#[cfg(test)] mod tests` is 2,821 lines. Move into per-submodule test modules.
- 7844-7877: `use super::{...}` test block lists ~90 symbols.

## Bugs

- [low] 286: `build_iteration_prompt` indexes `queue.pending_ids[0]` unconditionally.
- [low] 7008-7023: `propagate_lane_receipts` stamps receipt fingerprint from canonical's *current* porcelain status, possibly including transient state.
- [latent] 4515: `refresh_parallel_plan_or_last_good` binds `last_good_plan` with `let _` and always returns the error — name promises fallback the body doesn't implement.

## Top refactor moves

1. Split into `parallel_command/` module tree — SAFE.
2. Extract `run_parallel_loop`'s post-`join_next` handling into per-outcome handlers — RISKY (core scheduler).
3. Unify duplicated `LaneLandingOutcome::Landed` handler into `record_landed` — SAFE-ish (~110 lines deleted).
4. Introduce `lane_repo.rs` git-IO layer — RISKY (large surface, 1:1 substitutions).
5. Make `harness` + verdict strings enums — RISKY (changes `assignment.json` schema).
6. Extract `nudge_one_lane` — SAFE.
7. Delete the `#[allow(dead_code)]` `LaneScopeBudget` trio — SAFE.
8. Move inline product-doctrine prompt text into `prompt.rs` — SAFE.
9. Replace `live.log` regex round-trip with JSON sidecar — RISKY.
10. Rename `refresh_parallel_plan_or_last_good`, drop unused param — SAFE.
