# ARCHIVED

## `TASK-001`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `README.md`
- Review result: passed after reconciling a stale README default-model claim. The detailed `auto bug` model layout now matches the live CLI defaults: Codex `gpt-5.5` `high` across finder/skeptic/reviewer/fixer/finalizer.
- Validation: receipt-backed proof in `.auto/symphony/verification-receipts/TASK-001.json` for `grep -n "thirteen\|sixteen" README.md`, `grep -n "Kimi.*finder\|Codex finalizer" README.md`, and `grep -n "auto steward\|auto audit\|auto symphony" README.md`.
- Completion artifacts: `README.md`
- Remaining blockers: none.

## `TASK-002`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `README.md`
- Review result: passed. The original heading grep in `REVIEW.md` was shell-malformed because its backtick was not escaped; the corrected receipt-backed command is recorded.
- Validation: receipt-backed proof in `.auto/symphony/verification-receipts/TASK-002.json` for `rg -n "^### \`" README.md`, `grep -nE "DRIFT.md|HINGES.md|RETIRE.md|HAZARDS.md|STEWARDSHIP-REPORT.md|PROMOTIONS.md" README.md`, and `grep -n "audit/DOCTRINE.md" README.md`.
- Completion artifacts: `README.md`
- Remaining blockers: none.

## `TASK-004`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/audit_command.rs`
- Review result: passed after fixing a required correctness issue: `auto audit --resume` now compares manifest `content_hash` values against live file content before skipping audited files.
- Validation: receipt-backed proof in `.auto/symphony/verification-receipts/TASK-004.json` for `cargo test audit_command::tests::`.
- Completion artifacts: `src/audit_command.rs`
- Remaining blockers: none.

## `TASK-003`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/codex_exec.rs`
- Review result: passed. The cited `--lib` cargo commands were invalid for this bin-only crate, so receipt-backed proof uses the truthful binary-target commands.
- Validation: receipt-backed proof in `.auto/symphony/verification-receipts/TASK-003.json` for `cargo clippy -p autodev --bins -- -D warnings`, `cargo test codex_exec`, and `cargo build`.
- Completion artifacts: `src/codex_exec.rs`
- Remaining blockers: none.

## `TASK-005`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_usage.rs`, `src/util.rs`
- Review result: passed after hardening sensitive writes to open/create files with owner-only mode instead of writing first and chmodding after.
- Validation: receipt-backed proof in `.auto/symphony/verification-receipts/TASK-005.json` for `cargo test util::tests::chmod_0o600_if_unix_sets_owner_only_mode`, `cargo test quota_config::tests::save_writes_owner_only`, `cargo test quota_state::tests::save_writes_owner_only`, and `cargo test util::tests::write_0o600_if_unix_tightens_existing_file_before_write`.
- Completion artifacts: `src/util.rs`, `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_usage.rs`
- Remaining blockers: none.

## `TASK-006`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/quota_status.rs`, `src/quota_usage.rs`
- Review result: passed after fixing a required security finding in the quota blast radius: quota usage error sanitization now scans the full anyhow chain for token material, and quota selection no longer logs raw `{e:#}` usage-fetch failures.
- Validation: the cited `cargo test --lib ...` commands are invalid for this bin-only crate; corrected proof is `cargo test quota_usage::tests::claude_refresh_error_does_not_leak_body`, `cargo test quota_status::tests::print_does_not_leak_token_chain`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `src/quota_usage.rs`, `src/quota_status.rs`
- Remaining blockers: none.

## `TASK-008`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `.github/workflows/ci.yml`
- Review result: passed after fixing a required CI-fidelity finding: `cargo fmt --check` failed on live `src/quota_exec.rs`, so rustfmt was applied to keep the workflow executable against the current tree.
- Validation: `actionlint .github/workflows/ci.yml`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `.github/workflows/ci.yml`
- Remaining blockers: none.

