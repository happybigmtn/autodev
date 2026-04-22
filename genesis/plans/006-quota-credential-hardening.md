# Plan 006 — Quota credential file permissions and error-message scrubbing

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

## Purpose / Big Picture

`auto quota` stores provider access tokens on disk as plaintext. `quota_config.rs:109, 224-227`, `quota_state.rs:51`, and `quota_usage.rs:150` all use `fs::write` with default umask. On a typical Linux developer machine, that means `auth.json` and `.credentials.json` under `~/.config/quota-router/profiles/<provider>-<account>/` are readable by the owner (correct) and — depending on umask — potentially by the group or world.

Additionally, several error paths in `quota_usage.rs` and `quota_status.rs` include raw provider API response bodies in their error messages (`quota_usage.rs:125-126`, `quota_usage.rs:245`, `quota_status.rs:75`). Depending on what a provider returns on failure, those responses can include refresh tokens or other sensitive metadata.

This plan applies two narrow fixes:

1. All files written under `~/.config/quota-router/profiles/*` are created or updated with mode `0o600` (owner read/write only) on Unix. Non-Unix platforms are unchanged (the current code already relies on `std::fs` portability).
2. Error messages that currently include upstream response bodies are rewritten to redact bodies by default, with opt-in verbose mode via a flag or env var for debugging.

The operator gains a credential store that matches the threat model of a local CLI (owner-only access) and error output that does not leak token material into terminals, shell history, or CI logs. Observable: `stat -c '%a' ~/.config/quota-router/profiles/*/auth.json` returns `600` after a `auto quota accounts capture` run; and `auto quota status` on a refresh failure prints a redacted error instead of a raw body.

## Requirements Trace

- **R1.** Every file written by `quota_config.rs`, `quota_state.rs`, `quota_exec.rs`, or `quota_usage.rs` under the quota-router config directory has mode `0o600` on Unix.
- **R2.** No existing file under the quota-router config directory is changed in place without the mode being re-applied on write.
- **R3.** Error messages that currently include provider response bodies are replaced with a redacted form by default. Bodies are only included when an opt-in environment variable is set (`AUTO_QUOTA_VERBOSE_ERRORS=1`).
- **R4.** At least three tests cover: (a) a newly written file under the config dir has mode `0o600` on Unix, (b) a file rewritten by `save_state` retains mode `0o600`, (c) error-message redaction returns the expected redacted string for a sample Claude token-refresh error.
- **R5.** No change to the wire format of `config.toml`, `state.json`, `auth.json`, or any file the quota router reads or writes.
- **R6.** No change to quota rotation thresholds (`WEEKLY_FLOOR_PCT = 10`, `SESSION_FLOOR_PCT = 25`).
- **R7.** Non-Unix platforms compile and run; permission enforcement is skipped there via `#[cfg(unix)]` gating.

## Scope Boundaries

- **Changing:** `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_exec.rs`, `src/quota_usage.rs`, `src/quota_status.rs`, possibly `src/util.rs` if a shared `atomic_write_with_mode` helper is introduced.
- **Not changing:** `src/quota_accounts.rs`, `src/quota_patterns.rs`, `src/quota_selector.rs` (these do not write credential material).
- **Not changing:** wire formats, CLI subcommands, rotation logic, or provider endpoints.
- **Not introducing:** encryption at rest, a keyring integration, or `keychain-rs` crate. Those are separate product decisions; this plan is file-mode and log-scrub only.

## Progress

- [ ] Identify every `fs::write` call under the quota config dir.
- [ ] Decide whether to extend `atomic_write` to accept a mode parameter or to add a new helper `atomic_write_with_mode`.
- [ ] Implement mode enforcement on all writes.
- [ ] Identify error messages that include raw response bodies.
- [ ] Implement redaction helper and replace call sites.
- [ ] Add `AUTO_QUOTA_VERBOSE_ERRORS` env-var opt-in.
- [ ] Write tests.
- [ ] Run `cargo test` and `cargo clippy -D warnings`.
- [ ] Commit.

## Surprises & Discoveries

None yet. Possible surprise: some call site writes the credential file via `serde_json::to_writer` plus a `File::create`, in which case the mode must be set after creation via `set_permissions`, not via a `fs::write` wrapper.

## Decision Log

