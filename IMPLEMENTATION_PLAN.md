# IMPLEMENTATION_PLAN

## Priority Work

- [x] `AD-001` Backend invocation inventory and policy gate

  Spec: `specs/230426-backend-invocation-policy-and-model-routing.md`
  Why now: Backend behavior is spread across direct `Command::new` calls, shared Claude and Codex wrappers, quota-routed Codex paths, Kimi, PI, Symphony, loop, parallel, bug, nemesis, audit, QA, health, ship, steward, and generation; refactoring before inventory risks changing model defaults or dangerous flag semantics accidentally.
  Codebase evidence: `src/generation.rs` routes generation authoring through `run_logged_author_phase` and direct `run_claude_prompt`; `src/codex_exec.rs` and `src/claude_exec.rs` own shared execution wrappers; `src/bug_command.rs`, `src/nemesis.rs`, and `src/audit_command.rs` still spawn provider commands directly; `src/symphony_command.rs` can route planner execution through `auto quota open codex exec`.
  Owns: `docs/decisions/backend-invocation-policy.md`
  Integration touchpoints: `src/generation.rs`, `src/codex_exec.rs`, `src/claude_exec.rs`, `src/kimi_backend.rs`, `src/pi_backend.rs`, `src/bug_command.rs`, `src/nemesis.rs`, `src/audit_command.rs`, `src/symphony_command.rs`, `src/parallel_command.rs`
  Scope boundary: Research and policy documentation only; do not change command construction, model defaults, reasoning effort defaults, quota routing, or dangerous flags.
  Acceptance criteria: The policy file lists every live backend invocation path with binary, argv shape, model source, effort source, quota behavior, output handling, log path, timeout or futility behavior when present, and dangerous permission posture; it explicitly records unresolved decisions instead of choosing a provider abstraction by implication.
  Verification: `rg -n "Command::new|TokioCommand::new|run_codex_exec|run_claude_exec|run_claude_prompt|run_logged_author_phase|kimi-cli|pi_bin|dangerously|--yolo|quota open" src`; `rg -n "src/generation.rs|src/codex_exec.rs|src/claude_exec.rs|src/kimi_backend.rs|src/pi_backend.rs|src/bug_command.rs|src/nemesis.rs|src/audit_command.rs|src/symphony_command.rs|src/parallel_command.rs" docs/decisions/backend-invocation-policy.md`
  Required tests: none
  Completion artifacts: `docs/decisions/backend-invocation-policy.md`
  Dependencies: none
  Estimated scope: S
  Completion signal: Backend policy file exists, names all verified invocation surfaces, and states that no default or behavior change shipped in this task.

- [x] `AD-002` Shared implementation-plan parser core

  Spec: `specs/230426-shared-task-parser-and-blocked-preservation.md`
  Why now: Generation, parallel, Symphony, review, and completion evidence currently parse task status and fields independently, and generation recognizes `[ ]`, `[~]`, and `[x]` but not `[!]`; a shared core is the safest prerequisite for preserving blocked work.
  Codebase evidence: `src/generation.rs` has `parse_plan_task_header` for `[ ]`, `[~]`, `[x]`, and `[X]`; `src/parallel_command.rs` has `LoopTaskStatus` plus dependency and completion-path parsing; `src/symphony_command.rs` has its own `TaskStatus` parser for `[ ]`, `[!]`, `[~]`, `[x]`, and `[X]`; `src/completion_artifacts.rs` separately extracts `Verification:` and `Completion artifacts:`.
  Owns: `src/task_parser.rs`, `src/main.rs`
  Integration touchpoints: `src/generation.rs`, `src/parallel_command.rs`, `src/symphony_command.rs`, `src/review_command.rs`, `src/completion_artifacts.rs`
  Scope boundary: Add the shared parser module and fixture coverage without migrating all consumers; preserve existing command behavior outside tests that exercise the new parser directly.
  Acceptance criteria: The parser represents task ID, title, status, markdown body, dependencies, verification text, completion artifacts, and completion-path target; `[ ]`, `[~]`, `[!]`, `[x]`, and `[X]` map to stable status values; `Dependencies: none` yields an empty dependency set; multiline dependencies and parallelism notes do not invent task IDs.
  Verification: Before implementation, run `cargo test task_parser::tests::parses_all_plan_statuses_and_fields` and confirm the new test is absent or failing; after implementation, run `cargo test task_parser::tests::parses_all_plan_statuses_and_fields`; run `cargo test task_parser::tests::dependencies_none_and_multiline_notes_are_stable`; run `cargo test task_parser::tests::completion_path_placeholders_are_metadata_not_ready_work`
  Required tests: `task_parser::tests::parses_all_plan_statuses_and_fields`, `task_parser::tests::dependencies_none_and_multiline_notes_are_stable`, `task_parser::tests::completion_path_placeholders_are_metadata_not_ready_work`
  Completion artifacts: none
  Dependencies: none
  Estimated scope: M
  Completion signal: Shared parser tests pass and no existing command parser has been removed or behavior-changed yet.

- [x] `AD-003` Preserve blocked tasks during generated plan merges

  Spec: `specs/230426-planning-corpus-and-generation.md`
  Why now: `auto gen` merges generated plans with existing open tasks, but the current generation parser does not recognize `[!]`, so blocked tasks can be lost or ignored during regeneration unless generation moves to the shared parser path.
  Codebase evidence: `src/generation.rs` merges existing unchecked tasks in `merge_generated_plan_with_existing_open_tasks`; the local `parse_plan_task_header` only treats `[ ]` and `[~]` as unchecked and `[x]` or `[X]` as checked; `src/symphony_command.rs` and `src/parallel_command.rs` already treat `[!]` as blocked.
  Owns: `src/generation.rs`, `src/task_parser.rs`
  Integration touchpoints: `auto gen --plan-only`, `auto gen --sync-only`, `auto reverse`, `src/parallel_command.rs`, `src/symphony_command.rs`
  Scope boundary: Limit the behavior change to generated plan parsing and merge preservation; do not add snapshot-only generation, sync policy changes, root spec promotion, or executor scheduling changes.
  Acceptance criteria: Existing `[!]` tasks with stable backtick IDs survive generated plan merges when the generated plan does not contain the same ID; existing `[ ]` and `[~]` preservation behavior remains unchanged; checked `[x]` and `[X]` rows are still not preserved into active queues.
  Verification: Before implementation, run `cargo test generation::tests::merge_generated_plan_preserves_blocked_tasks` and confirm it fails or is absent; after implementation, run `cargo test generation::tests::merge_generated_plan_preserves_blocked_tasks`; run `cargo test generation::tests::merges_existing_open_tasks_not_present_in_new_plan`; run `cargo test parallel_command::tests::parse_loop_plan_tracks_ready_and_blocked_dependencies`; run `cargo test symphony_command::tests::parse_tasks_extracts_pending_items_and_dependencies`
  Required tests: `generation::tests::merge_generated_plan_preserves_blocked_tasks`, `generation::tests::merges_existing_open_tasks_not_present_in_new_plan`, `parallel_command::tests::parse_loop_plan_tracks_ready_and_blocked_dependencies`, `symphony_command::tests::parse_tasks_extracts_pending_items_and_dependencies`
  Completion artifacts: none
  Dependencies: `AD-002`
  Estimated scope: S
  Completion signal: Generated plan merge preserves blocked rows and the parallel and Symphony parser regressions still pass.