## `TASK-012`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `docs/decisions/loop-receipt-gating.md`
- Review result: passed as a documentation decision. The decision accurately records that `auto loop` remains prompt-only for receipt enforcement and that hard receipt gating lives in `auto parallel` reconciliation today.
- Validation: the cited `cargo test --lib loop_command::tests::downgrades_marker_when_receipt_missing` is invalid for this bin-only crate, and the corrected non-`--lib` filter selects zero tests because that hypothetical test does not exist. Truthful supporting proof is `cargo test loop_command::tests::`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `docs/decisions/loop-receipt-gating.md`
- Remaining blockers: none for this decision record; stale generated verification-command synthesis is tracked in `WORKLIST.md`.

## `TASK-009`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/loop_command.rs`, `src/main.rs`, `src/parallel_command.rs`, `src/review_command.rs`, `src/symphony_command.rs`
- Review result: passed after reconciling stale Symphony-root documentation. Current code has no hardcoded `/home/r/coding/symphony/elixir` default; `auto symphony run` requires `--symphony-root` or `AUTODEV_SYMPHONY_ROOT` and reports an actionable error when unset.
- Validation: the cited `cargo test --lib ...` command is invalid for this bin-only crate and `grep -n "/home/r/coding" src/` fails because `src/` is a directory. Corrected proof is `cargo test symphony_command::tests::run_requires_symphony_root_when_unset`, `cargo test symphony_run_help_mentions_symphony_root_env`, `! rg -n "/home/r/coding|symphony/elixir" src specs docs README.md`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `src/main.rs`, `src/symphony_command.rs`
- Remaining blockers: none.

## `TASK-013`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `docs/decisions/symphony-graphql-surface.md`, `specs/220426-symphony-linear-orchestration.md`
- Review result: passed after refreshing the Symphony orchestration spec to match the accepted GraphQL-surface decision and the current Symphony-root resolution behavior.
- Validation: `rg -n "GraphQL-only|linear_tracker.rs|symphony_command.rs|AUTODEV_SYMPHONY_ROOT" docs/decisions/symphony-graphql-surface.md specs/220426-symphony-linear-orchestration.md`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `docs/decisions/symphony-graphql-surface.md`, `specs/220426-symphony-linear-orchestration.md`
- Remaining blockers: none.

## `TASK-014`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/util.rs`
- Review result: passed. The stale queue blocker claimed the verification receipt was missing, but `.auto/symphony/verification-receipts/TASK-014.json` exists and the three `atomic_write` regression tests pass against the live tree.
- Validation: `cargo test util::tests::atomic_write_creates_missing_parent_dir`, `cargo test util::tests::atomic_write_handles_rapid_succession_collisions`, `cargo test util::tests::atomic_write_works_outside_git_repo`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `src/util.rs`
- Remaining blockers: none.

## `TASK-010`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/steward_command.rs`
- Review result: passed after fixing a required conditional-side-effect issue: `auto steward` now refuses no-planning-surface repos before creating the default `steward/` output directory, and a regression test asserts the refusal leaves that directory absent.
- Validation: `cargo test steward_command::tests::dry_run_succeeds_without_planning_surface`, `cargo test steward_command::tests::refuses_to_run_when_no_planning_surface_present`, `cargo test steward_command::tests::refusal_does_not_create_default_output_dir`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `src/steward_command.rs`
- Remaining blockers: none.

## `TASK-015`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/quota_exec.rs`
- Review result: passed after fixing a required credential-copy hardening issue: quota swap, backup, and restore paths now read source bytes and write through the owner-only writer instead of copying first and chmodding after.
- Validation: `cargo test quota_exec::tests::swap_credentials_enforces_0o600`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `src/quota_exec.rs`
- Remaining blockers: none.

## `TASK-007`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `IMPLEMENTATION_PLAN.md`
- Review result: passed. The live receipt `.auto/symphony/verification-receipts/TASK-007.json` supports the current-state baseline checkpoint.
- Validation: `cargo build`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `IMPLEMENTATION_PLAN.md` current-state baseline note
- Remaining blockers: none.

