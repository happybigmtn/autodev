# Specification: Scheduler Completion And Lane Resume

## Objective

Make `auto parallel`, `auto loop`, and related status surfaces dispatch only current, dependency-ready, evidence-backed work, and make lane resume reject stale task assignments.

## Source Of Truth

- Runtime owners: `src/parallel_command.rs`, `src/loop_command.rs`, `src/task_parser.rs`, `src/completion_artifacts.rs`, `src/verification_lint.rs`, `src/linear_tracker.rs`, `src/symphony_command.rs`.
- Queue owners: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `WORKLIST.md`, `.auto/parallel/**`, `.auto/symphony/verification-receipts/*.json`.
- UI consumers: `auto parallel`, `auto parallel status`, tmux panes, `.auto/parallel/live.log`, lane prompts, `auto loop`, `auto review`, `auto steward`, Linear sync status.
- Generated artifacts: lane repos under `.auto/parallel/lanes/*`, lane `task-id`, worker pid files, host/lane stdout/stderr logs, salvage markdown, queue sync commits, verification receipts.
- Retired/superseded surfaces: last-good queue fallback as default production dispatch, lane resume based only on task id, checked-row completion that conflicts with evidence policy, and stale lane recovery shown as live work.

## Evidence Status

Verified facts grounded in code or primary repo files:

- `parallel_command` protects shared queue files from lane edits and tells workers the host owns queue reconciliation, verified by `rg -n "SHARED_QUEUE_FILES|host owns queue reconciliation|Do not edit these shared queue files" src/parallel_command.rs`.
- `auto parallel status` prints repo root, branch, run root, tmux state, host pids, lane state, stale recovery, latest lane logs, and health summaries, verified by `rg -n "run_parallel_status|tmux:|host pids|lanes:|recovery: stale" src/parallel_command.rs`.
- `refresh_parallel_plan_or_last_good` currently warns and continues from the last good queue snapshot if refresh fails, verified by `rg -n "refresh_parallel_plan_or_last_good|last good queue snapshot" src/parallel_command.rs`.
- Lane task identity is persisted in a `task-id` file, verified by `rg -n "LANE_TASK_ID_FILE|read_lane_task_id|write_lane_task_id" src/parallel_command.rs`.
- Completion evidence currently requires review handoff, verification receipt when executable commands exist, declared artifacts, and audit-finding closure, verified by `rg -n "is_fully_evidenced|missing REVIEW.md handoff|verification_receipt_present|missing_completion_artifacts" src/completion_artifacts.rs`.
- `tests/parallel_status.rs` already covers status with no lanes and stale lane recovery, verified by `rg -n "parallel_status_reports_health_when_run_root_has_no_lanes|parallel_status_reports_stale_lane_recovery_without_live_host" tests/parallel_status.rs`.

Recommendations for the intended system:

- Production dispatch must fail closed when current plan refresh fails; last-good fallback should require explicit recovery mode.
- Lane assignment metadata must include task id, task body hash, dependency list hash, verification text hash, base commit, branch, and assignment hash.
- `auto parallel status` should surface a single launch/resume/land safety verdict derived from the same evidence and lane metadata checks that dispatch uses.
- Checked rows with accepted archive/direct-review evidence need an explicit evidence class rather than demotion by convention.

Hypotheses / unresolved questions:

- The lane-assignment metadata format is not settled; JSON under each lane is likely better than expanding `task-id`.
- Linear usage-limit fallback may remain acceptable for status, but production dispatch should state when Linear freshness is skipped.
- The current empty `REVIEW.md` convention is intentional in this checkout, but code needs evidence-class support before treating it as generally complete.

## Runtime Contract

- `task_parser` owns markdown row parsing for id, status, dependencies, completion path, artifacts, and verification text.
- `completion_artifacts` owns evidence classification and must say whether a task is fully evidenced, locally repairable, external/live follow-up, or blocked.
- `parallel_command` owns dependency-ready selection, lane creation, lane resume, lane landing, stale recovery status, and host queue updates.
- `loop_command` owns serial task selection and must share the same eligibility rules.
- If current plan refresh fails, if assignment hashes mismatch, if dependencies changed, or if verification requirements changed, production dispatch must fail closed or enter explicit recovery.

## UI Contract

- `auto parallel status` must clearly distinguish live host work, stale lane residue, dependency blockers, external blockers, and safe launch state.
- Lane prompts must render host-parsed executable verification commands and narrative guidance separately; workers must not treat narrative prose as shell.
- Terminal output must not mark `[x]` unless the runtime evidence gate says the task is complete or a named evidence class/waiver is accepted.
- Status UI must not reimplement dependency parsing; it must display `task_parser` and scheduler results.
- Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- `.auto/parallel/live.log`, `host.stdout.log`, `host.stderr.log`.
- `.auto/parallel/lanes/lane-*/task-id`, future assignment metadata JSON, `worker.pid`, `stdout.log`, `stderr.log`, and lane repo commits.
- `.auto/parallel/salvage/*.md`.
- Host queue sync commits and `.auto/symphony/verification-receipts/*.json`.
- Optional Linear sync artifacts through `auto symphony sync`.

## Fixture Policy

- Synthetic run roots, lane dirs, fake pids, stale cherry-pick states, and fixture root plans belong in tests.
- Production scheduler code must inspect the live root plan, live lane dirs, live git status, and live receipts; it must not rely on fixture queue data.
- Fixture receipts may test parser behavior but cannot satisfy real task completion.

## Retired / Superseded Surfaces

- Retire default last-good queue dispatch for production mode.
- Retire lane resume keyed only by task id.
- Retire stale `.auto/parallel` recovery notes that make completed root work look in flight.
- Retire prose-only dependency hints that are not represented in `Dependencies:`.

## Acceptance Criteria

- If `IMPLEMENTATION_PLAN.md` cannot be refreshed or parsed, `auto parallel` does not dispatch new lanes unless explicit recovery mode is selected.
- A changed task body, dependency list, verification text, base commit, branch, or assignment hash prevents lane resume and prints the stale field.
- `auto parallel status` prints one safety verdict covering launch, resume, and landing readiness.
- A checked-row/empty-review fixture is classified by explicit evidence class, not blindly demoted or blindly accepted.
- `auto loop` and `auto parallel` choose the same ready task set from the same fixture plan.
- Stale lane recovery with no host pid/tmux worker is shown as stale residue, not active progress.

## Verification

- `cargo test task_parser::tests`
- `cargo test completion_artifacts::tests`
- `cargo test loop_command::tests`
- `cargo test parallel_command::tests`
- `cargo test --test parallel_status`
- `rg -n "refresh_parallel_plan_or_last_good|LANE_TASK_ID_FILE|build_parallel_lane_prompt|ready_parallel_tasks|inspect_task_completion_evidence" src/parallel_command.rs src/completion_artifacts.rs`
- `cargo run --quiet -- parallel status`

## Review And Closeout

- A reviewer runs a failing refresh fixture and proves no lane is spawned by checking `.auto/parallel/lanes`.
- A reviewer mutates one assignment field in a lane fixture and proves resume fails with the exact field name.
- Grep proof must show there is no production path that calls last-good fallback without the explicit recovery flag or mode.
- Closeout includes live `auto parallel status` output summarized in the review artifact, including whether remaining degraded state is stale, blocked, or safe.

## Open Questions

- What should the explicit recovery flag be named?
- Should evidence classes live in `CompletionGapKind`, root markdown fields, receipt JSON, or all three?
- Should stale lane cleanup be a status suggestion only, or an explicit `auto parallel prune-stale` action?
