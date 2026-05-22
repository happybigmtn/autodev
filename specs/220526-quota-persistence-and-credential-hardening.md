# Specification: Quota Persistence And Credential Hardening

## Objective

Harden quota config/state persistence and credential sync so unsafe persisted account data, interrupted writes, and symlinked credential paths fail closed.

This is P1 because it protects local credentials and account routing state. It follows the central validation/evidence P0s because quota already has several mitigations, but it outranks cosmetic docs because it guards operator secrets.

## Source Of Truth

- Runtime owner modules/APIs: `src/quota_config.rs::QuotaConfig`, `src/quota_config.rs::validate_account_name`, `src/quota_config.rs::copy_auth_to_profile`, `src/quota_state.rs::QuotaState`, `src/quota_exec.rs::swap_credentials`, `src/quota_exec.rs::sync_newer_claude_credentials`, `src/util.rs::write_0o600_if_unix`, `src/util.rs::atomic_write`.
- UI consumers: `auto quota status`, `auto quota select`, `auto quota open`, `auto quota accounts add/list/remove/capture`, quota-router stderr, backend command logs.
- Generated/runtime artifacts: platform config `quota-router/config.toml`, platform config `quota-router/state.json`, `quota-router/profiles/**`, `quota-router/backup/**`, `.auto/quota-recovery/**`, `.auto/symphony/verification-receipts/*.json`.
- Retired/superseded surfaces: direct truncate writes for quota config/state, raw `fs::copy` for Claude credential refresh, persisted unsafe account names accepted on load, symlinked credential/profile paths accepted by any capture/sync/swap path.

## Evidence Status

Verified facts grounded in code:

- `src/quota_config.rs:85-90` validates account names before building profile dirs and checks profile path containment.
- `src/quota_config.rs:93-100` loads config TOML but does not call `validate_account_names()` after parse.
- `src/quota_config.rs:111-118` saves config with `write_0o600_if_unix`.
- `src/quota_config.rs:191-202` validates account names before saving or mutating config.
- `src/quota_config.rs:205-227` supports isolated Codex profile homes with `codex-home/auth.json`.
- `src/quota_config.rs:229-279` captures provider auth through a staged profile directory before replacing the profile.
- `src/quota_config.rs:299-328` checks logical and canonical profile containment.
- `src/quota_config.rs:375-391` refuses to replace symlinked profile directories.
- `src/quota_config.rs:406-471` rejects symlinked or non-regular credential paths during capture.
- `src/quota_state.rs:37-45` loads state JSON but does not validate persisted account map keys after parse.
- `src/quota_state.rs:47-53` saves state with `write_0o600_if_unix`.
- `src/quota_state.rs:59-96` validates account names on mutation methods.
- `src/quota_state.rs:98-131` refreshes cooldowns and resets account state without validating existing persisted keys.
- `src/quota_exec.rs:118-139` copies individual credential files with symlink refusal and owner-only writes.
- `src/quota_exec.rs:141-186` recursively copies credential dirs while rejecting symlinks and non-regular paths.
- `src/quota_exec.rs:188-215` refreshes Claude profile credentials with raw `fs::copy` from active credentials to profile credentials.
- `src/quota_exec.rs:230-294` swaps profile credentials into active auth, including symlink checks for listed profile entries.
- `src/util.rs:518-534` writes owner-only files by opening the target with truncate semantics.
- `src/util.rs:548-570` provides `atomic_write`, but it does not set owner-only mode and is not currently used by quota config/state saves.
- `docs/decisions/quota-backend-prompt-transport.md:26` records account slug validation as part of the quota safety decision.

Recommendations for the intended system:

- Add a shared atomic owner-only write helper, or extend `atomic_write` with a mode-safe variant, then use it for quota config and state.
- Validate all account names after loading persisted config and state.
- Replace raw Claude credential refresh copy with symlink-refusing, owner-only write behavior.
- Preserve isolated Codex `CODEX_HOME` behavior while hardening only persistence and credential file movement.
- Keep Kimi/PI argv prompt transport as an explicit limitation unless provider documentation or local help proves a safer transport.

Hypotheses / unresolved questions:

- It is unresolved whether `write_0o600_if_unix` should itself become atomic or whether quota should call a new `atomic_write_0o600_if_unix` helper.
- It is unresolved whether invalid persisted quota state should abort all quota commands or quarantine only the invalid account entries.
- It is unresolved whether state cooldown refresh should validate keys before or after applying cooldown mutation.

## Runtime Contract

