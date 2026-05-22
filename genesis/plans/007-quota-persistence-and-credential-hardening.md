# Harden Quota Persistence And Credential Sync

## Priority Decision

P1. This is the sixth implementation slice. Score: medium/high operator value, medium design clarity, medium engineering leverage, direct evidence, and high parallel executability. It follows the central control-loop fixes because quota already has meaningful mitigations, but it outranks cosmetic docs because it protects account state and credentials.

## User / Operator Outcome

An operator can trust quota account state and profile credential sync to fail closed on unsafe persisted data, preserve owner-only permissions, reject symlink surprises, and avoid truncating state on interrupted writes.

## Evidence

- `src/quota_config.rs` validates account names and checks profile directory containment.
- `src/quota_exec.rs` and `src/quota_config.rs` reject symlinked credential capture inputs and use owner-only writes in several paths.
- `QuotaConfig::save` and `QuotaState::save` use direct owner-only writes, not temp+rename atomic writes.
- `QuotaConfig::load` and `QuotaState::load` parse persisted data but do not fully revalidate unsafe account names on load.
- Claude credential sync uses raw copy behavior in one path before equivalent symlink and owner-only checks.
- Kimi/PI prompt argv transport is an accepted limitation, not a proven safe replacement target.

## Scope Boundary

Do not redesign provider backends. Do not claim a safer Kimi/PI prompt transport without primary provider evidence. Do not alter quota selection behavior except where unsafe persisted state must fail closed. Do not touch orchestration files outside quota/backend helpers unless tests require shared utility changes.

## Implementation Slice

Goal: make quota persistence and credential sync atomic, owner-only, and load-validated.

Dependencies: none if the worker owns quota files only; otherwise run after plans 002 and 003 so validation noise is lower.

Files likely to modify:

- `src/quota_config.rs`
- `src/quota_state.rs`
- `src/quota_exec.rs`
- `src/util.rs` if a shared atomic owner-only write helper is needed.
- quota-related tests embedded in those modules.

Tests to add or modify:

- `quota_config::tests::load_rejects_unsafe_account_names`
- `quota_state::tests::load_rejects_unsafe_account_names`
- `quota_config::tests::save_is_atomic_owner_only_and_rejects_destination_symlink`
- `quota_state::tests::save_is_atomic_owner_only_and_rejects_destination_symlink`
- `quota_exec::tests::sync_newer_claude_credentials_rejects_symlinked_profile_credentials`
- `quota_exec::tests::sync_newer_claude_credentials_preserves_owner_only_mode`

Approach:

1. Add or reuse a temp-file-plus-rename helper that preserves owner-only permissions and refuses symlink destinations.
2. Use it for quota config and quota state saves.
3. Revalidate account names and profile references when loading persisted config/state.
4. Replace raw Claude credential copy with symlink-refusing, owner-only credential writes.
5. Keep Kimi/PI prompt argv behavior documented as an accepted limitation unless a separate research gate proves a safer transport.

## Verification

From the repository root:

    cargo test quota_config::tests
    cargo test quota_state::tests
    cargo test quota_exec::tests
    cargo test quota_usage::tests
    cargo test quota_status::tests
    cargo test util::tests::checkpoint_excludes_generated_and_runtime_paths
    cargo test
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: unsafe persisted quota data fails closed, saves are atomic and owner-only, Claude credential sync refuses symlinked profile credentials, and full validation remains green.

## Deferred

- Replacing Kimi/PI argv prompt transport.
- Quota dashboard UX.
- Provider-specific OAuth or account-refresh automation.
- Broad backend invocation policy rewrite.