- **2026-04-21 — File-mode fix, not encryption.** Taste / User Challenge. Encryption at rest introduces a key-management problem that is meaningful product work (keyring? user passphrase? OS keychain?). The immediate attack surface — shared-user machines, accidental upload of `~/.config` to a backup — is addressed adequately by `0o600` without that product work. Encryption is a future plan.
- **2026-04-21 — Redaction on by default.** Mechanical. Error output should not include token material; this is not a product preference, it is a default that aligns with the rest of the codebase's habit of not logging `ANTHROPIC_API_KEY`.
- **2026-04-21 — Verbose-errors opt-in via env var, not CLI flag.** Taste. Keeps the CLI surface unchanged; operators who need raw error bodies for debugging can set `AUTO_QUOTA_VERBOSE_ERRORS=1` for that shell.
- **2026-04-21 — Do not unify all write helpers in this plan.** Mechanical. A cross-module `atomic_write_with_mode` is part of Plan 007's consolidation if tests show it is a clean extraction; this plan keeps the helper local if unification adds friction.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Files to read before editing:

- `src/quota_config.rs` — profile load/save. Lines 109, 224-227 write credential files via `fs::write`.
- `src/quota_state.rs` — selection state. Line 51 writes `state.json` via `fs::write`.
- `src/quota_exec.rs` — credential swap/restore. Lines 155-156, 214-217, 253-256 perform write operations around lock acquisition.
- `src/quota_usage.rs` — usage-API client. Lines 112-118, 125-126, 150, 245, 350-372 include request/response handling with potentially sensitive error paths.
- `src/quota_status.rs` — status display. Line 75 prints error chains.
- `src/util.rs::atomic_write` — existing helper at lines ~404-426. Candidate for extension.

Reference thresholds to preserve:
- `src/quota_selector.rs:8` — `WEEKLY_FLOOR_PCT = 10`
- `src/quota_selector.rs:10` — `SESSION_FLOOR_PCT = 25`
- `src/quota_usage.rs:48` — Claude 5h session window.

## Plan of Work

1. **Inventory write sites.** `rg 'fs::write|File::create' src/quota_*.rs` to produce the list.
2. **Design mode-enforcement helper.** Prefer extending `util::atomic_write` with an optional mode, gated on `#[cfg(unix)]`. Alternative: a new `quota_fs::write_credential` that uses `std::os::unix::fs::OpenOptionsExt::mode(0o600)` on open.
3. **Apply helper at every write site.** Include renames and rewrites.
4. **Inventory error messages.** `rg 'bail!.*body|bail!.*status|eprintln!.*token' src/quota_*.rs`.
5. **Write a redaction helper.** `fn redact_body(raw: &str, verbose: bool) -> Cow<str>` that returns `"<redacted>"` when `!verbose` and the original otherwise.
6. **Replace call sites.** Each error that embeds a body consults the env var through a helper.
7. **Add tests.** Unix-only permission tests gated by `#[cfg(unix)]`; redaction tests platform-agnostic.
8. **Run the full suite.** `cargo test`, `cargo clippy -D warnings`.
9. **Commit.**

## Implementation Units

**Unit 1 — Introduce mode-aware write helper.**
- Goal: a single function in `util.rs` (or a new `src/secure_fs.rs`) that writes bytes atomically and applies `0o600` on Unix.
- Requirements advanced: R1, R2, R7.
- Dependencies: none.
- Files to create or modify: `src/util.rs` (preferred) or `src/secure_fs.rs` (if the extension to `util.rs` grows past ~30 lines).
- Tests to add or modify: one Unix-gated test verifying the written file is mode `0o600`.
- Approach: use `std::os::unix::fs::OpenOptionsExt::mode(0o600)` on `OpenOptions`. On non-Unix, fall back to existing atomic_write semantics without permission enforcement.
- Test scenarios: writing `/tmp/.../cred.json`, then `stat` → mode `0o600`. Rewriting the same path preserves mode `0o600` even if the previous file had `0o644`.
- Test expectation: the new test passes.

