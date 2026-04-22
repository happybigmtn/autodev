# Specification: Execution pipeline — `auto loop` and `auto parallel`

## Objective

Pin down the plan-queue burn-down contract used by `auto loop` (single-worker, one task at a time) and `auto parallel` (multi-lane tmux-backed executor). Both commands must parse the same task queue markers, honor the same rebase-before-work / rebase-before-push posture, and require executable-`Verification:` tasks to ship a structured verification receipt rather than prose handoffs alone. `auto parallel` must launch, reconcile, and tear down tmux lanes without losing commits.

## Evidence Status

### Verified facts (code)

- `src/main.rs:63` declares `Loop`; `src/main.rs:65` declares `Parallel`.
- `LoopArgs` defaults (per `src/main.rs:575-660`): model `gpt-5.4`, reasoning effort `xhigh`, Codex binary `codex`, unlimited iterations by default.
- `ParallelArgs` defaults include five workers (`--max-concurrent-workers`, default 5) per `README.md:42` and `corpus/SPEC.md` table.
- Task queue markers parsed identically across `loop_command.rs`, `parallel_command.rs`, `review_command.rs`, `generation.rs` (per `corpus/SPEC.md` §"Task queue protocol"):
  - `- [ ]` pending and runnable,
  - `- [!]` blocked (skipped by `loop`),
  - `- [x]` completed,
  - `- [~]` partially done / historical gap.
- `auto parallel` host reconciliation requires a verification receipt at `.auto/symphony/verification-receipts/<task_id>.json` (shape validated by `src/completion_artifacts.rs:13-20` and path referenced in `completion_artifacts.rs:123`) before marking an executable-`Verification:` task completed; prose-only handoff leaves the task `[~]` per `corpus/SPEC.md` item 10.
- `src/completion_artifacts.rs:13-20` `TaskCompletionEvidence` fields: `has_review_handoff`, `verification_receipt_path`, `verification_receipt_present`, `verification_receipt_status`, `declared_completion_artifacts`, `missing_completion_artifacts`.
- `completion_artifacts.rs` has 13-16 tests covering receipt validation and narrative-only rejection (corpus ASSESSMENT; subagent confirmed 13 tests).
- Rebase-before-work and rebase-before-push both rely on `util::sync_branch_with_remote` (`src/util.rs:197`) and `util::push_branch_with_remote_sync` (`src/util.rs:239`).
- `parallel_command.rs` implements the real tmux lane orchestration (`corpus/SPEC.md` §"What is broken or half-built"); tmux helpers in `codex_exec.rs` are dead code and not invoked by `auto parallel`.
- Test counts (corpus ASSESSMENT): `parallel_command.rs` ~57-58 tests, `loop_command.rs` ~14-15 tests.

### Verified facts (docs)

- `README.md:42-45` describes the tmux session as `<repo>-parallel`, auto-launched when `auto parallel` runs outside tmux.
- `README.md:44-45` documents `auto parallel status` summarizes active tmux session, host process, lane task IDs, lane git state, worker PIDs, and latest lane log lines.
- `README.md:561-565` documents loop task markers match code (corpus verified).

### Recommendations (corpus)

- `corpus/PLANS.md` explicitly defers refactor of `parallel_command.rs` (7,853 LOC) until Plans 005 and 009 gates are satisfied; this spec must not assume internal surgery.
- `corpus/plans/011-integration-smoke-tests.md` proposes end-to-end smoke tests for the happy path; those tests will sit above this spec, not replace it.

### Hypotheses / unresolved questions

- Exact tmux session naming for non-standard repo paths (for example, trailing-slash or whitespace) is not source-verified here.
- Recovery semantics when a lane's `git worktree` is left dirty by a crash are not fully nailed down in this pass.

## Acceptance Criteria

- `auto loop` reads `IMPLEMENTATION_PLAN.md` top-to-bottom, picks the first `- [ ]` task, and runs exactly one implementation pass per iteration.
- `auto loop` skips tasks marked `- [!]` without removing them from the queue.
- `auto loop` never marks a task `- [x]` unless the worker returns success and the task's declared completion evidence (review handoff + optional verification receipt) is present.
- `auto loop` with `--max-iterations N` exits after N successful iterations.
- `auto loop` invokes `util::sync_branch_with_remote` before each iteration begins and before pushing at the end.
- `auto parallel` runs with five lanes by default; `--max-concurrent-workers` overrides.
- Outside an existing tmux session, `auto parallel` detaches into a `<repo>-parallel` tmux session and prints the session name.
- Each lane uses its own git worktree; lane worktrees are not reused across concurrent lanes.
- `auto parallel` host reconciliation reads `.auto/symphony/verification-receipts/<task_id>.json` for every task whose `Verification:` block contains executable steps.
- A task whose plan body declares an executable `Verification:` step but lacks a readable JSON receipt is marked `- [~]` (partial) and does not flip to `- [x]`.
- `auto parallel status` prints: active tmux session name, host process PID, lane task IDs, lane git state (branch, worktree path, dirty-count summary), worker PIDs, and tail of the latest lane log lines.
- All lane log files live under `.auto/parallel/<session>/lane-*/stdout.log` (lane log tailing path).
- Checkpoint safety runs before both commands start if tracked dirty state exists outside the `CHECKPOINT_EXCLUDE_RULES` set.
- Missing `codex` binary on `PATH` yields a non-zero exit with a named-dependency error.
- Missing `tmux` binary on `PATH` yields a non-zero exit on `auto parallel` when the command needs to create a session.

## Verification

- `cargo test -p autodev loop_command` and `cargo test -p autodev parallel_command` pass (~72 combined tests today).
- `cargo test -p autodev completion_artifacts` passes (13-16 tests) — this locks the verification-receipt contract.
- Fixture test: plan with a `- [ ]` task whose body has `Verification:` pointing at an executable script; `auto loop` runs with stubbed Codex and expected receipt path populated; assert task flips to `- [x]`. Then repeat without a receipt; assert task stays `- [~]` or `- [ ]`.
- Manual smoke: `auto parallel` on a five-task fixture inside an existing tmux session; assert five lane directories appear under `.auto/parallel/<session>/` and `auto parallel status` emits all required fields.
- Grep `src/loop_command.rs` and `src/parallel_command.rs` for the four task markers to verify a shared regex or constant is used (follow-on: consolidate per `corpus/plans/007-shared-util-extraction.md`).

## Open Questions

- Should `auto loop` attempt `auto_checkpoint_if_needed` per iteration or only once at run start? Code behavior should be documented clearly in the README.
- What is the user-facing error shape when a lane worktree exists but its branch has diverged from `origin/<branch>` — does the lane retry, abort, or escalate to the host? Not explicit in this spec.
- Should `auto parallel status` include quota-rotation history for the session, or stay lane-only?
- Are lane log files rotated/pruned, or kept indefinitely under `.auto/parallel/`? (`util::prune_old_entries` exists; use is not source-verified here.)
- When `auto loop` hits the Claude futility threshold (exit 137 from `codex_stream.rs`), does the current task stay `- [ ]` or flip to `- [!]`? See related spec on agent backends.
