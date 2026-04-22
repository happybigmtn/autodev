# ARCHIVED

## `TASK-001`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `README.md`
- Review result: passed after reconciling a stale README default-model claim. The detailed `auto bug` model layout now matches the live CLI defaults: Kimi `k2.6` finder/skeptic/reviewer/fixer plus Codex `gpt-5.4` finalizer.
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
