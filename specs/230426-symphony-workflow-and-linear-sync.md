# Specification: Symphony Workflow And Linear Sync

## Objective
Harden `auto symphony` so Linear issue sync, repo-specific workflow rendering, and foreground Symphony execution remain deterministic, evidence-aware, and safe when operator-provided branch, model, path, and remote values flow into generated executable workflow text.

## Evidence Status

### Verified Facts

- `auto symphony` is declared in the top-level command enum in `src/main.rs:97-98`.
- Symphony sync, workflow, and run argument structs include planner model, planner reasoning effort, Codex binary, no-AI planner, workspace root, max concurrent agents, poll interval, worker model, worker reasoning effort, and state names in `src/main.rs:124-287`.
- The Linear GraphQL endpoint constant is `https://api.linear.app/graphql` in `src/symphony_command.rs:26`.
- Symphony task status is represented as `Pending`, `Blocked`, `Partial`, and `Done` in `src/symphony_command.rs:279-284`.
- `run_sync` reads `IMPLEMENTATION_PLAN.md`, parses tasks, reconciles completion evidence, creates or updates Linear issues, and syncs blocker relations in `src/symphony_command.rs:417-632`.
- The planner command uses `auto quota open codex exec` when quota is configured, otherwise the configured Codex binary, and passes `--json`, `--dangerously-bypass-approvals-and-sandbox`, `--skip-git-repo-check`, `--cd`, `-m`, and `model_reasoning_effort` in `src/symphony_command.rs:929-953`.
- `render_workflow` writes a repo-specific workflow, reads the remote URL from git, and defaults the output path to `.auto/symphony/WORKFLOW.md` in `src/symphony_command.rs:1548-1588`.
- `run_foreground` requires `--symphony-root` or `AUTODEV_SYMPHONY_ROOT`, requires `<symphony_root>/bin/symphony`, renders the workflow, creates logs, and launches the external Symphony binary in `src/symphony_command.rs:1590-1668`.
- `parse_tasks` accepts `- [ ]`, `- [!]`, `- [~]`, `- [x]`, and `- [X]` task headers in `src/symphony_command.rs:1796-1825`.
- `parse_task_header` maps those task markers to Symphony task status in `src/symphony_command.rs:1841-1862`.
- `render_workflow_markdown` currently interpolates `spec.base_branch` into `git fetch origin {branch}` and `git checkout {branch}` in `src/symphony_command.rs:2058-2059`.
- The rendered Codex worker command quotes `CARGO_TARGET_DIR` but interpolates `spec.reasoning_effort` and `spec.model` into the shell command text in `src/symphony_command.rs:2090-2094`.
- `shell_quote` exists and escapes single quotes in `src/symphony_command.rs:2228-2230`, with a unit test at `src/symphony_command.rs:3120-3121`.
- The planning corpus says Symphony hardening should focus on branch, model, reasoning, path, and remote values without changing Linear API behavior, workspace layout, or the default model in `genesis/plans/004-symphony-workflow-rendering-hardening.md:7` and `genesis/plans/004-symphony-workflow-rendering-hardening.md:21`.

### Recommendations

- Add typed validators for branch names, model names, reasoning efforts, local paths, and remote URLs before workflow rendering.
- Apply shell quoting or structured YAML scalar rendering to every dynamic shell or YAML field, not only selected paths.
- Keep hostile scalar tests local and deterministic; they should not call Linear or the external Symphony runtime.
- Preserve current Linear sync semantics while workflow rendering is hardened.

### Hypotheses / Unresolved Questions

- It is unresolved whether invalid branch and model values should be rejected at CLI parsing time or at workflow render time.
- It is unresolved whether remote URLs should be shell-quoted only or also validated against allowed URL schemes.
- It is unresolved whether Symphony workflow YAML should use a typed serializer rather than handwritten markdown/YAML text.

## Acceptance Criteria

- Rendering a workflow with a hostile branch string cannot add a second shell command to any `beforeRun` step.
- Rendering a workflow with a hostile model string cannot add extra shell arguments or YAML keys to the Codex command.
- Rendering a workflow with a hostile reasoning-effort string cannot add extra shell arguments or YAML keys to the Codex command.
- Rendering a workflow with spaces, quotes, or shell metacharacters in paths produces valid workflow text or returns a validation error before writing the file.
- Existing deterministic dependency parsing and blocker relation sync still work for multiline dependencies and `none` dependencies.
- `auto symphony run` reports a direct, actionable error when `AUTODEV_SYMPHONY_ROOT` is unset and `--symphony-root` is not provided.
- Completion reconciliation does not mark a terminal Linear issue done locally unless repo-local review handoff, receipt evidence, and declared artifacts are complete.

## Verification

- `cargo test symphony_command::tests::workflow_render_is_repo_specific`
- `cargo test symphony_command::tests::shell_quote_escapes_single_quotes`
- `cargo test symphony_command::tests::parse_tasks_extracts_pending_items_and_dependencies`
- Add and run tests for hostile branch, model, reasoning-effort, remote URL, and path values.
- `rg -n "git fetch origin \\{|git checkout \\{|model_reasoning_effort=\\{}|--model \\{}" src/symphony_command.rs`

## Open Questions

- Which branch-name grammar should be accepted: git refname-valid values, local branch shorthand, or a narrower operator-safe subset?
- Should Symphony render failures leave the previous `.auto/symphony/WORKFLOW.md` in place or remove it to avoid stale runs?
- Should Linear sync default to deterministic planning when quota is configured but Codex usage refresh fails?