- [x] `AD-004` Parser and generation checkpoint

  Spec: `specs/230426-verification-receipts-and-completion-evidence.md`
  Why now: Shared parsing and generated-plan merge preservation are central queue-truth risks; a checkpoint keeps future work from widening parser adoption before the blocked-task preservation proof is reviewed.
  Codebase evidence: `src/completion_artifacts.rs` already requires review handoff, verification receipts when executable commands exist, and declared artifacts before full completion; `src/parallel_command.rs` demotes completed tasks when evidence drifts; the parser and generation changes need the same evidence discipline.
  Owns: `REVIEW.md`
  Integration touchpoints: `src/task_parser.rs`, `src/generation.rs`, `src/completion_artifacts.rs`, `.auto/symphony/verification-receipts`
  Scope boundary: Validation and handoff only; do not migrate loop, parallel, review, or Symphony adapters in this checkpoint.
  Acceptance criteria: `REVIEW.md` contains a checkpoint entry for `AD-004` listing parser tests run, generated-merge tests run, any skipped proof, and whether parser adoption may proceed to other consumers.
  Verification: `cargo test task_parser::tests::parses_all_plan_statuses_and_fields`; `cargo test generation::tests::merge_generated_plan_preserves_blocked_tasks`; `cargo test completion_artifacts::tests::inspect_task_completion_evidence_requires_review_and_receipts`; `rg -n "AD-004|parser|blocked" REVIEW.md`
  Required tests: `task_parser::tests::parses_all_plan_statuses_and_fields`, `generation::tests::merge_generated_plan_preserves_blocked_tasks`, `completion_artifacts::tests::inspect_task_completion_evidence_requires_review_and_receipts`
  Completion artifacts: `REVIEW.md`
  Dependencies: `AD-002`, `AD-003`
  Estimated scope: XS
  Completion signal: Review handoff records the parser checkpoint and names the next permitted parser migration surface.

- [x] `AD-005` Snapshot-only generation decision

  Spec: `specs/230426-planning-corpus-and-generation.md`
  Why now: Current generation always syncs verified generated specs and plans back to root after normal authoring, while the generated specs describe snapshot-only behavior as a recommendation and still list the product shape as unresolved.
  Codebase evidence: `src/generation.rs` calls `sync_verified_generation_outputs` after spec and plan generation; `--sync-only` verifies generated outputs and also syncs them; `GenerationArgs` has `--plan-only` and `--sync-only` but no no-sync or spec-only flag.
  Owns: `docs/decisions/snapshot-only-generation.md`
  Integration touchpoints: `auto gen`, `auto reverse`, `src/generation.rs`, `src/corpus.rs`, root `specs/`, root `IMPLEMENTATION_PLAN.md`, `gen-*`
  Scope boundary: Decision and design only; do not add CLI flags, change sync behavior, or rewrite generated/root planning files.
  Acceptance criteria: The decision records whether snapshot-only generation is a new flag, a mode on existing flags, or an internal command contract; it states how root sync is invoked explicitly; it defines how generated `Spec:` paths map to root paths without relying on future unverified details.
  Verification: `rg -n "snapshot-only|no-sync|sync-only|plan-only|root specs|IMPLEMENTATION_PLAN.md" docs/decisions/snapshot-only-generation.md`; `rg -n "sync_verified_generation_outputs|sync_only|plan_only" src/generation.rs`
  Required tests: none
  Completion artifacts: `docs/decisions/snapshot-only-generation.md`
  Dependencies: `AD-003`, `AD-004`
  Estimated scope: XS
  Completion signal: Snapshot-only product shape is recorded as a decision with explicit non-goals and implementation prerequisites.

- [x] `AD-006` First-run no-model preflight decision

  Spec: `specs/230426-first-run-ci-and-installed-binary-proof.md`
  Why now: The repo is developer-facing and has no no-model first-success command; `auto health` invokes Codex, so new contributors cannot prove local layout, help, and installed-binary basics without credentials.
  Codebase evidence: `src/main.rs` has 17 top-level command variants and no `Doctor` command; `HealthArgs` defaults to model `gpt-5.5`, effort `high`, and `codex` binary; `.github/workflows/ci.yml` runs fmt, clippy, and tests but not installed-binary proof; README documents `cargo install --path . --root ~/.local` and `auto --version`.
  Owns: `docs/decisions/first-run-preflight.md`
  Integration touchpoints: `src/main.rs`, `src/health_command.rs`, `.github/workflows/ci.yml`, `README.md`, `Cargo.toml`
  Scope boundary: Decision and contract only; do not implement `auto doctor`, CI install proof, or README changes in this task.
  Acceptance criteria: The decision names the command shape, required versus optional tool checks, no-network/no-model constraints, expected output categories, and whether the command writes artifacts by default.
  Verification: `rg -n "Doctor|doctor|self-test|preflight" src/main.rs src/*_command.rs README.md`; `rg -n "first-run|no-model|codex|claude|pi|gh|auto --version" docs/decisions/first-run-preflight.md`
  Required tests: none
  Completion artifacts: `docs/decisions/first-run-preflight.md`
  Dependencies: none
  Estimated scope: XS
  Completion signal: First-run command shape is decided without relabeling model-backed `auto health` as a no-model path.

- [x] `AD-007` No-model first-run command

  Spec: `specs/230426-first-run-ci-and-installed-binary-proof.md`
  Why now: Once the command shape is decided, the repo needs a zero-friction local success path that reports missing external tools clearly instead of requiring model credentials or live services.
  Codebase evidence: `Cargo.toml` declares binary `auto`; `build.rs` embeds git SHA, dirty state, and build profile; `src/util.rs` exposes `CLI_LONG_VERSION`; current `auto health` writes `.auto/health` and invokes Codex, so it is not the no-model path.
  Owns: `src/main.rs`, `src/doctor_command.rs`, `README.md`
  Integration touchpoints: `auto --help`, `auto corpus --help`, `auto gen --help`, `auto parallel --help`, `auto quota --help`, `auto symphony --help`, `Cargo.toml`, `build.rs`, `src/util.rs`
  Scope boundary: Implement local checks and documentation only; do not call Codex, Claude, Kimi, PI, Linear, Symphony, Docker, browser automation, or network endpoints.
  Acceptance criteria: The first-run command succeeds in a fresh checkout without model credentials; required repo layout and binary metadata checks fail with actionable messages; optional tool checks for `codex`, `claude`, `pi`, and `gh` are reported as capability warnings or explicit failures according to the decision; README lists the exact first-success commands.
  Verification: Before implementation, run `cargo test doctor_command::tests::doctor_reports_missing_optional_tools_without_panicking` and confirm it fails or is absent; after implementation, run `cargo test doctor_command::tests::doctor_reports_missing_optional_tools_without_panicking`; run `cargo test doctor_command::tests::doctor_checks_expected_help_surfaces`; run `cargo test doctor_command_is_parseable`
  Required tests: `doctor_command::tests::doctor_reports_missing_optional_tools_without_panicking`, `doctor_command::tests::doctor_checks_expected_help_surfaces`, `doctor_command_is_parseable`
  Completion artifacts: `README.md`
  Dependencies: `AD-006`
  Estimated scope: M
  Completion signal: The decided no-model command is implemented, tested, and documented as distinct from model-backed health.

