# IMPLEMENTATION_PLAN

Verified current-state baseline (2026-04-22, branch `main`):
- `cargo test`: 333 passed (single suite)
- `cargo clippy --all-targets --all-features -- -D warnings`: clean
- `cargo fmt --check`: clean
- `auto --version`: prints exactly `auto <ver>` / `commit:` / `dirty:` / `profile:` (4 lines)
- 16 CLI commands wired (`src/main.rs:52-96`); `.github/` and `audit/` directories absent; `steward/` empty.

## Priority Work

- [~] `TASK-001` Update README inventory and defaults to current 16-command surface

    Spec: `specs/220426-readme-truth-pass.md`
    Why now: `README.md:11` claims "thirteen commands" and the inventory list at `README.md:13-25` omits `auto steward`, `auto audit`, and `auto symphony`; `README.md:39` still says MiniMax finder while `src/main.rs:514-515` defaults `--finder-model = "k2.6"`; `README.md:54-55` still says Nemesis runs a PI audit pair by default while `src/main.rs:1065-1116` defaults audit/synthesis/fixer to Kimi `k2.6` and Codex only for the finalizer. This is the highest-leverage user-visible truth gap and unblocks every downstream README truth pass.
    Codebase evidence: `README.md:11`, `README.md:13-25`, `README.md:39`, `README.md:54-55`, `README.md:74-83`, `README.md:1080-1082`; `src/main.rs:52-96` (16 variants confirmed); `src/main.rs:514-552` (bug defaults Kimi `k2.6` finder/skeptic/reviewer/fixer + `gpt-5.4` `high` finalizer); `src/main.rs:1065-1116` and `src/nemesis.rs:637-710` (Nemesis Kimi-first defaults + Codex finalizer).
    Owns: `README.md`
    Integration touchpoints: none (docs-only edit)
    Scope boundary: only the inventory line at `:11`, the inventory bullets at `:13-25`, the side-lane bullets at `:74-83`, the bug defaults bullet at `:39`, the Nemesis defaults bullet at `:54-55`, and the matching design-goal enumeration at `:1080-1082`. Do NOT add new detailed `### auto steward/audit/symphony` subsections in this task — that ships in TASK-002 to keep diffs focused.
    Acceptance criteria: README says "sixteen commands"; inventory list contains all 16 commands in the same order as `src/main.rs:52-96`; `:39` reads "Kimi `k2.6` finder, skeptic, fixer, and reviewer with a final `gpt-5.4` `high` Codex finalizer."; `:54-55` describes Nemesis as Kimi `k2.6` audit/synthesis/fixer plus Codex `gpt-5.4` finalizer rather than a PI audit pair; side-lane bullets call out steward/audit/symphony; design-goal enumeration matches.
    Verification: `rg -n 'sixteen commands' README.md`; `! rg -n 'thirteen|MiniMax finder|PI audit pair by default' README.md`; `rg -n 'auto steward|auto audit|auto symphony' README.md`.
    Required tests: none (docs-only)
    Completion artifacts: `README.md`
    Dependencies: none
    Estimated scope: S
    Completion signal: README inventory, bug defaults paragraph, and Nemesis defaults paragraph match the live CLI surface; no diff produced by `grep -n "thirteen\|MiniMax finder\|PI audit pair by default" README.md`.

- [~] `TASK-002` Add `### auto steward`, `### auto audit`, `### auto symphony` detailed-guide subsections to README

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