## `TASK-011`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: none recorded by host
- Review result: passed after reconciling the stale handoff claim with the live receipt `.auto/symphony/verification-receipts/TASK-011.json`. The task is a baseline checkpoint with receipt-backed proof and no owned code surface beyond the already-recorded plan state.
- Validation: `actionlint .github/workflows/ci.yml`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `COMPLETED.md`
- Remaining blockers: none.

## `TASK-016`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `Cargo.lock`, `Cargo.toml`
- Review result: passed after reconciling stale queue prose. The live receipt `.auto/symphony/verification-receipts/TASK-016.json` now records the previously missing tag checks, and `refs/tags/v0.2.0` resolves to the release-baseline tag.
- Validation: `git tag -l v0.2.0`, `git cat-file -p v0.2.0`, `cargo build`, `./target/debug/auto --version`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo check`, and `cargo test`.
- Completion artifacts: `COMPLETED.md`, `refs/tags/v0.2.0`
- Remaining blockers: none.

## Nemesis Audit Findings (spec: specs/050426-nemesis-audit.md)
- Review result: passed after fixing a [Required] NEM-F2 regression found during review. `resolve_auditor_model` now preserves an explicit non-default `--model` before applying `--kimi` or `--minimax`, while shorthand flags still select their models when the default model is otherwise in use. The remaining NEM-F1 and NEM-F3 through NEM-F10 claims matched the live code and regression tests.
- Validation: `cargo test` (365 passed), `cargo check`, `cargo fmt --check`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `src/nemesis.rs`, `src/util.rs`, `src/bug_command.rs`, `specs/050426-nemesis-audit.md`
- Remaining blockers: none.

## Quota Router Primary Selection and Review Repo Discovery
- Review result: passed after fixing a [Required] review discovery regression found during review. `auto review` now parses with sibling repo discovery enabled by default, and a parser-level regression test covers that exposed CLI behavior. The quota primary-account selection, status marker, and session-headroom fallback claims matched the live code and tests.
- Validation: `cargo test` (365 passed), `cargo check`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo install --path . --root ~/.local`.
- Completion artifacts: `src/main.rs`, `src/quota_exec.rs`, `src/quota_selector.rs`, `src/quota_status.rs`, `src/review_command.rs`
- Remaining blockers: none.

## `TASK-016`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `Cargo.lock`, `Cargo.toml`, `COMPLETED.md`, `refs/tags/v0.2.0`
- Review result: passed after reconciling stale duplicate queue prose. In the current checkout, `.auto/symphony/verification-receipts/TASK-016.json` exists, `Cargo.toml` and `Cargo.lock` are at `0.2.0`, and `refs/tags/v0.2.0` resolves to annotated tag object `0cef62a2f472a16e578fc433dc83c70d7bfe5085` pointing at `461fe8d04853fbd58edfbf81c5ebe53e31c77ed3`. The tag annotation lists the release baseline through `TASK-015`; later follow-on work is newer than the tag and is not part of this artifact.
- Validation: `cargo build`, `./target/debug/auto --version`, `git tag -l v0.2.0`, `git cat-file -p v0.2.0`, `cargo fmt --check`, `cargo check`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `COMPLETED.md`, `refs/tags/v0.2.0`
- Remaining blockers: none.