- [x] `AD-008` Quota profile capture rejects symlinks and stale credentials

  Spec: `specs/230426-quota-router-and-credential-safety.md`
  Why now: Profile capture currently copies active auth into quota profiles with raw file copies and recursive symlink preservation, which can leak stale or linked credential material between accounts.
  Codebase evidence: `src/quota_config.rs` copies Codex `auth.json`, Claude credential files, `statsig`, and `.claude.json` with `fs::copy`; its recursive copy uses `symlink_metadata` and recreates symlinks; config and state saves already use `write_0o600_if_unix`, proving owner-only writes are available.
  Owns: `src/quota_config.rs`
  Integration touchpoints: `auto quota accounts capture`, `src/quota_exec.rs`, `src/util.rs`, `~/.config/quota-router/profiles`
  Scope boundary: Harden capture into profile directories only; do not change account selection floors, usage refresh, active credential swapping, or quota-open execution.
  Acceptance criteria: Capturing Codex writes exactly one regular `auth.json` with owner-only mode on Unix; capturing Claude writes only supported regular credential files/directories; symlinked or non-regular sources produce path-specific human-readable errors; recapturing removes stale destination credentials that disappeared from the active source.
  Verification: Before implementation, run `cargo test quota_config::tests::capture_rejects_symlinked_codex_auth` and confirm it fails or is absent; after implementation, run `cargo test quota_config::tests::capture_rejects_symlinked_codex_auth`; run `cargo test quota_config::tests::capture_prunes_stale_profile_files`; run `cargo test quota_config::tests::save_writes_owner_only`
  Required tests: `quota_config::tests::capture_rejects_symlinked_codex_auth`, `quota_config::tests::capture_prunes_stale_profile_files`, `quota_config::tests::save_writes_owner_only`
  Completion artifacts: none
  Dependencies: none
  Estimated scope: M
  Completion signal: Capture rejects symlinks, prunes stale profile files, and keeps owner-only config behavior intact.

- [x] `AD-009` Quota active credential restore covers Claude home JSON

  Spec: `specs/230426-quota-router-and-credential-safety.md`
  Why now: Quota-routed Claude execution backs up both `~/.claude` and `~/.claude.json`, but the normal provider restore helper only restores the Claude directory, leaving one credential surface inconsistent outside guard-drop cleanup paths.
  Codebase evidence: `src/quota_exec.rs` backs up `backup/claude` and `backup/claude.json` in `swap_credentials`; `restore_credentials` restores Codex auth and Claude directory but does not restore `backup/claude.json`; `copy_dir_recursive` in the same file also preserves symlinks during execution-time copies.
  Owns: `src/quota_exec.rs`
  Integration touchpoints: `auto quota open claude`, `auto quota open codex`, `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_usage.rs`
  Scope boundary: Fix restore and execution-time copy safety only; do not redesign quota selection, change cooldowns, or alter usage refresh prompts.
  Acceptance criteria: Claude quota-open restores both `~/.claude` and `~/.claude.json` after success, command failure, quota exhaustion, and explicit restore paths; Codex restore behavior remains intact; execution-time copies reject symlinks instead of recreating them in active auth.
  Verification: Before implementation, run `cargo test quota_exec::tests::restore_credentials_restores_claude_json_backup` and confirm it fails or is absent; after implementation, run `cargo test quota_exec::tests::restore_credentials_restores_claude_json_backup`; run `cargo test quota_exec::tests::swap_credentials_restores_claude_json_on_drop`; run `cargo test quota_exec::tests::swap_credentials_enforces_0o600`; run `cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error -- --nocapture`
  Required tests: `quota_exec::tests::restore_credentials_restores_claude_json_backup`, `quota_exec::tests::swap_credentials_restores_claude_json_on_drop`, `quota_exec::tests::swap_credentials_enforces_0o600`, `quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error`
  Completion artifacts: none
  Dependencies: `AD-008`
  Estimated scope: M
  Completion signal: Quota restore tests cover Claude home JSON and existing Codex human-refresh error surfacing still passes.

- [x] `AD-010` Quota credential safety checkpoint

  Spec: `specs/230426-checkpoint-security-and-artifact-policy.md`
  Why now: Credential capture and restore are security-sensitive, and downstream checkpoint, CI, release, and automation work should not proceed until the quota hardening results are recorded with any blockers.
  Codebase evidence: `src/util.rs` has checkpoint exclusions for `.auto`, `.claude/worktrees`, `bug`, `nemesis`, and top-level `gen-*`, but not `genesis`; `src/quota_config.rs` and `src/quota_state.rs` use owner-only writes; quota profile capture and execution restore are the immediate security gate from the specs.
  Owns: `REVIEW.md`
  Integration touchpoints: `src/quota_config.rs`, `src/quota_exec.rs`, `src/util.rs`, `auto quota accounts capture`, `auto quota open`
  Scope boundary: Validation and handoff only; do not change checkpoint exclusions, broad audit staging, or release gates in this task.
  Acceptance criteria: `REVIEW.md` records the quota credential checkpoint, exact tests run, any remaining symlink/stale-profile risks, and whether downstream security checkpoint work may proceed.
  Verification: `cargo test quota_config::tests::capture_rejects_symlinked_codex_auth`; `cargo test quota_config::tests::capture_prunes_stale_profile_files`; `cargo test quota_exec::tests::restore_credentials_restores_claude_json_backup`; `cargo test quota_exec::tests::swap_credentials_enforces_0o600`; `rg -n "AD-010|quota|credential|symlink|stale" REVIEW.md`
  Required tests: `quota_config::tests::capture_rejects_symlinked_codex_auth`, `quota_config::tests::capture_prunes_stale_profile_files`, `quota_exec::tests::restore_credentials_restores_claude_json_backup`, `quota_exec::tests::swap_credentials_enforces_0o600`
  Completion artifacts: `REVIEW.md`
  Dependencies: `AD-008`, `AD-009`
  Estimated scope: XS
  Completion signal: Review handoff states whether quota credential safety is green, blocked, or partial with concrete residual risks.