- [~] `TASK-003` Remove dead tmux scaffolding from `src/codex_exec.rs`

    Spec: `specs/220426-agent-backend-execution.md`
    Why now: file-wide `#![allow(dead_code)]` at `src/codex_exec.rs:1` is hiding ~430 lines of orphaned tmux helpers (`run_codex_exec_in_tmux_with_env`, `ensure_tmux_lanes`, `spawn_codex_in_tmux`, `render_tmux_codex_script`, `wait_for_tmux_completion`, `read_completed_success`, `codex_stdout_has_agent_progress`, `read_status`, `read_pid`, `tmux_worker_is_alive`, `process_alive`, `ensure_tmux_lane`, `tmux_session_exists`, `tmux_window_exists`, `run_tmux_owned`). Verified unused: `grep` for these symbols across `src/` returns matches only in `codex_exec.rs` itself; `parallel_command.rs` only imports `run_codex_exec_with_env`. Deleting them lets us drop the file-wide `#![allow(dead_code)]`, recovers signal from clippy, and shrinks the file from ~700 to ~300 lines.
    Codebase evidence: `src/codex_exec.rs:1` (`#![allow(dead_code)]`), `src/codex_exec.rs:122-200` (`run_codex_exec_in_tmux_with_env`, `ensure_tmux_lanes`), `src/codex_exec.rs:311-462` (`spawn_codex_in_tmux`, `render_tmux_codex_script`, `wait_for_tmux_completion`), `src/codex_exec.rs:481-615` (helpers), `src/codex_exec.rs:625-643` (log_stderr, read_stream — keep, callers exist via `run_codex_exec_with_env`).
    Owns: `src/codex_exec.rs`
    Integration touchpoints: only `src/parallel_command.rs:16` (`run_codex_exec_with_env`), and the eight other callers using `run_codex_exec` (none touch tmux helpers).
    Scope boundary: delete the listed dead functions and the file-wide `#![allow(dead_code)]`. Keep `run_codex_exec`, `run_codex_exec_with_env`, `spawn_codex`, `write_worker_pid`, `clear_worker_pid`, `log_stderr`, `read_stream`, `shell_quote_path`, `shell_quote`, and `remove_if_exists` (all referenced by the surviving code paths). Do NOT introduce a new `LlmBackend` trait — that is research-only per spec.
    Acceptance criteria: `grep -n "#!\[allow(dead_code)\]" src/codex_exec.rs` returns no matches; `grep -nE "run_codex_exec_in_tmux_with_env|ensure_tmux_lanes|spawn_codex_in_tmux|render_tmux_codex_script|wait_for_tmux_completion|tmux_worker_is_alive|tmux_session_exists|tmux_window_exists|run_tmux_owned|TmuxCodexRunConfig" src/` returns zero matches; `cargo clippy --all-targets --all-features -- -D warnings` stays clean.
    Verification: `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test codex_exec`; `cargo build` finishes without dead-code warnings against the dropped `#![allow]`.
    Required tests: none (deletion-only refactor; existing `codex_stream` futility tests already cover the surviving exec path)
    Completion artifacts: `src/codex_exec.rs`
    Dependencies: none
    Estimated scope: S
    Completion signal: file shrinks substantially, `#![allow(dead_code)]` is gone, and clippy stays clean on the unmodified surviving callers.

- [~] `TASK-004` Lock `auto audit` verdict-application behavior with regression tests

    Spec: `specs/220426-audit-doctrine-pass.md`
    Why now: `src/audit_command.rs` is 1145 lines with only 4 tests, all targeting `glob_match` and `sha256_hex` (`src/audit_command.rs:1145-1175`). The verdict-application branches at `:797-889` (CLEAN / DRIFT-SMALL+SLOP / DRIFT-LARGE+REFACTOR / RETIRE / unknown) have zero coverage, so the next refactor or doctrine change can silently break the dispatch contract. The bail at `:1050` (`auto audit currently requires --use-kimi-cli`) also has no test.
    Codebase evidence: `src/audit_command.rs:797-889` (`apply_verdict`), `src/audit_command.rs:1046-1051` (kimi-cli bail), `src/audit_command.rs:891-919` (`apply_patch`), `src/audit_command.rs:1145-1175` (existing tests).
    Owns: `src/audit_command.rs`
    Integration touchpoints: `src/util.rs::atomic_write` (used by verdict writes); does not touch other modules.
    Scope boundary: add tests inside the existing `#[cfg(test)] mod tests` block. Do NOT change runtime behavior. Do NOT wire `FileVerdict::touched_paths` or `FileVerdict::escalate` (those are spec-flagged as deferred). If a helper needs a small visibility bump (e.g., extracting a pure dispatch helper from `apply_verdict` for testability), keep the change minimal and module-private.
    Acceptance criteria: new tests cover the unknown-verdict branch (returns `Pending`), CLEAN branch (returns `Audited` with no commit), missing-`patch.diff` downgrade-to-worklist branch, and the `--use-kimi-cli=false` bail; tests use a `tempfile::TempDir`-style approach already established by `src/util.rs:903-1099` test helpers (no new dev-deps).
    Verification: `cargo test audit_command::tests::` followed by the four new test names (`apply_verdict_clean_returns_audited`, `apply_verdict_unknown_leaves_pending`, `apply_verdict_drift_small_without_patch_promotes_to_worklist`, `run_audit_requires_use_kimi_cli`); also re-run the four existing tests `glob_match_handles_double_star_prefix`, `sha256_hex_is_deterministic`, `glob_match_handles_extension_wildcard`, `glob_match_handles_literal_path`.
    Required tests: `apply_verdict_clean_returns_audited`, `apply_verdict_unknown_leaves_pending`, `apply_verdict_drift_small_without_patch_promotes_to_worklist`, `run_audit_requires_use_kimi_cli`
    Completion artifacts: `src/audit_command.rs`
    Dependencies: none
    Estimated scope: M
    Completion signal: four new tests pass deterministically and `cargo test audit_command` reports >=8 tests.

