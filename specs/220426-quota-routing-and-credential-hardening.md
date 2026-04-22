# Specification: `auto quota` — account routing and credential hardening

## Objective

Keep `auto quota` a reliable multi-account multiplexer for Claude and Codex sessions: it captures the active provider credential files into named profiles, tracks per-account usage, respects weekly and session floors, rotates to the next viable account on exhaustion, and never lets concurrent rotations corrupt credentials. At the same time, close the current security gap by writing every credential and state file with Unix mode `0o600` and scrubbing token-refresh response bodies out of error logs.

## Evidence Status

### Verified facts (code)

- `src/main.rs:93` declares `Quota`; the `QuotaSubcommand` tree is at `src/main.rs:290-301` (`Status`, `Select`, `Accounts`, `Reset`, `Open`).
- `AccountsCommand` subtree at `src/main.rs:331-340`: `Add`, `List`, `Remove`, `Capture`.
- `src/quota_selector.rs:8-10`: `WEEKLY_FLOOR_PCT: u32 = 10`, `SESSION_FLOOR_PCT: u32 = 25`.
- `src/quota_state.rs:11`: `EXHAUSTION_COOLDOWN_HOURS: i64 = 1`.
- Rotation-on-exhaustion is driven by pattern matching in `src/quota_patterns.rs` against rate-limit / quota-exceeded / 429 / overloaded messages (`corpus/SPEC.md` item 8).
- `fd_lock::RwLock` serializes credential swaps (`src/quota_exec.rs`; `Cargo.toml` lists `fd-lock = "4"`).
- Credential layout: per-profile dirs under `~/.config/quota-router/profiles/<provider>-<name>/` containing raw `auth.json` (Codex) or `.claude.json` / `.claude` (Claude). Managed by `src/quota_config.rs:29-30,109` and `src/quota_usage.rs:150`.
- **Plaintext credentials at rest (security gap).** Production `fs::write` callsites with default umask and no `chmod 0o600`:
  - `src/quota_config.rs:109`
  - `src/quota_state.rs:51`
  - `src/quota_usage.rs:150`
  Test-only helpers also write credential-shaped fixtures at `src/quota_usage.rs:506,514`; they are not production hardening targets.
  (`corpus/ASSESSMENT.md` §"Security risks" item 1).
- **Error-message credential leakage (security gap).** Token-refresh bodies and full error chains are printed on failure paths at `src/quota_usage.rs:125-126,245` and `src/quota_status.rs:75` (`corpus/ASSESSMENT.md` §"Security risks" item 2).
- Claude invocation is hardcoded with `--dangerously-skip-permissions` at `src/claude_exec.rs:161`.
- Codex invocation is hardcoded with `--dangerously-bypass-approvals-and-sandbox` at `src/codex_exec.rs:217, 221, 430`.
- Test count per corpus ASSESSMENT: `quota_config` ~5, `quota_exec` ~3, `quota_patterns` ~8 tests.

### Verified facts (docs)

- `README.md:442-470` documents quota routing with `WEEKLY_FLOOR_PCT = 10` and `SESSION_FLOOR_PCT = 25` — matches code.
- `README.md` is silent on credential-file permissions — operator has no warning that secrets sit at default umask.

### Recommendations (corpus)

- `corpus/plans/006-quota-credential-hardening.md` specifies `chmod 0o600` on every `fs::write` that produces a credential file, gated behind `#[cfg(unix)]` to stay compatible with Windows stdlib-only callers.
- Scrub token-refresh body content from error paths at `quota_usage.rs:125-126,245` and `quota_status.rs:75`; preserve enough structured data (error code, provider, account name) to diagnose without leaking secrets.
- Encryption at rest for credential files is explicitly out of scope for this pass (`corpus/GENESIS-REPORT.md` §"Not Doing"); remains a backlog item.

### Hypotheses / unresolved questions