- [x] `AD-011` Harden Symphony workflow scalar rendering

  Spec: `specs/230426-symphony-workflow-and-linear-sync.md`
  Why now: `auto symphony workflow` renders branch, model, reasoning effort, paths, and remote URL into executable workflow text, and branch/model/reasoning strings currently reach shell or YAML command text without full validation or quoting.
  Codebase evidence: `src/symphony_command.rs` interpolates `spec.base_branch` into `git fetch origin {branch}`, `git checkout {branch}`, `git rev-list`, `merge-base`, and `git pull`; it interpolates `spec.reasoning_effort` and `spec.model` into the Codex command; `shell_quote` exists but is only applied to selected path values.
  Owns: `src/symphony_command.rs`
  Integration touchpoints: `auto symphony workflow`, `auto symphony run`, `.auto/symphony/WORKFLOW.md`, Linear project workflow config, `AUTODEV_SYMPHONY_ROOT`
  Scope boundary: Local render validation and quoting only; do not change Linear GraphQL sync semantics, issue states, workspace layout, default model, or external Symphony runtime behavior.
  Acceptance criteria: Hostile branch, model, reasoning-effort, path, and remote URL values cannot inject extra shell commands, shell arguments, or YAML keys; invalid values return actionable errors before writing a workflow; existing repo-specific workflow rendering and Symphony root errors still behave.
  Verification: Before implementation, run `cargo test symphony_command::tests::workflow_render_rejects_hostile_branch` and confirm it fails or is absent; after implementation, run `cargo test symphony_command::tests::workflow_render_rejects_hostile_branch`; run `cargo test symphony_command::tests::workflow_render_rejects_hostile_model_and_effort`; run `cargo test symphony_command::tests::workflow_render_is_repo_specific`; run `cargo test symphony_command::tests::shell_quote_escapes_single_quotes`; run `cargo test symphony_command::tests::run_requires_symphony_root_when_unset`
  Required tests: `symphony_command::tests::workflow_render_rejects_hostile_branch`, `symphony_command::tests::workflow_render_rejects_hostile_model_and_effort`, `symphony_command::tests::workflow_render_is_repo_specific`, `symphony_command::tests::shell_quote_escapes_single_quotes`, `symphony_command::tests::run_requires_symphony_root_when_unset`
  Completion artifacts: none
  Dependencies: none
  Estimated scope: M
  Completion signal: Hostile scalar render tests fail before the fix, pass after the fix, and existing deterministic workflow tests remain green.

- [x] `AD-012` Verification receipt output policy decision

  Spec: `specs/230426-verification-receipts-and-completion-evidence.md`
  Why now: Receipt inspection can reject missing, failed, corrupted, or incomplete receipts, but zero-test detection needs runner output or structured proof details that receipts do not currently store.
  Codebase evidence: `scripts/run-task-verification.sh` records task ID, exact command, argv, and exit code through `scripts/verification_receipt.py`; `src/completion_artifacts.rs` checks command coverage and pass/fail status but does not inspect command stdout/stderr or detect `0 tests`; parallel prompts only tell workers not to count zero-test output.
  Owns: `docs/decisions/verification-receipt-policy.md`
  Integration touchpoints: `scripts/run-task-verification.sh`, `scripts/verification_receipt.py`, `src/completion_artifacts.rs`, `src/parallel_command.rs`, `.auto/symphony/verification-receipts`
  Scope boundary: Decision and schema proposal only; do not change wrapper behavior, receipt JSON shape, or completion evidence logic.
  Acceptance criteria: The decision states what output summary is stored, maximum byte limits, redaction posture, which test runners get zero-test detection first, and whether receipt write failures become fatal.
  Verification: `rg -n "zero-test|0 tests|stdout|stderr|receipt|redact|fatal" docs/decisions/verification-receipt-policy.md`; `rg -n "verification_receipt|run-task-verification|0 tests|receipt write" scripts src/completion_artifacts.rs src/parallel_command.rs`
  Required tests: none
  Completion artifacts: `docs/decisions/verification-receipt-policy.md`
  Dependencies: none
  Estimated scope: XS
  Completion signal: Receipt schema and zero-test policy are decided before implementation depends on output details.

- [x] `AD-013` Receipt-backed zero-test detection

  Spec: `specs/230426-verification-receipts-and-completion-evidence.md`
  Why now: Automation can currently accept a receipt for a command that exits zero even when the test runner reports no tests executed; this weakens completion evidence for loop, parallel, Symphony, and review handoff.
  Codebase evidence: `src/completion_artifacts.rs` requires wrapper receipts for executable verification commands and checks expected command coverage; `scripts/run-task-verification.sh` records exit status but discards command output; `scripts/verification_receipt.py` writes receipt entries without output summary fields.
  Owns: `scripts/run-task-verification.sh`, `scripts/verification_receipt.py`, `src/completion_artifacts.rs`
  Integration touchpoints: `.auto/symphony/verification-receipts`, `src/parallel_command.rs`, `src/symphony_command.rs`, `REVIEW.md`
  Scope boundary: Implement the decided receipt output summary and zero-test rejection for Cargo and one non-Cargo runner only; do not sign receipts, move receipt storage, or classify network/deployment commands in this task.
  Acceptance criteria: The wrapper records the decided output summary without leaking excessive output; receipt inspection rejects successful Cargo receipts that report `0 tests`, and rejects one additional runner's zero-test form; existing receipt command matching by exact command or argv-equivalent still works.
  Verification: Before implementation, run `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests` and confirm it fails or is absent; after implementation, run `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests`; run `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_pytest_tests`; run `cargo test completion_artifacts::tests::inspect_task_completion_evidence_accepts_quoted_command_receipts_with_argv`; run `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_failed_receipts`
  Required tests: `completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests`, `completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_pytest_tests`, `completion_artifacts::tests::inspect_task_completion_evidence_accepts_quoted_command_receipts_with_argv`, `completion_artifacts::tests::inspect_task_completion_evidence_rejects_failed_receipts`
  Completion artifacts: none
  Dependencies: `AD-012`
  Estimated scope: M
  Completion signal: Zero-test receipts no longer satisfy completion evidence for the covered runners and existing receipt matching still passes.

- [ ] `AD-014` Symphony and receipt evidence checkpoint

  Spec: `specs/230426-symphony-workflow-and-linear-sync.md`
  Why now: Workflow rendering and completion evidence both affect unattended execution safety; future Linear sync or external Symphony runtime changes should wait until these local deterministic proofs are recorded.
  Codebase evidence: `src/symphony_command.rs` already reconciles terminal Linear issues through `inspect_task_completion_evidence` before marking local tasks done; `src/completion_artifacts.rs` is the shared evidence gate; `scripts/run-task-verification.sh` is the receipt-producing wrapper.
  Owns: `REVIEW.md`
  Integration touchpoints: `src/symphony_command.rs`, `src/completion_artifacts.rs`, `scripts/run-task-verification.sh`, `.auto/symphony/verification-receipts`
  Scope boundary: Validation and handoff only; do not call Linear, render a live external Symphony workflow, or launch Symphony runtime.
  Acceptance criteria: `REVIEW.md` records hostile workflow render test outcomes, zero-test receipt outcomes, any intentionally untested live Linear or Symphony surfaces, and a go/no-go decision for adapter migration work.
  Verification: `cargo test symphony_command::tests::workflow_render_rejects_hostile_branch`; `cargo test symphony_command::tests::workflow_render_rejects_hostile_model_and_effort`; `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests`; `cargo test completion_artifacts::tests::inspect_task_completion_evidence_requires_wrapper_for_executable_verification`; `rg -n "AD-014|Symphony|zero-test|receipt" REVIEW.md`
  Required tests: `symphony_command::tests::workflow_render_rejects_hostile_branch`, `symphony_command::tests::workflow_render_rejects_hostile_model_and_effort`, `completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests`, `completion_artifacts::tests::inspect_task_completion_evidence_requires_wrapper_for_executable_verification`
  Completion artifacts: `REVIEW.md`
  Dependencies: `AD-011`, `AD-013`
  Estimated scope: XS
  Completion signal: Review handoff records local green proof or specific blockers for workflow rendering and receipt evidence.