- [~] `TASK-005` Tighten Unix permissions on quota credential / config / state writes

    Spec: `specs/220426-quota-routing-and-credential-hardening.md`
    Why now: `src/quota_config.rs:109`, `src/quota_state.rs:51`, and `src/quota_usage.rs:150` call plain `fs::write` for credentials/state/config under `~/.config/quota-router/` and the Claude OAuth credentials file. Anything not chmodded to 0o600 leaves OAuth refresh tokens readable to other users on multi-user hosts. This is a small, narrow security fix.
    Codebase evidence: `src/quota_config.rs:104-111` (config save), `src/quota_state.rs:46-53` (state save), `src/quota_usage.rs:115-155` (`refresh_claude_if_needed` writes back rotated credentials at `:150`).
    Owns: `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_usage.rs`
    Integration touchpoints: `~/.config/quota-router/state.json`, `~/.config/quota-router/config.toml`, `~/.config/<provider>/profile/.credentials.json`. Does NOT touch `quota_exec.rs` swap paths or test-only writes inside `#[cfg(test)]` modules.
    Scope boundary: introduce a small helper `chmod_0o600_if_unix(path: &Path) -> Result<()>` (gated `#[cfg(unix)]`, no-op `#[cfg(not(unix))]`) co-located in `src/util.rs` next to `atomic_write`. Call it after each of the three production `fs::write` callsites. Do NOT migrate the writes to `atomic_write` (different acceptance contract; see TASK-006 hypothesis). Do NOT add encryption-at-rest (explicitly out of scope per spec).
    Acceptance criteria: each of the three writes is followed by `chmod_0o600_if_unix(&path)?`; the helper sets `0o600` via `std::os::unix::fs::PermissionsExt::set_mode`; on non-Unix the helper compiles and is a no-op.
    Verification: `cargo test util::tests::chmod_0o600_if_unix_sets_owner_only_mode`; `cargo test quota_config::tests::save_writes_owner_only`; `cargo test quota_state::tests::save_writes_owner_only`.
    Required tests: `chmod_0o600_if_unix_sets_owner_only_mode`, `save_writes_owner_only` (in `quota_config`), `save_writes_owner_only` (in `quota_state`)
    Completion artifacts: `src/util.rs`, `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_usage.rs`
    Dependencies: none
    Estimated scope: S
    Completion signal: new tests pass under Linux; clippy stays clean; `metadata.permissions().mode() & 0o777 == 0o600` for every affected production path.

- [~] `TASK-006` Scrub raw response bodies and full error chains from quota error surfaces

    Spec: `specs/220426-quota-routing-and-credential-hardening.md`
    Why now: `src/quota_usage.rs:126` interpolates the raw HTTP body into the bail message (`"Claude token refresh returned {status}: {body}"`); `src/quota_usage.rs:245` and `src/quota_status.rs:75` print full anyhow chains via `{e:#}`. A failed refresh that includes an invalid_grant body or a server-echoed token leaks the credential into stdout/logs.
    Codebase evidence: `src/quota_usage.rs:123-126` (raw body interpolation), `src/quota_usage.rs:243-246` (`fetch_claude_usage` error surface), `src/quota_status.rs:70-77` (status print path).
    Owns: `src/quota_usage.rs`, `src/quota_status.rs`
    Integration touchpoints: callers in `src/quota_exec.rs` and `src/quota_status.rs` only — no API contract change for callers; the error-message text shrinks but the `Err` type stays the same.
    Scope boundary: replace the body-interpolation in the Claude refresh bail with a fixed message like `"Claude token refresh failed: provider=claude account=<name> http=<status>"` (no body, no chain); narrow the `{e:#}` in `quota_status.rs:75` to the first cause via `e.to_string()` after sanitizing; same for `quota_usage.rs:245`. Do NOT touch the Codex refresh path (`refresh_codex_with_cli`) — spec only requires Claude scrubbing today; document in code comment that Codex CLI stderr is left to the CLI's own redaction.
    Acceptance criteria: new tests assert the error message does NOT contain known-token markers (`access_token`, `refresh_token`, `Bearer `, `eyJ` JWT prefix) for both the refresh-bail and the status-print paths; tests use a stubbed `anyhow::Error::msg` carrying a fake token-bearing body to validate scrubbing works on real input shape.
    Verification: `cargo test quota_usage::tests::claude_refresh_error_does_not_leak_body`; `cargo test quota_status::tests::print_does_not_leak_token_chain`.
    Required tests: `claude_refresh_error_does_not_leak_body`, `print_does_not_leak_token_chain`
    Completion artifacts: `src/quota_usage.rs`, `src/quota_status.rs`
    Dependencies: TASK-005
    Estimated scope: S
    Completion signal: both tests pass and message-shape regression tests block re-introduction of `{body}` or `{e:#}` in those exact callsites.