## `AD-001`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `docs/decisions/backend-invocation-policy.md`
- Review result: passed. The decision is a documentation inventory gate, and the current backend surfaces still match the policy's provider, quota, logging, timeout, and dangerous-flag inventory. No code-path widening or trust-boundary regression was found in this batch surface.
- Validation: `rg -n "Command::new|TokioCommand::new|run_codex_exec|run_claude_exec|run_claude_prompt|run_logged_author_phase|kimi-cli|pi_bin|dangerously|--yolo|quota open" src`, `rg -n "src/generation.rs|src/codex_exec.rs|src/claude_exec.rs|src/kimi_backend.rs|src/pi_backend.rs|src/bug_command.rs|src/nemesis.rs|src/audit_command.rs|src/symphony_command.rs|src/parallel_command.rs" docs/decisions/backend-invocation-policy.md`, `cargo fmt --check`, `cargo check`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `docs/decisions/backend-invocation-policy.md`
- Remaining blockers: none.

## `AD-012`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `docs/decisions/verification-receipt-policy.md`
- Review result: passed. The policy accurately records the intended receipt-output and zero-test contract, while current implementation follow-up is owned by later receipt-hardening tasks rather than this documentation decision.
- Validation: `rg -n "verification_receipt|run-task-verification|0 tests|receipt write" scripts src/completion_artifacts.rs src/parallel_command.rs`, `rg -n "zero-test|0 tests|stdout|stderr|receipt|redact|fatal" docs/decisions/verification-receipt-policy.md`, `cargo fmt --check`, `cargo check`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `docs/decisions/verification-receipt-policy.md`
- Remaining blockers: none.

## `AD-006`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `docs/decisions/first-run-preflight.md`
- Review result: passed after reconciling stale receipt prose. The formerly cited missing command is now present as a quoted, passing receipt entry, and the decision remains consistent with the later implemented `auto doctor` surface.
- Validation: `rg -n "Doctor|doctor|self-test|preflight" src/main.rs src/*_command.rs README.md`, `rg -n "first-run|no-model|codex|claude|pi|gh|auto --version" docs/decisions/first-run-preflight.md`, `cargo fmt --check`, `cargo check`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `docs/decisions/first-run-preflight.md`
- Remaining blockers: none.

## `AD-008`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/quota_config.rs`
- Review result: passed. The quota profile capture path now stages credential copies, rejects symlinked or non-regular credential sources, writes copied files owner-only, and replaces profile directories without preserving stale files. Adjacent tests cover the trust-boundary and stale-profile cases.
- Validation: `cargo test quota_config::tests::capture_rejects_symlinked_codex_auth`, `cargo test quota_config::tests::capture_prunes_stale_profile_files`, `cargo test quota_config::tests::save_writes_owner_only`, `cargo test quota_config::tests::`, `cargo fmt --check`, `cargo check`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `src/quota_config.rs`
- Remaining blockers: none.

## `AD-011`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/symphony_command.rs`
- Review result: passed. The workflow renderer validates hostile scalar inputs before writing shell/YAML workflow text, quotes branch/model/effort/path values on the generated command surfaces, and preserves the unattended Symphony operating contract. No wider trust-boundary or performance issue was found.
- Validation: `cargo test symphony_command::tests::run_requires_symphony_root_when_unset`, `cargo test symphony_command::tests::shell_quote_escapes_single_quotes`, `cargo test symphony_command::tests::workflow_render_is_repo_specific`, `cargo test symphony_command::tests::workflow_render_rejects_hostile_branch`, `cargo test symphony_command::tests::workflow_render_rejects_hostile_model_and_effort`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: none
- Remaining blockers: none.

## `AD-002`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/main.rs`, `src/task_parser.rs`
- Review result: passed. The shared task parser correctly preserves status, body, dependency, verification, completion-artifact, and completion-path metadata needed by later queue/plan consumers. No parser-boundary regression was found in adjacent command surfaces.
- Validation: `cargo test task_parser::tests::completion_path_placeholders_are_metadata_not_ready_work`, `cargo test task_parser::tests::dependencies_none_and_multiline_notes_are_stable`, `cargo test task_parser::tests::parses_all_plan_statuses_and_fields`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: none
- Remaining blockers: none.

