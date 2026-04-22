# REVIEW

## `TASK-016`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `Cargo.lock`, `Cargo.toml`
- Scope exceptions: none recorded by host.
- Validation: verification receipt `/home/r/Coding/autodev/.auto/symphony/verification-receipts/TASK-016.json` is missing command(s): `git cat-file -p v0.2.0`, `git tag -l v0.2.0`
- Completion artifacts: `COMPLETED.md`, `refs/tags/v0.2.0`
- Remaining blockers: missing REVIEW.md handoff; verification receipt `/home/r/Coding/autodev/.auto/symphony/verification-receipts/TASK-016.json` is missing command(s): `git cat-file -p v0.2.0`, `git tag -l v0.2.0`; missing completion artifact(s): `refs/tags/v0.2.0`

## Nemesis Audit Findings (spec: specs/050426-nemesis-audit.md)

All 10 findings from the nemesis audit are verified as addressed in the live codebase.
Primary fix commit: 2079927 (`autodev: nemesis fixes`).

- **NEM-F1** Output dir wipe: `prepare_output_dir` archives before wipe; `annotate_output_recovery` documents archive path on failure
- **NEM-F2** Flag override: `resolve_auditor_model` gives explicit `--model` precedence over `--kimi`/`--minimax`
- **NEM-F3** Checkpoint exclusion: centralized via `CHECKPOINT_EXCLUDE_RULES` in `util.rs`
- **NEM-F4** atomic_write temp leak: `atomic_write_failure` helper cleans up on both write and rename errors; two tests cover both paths
- **NEM-F5** Non-atomic staging: `commit_nemesis_outputs_if_needed` snapshots index and restores on failure
- **NEM-F6** Halt on first prune: `ensure_repo_layout_with` collects all failures; test proves all targets visited
- **NEM-F7** Redundant PI prune: removed from nemesis `run_pi`; bug command prunes only at phase boundaries
- **NEM-F8** Verify race window: `verify_nemesis_outputs` checks both files together with all 4 match arms
- **NEM-F9** Date collision: `next_nemesis_spec_destination` uses `%d%m%y-%H%M%S` format
- **NEM-F10** Pre-flight validation: `run_nemesis` checks `pending_tasks.is_empty()` before invoking Codex

Validation: `cargo test` (131 passed), `cargo check` (clean)

## Quota Router Primary Selection and Review Repo Discovery

Added manual primary-account selection for quota-routed Codex and Claude execution, plus the
session-headroom fallback that skips a preferred account once it drops below 25% remaining in the
session or 5h window.

- `auto quota select <provider>` now prompts for the provider account and persists it as that
  provider's primary account
- quota selection prefers the primary account while it remains session-healthy, then falls through
  to the next best candidate using session and weekly headroom
- `auto quota status` marks the current primary account
- `auto review` once again auto-discovers sibling git repos by default, matching its documented
  behavior and tests

Validation: `cargo test` (135 passed), `cargo check` (clean), `cargo install --path . --root ~/.local`
