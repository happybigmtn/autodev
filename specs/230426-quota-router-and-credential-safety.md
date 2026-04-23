# Specification: Quota Router And Credential Safety

## Objective
Make quota-account routing safe enough for unattended agent execution by preserving exact active credentials, preventing stale or symlinked profile data from leaking across accounts, and keeping quota usage errors actionable without exposing secrets.

## Evidence Status

### Verified Facts

- `auto quota` is a top-level command in `src/main.rs:95`, with `status`, `select`, `accounts`, `reset`, and `open` subcommands defined in `src/main.rs:292-363` and dispatched in `src/main.rs:1270`.
- Quota config uses the OS config directory plus `quota-router/config.toml`, and profile directories are under `quota-router/profiles/<provider>-<name>` in `src/quota_config.rs:9-11` and `src/quota_config.rs:70-86`.
- Provider auth sources are `~/.claude` for Claude and `~/.codex/auth.json` for Codex in `src/quota_config.rs:28-33`.
- Config saves call `write_0o600_if_unix` in `src/quota_config.rs:106-112`, quota state saves call the same helper in `src/quota_state.rs:47`, and the Unix helper tightens file mode to `0o600` in `src/util.rs:491-510`.
- Profile capture currently uses raw `fs::copy` for Codex `auth.json`, Claude credential files, and `.claude.json` in `src/quota_config.rs:196`, `src/quota_config.rs:218`, and `src/quota_config.rs:227`.
- Recursive profile capture preserves symlinks instead of rejecting them in `src/quota_config.rs:243-250`, and recursive execution-time copy also preserves symlinks in `src/quota_exec.rs:77-84`.
- `swap_credentials` backs up Claude `~/.claude` and `~/.claude.json` in `src/quota_exec.rs:152-190`.
- `restore_credentials` restores Codex backup auth and Claude backup directory, but the normal provider restore path shown in `src/quota_exec.rs:365-385` does not restore `backup/claude.json`.
- Quota selection uses `WEEKLY_FLOOR_PCT = 10` and `SESSION_FLOOR_PCT = 25` in `src/quota_selector.rs:7-10`.
- Exhaustion cooldown is one hour in `src/quota_state.rs:11`.
- Codex usage refresh sends the prompt `Reply with OK only.` through the Codex CLI in `src/quota_usage.rs:20` and `src/quota_usage.rs:428`.
- The existing test `codex_cli_refresh_surfaces_human_refresh_error` lives in `src/quota_usage.rs:631`.
- The planning corpus identifies credential restore, profile capture, symlink handling, and consistent locking as the intended hardening surface in `genesis/plans/003-quota-credential-restore-and-profile-hardening.md:17` and `genesis/ASSESSMENT.md:86-87`.

### Recommendations

- Replace raw credential `fs::copy` calls with a credential-copy helper that rejects symlinks, writes regular files with owner-only mode where possible, and produces path-specific errors.
- Clear or rebuild profile destination directories during account capture so removed source files cannot remain as stale credentials.
- Restore `.claude.json` through the same success, failure, and drop paths as `~/.claude`.
- Add lock coverage for all load-modify-save paths that mutate quota config, state, selected account, active auth, or profile contents.
- Keep selection floor values configurable only after current restore and profile safety are tested; they are current constants, not product policy proof.

### Hypotheses / Unresolved Questions

- It is unresolved whether symlinked files inside `~/.claude` are legitimate for any supported operator setup.
- It is unresolved whether stale profile pruning should delete the entire profile directory first or reconcile file by file for better diagnostics.
- It is unresolved whether failed receipt writes from quota-routed subprocesses should abort the run or only mark the run unverifiable.

## Acceptance Criteria

- Capturing a Codex profile writes exactly one regular `auth.json` file and rejects symlinked or non-regular auth paths with a human-readable error.
- Capturing a Claude profile writes only supported regular credential files and directories and rejects symlinks instead of recreating them.
- Re-capturing a profile after a source credential file is removed also removes the stale destination file.
- Running a quota-routed Claude command restores both `~/.claude` and `~/.claude.json` after success, quota exhaustion, command failure, and panic/drop cleanup.
- Running a quota-routed Codex command restores the original `~/.codex/auth.json` after success, quota exhaustion, command failure, and panic/drop cleanup.
- Credential backup files, restored files, config files, and state files are owner-only on Unix when the filesystem supports permissions.
- Quota selection still skips accounts below the current weekly floor and prefers accounts at or above the current session floor.
- Human refresh failures for Codex usage surface a user-actionable message and do not include raw access tokens, refresh tokens, or full auth JSON.

## Verification

- `cargo test quota_exec::tests::swap_credentials_enforces_0o600`
- `cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error -- --nocapture`
- `cargo test quota_config::tests:: -- --list`
- `cargo test quota_selector::tests:: -- --list`
- Add and run regressions for `.claude.json` restore on success and failure.
- Add and run regressions for symlink rejection and stale profile pruning in account capture.

## Open Questions

- Should quota account capture support a documented allowlist of Claude subdirectories beyond current credential and `statsig` paths?
- Should quota state and config share one lock file or keep provider-scoped locks plus a separate state lock?
- Should quota restore errors leave backups in place for manual recovery or abort after restoring the highest-priority original files?
