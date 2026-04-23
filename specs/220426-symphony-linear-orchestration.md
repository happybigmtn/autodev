# Specification: `auto symphony` — Linear sync and runtime orchestration

## Objective

Keep `auto symphony` the operator's bridge between `IMPLEMENTATION_PLAN.md` and a Linear.app project: `sync` pushes unchecked plan items into Linear and detects drift categories (`missing`, `stale`, `terminal`, `completed_active`); `workflow` renders a repo-specific `WORKFLOW.md` that the Symphony runtime consumes; `run` launches the foreground Symphony dashboard to execute Linear-backed issues in parallel. The subcommand surface, flag defaults, and GraphQL query set must not regress silently.

## Evidence Status

### Verified facts (code)

- `src/main.rs:95` declares `Symphony`; `SymphonySubcommand` at `src/main.rs:111-118` covers `Sync`, `Workflow`, `Run`.
- `SymphonySyncArgs` (`src/main.rs:121-149`): `--repo-root`, `--project-slug`, `--todo-state` (default `"Todo"`), `--planner-model` (default `"gpt-5.5"`), `--planner-reasoning-effort` (default `"high"`), `--codex-bin` (default `"codex"`), `--no-ai-planner`.
- `SymphonyWorkflowArgs` (`src/main.rs:151-200+`): `--repo-root`, `--project-slug`, `--output`, `--workspace-root`, `--base-branch`, `--max-concurrent-agents` (default `1`), `--poll-interval-ms` (default `5_000`), `--model` (default `"gpt-5.5"`), `--reasoning-effort` (default `"high"`), `--in-progress-state` (default `"In Progress"`), `--done-state` (default `"Done"`), `--blocked-state` (optional).
- `SymphonyRunArgs` (`src/main.rs:201-286`) includes `--symphony-root` with no hardcoded default; `auto symphony run` resolves an explicit path first, then `AUTODEV_SYMPHONY_ROOT`, and fails with an actionable error when both are unset (`src/symphony_command.rs:1672-1682`).
- `symphony_command.rs` is ~3,062 LOC (corpus ASSESSMENT). Tests ~18 covering GraphQL query construction and state parsing.
- Host-side `auto parallel` GraphQL tracker queries live in `src/linear_tracker.rs`:
  - `FETCH_PROJECT_QUERY` (`linear_tracker.rs:16`) for project/team/state lookup.
  - `FETCH_PROJECT_ISSUES_QUERY` (`linear_tracker.rs:42`) paginates issues.
  - `UPDATE_ISSUE_STATE_MUTATION` (`linear_tracker.rs:62`).
  - `ARCHIVE_ISSUE_MUTATION` (`linear_tracker.rs:76`).
- `auto symphony sync` GraphQL queries live in `src/symphony_command.rs`:
  - `FETCH_PROJECT_QUERY` (`symphony_command.rs:30`) for project/team/state lookup.
  - `FETCH_PROJECT_ISSUES_QUERY` (`symphony_command.rs:56`) paginates issues with archived, priority, and blocker-relation fields.
  - `CREATE_ISSUE_MUTATION` (`symphony_command.rs:96`).
  - `UPDATE_ISSUE_MUTATION` (`symphony_command.rs:143`).
  - `UPDATE_ISSUE_AND_STATE_MUTATION` (`symphony_command.rs:186`).
  - `ARCHIVE_ISSUE_MUTATION` (`symphony_command.rs:231`).
  - `UNARCHIVE_ISSUE_MUTATION` (`symphony_command.rs:239`).
  - `DELETE_RELATION_MUTATION` (`symphony_command.rs:247`).
  - `CREATE_RELATION_MUTATION` (`symphony_command.rs:255`).
- `src/symphony_command.rs` also renders workflow prompt examples for external Symphony worker use via `linear_graphql`: `IssueContext`, `UpdateIssueState`, and `AddComment` (`symphony_command.rs:2152-2186`). These are prompt contract, not direct Rust HTTP egress.
- Drift-category fields (`linear_tracker.rs:128-131`):
  - `missing_task_ids`: in plan, not in Linear (`linear_tracker.rs:230`).
  - `stale_task_ids`: content/title drifted (`linear_tracker.rs:244`).
  - `terminal_task_ids`: Linear issue in terminal state (`linear_tracker.rs:238`).
  - `completed_active_task_ids`: Linear issue closed but plan row still active (`linear_tracker.rs:256`).
- `main.rs` has two tests covering Symphony `--sync-first` argument validation (`corpus/ASSESSMENT.md` §"Test gaps").

### Verified facts (docs)