- [ ] `TASK-007` Checkpoint: re-confirm clean baseline after the high-risk hardening cluster

    Spec: `specs/220426-build-provenance-and-ci-bootstrap.md`
    Why now: spec part (b) (CI bootstrap) explicitly gates on `cargo clippy -D warnings` being clean. TASK-003 (dead-code deletion), TASK-005 (perm tightening), and TASK-006 (error scrubbing) all touch hot files; before adding CI we must reverify that fmt + clippy + tests are still green and document the verified baseline so the CI workflow won't fail on day one.
    Codebase evidence: `AGENTS.md:13-16` documents `cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo build`, `cargo install --path . --root ~/.local`.
    Owns: `gen-20260422-040815/IMPLEMENTATION_PLAN.md` (this file — append a CHECKPOINT note in COMPLETED.md or under the task), `COMPLETED.md`
    Integration touchpoints: none
    Scope boundary: run the four validation commands listed below and capture verbatim output (test count, fmt diff size, clippy summary). If anything regresses, stop the line and open a new task instead of widening this one. No code edits in this task.
    Acceptance criteria: `cargo fmt --check` exits 0; `cargo clippy --all-targets --all-features -- -D warnings` exits 0; `cargo test` exits 0 with test count >= 332 (current baseline; TASK-004/005/006 should add tests); a one-paragraph note appended to `COMPLETED.md` records the verified counts.
    Verification: `cargo fmt --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test`; `cargo build`.
    Required tests: none
    Completion artifacts: `COMPLETED.md`
    Dependencies: TASK-003, TASK-004, TASK-005, TASK-006
    Estimated scope: XS
    Completion signal: COMPLETED.md notes the post-cluster baseline (fmt clean, clippy clean, test count) so TASK-008 can wire CI confidently.

- [x] `TASK-008` Bootstrap minimal `.github/workflows/ci.yml` with fmt + clippy + test on push and PR

    Spec: `specs/220426-build-provenance-and-ci-bootstrap.md`
    Why now: there is no CI today (`.github/` is absent); every regression has had to be caught by an operator running `cargo test` locally. The repo is already clippy-D-warnings-clean, fmt-clean, and 332 tests green, so wiring CI now is low-risk and locks the baseline.
    Codebase evidence: `ls /home/r/Coding/autodev/.github/` returns "No such file or directory"; `AGENTS.md:13-16` lists the canonical validation commands; `Cargo.toml:8-10` defines the single `auto` binary; `build.rs` already handles tarball-without-`.git` fallback (`build.rs:8,46-62`).
    Owns: `.github/workflows/ci.yml`
    Integration touchpoints: none (greenfield)
    Scope boundary: one workflow file. Triggers: `push` to any branch, `pull_request` against `main`. Single matrix entry: `ubuntu-latest`, stable Rust via `dtolnay/rust-toolchain@stable` SHA-pinned. Steps in order: `actions/checkout@<sha>` (`persist-credentials: false`), `dtolnay/rust-toolchain@stable` with `components: rustfmt,clippy`, `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`. NO macOS matrix, NO `actions/cache`, NO release/publish jobs, NO smoke tests — those are explicit follow-ons per spec.
    Acceptance criteria: workflow file exists; SHA-pinned actions with `# vX.Y.Z` comments; `persist-credentials: false`; clean under `actionlint` and `zizmor`; running the workflow against current `main` would pass (validated locally by re-running the three commands and confirming exit 0).
    Verification: `actionlint .github/workflows/`; `zizmor .github/workflows/`; locally simulate by running each step's command in-shell and confirming exit 0.
    Required tests: none (CI infrastructure)
    Completion artifacts: `.github/workflows/ci.yml`
    Dependencies: TASK-007
    Estimated scope: S
    Completion signal: `actionlint` and `zizmor` both pass against the new workflow with zero warnings, and the three commands the workflow runs all pass locally.

- [~] `TASK-009` Replace the operator-specific hardcoded default in `auto symphony run`

    Spec: `specs/220426-symphony-linear-orchestration.md`
    Why now: `src/main.rs:277` hardcodes `default_value = "/home/r/coding/symphony/elixir"` for `--symphony-root`. Anyone but the original operator hits a path that doesn't exist and gets a confusing error instead of a clean missing-arg error. Spec explicitly flags this as a DX defect that the symphony spec wants removed.
    Codebase evidence: `src/main.rs:274-278` (`#[arg(long, default_value = "/home/r/coding/symphony/elixir")] symphony_root: PathBuf`); the value flows into `src/symphony_command.rs::run_foreground` at `src/symphony_command.rs:1589-1624`.
    Owns: `src/main.rs`, `src/symphony_command.rs`
    Integration touchpoints: `auto symphony run` invocation path in `src/symphony_command.rs::run_symphony` and downstream `run_foreground`/`render_workflow`.
    Scope boundary: change the arg to `Option<PathBuf>` (no `default_value`), make it `--symphony-root` required for `auto symphony run` either via clap `required = true` OR via a runtime resolver that reads `AUTODEV_SYMPHONY_ROOT` env var first, then bails with a clear named-dependency error. Pick the env-var path because it matches the spec's "no operator-specific defaults" guidance and lets the env var be set in `~/.config/autodev/env` without touching the binary. Do NOT touch `auto symphony sync` or `auto symphony workflow` defaults — they don't read this arg.
    Acceptance criteria: `src/main.rs:277` no longer contains the literal `/home/r/coding/`; running `auto symphony run` with neither `--symphony-root` nor `AUTODEV_SYMPHONY_ROOT` exits non-zero with a clear "missing symphony root" error naming both override mechanisms; passing `--symphony-root <path>` or exporting `AUTODEV_SYMPHONY_ROOT=<path>` works; help text mentions both.
    Verification: `cargo test symphony_command::tests::run_requires_symphony_root_when_unset`; `! rg -n '/home/r/coding' src`.
    Required tests: `run_requires_symphony_root_when_unset`
    Completion artifacts: `src/main.rs`, `src/symphony_command.rs`
    Dependencies: none
    Estimated scope: S
    Completion signal: hardcoded operator path is gone, missing-root produces a clear named error, env-var override works.