- [x] `AD-015` Audit commits use scoped pathspecs

  Spec: `specs/230426-checkpoint-security-and-artifact-policy.md`
  Why now: `auto audit` can apply fixes or append planning entries and then stage broadly through `commit_all`, which conflicts with the repo's generated artifact and credential safety posture.
  Codebase evidence: `src/audit_command.rs` defines `commit_all` as `git add -A` followed by commit; `src/util.rs` has checkpoint exclusion rules for generated/runtime paths, but audit commit staging does not reuse a scoped pathspec helper.
  Owns: `src/audit_command.rs`
  Integration touchpoints: `auto audit`, `WORKLIST.md`, `IMPLEMENTATION_PLAN.md`, `src/util.rs`, `git add`
  Scope boundary: Replace broad audit staging with explicit pathspec staging or an explicit narrow exception for the exact audit output path; do not change audit prompt doctrine, verdict taxonomy, backend selection, or Kimi/PI behavior.
  Acceptance criteria: Audit commits stage only the file under audit plus intended durable queue/report files; generated directories, quota profiles, `.auto`, `bug`, `nemesis`, and `gen-*` paths are not silently included; any unavoidable broad staging path emits a clear exception reason and has test coverage.
  Verification: Before implementation, run `cargo test audit_command::tests::commit_audit_outputs_uses_scoped_pathspecs` and confirm it fails or is absent; after implementation, run `cargo test audit_command::tests::commit_audit_outputs_uses_scoped_pathspecs`; run `cargo test audit_command::tests::audit_commit_excludes_generated_and_runtime_artifacts`; run `cargo test util::tests::checkpoint_excludes_generated_and_runtime_paths`
  Required tests: `audit_command::tests::commit_audit_outputs_uses_scoped_pathspecs`, `audit_command::tests::audit_commit_excludes_generated_and_runtime_artifacts`, `util::tests::checkpoint_excludes_generated_and_runtime_paths`
  Completion artifacts: none
  Dependencies: `AD-010`
  Estimated scope: M
  Completion signal: Audit commit tests prove scoped staging and the checkpoint exclusion regression still passes.

- [x] `AD-016` Report-only QA dirty-state enforcement

  Spec: `specs/230426-quality-pipelines-and-release-lifecycle.md`
  Why now: `auto qa-only` is prompt-level report-only, but the runner does not mechanically fail if the model changes files other than `QA.md`.
  Codebase evidence: `src/qa_only_command.rs` prompt tells the worker not to change source, tests, build config, or docs other than `QA.md`; `run_qa_only` records prompt logs and invokes Codex, but it does not compare dirty state before and after the run.
  Owns: `src/qa_only_command.rs`
  Integration touchpoints: `auto qa-only`, `QA.md`, `.auto/qa-only`, `.auto/logs`, `git status --short`
  Scope boundary: Enforce report-only dirty-state behavior for `auto qa-only` only; do not change `auto qa`, review, ship, or Codex execution wrappers.
  Acceptance criteria: A `qa-only` run that changes files outside `QA.md` and allowed `.auto` logs fails with a clear dirty-state report; a run that only creates or updates `QA.md` and logs succeeds; pre-existing dirty files are reported distinctly from new report-only violations.
  Verification: Before implementation, run `cargo test qa_only_command::tests::qa_only_rejects_non_report_file_changes` and confirm it fails or is absent; after implementation, run `cargo test qa_only_command::tests::qa_only_rejects_non_report_file_changes`; run `cargo test qa_only_command::tests::qa_only_allows_qa_md_and_auto_logs`; run `cargo test qa_only_command::tests::qa_only_reports_preexisting_dirty_state`
  Required tests: `qa_only_command::tests::qa_only_rejects_non_report_file_changes`, `qa_only_command::tests::qa_only_allows_qa_md_and_auto_logs`, `qa_only_command::tests::qa_only_reports_preexisting_dirty_state`
  Completion artifacts: none
  Dependencies: none
  Estimated scope: S
  Completion signal: QA-only dirty-state tests pass and the command fails loudly on non-report modifications.

- [x] `AD-017` Release readiness gate decision

  Spec: `specs/230426-quality-pipelines-and-release-lifecycle.md`
  Why now: Release readiness touches installed-binary proof, QA/health freshness, review state, blockers, rollback, monitoring, PR state, and validation evidence; implementing a mechanical gate before those inputs are defined would bake in unverified product policy.
  Codebase evidence: `src/ship_command.rs` is a prompted release workflow that asks the model to maintain `SHIP.md`; `.github/workflows/ci.yml` does not install `auto` or run the installed binary; `src/qa_only_command.rs` is report-only by prompt until dirty-state enforcement lands; `src/completion_artifacts.rs` owns receipt-backed proof for task completion.
  Owns: `docs/decisions/release-readiness-gate.md`
  Integration touchpoints: `auto ship`, `SHIP.md`, `QA.md`, `HEALTH.md`, `.github/workflows/ci.yml`, `scripts/run-task-verification.sh`, `src/ship_command.rs`
  Scope boundary: Decision only; do not implement ship-gate code, CI changes, or release documentation updates in this task.
  Acceptance criteria: The decision states which command enforces release readiness, which validation receipts are required, how stale QA/health evidence is detected, what blocks release, and how installed-binary proof is attached to `SHIP.md`.
  Verification: `rg -n "installed-binary|QA.md|HEALTH.md|SHIP.md|receipt|rollback|monitoring|PR" docs/decisions/release-readiness-gate.md`; `rg -n "SHIP.md|QA.md|HEALTH.md|cargo install|auto --version|run-task-verification" src README.md .github/workflows/ci.yml scripts`
  Required tests: none
  Completion artifacts: `docs/decisions/release-readiness-gate.md`
  Dependencies: `AD-007`, `AD-013`, `AD-015`, `AD-016`
  Estimated scope: XS
  Completion signal: Release gate policy is decided with explicit blockers and implementation prerequisites.

- [x] `AD-018` Quality and security checkpoint

  Spec: `specs/230426-quality-pipelines-and-release-lifecycle.md`
  Why now: Audit staging, QA-only dirty-state enforcement, and release gate policy are the next risky cluster after parser, quota, and Symphony work; a checkpoint prevents release or CI work from proceeding on stale assumptions.
  Codebase evidence: `src/audit_command.rs` currently has broad `git add -A` staging; `src/qa_only_command.rs` relies on prompt-only report discipline; `src/ship_command.rs` records ship requirements through a prompt rather than a mechanical gate.
  Owns: `REVIEW.md`
  Integration touchpoints: `src/audit_command.rs`, `src/qa_only_command.rs`, `src/ship_command.rs`, `docs/decisions/release-readiness-gate.md`
  Scope boundary: Validation and handoff only; do not add CI installed-binary proof or ship-gate code in this checkpoint.
  Acceptance criteria: `REVIEW.md` records audit staging proof, qa-only dirty-state proof, release gate decision status, and whether follow-on CI and ship-gate work may proceed.
  Verification: `cargo test audit_command::tests::commit_audit_outputs_uses_scoped_pathspecs`; `cargo test qa_only_command::tests::qa_only_rejects_non_report_file_changes`; `rg -n "AD-018|audit|qa-only|release gate" REVIEW.md`; `rg -n "release readiness|installed-binary" docs/decisions/release-readiness-gate.md`
  Required tests: `audit_command::tests::commit_audit_outputs_uses_scoped_pathspecs`, `qa_only_command::tests::qa_only_rejects_non_report_file_changes`
  Completion artifacts: `REVIEW.md`
  Dependencies: `AD-015`, `AD-016`, `AD-017`
  Estimated scope: XS
  Completion signal: Review handoff states the release lifecycle is ready for CI and ship-gate implementation or names concrete blockers.