**Unit 2 — Apply the helper to all quota write sites.**
- Goal: no direct `fs::write` call remains in the quota credential write paths.
- Requirements advanced: R1, R2.
- Dependencies: Unit 1.
- Files to create or modify: `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_usage.rs`, `src/quota_exec.rs`.
- Tests to add or modify: add one integration-style test (behind `#[cfg(unix)]`) that exercises `save_state` against a tempdir and asserts mode `0o600`.
- Approach: `apply_patch`, call-site by call-site. Preserve content and error handling.
- Test scenarios: state file created at mode `0o600`; rewriting via `save_state` preserves the mode.
- Test expectation: the new test passes; all existing quota tests remain green.

**Unit 3 — Error-message redaction helper.**
- Goal: a single function that decides whether to include an upstream response body in an error message.
- Requirements advanced: R3.
- Dependencies: none.
- Files to create or modify: `src/quota_usage.rs` (or a small helper module).
- Tests to add or modify: add tests covering both redacted and verbose modes.
- Approach: `fn format_upstream_error(status: StatusCode, body: &str) -> String` that consults `std::env::var("AUTO_QUOTA_VERBOSE_ERRORS")`.
- Test scenarios: default mode returns `"Claude token refresh returned 401: <redacted; set AUTO_QUOTA_VERBOSE_ERRORS=1 to include>"`; verbose mode returns `"Claude token refresh returned 401: <body>"`.
- Test expectation: tests pass.

**Unit 4 — Apply redaction at the known leak sites.**
- Goal: every error message currently including `body` or full error-chain token material uses the helper.
- Requirements advanced: R3.
- Dependencies: Unit 3.
- Files to create or modify: `src/quota_usage.rs` (lines ~125-126, ~245), `src/quota_status.rs` (line ~75 if it prints a provider body chain).
- Tests to add or modify: supplement Unit 3's tests with one end-to-end test asserting the bail message uses the helper.
- Approach: `apply_patch`.
- Test scenarios: a synthetic failure produces the redacted message; setting `AUTO_QUOTA_VERBOSE_ERRORS=1` in the test environment produces the verbose message.
- Test expectation: tests pass.

## Concrete Steps

From the repository root:

1. Inventory:
   ```
   rg 'fs::write|File::create' src/quota_*.rs src/util.rs
   rg 'bail!' src/quota_*.rs
   rg 'body|response_body' src/quota_usage.rs src/quota_status.rs
   ```
2. Draft the mode-aware helper in `src/util.rs`. Re-run existing tests:
   ```
   cargo test util
   ```
3. Apply the helper at the quota write sites. Run:
   ```
   cargo build
   cargo test quota
   ```
4. Draft and apply the redaction helper. Run:
   ```
   cargo test quota_usage
   ```
5. Full validation:
   ```
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   ```
6. Spot-check permissions manually (outside test harness) on a scratch directory if convenient.
7. Commit:
   ```
   git add src/util.rs src/quota_*.rs
   git commit -m "quota: enforce 0o600 on credential files and redact error bodies"
   ```

## Validation and Acceptance

- **Observable 1.** `rg 'fs::write' src/quota_*.rs` returns no credential write sites (non-credential writes, if any, may remain but should be reviewed).
- **Observable 2.** A Unix-platform test exercises `save_state` and `stat` confirms mode `0o600`.
- **Observable 3.** A test confirms the redaction helper returns the redacted default and the verbose variant under env-var control.
- **Observable 4.** `cargo test` passes with at least three new tests tied to this plan's requirements.
- **Observable 5.** `cargo clippy --all-targets --all-features -- -D warnings` is clean.

Fail-before-fix: on the pre-change baseline, creating a new quota profile and calling `stat -c '%a'` on the resulting `auth.json` returns the umask-derived mode (often `644`), not `600`.

## Idempotence and Recovery

- Changes are purely in-tree edits and new tests. Rerunning `cargo test` is safe.
- If the mode-helper extension triggers clippy warnings elsewhere, address them module-by-module and re-run.
- If a redaction site is missed, a follow-on small commit can add it without reopening the plan.

## Artifacts and Notes

- Baseline `stat` output for a pre-change credential file: (to be filled).
- Post-change `stat` output: (to be filled).
- Test-name list for new tests: (to be filled).
- Commit hash: (to be filled).

## Interfaces and Dependencies

- **Depends on:** Plan 005 gate passed.
- **Used by:** Plan 009 gate verifies credential mode on the operator's machine.
- **External:** none at runtime. `cargo test` runs only the new tests, which use tempdirs.
- **OS constraints:** R7 gates Unix-specific code under `#[cfg(unix)]`.