- [ ] `TASK-010` Enforce "no planning surface" refusal in `auto steward`

    Spec: `specs/220426-steward-mid-flight-reconciliation.md`
    Why now: `detect_planning_surface` at `src/steward_command.rs:251-267` may return an empty list, but `run_steward` (`src/steward_command.rs:30-220`) currently continues and only embeds a "(none detected — consider `auto corpus` instead)" hint inside the prompt. Spec acceptance criterion #1 explicitly says steward must "refuse to run or flag the repo as no active planning surface" — current behavior silently runs anyway and pays for an entire Codex turn.
    Codebase evidence: `src/steward_command.rs:56` (call to `detect_planning_surface`), `src/steward_command.rs:76-83` (planning_surface print line), `src/steward_command.rs:251-267` (helper definition); no early-return on empty.
    Owns: `src/steward_command.rs`
    Integration touchpoints: `src/main.rs:74-80` (StewardArgs comment), `crate::StewardArgs.dry_run`/`report_only` flags (must continue to bypass the refusal so operators can preview).
    Scope boundary: when `planning_surface.is_empty()` AND `!args.dry_run` AND `!args.report_only`, bail with a concrete message naming the nine probed paths and recommending `auto corpus`. Keep `--dry-run` and `--report-only` as escape hatches that still print the plan and prompt path. Do NOT change deliverable list, models, or finalizer behavior.
    Acceptance criteria: new test stands up a tempdir-backed git repo with no planning files and asserts `run_steward` errors with a message containing `"no active planning surface"` and the recommendation `auto corpus`; `--dry-run` against the same fixture exits 0.
    Verification: `cargo test steward_command::tests::refuses_to_run_when_no_planning_surface_present`; `cargo test steward_command::tests::dry_run_succeeds_without_planning_surface`.
    Required tests: `refuses_to_run_when_no_planning_surface_present`, `dry_run_succeeds_without_planning_surface`
    Completion artifacts: `src/steward_command.rs`
    Dependencies: none
    Estimated scope: S
    Completion signal: tests pass, hand-running `auto steward` in a greenfield clone exits non-zero with the actionable error.