- [x] `TASK-002` Add `### auto steward`, `### auto audit`, `### auto symphony` detailed-guide subsections to README

    Spec: `specs/220426-readme-truth-pass.md`
    Why now: `README.md` has detailed `###` subsections for the 13 historical commands (`README.md:86-849`) but none for the three commands added since the README was last truthful. Operators currently have no entry-point doc for these three commands.
    Codebase evidence: existing subsection headers at `README.md:86,180,258,303,374,442,483,567,607,677,724,770,849`; concrete artifact lists in `src/steward_command.rs:21-28` (DRIFT/HINGES/RETIRE/HAZARDS/STEWARDSHIP-REPORT/PROMOTIONS), `src/audit_command.rs:131-138` (verdict shape), `src/main.rs:834` (audit doctrine default `audit/DOCTRINE.md`), `src/symphony_command.rs:402-417` (subcommand entry), `src/main.rs:111-118` (symphony Sync/Workflow/Run).
    Owns: `README.md`
    Integration touchpoints: none
    Scope boundary: three new subsections (`### auto steward`, `### auto audit`, `### auto symphony`) inserted in the same Purpose/What it reads/What it produces/Defaults pattern used by existing subsections. Each must list the actual deliverables/artifact paths from the source-verified evidence above. No edits to Defaults section, no edits to PR/CI prose.
    Acceptance criteria: each new subsection has Purpose, What it reads, What it produces (with concrete file paths from the evidence), and Defaults; `auto steward` subsection lists all six STEWARD_DELIVERABLES; `auto audit` subsection states the doctrine default `audit/DOCTRINE.md` and the six verdict variants `CLEAN`/`DRIFT-SMALL`/`DRIFT-LARGE`/`SLOP`/`RETIRE`/`REFACTOR`; `auto symphony` subsection enumerates the three subcommands with one-line purposes.
    Verification: `rg -n '^### .*auto (steward|audit|symphony)' README.md`; `rg -n 'DRIFT.md|HINGES.md|RETIRE.md|HAZARDS.md|STEWARDSHIP-REPORT.md|PROMOTIONS.md' README.md`; `rg -n 'audit/DOCTRINE.md' README.md`.
    Required tests: none (docs-only)
    Completion artifacts: `README.md`
    Dependencies: TASK-001
    Estimated scope: S
    Completion signal: three new subsections present and grep checks pass.

- [x] `TASK-014` Add tmpdir / missing-parent / rapid-collision regression tests for `util::atomic_write`

    Spec: `specs/220426-shared-util-layer.md`
    Why now: spec asks for three explicit test cases on `atomic_write` (tmpdir-not-a-git-repo behavior, missing parent dir auto-create, rapid-succession collision tiebreaker). Existing tests at `src/util.rs:1032-1091` cover only rename-failure cleanup and write-failure cleanup. The collision-tiebreaker test is the one most likely to catch a real bug — the temp filename uses `Utc::now().timestamp_nanos_opt().unwrap_or_default()` (`src/util.rs:413`), which can collide if two threads call into the same directory in the same nanosecond on systems where `timestamp_nanos_opt` returns `None`.
    Codebase evidence: `src/util.rs:404-426` (atomic_write), `src/util.rs:413` (`unwrap_or_default()` on nanos), `src/util.rs:1032-1091` (existing tests).
    Owns: `src/util.rs`
    Integration touchpoints: none (test-only addition)
    Scope boundary: add three tests inside the existing `#[cfg(test)] mod tests` block. Do NOT change `atomic_write` runtime behavior; if the rapid-collision test surfaces a real bug, open a separate task to fix it. Stay within the standard library and `tempfile`-style temp-dir creation already used elsewhere in the file.
    Acceptance criteria: tests exercise the three scenarios; the missing-parent test confirms `atomic_write` calls `create_dir_all`; the rapid-collision test spawns a small fixed number of threads writing to the same path and verifies all writes complete and the final file matches the last writer's bytes (or — if the test surfaces a deterministic collision — fails clearly so we file a real-fix task).
    Verification: `cargo test util::tests::atomic_write_creates_missing_parent_dir`; `cargo test util::tests::atomic_write_handles_rapid_succession_collisions`; `cargo test util::tests::atomic_write_works_outside_git_repo`.
    Required tests: `atomic_write_creates_missing_parent_dir`, `atomic_write_handles_rapid_succession_collisions`, `atomic_write_works_outside_git_repo`
    Completion artifacts: `src/util.rs`
    Dependencies: TASK-011
    Estimated scope: S
    Completion signal: three new tests pass; spec acceptance for the shared util layer test ask is closed.

- [~] `TASK-016` Tag `v0.2.0` once the priority + first follow-on cluster is verified clean

    Spec: `specs/220426-release-ship.md`
    Why now: `Cargo.toml` is still on `0.1.0`; once the visible drift (README, CI, dead code, hardening) is closed, cutting a `0.2.0` annotated tag locks the verified baseline. Spec frames this as a preservation contract; the only new work here is the actual tag.
    Codebase evidence: `Cargo.toml:3` (`version = "0.1.0"`), `build.rs:8-62` provenance already wired, `src/util.rs:9-17` `CLI_LONG_VERSION`.
    Owns: `refs/tags/v0.2.0`
    Integration touchpoints: `Cargo.toml`, `Cargo.lock`, `COMPLETED.md`.
    Scope boundary: bump `Cargo.toml` version to `0.2.0`, regenerate `Cargo.lock`, append a `## v0.2.0` section to `COMPLETED.md` summarizing the closed task IDs, and create the annotated tag locally. Do NOT push the tag in this task — `auto ship` (or a separate operator step) handles publishing and PR plumbing per spec.
    Acceptance criteria: `Cargo.toml` reads `version = "0.2.0"`; `cargo build` regenerates `Cargo.lock` cleanly; `git tag -l v0.2.0` returns `v0.2.0`; the tag's annotation message lists TASK-001..TASK-011 plus any closed follow-ons; `auto --version` first line reads `auto 0.2.0`.
    Verification: `cargo build && ./target/debug/auto --version | head -1` (must read `auto 0.2.0`); `git tag -l v0.2.0` returns `v0.2.0`; `git cat-file -p v0.2.0` shows annotated message with task list.
    Required tests: none (release-mechanics only; `cargo test` regression already covered by prior checkpoints)
    Completion artifacts: `Cargo.toml`, `Cargo.lock`, `COMPLETED.md`, `refs/tags/v0.2.0`
    Dependencies: TASK-011
    Estimated scope: S
    Completion signal: annotated `v0.2.0` tag exists locally with the closed task list, `auto --version` confirms the bump.