## `AD-016`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/qa_only_command.rs`
- Review result: passed after fixing a [Required] dirty-state detection gap found during review. `auto qa-only` now reads `git status --porcelain=v1 -z` instead of human-formatted short status, so pre-existing dirty paths with spaces are fingerprinted by their real repo-relative path and subsequent source edits are still reported as violations.
- Validation: `cargo test qa_only_command::tests::qa_only_allows_qa_md_and_auto_logs`, `cargo test qa_only_command::tests::qa_only_rejects_non_report_file_changes`, `cargo test qa_only_command::tests::qa_only_reports_preexisting_dirty_state`, `cargo test qa_only_command::tests::qa_only_detects_changes_to_preexisting_dirty_paths_with_spaces`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `src/qa_only_command.rs`
- Remaining blockers: none.

## `AD-013`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `scripts/run-task-verification.sh`, `scripts/verification_receipt.py`, `src/completion_artifacts.rs`
- Review result: passed. The verification wrapper captures stdout/stderr summaries into receipts, successful command receipt-write failures fail closed, and completion evidence rejects receipt-backed zero-test runs for supported runners.
- Validation: `cargo test completion_artifacts::tests::inspect_task_completion_evidence_accepts_quoted_command_receipts_with_argv`, `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_failed_receipts`, `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests`, `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_pytest_tests`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: none
- Remaining blockers: none.

## `AD-009`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/quota_exec.rs`
- Review result: passed after fixing a [Required] restore edge found during review. The quota swap guard now records whether each auth target existed before the swap, restores from backups when present, and removes temporary active auth files when no original existed. Credential writes also create missing auth parent directories before the owner-only write.
- Validation: `cargo test quota_exec::tests::restore_credentials_restores_claude_json_backup`, `cargo test quota_exec::tests::swap_credentials_enforces_0o600`, `cargo test quota_exec::tests::swap_credentials_rejects_symlinked_claude_profile_paths`, `cargo test quota_exec::tests::swap_credentials_restores_claude_json_on_drop`, `cargo test quota_exec::tests::swap_credentials_removes_codex_auth_when_no_original_existed`, `cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error -- --nocapture`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `src/quota_exec.rs`
- Remaining blockers: none.

## `AD-003`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/generation.rs`, `src/task_parser.rs`
- Review result: passed. Generated plan merging now uses the shared task-header parser, so existing `[!]` blocked tasks are preserved as open work when absent from a newly generated plan while `[x]` and `[X]` rows remain completed and are not requeued. No parser-boundary, trust-boundary, or performance issue was found in the reviewed surface.
- Validation: `cargo test generation::tests::merge_generated_plan_preserves_blocked_tasks`, `cargo test generation::tests::merges_existing_open_tasks_not_present_in_new_plan`, `cargo test parallel_command::tests::parse_loop_plan_tracks_ready_and_blocked_dependencies`, `cargo test symphony_command::tests::parse_tasks_extracts_pending_items_and_dependencies`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: none
- Remaining blockers: none.

## `AD-010`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: none recorded by host
- Review result: passed as a validation checkpoint. The live receipt and rerun proofs cover quota profile symlink rejection, stale profile pruning, Claude home JSON restore, and owner-only credential swaps; the checkpoint owns queue handoff rather than new code.
- Validation: `cargo test quota_config::tests::capture_rejects_symlinked_codex_auth`, `cargo test quota_config::tests::capture_prunes_stale_profile_files`, `cargo test quota_exec::tests::restore_credentials_restores_claude_json_backup`, `cargo test quota_exec::tests::swap_credentials_enforces_0o600`, `rg -n "AD-010|quota|credential|symlink|stale" ARCHIVED.md`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `REVIEW.md`
- Remaining blockers: none.