- Non-atomic state updates (`src/quota_state.rs:36-53` load-modify-save without a held lock on the state file itself across concurrent `auto` invocations) are flagged as a risk but not triggered in practice; scope of fix is uncertain.
- Whether `auto quota capture` reads the running-browser session directly or via OS-keychain APIs is not fully verified in this pass.

## Acceptance Criteria

- `auto quota status` prints per-account weekly and session usage percent, next reset time, and which account is currently selected.
- `auto quota select claude` and `auto quota select codex` choose the primary account for the provider: if only one account exists, it is selected automatically; if multiple exist, `run_quota_select` prompts for a numbered choice (`src/quota_exec.rs:413-447`). The command updates `~/.config/quota-router/config.toml`, swaps active credentials, and surfaces a confirmation line.
- `auto quota reset [account-name]` clears the exhausted flag and active lease count for the named account, or all accounts when the name is omitted (`src/quota_state.rs:103-119`). There is no `--weekly` flag today.
- `auto quota open claude|codex -- <args...>` selects the best available account, launches the provider CLI with that account's active credentials, waits for exit, and restores prior credentials (`src/quota_exec.rs:388-410`).
- `auto quota accounts list` prints configured accounts with name, provider, and profile presence (`ok` / `MISSING`) (`src/quota_accounts.rs:33-68`).
- `auto quota accounts capture <account-name>` copies the current active provider credentials into the named profile directory (`src/quota_accounts.rs:95-108`).
- `auto quota accounts remove <account-name>` removes the config entry, clears the selected account if it matched, and deletes the profile directory (`src/quota_accounts.rs:71-92`, `src/quota_config.rs:170-178`). It does not currently prune historical `quota_state` entries.
- Credential writes use Unix mode `0o600`: for every `fs::write` call that produces a file under `~/.config/quota-router/profiles/` (and for the top-level config + state files), the written file's mode is `0o600` after write on Unix.
- On non-Unix platforms the permission tightening is a no-op without breaking compilation (`#[cfg(unix)]` gate).
- Quota-router error paths do not include the raw body of any token-refresh HTTP response; errors may name the provider, account, HTTP status code, and a short machine-readable reason but must not include refresh tokens, access tokens, or cookie payloads.
- Rotation on exhaustion triggers only when the exhaustion pattern matches (rate-limit / quota-exceeded / 429 / overloaded); unrelated failures do not rotate.
- Weekly floor of `10%` and session floor of `25%` are applied: an account below either floor is not selected for a new run.
- `EXHAUSTION_COOLDOWN_HOURS = 1` is honored: an account flagged exhausted returns to candidacy after one hour via `refresh_cooldowns()`.
- Credential swaps hold the `fd_lock::RwLock` for the entire swap; two concurrent `auto` runs cannot interleave writes on the same account's credential file.
- `auto quota` does not print raw `auth.json` or `.claude.json` contents to stdout or stderr on any code path.

## Verification

- Unit tests in `quota_config.rs`, `quota_state.rs`, `quota_patterns.rs` remain green after hardening changes.
- Add a test that writes a credential via the real production path in a tmpdir and asserts `metadata.permissions().mode() & 0o777 == 0o600` on Unix.
- Add a negative test for error-message scrubbing: a mocked HTTP 401 whose body contains a fake refresh token; the surfaced error message must not contain the fake token.
- Smoke test `auto quota status` / `auto quota select` / `auto quota reset` on a hermetic fixture profile directory.
- Concurrent-write test: launch two processes that both call the rotation path and assert neither produces a partially-written credential file.

## Open Questions

- Should `auto quota` gain a `--dry-run` mode that shows what rotation *would* do without mutating state?
- Encryption at rest — operator-facing implications (key management, passphrase prompts) block a default choice; leave as backlog or prototype this pass?
- Should the non-atomic load-modify-save in `quota_state.rs:36-53` be promoted to an `fd_lock`-held transaction across the state file, or is the concurrency window too small to matter in practice?
- Should `--dangerously-skip-permissions` (Claude) and `--dangerously-bypass-approvals-and-sandbox` (Codex) get an operator-facing opt-out flag, or stay always-on for tool-chain consistency?