## Follow-On Work

- [x] `AD-F01` Snapshot-only generation implementation

  Spec: `specs/230426-planning-corpus-and-generation.md`
  Why now: The generator needs an explicit snapshot-only path so planning runs can produce `gen-*` outputs without mutating root specs, root implementation plan, `genesis/`, or source files.
  Codebase evidence: `src/generation.rs` syncs generated specs and plans to root through `sync_verified_generation_outputs`; `GenerationArgs` has `--plan-only` and `--sync-only` but no explicit no-sync output mode; generated spec verification already validates files under `<output>/specs`.
  Owns: `src/main.rs`, `src/generation.rs`, `README.md`
  Integration touchpoints: `auto gen`, `auto reverse`, `.auto/state`, root `specs/`, root `IMPLEMENTATION_PLAN.md`, `gen-*`
  Scope boundary: Implement the decided snapshot-only path only; do not alter normal sync behavior unless the decision explicitly requires a new opt-in command for sync.
  Acceptance criteria: Snapshot-only generation writes specs and implementation plan only under the requested output directory; root `specs/`, root `IMPLEMENTATION_PLAN.md`, `genesis/`, and source files remain unchanged; normal sync path remains available and tested.
  Verification: Before implementation, run `cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs` and confirm it fails or is absent; after implementation, run `cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs`; run `cargo test generation::tests::sync_replaces_same_day_duplicate_root_specs_with_canonical_snapshot`; run `cargo test generation::tests::generated_plan_rejects_missing_spec_refs`
  Required tests: `generation::tests::snapshot_only_generation_does_not_sync_root_outputs`, `generation::tests::sync_replaces_same_day_duplicate_root_specs_with_canonical_snapshot`, `generation::tests::generated_plan_rejects_missing_spec_refs`
  Completion artifacts: `README.md`
  Dependencies: `AD-005`
  Estimated scope: M
  Completion signal: Snapshot-only mode is implemented, documented, and proven not to mutate root planning surfaces.

- [x] `AD-F02` CI installed-binary proof

  Spec: `specs/230426-first-run-ci-and-installed-binary-proof.md`
  Why now: CI currently validates source with fmt, clippy, and tests, but it does not prove the installable `auto` binary exposes version and help behavior from PATH.
  Codebase evidence: `.github/workflows/ci.yml` runs formatting, clippy, and an unfiltered Rust test step; `README.md` documents `cargo install --path . --root ~/.local` and `auto --version`; `Cargo.toml` declares binary name `auto`.
  Owns: `.github/workflows/ci.yml`, `README.md`
  Integration touchpoints: `cargo install --path . --root`, `auto --version`, `auto --help`, `auto corpus --help`, `auto gen --help`, `auto parallel --help`, `auto quota --help`, `auto symphony --help`
  Scope boundary: Add installed-binary proof to CI and docs only; do not change package versioning, release tagging, or `auto ship`.
  Acceptance criteria: CI installs `auto` into a temporary root, prepends that root to PATH, verifies `auto --version`, and smoke-checks the required help surfaces; README first-run and CI instructions stay consistent.
  Verification: `cargo test doctor_command::tests::doctor_checks_expected_help_surfaces`; `rg -n "cargo install --path \\. --root|auto --version|auto --help|auto corpus --help|auto gen --help|auto parallel --help|auto quota --help|auto symphony --help" .github/workflows/ci.yml README.md`
  Required tests: `doctor_command::tests::doctor_checks_expected_help_surfaces`
  Completion artifacts: `.github/workflows/ci.yml`, `README.md`
  Dependencies: `AD-007`
  Estimated scope: S
  Completion signal: CI contains installed-binary proof and README names the same commands.

- [x] `AD-F03` Backend policy metadata model

  Spec: `specs/230426-backend-invocation-policy-and-model-routing.md`
  Why now: After the invocation inventory is reviewed, code can start expressing backend policy as data instead of scattered argv construction.
  Codebase evidence: Shared Codex and Claude wrappers exist in `src/codex_exec.rs` and `src/claude_exec.rs`, while generation, bug, nemesis, audit, Symphony, loop, and parallel still own direct or wrapper-specific invocation details.
  Owns: `src/backend_policy.rs`, `src/main.rs`
  Integration touchpoints: `src/generation.rs`, `src/codex_exec.rs`, `src/claude_exec.rs`, `src/kimi_backend.rs`, `src/pi_backend.rs`, `src/bug_command.rs`, `src/nemesis.rs`, `src/audit_command.rs`, `src/symphony_command.rs`
  Scope boundary: Add policy metadata and serialization tests only; do not route commands through a new runtime abstraction or change argv construction in this task.
  Acceptance criteria: A policy data model can serialize provider name, model, effort, quota routing, dangerous flags, JSON/output mode, logging posture, timeout/futility posture, and context window; every known provider family from the inventory has a static fixture.
  Verification: Before implementation, run `cargo test backend_policy::tests::serializes_known_backend_policy_inventory` and confirm it fails or is absent; after implementation, run `cargo test backend_policy::tests::serializes_known_backend_policy_inventory`; run `cargo test generation::tests::generation_author_backend_uses_codex_for_non_claude_models`; run `cargo test kimi_backend::tests::exec_args_contain_yolo_and_print_and_stream_json`; run `cargo test pi_backend::tests::minimax_alias_defaults_to_m27_highspeed`
  Required tests: `backend_policy::tests::serializes_known_backend_policy_inventory`, `generation::tests::generation_author_backend_uses_codex_for_non_claude_models`, `kimi_backend::tests::exec_args_contain_yolo_and_print_and_stream_json`, `pi_backend::tests::minimax_alias_defaults_to_m27_highspeed`
  Completion artifacts: none
  Dependencies: `AD-001`
  Estimated scope: M
  Completion signal: Backend policy metadata serializes all inventoried provider families without changing runtime invocation behavior.