## `AD-F03`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/backend_policy.rs`, `src/main.rs`
- Review result: passed. The backend policy module is static inventory metadata, is registered in the binary, and its test pins the known backend/provider set plus critical model and quota-routing values. No behavior default changed in this surface.
- Validation: `cargo test backend_policy::tests::serializes_known_backend_policy_inventory`, `cargo test generation::tests::generation_author_backend_uses_codex_for_non_claude_models`, `cargo test kimi_backend::tests::exec_args_contain_yolo_and_print_and_stream_json`, `cargo test pi_backend::tests::minimax_alias_defaults_to_m27_highspeed`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: none
- Remaining blockers: none.

## `AD-004`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: none recorded by host
- Review result: passed as a parser/generation checkpoint. The live proofs cover shared status parsing, blocked-task merge preservation, and completion-evidence gating; no additional parser migration is hidden in this checkpoint surface.
- Validation: `cargo test task_parser::tests::parses_all_plan_statuses_and_fields`, `cargo test generation::tests::merge_generated_plan_preserves_blocked_tasks`, `cargo test completion_artifacts::tests::inspect_task_completion_evidence_requires_review_and_receipts`, `rg -n "AD-004|parser|blocked" ARCHIVED.md`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `REVIEW.md`
- Remaining blockers: none.

## `TASK-002`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `README.md`
- Review result: passed. The README now has detailed `auto steward`, `auto audit`, and `auto symphony` subsections with the expected artifact paths, doctrine default, and command entries; this is a newer README tightening beyond the earlier archived TASK-002 handoff.
- Validation: `rg -n '^### .*auto (steward|audit|symphony)' README.md`, `rg -n 'DRIFT.md|HINGES.md|RETIRE.md|HAZARDS.md|STEWARDSHIP-REPORT.md|PROMOTIONS.md' README.md`, `rg -n 'audit/DOCTRINE.md' README.md`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings`.
- Completion artifacts: `README.md`
- Remaining blockers: none.

## `AD-005`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `docs/decisions/snapshot-only-generation.md`
- Review result: passed as a documentation decision. The decision record truthfully scopes snapshot-only generation as an accepted product contract, and the current generation code is consistent with its root-sync boundaries.
- Validation: receipt-backed proof in `.auto/symphony/verification-receipts/AD-005.json` for `cargo test`, `rg -n "snapshot-only|no-sync|sync-only|plan-only|root specs|IMPLEMENTATION_PLAN.md" docs/decisions/snapshot-only-generation.md`, and `rg -n "sync_verified_generation_outputs|sync_only|plan_only" src/generation.rs`.
- Completion artifacts: `docs/decisions/snapshot-only-generation.md`
- Remaining blockers: none.

## `AD-015`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/audit_command.rs`, `src/util.rs`
- Review result: passed after fixing a required trust-boundary issue: audit commit scopes now convert repo paths to literal Git pathspecs before `git add`, `git status`, and `git commit`, so pathspec-magic-shaped filenames cannot widen the committed surface.
- Validation: `cargo test audit_command::tests::`; receipt-backed prior proof in `.auto/symphony/verification-receipts/AD-015.json` for `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, `cargo test audit_command::tests::audit_commit_excludes_generated_and_runtime_artifacts`, `cargo test audit_command::tests::commit_audit_outputs_uses_scoped_pathspecs`, and `cargo test util::tests::checkpoint_excludes_generated_and_runtime_paths`.
- Completion artifacts: none
- Remaining blockers: none.

## `AD-F04`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/completion_artifacts.rs`, `src/parallel_command.rs`, `src/review_command.rs`, `src/symphony_command.rs`, `src/task_parser.rs`
- Review result: passed. The shared task parser migration preserves dependency parsing, partial completion-path placeholders, completed-plan harvesting, and narrative verification handling across the executor adapters.
- Validation: receipt-backed proof in `.auto/symphony/verification-receipts/AD-F04.json` for the cited parser and completion-artifact regression tests.
- Completion artifacts: none
- Remaining blockers: none.