- [ ] `TASK-011` Checkpoint: re-confirm clean baseline before opening Follow-On work

    Spec: `specs/220426-build-provenance-and-ci-bootstrap.md`
    Why now: closes the second priority cluster (TASK-008/009/010) before any Follow-On is picked up. Operators (or the CI workflow itself once TASK-008 lands) need a recorded checkpoint so anyone resuming the queue knows the baseline.
    Codebase evidence: `AGENTS.md:13-16` validation commands.
    Owns: `COMPLETED.md`
    Integration touchpoints: none
    Scope boundary: rerun `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, and (if TASK-008 is merged) `actionlint .github/workflows/`. Capture counts in a one-paragraph appendix. No code changes.
    Acceptance criteria: all four commands exit 0; test count >= post-TASK-007 baseline plus the tests added in TASK-009 (1) and TASK-010 (2); COMPLETED.md notes the verified counts and timestamp.
    Verification: `cargo fmt --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test`; `actionlint .github/workflows/` (only if TASK-008 is merged).
    Required tests: none
    Completion artifacts: `COMPLETED.md`
    Dependencies: TASK-008, TASK-009, TASK-010
    Estimated scope: XS
    Completion signal: COMPLETED.md gains a new dated entry confirming the baseline before Follow-On work begins.

## Follow-On Work

- [~] `TASK-012` Decide whether to enforce verification-receipt presence inside `auto loop`

    Spec: `specs/220426-execution-loop-and-parallel.md`
    Why now: spec asserts "loop never marks `[x]` unless evidence is present", but `src/loop_command.rs` does not import `completion_artifacts`; the gating exists only in the prompt and in `parallel_command.rs` / `review_command.rs`. This is a real spec/code drift but spec also flags loop-integration as one of the open questions. Need a decision (research-shaped) before code.
    Codebase evidence: `src/loop_command.rs:1-15` (no `completion_artifacts` import); `src/completion_artifacts.rs:13-20` (`TaskCompletionEvidence`), `src/completion_artifacts.rs:122` (verification_receipt path); `src/parallel_command.rs` and `src/review_command.rs` both call `inspect_task_completion_evidence`.
    Owns: `docs/decisions/loop-receipt-gating.md` (new short ADR), `src/loop_command.rs` (only if the decision is "enforce")
    Integration touchpoints: `src/completion_artifacts.rs::inspect_task_completion_evidence`, `src/loop_command.rs::run_loop` post-iteration check.
    Scope boundary: research-shaped task. Read three things — the spec acceptance criterion, the actual prompt that loop sends (currently lines 17-100+ of `loop_command.rs`), and `parallel_command.rs`'s receipt-check pattern — and write a one-page decision doc recommending one of: (a) enforce in Rust (downgrade `[x]` to `[~]` if the receipt is missing), (b) keep the prompt-only enforcement and document why, or (c) add a soft warning without rewriting markers. If the decision is (a), open a follow-up implementation task; do not implement here.
    Acceptance criteria: decision doc exists and explicitly cites the spec criterion + the two existing call sites; if recommendation is to implement, a follow-up TASK row is added to this plan with concrete acceptance criteria scoped to a single function.
    Verification: review of the decision doc; if implementation lands later, `cargo test loop_command::tests::downgrades_marker_when_receipt_missing`.
    Required tests: none for the decision pass; `downgrades_marker_when_receipt_missing` for the optional follow-up implementation
    Completion artifacts: `docs/decisions/loop-receipt-gating.md`
    Dependencies: TASK-011
    Estimated scope: S
    Completion signal: decision doc lands; either a new implementation task is queued or the current behavior is documented as accepted.

- [x] `TASK-013` Decide whether to dedupe Linear GraphQL surface between `linear_tracker.rs` and `symphony_command.rs`

    Spec: `specs/220426-symphony-linear-orchestration.md`
    Why now: spec says `linear_tracker.rs` GraphQL queries are the sole egress to Linear, but `src/symphony_command.rs:30-200+` carries a parallel set (`AutoSymphonyProject`, `AutoSymphonyProjectIssues`, `CREATE_ISSUE_MUTATION`, `UPDATE_ISSUE_MUTATION`, `UPDATE_ISSUE_AND_STATE_MUTATION`). Either the spec is too narrow or the code drifted. Need a research-shaped review before consolidation.
    Codebase evidence: `src/linear_tracker.rs:16,42,62,76` (queries) and `:128-131` (drift fields); duplicate query strings in `src/symphony_command.rs:30-200`.
    Owns: `docs/decisions/symphony-graphql-surface.md` (new short ADR)
    Integration touchpoints: `src/symphony_command.rs::run_symphony`, `src/linear_tracker.rs`
    Scope boundary: read both modules, list the union of GraphQL operations, and recommend either (a) consolidate `symphony_command.rs` queries into `linear_tracker.rs` and re-export, or (b) widen the spec to cover both surfaces and document why they differ. Implementation lands in a follow-up if consolidation is chosen.
    Acceptance criteria: decision doc names every GraphQL operation in both files and recommends one path; if recommending consolidation, a follow-up implementation task is added with bounded scope (single direction, single PR).
    Verification: read of the decision doc; no code change in this task.
    Required tests: none
    Completion artifacts: `docs/decisions/symphony-graphql-surface.md`
    Dependencies: TASK-011
    Estimated scope: S
    Completion signal: decision doc lands and either a follow-up implementation task is queued or the spec is updated to match reality.

- [~] `TASK-014` Add tmpdir / missing-parent / rapid-collision regression tests for `util::atomic_write`

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

- [ ] `TASK-015` Add 0o600 enforcement to credential SWAP path in `quota_exec.rs`

    Spec: `specs/220426-quota-routing-and-credential-hardening.md`
    Why now: spec inventory of `fs::write` callsites is incomplete. `src/quota_exec.rs` performs `fs::copy` on credential files in `swap_credentials`/`copy_profile_to_active_auth` and creates lockfiles under `~/.config/quota-router/`; on filesystems where `fs::copy` doesn't preserve source mode strictly, the copied `auth.json` may end up world-readable. Worth gating behind TASK-005 to confirm the helper exists first.
    Codebase evidence: `src/quota_exec.rs` (`swap_credentials`, `copy_profile_to_active_auth`, `~/.config/quota-router/backup/` writes), uses `fs::copy` rather than `fs::write`.
    Owns: `src/quota_exec.rs`
    Integration touchpoints: `~/.config/<provider>/profile/auth.json`, `~/.config/quota-router/backup/`
    Scope boundary: after each `fs::copy` of a credential or backup file, call the `chmod_0o600_if_unix` helper added in TASK-005. Do NOT change rotation behavior or backup naming.
    Acceptance criteria: every credential-`fs::copy` callsite is followed by a chmod call; new test asserts the active `auth.json` has `0o600` after a swap (uses the existing test scaffolding pattern in the quota tests).
    Verification: `cargo test quota_exec::tests::swap_credentials_enforces_0o600`.
    Required tests: `swap_credentials_enforces_0o600`
    Completion artifacts: `src/quota_exec.rs`
    Dependencies: TASK-005, TASK-011
    Estimated scope: S
    Completion signal: test passes; complementary coverage to TASK-005.

- [ ] `TASK-016` Tag `v0.2.0` once the priority + first follow-on cluster is verified clean

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

## Completed / Already Satisfied

- `specs/220426-cli-command-surface.md` — 16-command CLI surface verified at `src/main.rs:52-96`; nested `Quota` (`Status`/`Select`/`Accounts`/`Reset`/`Open` plus `AccountsCommand` `Add`/`List`/`Remove`/`Capture`) and `Symphony` (`Sync`/`Workflow`/`Run`) trees confirmed at `src/main.rs:289-340` and `:111-118`; `auto --version` emits exactly four lines (`auto <ver>` / `commit:` / `dirty:` / `profile:`) per `src/util.rs:9-17` and direct binary check; both unit tests in `src/main.rs:1190-1213` pass.
- `specs/220426-shared-util-layer.md` (preservation half) — every named util function is present at the expected line in `src/util.rs` (`git_repo_root` :62, `auto_checkpoint_if_needed` :133, `sync_branch_with_remote` :197, `push_branch_with_remote_sync` :239, `ensure_repo_layout` :324, `timestamp_slug` :383, `atomic_write` :404), `CLI_LONG_VERSION` :9, `CHECKPOINT_EXCLUDE_RULES` :54 (5 elements). Test file already covers checkpoint exclusion, atomic_write rename/write failure cleanup, sync rebase semantics, and `auto_checkpoint_if_needed` conflict-recovery (`src/util.rs:903-1099`). Remaining test gaps tracked in TASK-014.
- `specs/220426-bug-and-nemesis-hardening.md` (current behavior) — bug defaults verified at `src/main.rs:485-572` (Kimi `k2.6` `high` finder/skeptic/reviewer/fixer, `gpt-5.4` `high` finalizer, `--use-kimi-cli` default `true`, `chunk_size` 24); Nemesis defaults verified at `src/main.rs:1055-1138` and `src/nemesis.rs:637-710` (Kimi `k2.6` `high` audit/synthesis/fixer via `kimi-cli`, Codex `gpt-5.4` `high` finalizer, `--minimax` legacy opt-in); archive-then-wipe via `prepare_output_dir`/`maybe_prepare_output_dir`/`annotate_output_recovery` and `--report-only` implementation/finalizer short-circuit are recorded as fixed in `COMPLETED.md`. Spec's open questions (`--resume`, Codex 137 handling, report-only root-sync semantics) remain explicit hypotheses and stay off the priority queue.
- `specs/220426-build-provenance-and-ci-bootstrap.md` (part a only) — `build.rs` correctly emits `AUTODEV_GIT_SHA`, `AUTODEV_GIT_DIRTY`, `AUTODEV_BUILD_PROFILE` with rerun triggers on `.git/HEAD`, `packed-refs`, branch ref, `index`; tarball-without-`.git` fallback returns `"unknown"` (verified at `build.rs:8` and `:46-62`); `--version` output confirms three env vars all populated. CI workflow (part b) is queued as TASK-007/008.
- `specs/220426-agent-backend-execution.md` (current behavior) — `claude_exec`, `codex_exec`, `kimi_backend`, `pi_backend` wrappers all expose the spec'd entry points (`run_claude_exec` `:19`, `run_claude_with_futility` `:46`, `run_codex_exec` `:30`, `run_codex_exec_with_env` `:56`, kimi/pi resolve helpers); futility marker `137` and thresholds `8`/`16` (`src/codex_stream.rs:16,22`); kimi defaults (`KIMI_CLI_DEFAULT_MODEL`/`KIMI_CLI_MODEL_ENV`) and PI defaults present. Cleanup of `codex_exec.rs` dead tmux scaffold queued as TASK-003. `LlmBackend` trait is research-only and intentionally not queued.
- `specs/220426-artifact-formats-and-task-queue.md` — `util::atomic_write` matches spec exactly (`.{filename}.tmp-{pid}-{nanos}` pattern, parent auto-create, temp cleanup on both write and rename failure); plan-marker contract `- [ ]`/`- [!]`/`- [x]`/`- [~]` honored across `loop_command`, `parallel_command`, `review_command`. The shared task-marker helper and shared writer trait remain spec-flagged research and stay off this queue.
- `specs/220426-audit-doctrine-pass.md` (runtime behavior) — six verdict variants dispatched at `src/audit_command.rs:797-889`; doctrine missing-file bail at `:164-173`; `--use-kimi-cli` requirement bail at `:1050`; manifest hashes (`audited_doctrine_hash`/`audited_rubric_hash`) and resume modes (`Fresh`/`Resume`/`OnlyDrifted`) implemented at `:223-253`; per-file artifacts under `audit/files/<sha256[..16]>/` confirmed at `:732-734`. Test-coverage gap queued as TASK-004; `touched_paths`/`escalate` consumption explicitly deferred per spec.
- `specs/220426-execution-loop-and-parallel.md` (current behavior) — `auto loop` queue parsing, branch-resolution, sync-before-rebase, sync-before-push, `auto_checkpoint_if_needed`, `--max-iterations` exit, blocked-skip semantics all in `src/loop_command.rs`; `auto parallel` 5-lane tmux orchestration, status command, and verification-receipt path resolution via `completion_artifacts::inspect_task_completion_evidence` all in `src/parallel_command.rs`. Loop-side receipt enforcement decision queued as TASK-012.
- `specs/220426-planning-pipeline-corpus-gen-reverse.md` — `PlanningCorpus` shape matches spec (`src/corpus.rs:10-23`); `REQUIRED_PLAN_SECTIONS` constants, archive-then-wipe to `.auto/fresh-input/`, gen filename pattern `ddmmyy-<slug>[-counter].md`, and three-section plan validator all live in `src/generation.rs` (lines 109-115, 668, ~1754); reverse never mutates `IMPLEMENTATION_PLAN.md`.
- `specs/220426-quality-commands-qa-health-review.md` — `qa_command` and `review_command` wire `sync_branch_with_remote` / `push_branch_with_remote_sync`, while `qa_only_command` and `health_command` intentionally remain report-only wrappers with no rebase/push today; all four default to `gpt-5.4` `high` where applicable, QA/QA-only default to `standard` tier, write prompt logs through `atomic_write`, and `review_command::run_review` drains `COMPLETED.md` → `REVIEW.md` → `ARCHIVED.md`/`WORKLIST.md`/`LEARNINGS.md` with stale-batch detection via inline `HashMap<Vec<String>, usize>` (`src/review_command.rs:213,241`). The spec's `--json` output, `auto health` tier knob, and report-only rebase decision are explicit research items.
- `specs/220426-quota-routing-and-credential-hardening.md` (rotation half) — multi-account multiplexer, `fd_lock`-serialized swaps, `WEEKLY_FLOOR_PCT=10` / `SESSION_FLOOR_PCT=25` / `EXHAUSTION_COOLDOWN_HOURS=1` floors, primary-account preference with session-headroom fallback, and `auto quota select` persistence are all in place. Hardening half (perm tightening + body scrubbing) queued as TASK-005/006/015. Encryption-at-rest is explicitly out of scope per spec.
- `specs/220426-release-ship.md` — `auto ship` resolves base branch via `--base-branch`/`origin/HEAD`/`KNOWN_PRIMARY_BRANCHES`, runs Codex prep with the documented prompt, writes prompt log via `atomic_write`, syncs and pushes per `sync_branch_with_remote`/`push_branch_with_remote_sync`, and one regression test `default_ship_prompt_includes_operational_release_controls` is in place (`src/ship_command.rs`). Spec acceptance items that depend on the Codex agent's prompt-time behavior (atomic SHIP.md write, gh advisory, codex hard-fail) are noted as prompt-contract items rather than enforceable Rust gates.
- `specs/220426-steward-mid-flight-reconciliation.md` (current behavior) — six STEWARD_DELIVERABLES enumerated at `src/steward_command.rs:21-28`; `--skip-finalizer` short-circuit at `:163-167`; `auto_checkpoint_if_needed` invocations at `:88-96`, `:153-161`, `:202-213`; prompt log via `atomic_write` at `:106-112`; `verify_steward_deliverables` post-check at `:269-285`. The "no planning surface" enforcement gap is queued as TASK-010; deliverable-write atomicity is a Codex-side concern, not a Rust gate.
- `specs/220426-symphony-linear-orchestration.md` (current behavior) — three subcommands wired (`Sync`/`Workflow`/`Run`); GraphQL queries and four drift categories (`missing`/`stale`/`terminal`/`completed_active`) implemented in `src/linear_tracker.rs:16-263`; sync planner, workflow renderer, and foreground orchestrator implemented in `src/symphony_command.rs`. Hardcoded operator path queued as TASK-009; GraphQL surface dedupe decision queued as TASK-013.
- `specs/050426-nemesis-audit.md` (NEM-F1..NEM-F10) — all ten findings recorded as resolved in `COMPLETED.md` with primary fix commit `2079927`.