- [x] `AD-F04` Migrate executor adapters to shared parser

  Spec: `specs/230426-shared-task-parser-and-blocked-preservation.md`
  Why now: After the shared parser and generation preservation checkpoint, parallel, Symphony, review harvest, and completion evidence should agree on statuses, dependencies, verification, and artifacts through adapters instead of duplicated parsers.
  Codebase evidence: `src/parallel_command.rs`, `src/symphony_command.rs`, `src/review_command.rs`, and `src/completion_artifacts.rs` each parse task headers or task fields independently today; `src/parallel_command.rs` has the broadest dependency and completion-path scheduling coverage.
  Owns: `src/parallel_command.rs`, `src/symphony_command.rs`, `src/review_command.rs`, `src/completion_artifacts.rs`, `src/task_parser.rs`
  Integration touchpoints: `auto loop`, `auto parallel`, `auto symphony sync`, `auto review`, `.auto/symphony/verification-receipts`, `REVIEW.md`
  Scope boundary: Migrate adapters incrementally while preserving command-specific scheduling behavior; do not change worker prompt text, Linear API behavior, or completion evidence rules except to use shared parsed fields.
  Acceptance criteria: Parallel and Symphony status/dependency tests still pass through shared parser adapters; review harvest still moves completed plan items only; completion evidence reads verification and artifact fields through the shared parser; completion-path placeholders remain visible but unscheduled.
  Verification: `cargo test parallel_command::tests::parse_loop_plan_tracks_ready_and_blocked_dependencies`; `cargo test parallel_command::tests::parse_loop_plan_treats_partial_tasks_as_unfinished_dependencies`; `cargo test symphony_command::tests::parse_tasks_recognizes_partial_items`; `cargo test review_command::tests::harvest_completed_plan_items_flows_through_completed_queue`; `cargo test completion_artifacts::tests::verification_plan_preserves_narrative_without_treating_it_as_shell`
  Required tests: `parallel_command::tests::parse_loop_plan_tracks_ready_and_blocked_dependencies`, `parallel_command::tests::parse_loop_plan_treats_partial_tasks_as_unfinished_dependencies`, `symphony_command::tests::parse_tasks_recognizes_partial_items`, `review_command::tests::harvest_completed_plan_items_flows_through_completed_queue`, `completion_artifacts::tests::verification_plan_preserves_narrative_without_treating_it_as_shell`
  Completion artifacts: none
  Dependencies: `AD-004`
  Estimated scope: M
  Completion signal: Shared parser adapters back the major executor parsers while existing targeted parser and evidence tests remain green.

- [x] `AD-F05` Loop completion evidence convergence

  Spec: `specs/230426-parallel-loop-and-lane-recovery.md`
  Why now: `auto parallel` has stronger completion evidence enforcement than `auto loop`; loop should stop treating prompt-level completion as enough when a task declares executable verification and artifacts.
  Codebase evidence: `src/parallel_command.rs` reconciles landed lane work through completion evidence and marks partial when evidence is incomplete; `src/loop_command.rs` uses prompt-level rules to choose tasks and perform progress/commit checks without the same mechanical receipt/artifact gate.
  Owns: `src/loop_command.rs`, `src/completion_artifacts.rs`
  Integration touchpoints: `auto loop`, `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `.auto/symphony/verification-receipts`, `scripts/run-task-verification.sh`
  Scope boundary: Add loop completion evidence checks only; do not introduce tmux behavior, lane salvage, Linear sync, or parallel scheduling into `auto loop`.
  Acceptance criteria: Loop leaves a task partial when review handoff, required receipts, or declared artifacts are missing; narrative verification guidance is not executed as shell input; blocked `[!]` tasks and partial completion-path placeholders remain unscheduled.
  Verification: Before implementation, run `cargo test loop_command::tests::loop_marks_task_partial_when_completion_evidence_missing` and confirm it fails or is absent; after implementation, run `cargo test loop_command::tests::loop_marks_task_partial_when_completion_evidence_missing`; run `cargo test completion_artifacts::tests::inspect_task_completion_evidence_requires_wrapper_for_executable_verification`; run `cargo test parallel_command::tests::parse_loop_plan_skips_partial_completion_path_placeholders`
  Required tests: `loop_command::tests::loop_marks_task_partial_when_completion_evidence_missing`, `completion_artifacts::tests::inspect_task_completion_evidence_requires_wrapper_for_executable_verification`, `parallel_command::tests::parse_loop_plan_skips_partial_completion_path_placeholders`
  Completion artifacts: none
  Dependencies: `AD-004`, `AD-013`
  Estimated scope: M
  Completion signal: Loop completion behavior matches the shared evidence gate for receipt-backed tasks without adopting parallel lane mechanics.

- [ ] `AD-F06` Mechanical ship gate

  Spec: `specs/230426-quality-pipelines-and-release-lifecycle.md`
  Why now: Once release readiness policy and installed-binary proof exist, `auto ship` can fail before invoking a model when required local evidence is missing or stale.
  Codebase evidence: `src/ship_command.rs` currently delegates release readiness to a prompt and `SHIP.md`; `.github/workflows/ci.yml` has no installed-binary proof until the CI follow-on lands; `src/completion_artifacts.rs` can inspect task evidence for receipts and artifacts.
  Owns: `src/ship_command.rs`, `README.md`
  Integration touchpoints: `auto ship`, `SHIP.md`, `QA.md`, `HEALTH.md`, `.github/workflows/ci.yml`, `scripts/run-task-verification.sh`, `auto --version`
  Scope boundary: Add local pre-model gate checks only; do not push, open PRs, deploy, or change release branch detection in this task.
  Acceptance criteria: `auto ship` reports missing installed-binary proof, stale or missing QA/health evidence, red validation, unresolved blockers, or missing rollback/monitoring notes before model execution; the gate can be bypassed only through an explicit operator flag that records the reason in `SHIP.md`.
  Verification: Before implementation, run `cargo test ship_command::tests::ship_gate_fails_without_installed_binary_proof` and confirm it fails or is absent; after implementation, run `cargo test ship_command::tests::ship_gate_fails_without_installed_binary_proof`; run `cargo test ship_command::tests::ship_gate_reports_stale_qa_or_health`; run `cargo test ship_command::tests::default_ship_prompt_includes_operational_release_controls`
  Required tests: `ship_command::tests::ship_gate_fails_without_installed_binary_proof`, `ship_command::tests::ship_gate_reports_stale_qa_or_health`, `ship_command::tests::default_ship_prompt_includes_operational_release_controls`
  Completion artifacts: `README.md`
  Dependencies: `AD-017`, `AD-F02`
  Estimated scope: M
  Completion signal: Ship gate tests prove missing release evidence fails before model execution and docs describe the bypass semantics.

## Completed / Already Satisfied

- [x] `SAT-001` `Cargo.toml` already declares package `autodev` version `0.2.0` and binary `auto` at `src/main.rs`.

- [x] `SAT-002` `build.rs` already embeds git SHA, dirty state, and build profile, and `src/util.rs` exposes that metadata through `CLI_LONG_VERSION`.

- [x] `SAT-003` `.github/workflows/ci.yml` already runs formatting, clippy, and an unfiltered Rust test step.

- [x] `SAT-004` `src/main.rs` currently exposes 17 top-level command variants: `Corpus`, `Gen`, `Super`, `Reverse`, `Bug`, `Loop`, `Parallel`, `Qa`, `QaOnly`, `Health`, `Review`, `Steward`, `Audit`, `Ship`, `Nemesis`, `Quota`, and `Symphony`.

- [x] `SAT-005` `src/util.rs` already excludes `.auto`, `.claude/worktrees`, `bug`, `nemesis`, and top-level `gen-*` from checkpoint staging.

- [x] `SAT-006` `auto parallel status` is already implemented as a status-only action that reports repo, branch, run root, tmux state, host processes, lanes, frontier, and health without launching new work.

- [~] `SAT-007` `src/completion_artifacts.rs` already requires review handoff, verification receipt presence when executable verification exists, and declared completion artifact existence before task completion is fully evidenced.

- [~] `SAT-008` `src/symphony_command.rs` already requires `--symphony-root` or `AUTODEV_SYMPHONY_ROOT` for foreground Symphony runs and has deterministic parser tests for pending, blocked, partial, and completed task rows.