- `README.md:536` points at `auto symphony` from the `auto loop` description but does not list it in the top-level inventory. Drift documented in `corpus/SPEC.md`.
- Symphony artifact shape (rendered `WORKFLOW.md`) is operator-consumed; `corpus/SPEC.md` does not detail the exact schema, and the README does not describe it.

### Recommendations (corpus)

- Add `auto symphony` to the README inventory and write a short purpose / flag summary (`corpus/plans/002-readme-command-inventory-sync.md`).
- Keep the local Symphony Elixir root separate from rendered workflow output. `WORKFLOW.md` is repo runtime configuration; `--symphony-root` / `AUTODEV_SYMPHONY_ROOT` points at the local Symphony checkout.
- `corpus/DESIGN.md` §"What we are explicitly not designing for" excludes multi-user concurrency; Linear integration stays single-operator-per-repo.

### Hypotheses / unresolved questions

- Whether `--no-ai-planner` truly produces deterministic output (no LLM calls) is asserted by the arg doc-comment but not verified in this pass.
- Whether `auto symphony sync` is idempotent when run twice with no plan changes is not source-verified.
- Token handling for Linear's API (likely `LINEAR_API_KEY` env) is not called out in the README and not audited here.

## Acceptance Criteria

- `auto symphony --help` lists the three subcommands: `sync`, `workflow`, `run`.
- `auto symphony sync` reads the repo's `IMPLEMENTATION_PLAN.md`, computes drift against a Linear project, and updates or creates issues to reflect plan state using `UPDATE_ISSUE_STATE_MUTATION` and the project-fetch queries.
- `auto symphony sync --no-ai-planner` runs without invoking Codex (planner bypass) and uses deterministic dependency parsing only.
- `auto symphony sync` defaults: `--todo-state = "Todo"`, `--planner-model = "gpt-5.5"`, `--planner-reasoning-effort = "high"`, `--codex-bin = "codex"`.
- `auto symphony workflow` renders a `WORKFLOW.md` file (to `--output` or a default path) containing `workspace_root`, `poll_interval_ms`, `model`, `reasoning_effort`, `in_progress_state`, `done_state`, `blocked_state` fields; defaults match `SymphonyWorkflowArgs`.
- `auto symphony run` launches the Symphony runtime in the foreground, loading the rendered workflow; exits with the runtime's exit code.
- Drift detection marks a Linear issue `terminal` when its state is one of the configured terminal states (`Done`, archived) and the plan row is still `- [ ]`.
- Drift detection marks a Linear issue `completed_active` when Linear reports done/closed but the plan row is still active.
- Drift detection marks `missing` when a plan row has no Linear issue at all.
- Drift detection marks `stale` when title / body / metadata drifted between Linear and plan.
- Linear API egress stays GraphQL-only with no ad-hoc REST calls. Direct Rust egress is intentionally split between host-side `linear_tracker.rs` operations and `auto symphony sync` operations in `symphony_command.rs`; `docs/decisions/symphony-graphql-surface.md` names the current operation contract.
- Symphony runs do not retry requests silently; failures surface to the operator.
- Missing `codex` binary under `sync` with `--no-ai-planner=false` yields a named-dependency non-zero exit.
- `auto symphony` is added to the README inventory with at least a one-line purpose description.
- The operator-specific `SymphonyRunArgs.symphony_root` default remains absent; `auto symphony run` accepts `--symphony-root`, falls back to `AUTODEV_SYMPHONY_ROOT`, and errors when neither is set.

## Verification

- `cargo test -p autodev main` passes the two Symphony argument-validation tests.
- `cargo test -p autodev symphony_command` passes existing ~18 tests.
- `cargo test -p autodev linear_tracker` passes the ~5-7 tests covering drift detection and fingerprinting.
- Fixture test: plan with a single `- [ ]` task + a recorded Linear response indicating no such issue; assert the drift report classifies the task as `missing`.
- Fixture test: plan with a single `- [x]` task + Linear reporting the issue in state `In Progress`; assert the drift report classifies the task as `completed_active`.
- Add an integration test that calls `auto symphony workflow` in a tmpdir and asserts the output contains every configured field.

## Open Questions

- Should `auto symphony run` validate that the resolved Symphony checkout has already been built before rendering/syncing the repo workflow, or is the current late binary check sufficient?
- Is `LINEAR_API_KEY` the only auth mechanism, or does Symphony support OAuth flows? Should this be surfaced in the README?
- Should `auto symphony sync --no-ai-planner` be the default and AI-planner be opt-in, given that the AI path adds a mandatory `codex` dependency?
- When two `auto symphony sync` runs happen concurrently in the same repo, what is the intended contention behavior?
- Should Symphony honor the same `util::sync_branch_with_remote` posture as mutating quality commands before running?