## `AD-007`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `README.md`, `src/doctor_command.rs`, `src/main.rs`
- Review result: passed. The no-model `auto doctor` command is wired into the CLI, probes required local surfaces through the current executable, and reports missing external tools as capability warnings rather than first-run failures.
- Validation: receipt-backed proof in `.auto/symphony/verification-receipts/AD-007.json` for `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check`, `cargo test`, and the three doctor command regressions.
- Completion artifacts: `README.md`
- Remaining blockers: none.

## `AD-F05`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/loop_command.rs`
- Review result: passed. `auto loop` now reuses the shared task parser, skips partial completion-path placeholders, and demotes done tasks back to partial when required completion evidence is missing before the loop pushes.
- Validation: receipt-backed proof in `.auto/symphony/verification-receipts/AD-F05.json` for `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, and the cited completion-evidence / loop-queue regressions.
- Completion artifacts: none
- Remaining blockers: none.

## `TASK-014`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/util.rs`
- Review result: passed after fixing a [Required] collision-hardening gap found during review. The handoff claimed rapid-collision coverage, but the regression test created contention without forcing temp-name collisions; `atomic_write` now adds an in-process monotonic suffix to the PID and timestamp temp path so same-process rapid writes have a deterministic tie-breaker.
- Validation: `.auto/symphony/verification-receipts/TASK-014.json` exists; `cargo test util::tests::atomic_write_creates_missing_parent_dir`, `cargo test util::tests::atomic_write_handles_rapid_succession_collisions`, `cargo test util::tests::atomic_write_works_outside_git_repo`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `src/util.rs`
- Remaining blockers: none.

## `AD-F01`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `README.md`, `src/generation.rs`, `src/main.rs`, `src/super_command.rs`
- Review result: passed. Live code exposes `--snapshot-only`, rejects `--snapshot-only` with `--sync-only`, saves generation state, and skips root spec / root plan sync for the verified snapshot path; `README.md` documents promotion through later `--sync-only`.
- Validation: `.auto/symphony/verification-receipts/AD-F01.json` exists; `cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs`, `cargo test generation::tests::generated_plan_rejects_missing_spec_refs`, `cargo test generation::tests::sync_replaces_same_day_duplicate_root_specs_with_canonical_snapshot`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `README.md`, `src/generation.rs`, `src/main.rs`, `src/super_command.rs`
- Remaining blockers: none.

## `AD-017`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `docs/decisions/release-readiness-gate.md`
- Review result: passed as a release-policy decision. The decision truthfully keeps mechanical `auto ship` enforcement as follow-on work and defines required receipts, QA/health freshness, release blockers, installed-binary proof, rollback, monitoring, and PR-state evidence. The receipt included one failed duplicate `rg` command with an unquoted pipe pattern; the corrected quoted command and live review proof passed.
- Validation: `.auto/symphony/verification-receipts/AD-017.json` exists; `rg -n "installed-binary|QA.md|HEALTH.md|SHIP.md|receipt|rollback|monitoring|PR" docs/decisions/release-readiness-gate.md`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: `docs/decisions/release-readiness-gate.md`
- Remaining blockers: none.

## `SAT-001`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `Cargo.toml`, `src/main.rs`
- Review result: passed. The already-satisfied claim matches the live tree: `Cargo.toml` declares package `autodev` version `0.2.0` and binary `auto` at `src/main.rs`.
- Validation: `rg -n "name = \"autodev\"|version = \"0.2.0\"|name = \"auto\"|path = \"src/main.rs\"" Cargo.toml`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: none.
- Remaining blockers: none.

## `SAT-003`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `.github/workflows/ci.yml`
- Review result: passed. The already-satisfied claim matches the live tree: CI runs `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and unfiltered `cargo test`; it also includes the later installed-binary smoke step without weakening the claimed checks.
- Validation: `rg -n "cargo fmt --check|cargo clippy --all-targets --all-features -- -D warnings|cargo test|cargo install --path" .github/workflows/ci.yml`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Completion artifacts: none.
- Remaining blockers: none.