`QuotaConfig` owns configured account identity and selected account names. `QuotaState` owns exhaustion, lease, and usage metadata. `quota_exec` owns credential swap, restore, and profile refresh.

Runtime must fail closed when persisted config/state contains unsafe account names, when a credential source or destination is symlinked, when a profile path escapes the profiles root, or when an owner-only atomic write cannot be completed.

Quota persistence must not truncate the only state/config file before a complete replacement is ready.

## UI Contract

Quota status and quota-router stderr may summarize account readiness and credential sync results, but they must not duplicate path validation or credential safety rules. They must render sanitized errors from the runtime owners.

No UI output may expose credential contents, raw OAuth bodies, tokens, or profile file contents. Fixture provider output belongs only in tests.

Production UI must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; it must consume runtime helpers/generated contracts or render an explicit unavailable/error state when runtime truth is missing.

## Generated Artifacts

- Platform config `quota-router/config.toml`
- Platform config `quota-router/state.json`
- Platform config `quota-router/profiles/<provider>-<name>/**`
- Platform config `quota-router/backup/**`
- `.auto/quota-recovery/**`
- `.auto/symphony/verification-receipts/<TASK>.json` when task verification is run

Refresh commands:

```bash
auto quota accounts add <name> <codex|claude>
auto quota accounts capture <name>
auto quota select <codex|claude>
auto quota status
```

## Fixture Policy

Quota tests must use temp config homes, temp provider homes, synthetic provider stderr, and synthetic credential files. Production code must not import fixture credentials, sample accounts, copied OAuth JSON, or test provider output.

## Retired / Superseded Surfaces

- Direct truncate writes for quota config/state when an atomic owner-only replacement is available.
- Raw Claude credential refresh through `fs::copy`.
- Persisted unsafe account names accepted on load.
- Symlinked profile or credential paths accepted by capture, swap, restore, or refresh.

## Acceptance Criteria

- Loading config with an unsafe account name fails before any profile or credential mutation.
- Loading state with an unsafe account key fails before selection or cooldown mutation.
- Saving config is atomic and owner-only on Unix.
- Saving state is atomic and owner-only on Unix.
- Save paths refuse symlink destinations or otherwise fail before writing credential/account data through a symlink.
- Claude credential refresh rejects symlinked active or profile credential files.
- Claude credential refresh preserves owner-only mode on the profile credential file.
- Existing isolated Codex home behavior still injects `CODEX_HOME=<profile_dir>/codex-home` instead of swapping global `~/.codex/auth.json`.

## Verification

Run from `/home/r/coding/autodev`:

```bash
cargo test quota_config::tests
cargo test quota_state::tests
cargo test quota_exec::tests
cargo test quota_usage::tests
cargo test quota_status::tests
cargo test util::tests::checkpoint_excludes_generated_and_runtime_paths
cargo clippy --all-targets --all-features -- -D warnings
```

Add or confirm targeted tests:

```bash
cargo test quota_config::tests::load_rejects_unsafe_account_names
cargo test quota_state::tests::load_rejects_unsafe_account_names
cargo test quota_config::tests::save_is_atomic_owner_only_and_rejects_destination_symlink
cargo test quota_state::tests::save_is_atomic_owner_only_and_rejects_destination_symlink
cargo test quota_exec::tests::sync_newer_claude_credentials_rejects_symlinked_profile_credentials
cargo test quota_exec::tests::sync_newer_claude_credentials_preserves_owner_only_mode
```

Grep proof:

```bash
rg -n "validate_account_name|write_0o600_if_unix|atomic_write|fs::copy|sync_newer_claude_credentials|codex-home|symlinked credential" src/quota_config.rs src/quota_state.rs src/quota_exec.rs src/util.rs docs/decisions/quota-backend-prompt-transport.md
```

## Review And Closeout

A reviewer should inspect load paths, save paths, and Claude credential refresh paths, not just account mutation methods. The closeout proof must include a temp config-home test that writes unsafe persisted data directly to disk and proves runtime rejects it on load.

Closeout must also include a grep/assertion proof that raw `fs::copy` is no longer used for Claude credential refresh unless wrapped by a symlink-refusing owner-only helper.

## Open Questions

- Should invalid persisted quota state abort all quota commands or quarantine invalid entries with a visible warning?
- Should `atomic_write` gain a Unix mode argument, or should quota use a separate owner-only atomic helper?
- Should Kimi/PI prompt transport move off argv in this slice if provider support is discovered, or stay deferred to a separate research gate?
