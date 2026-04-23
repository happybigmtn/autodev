# Quota Credential Restore And Profile Hardening

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This plan makes `auto quota` safe enough to trust with provider credentials. An operator gains confidence that running through a selected Codex or Claude account does not leave the wrong auth file active, leak stale profile files, preserve unsafe symlinks, or write sensitive files with loose permissions.

The user can see it working through targeted quota tests that fail before the fix and pass after, plus preserved coverage for quota usage error-surfacing and a later full local test run at the release gate.

## Requirements Trace

- R1: Every backed-up credential path is restored or removed on success and failure.
- R2: Profile capture writes owner-only files on Unix.
- R3: Profile capture rejects or safely handles symlinks.
- R4: Profile capture prunes stale files so removed credentials do not persist.
- R5: Quota state/config load-modify-save paths use consistent locking where mutation can race.
- R6: The quota usage human-refresh error test remains covered and passing, or any recurrence is investigated with evidence.

## Scope Boundaries

This plan does not add encryption-at-rest. It does not change provider APIs, account selection policy, quota floor thresholds, or model defaults. It does not alter non-quota execution commands except where shared credential-copy helpers require tests.

## Progress

- [x] 2026-04-23: Code review identified incomplete Claude `.claude.json` restore.
- [x] 2026-04-23: Targeted rerun of `quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error` passed in Codex review.
- [ ] 2026-04-23: Add failing restore/profile tests.
- [ ] 2026-04-23: Implement transactional restore and safe profile copy.
- [ ] 2026-04-23: Re-run targeted and full validation.

## Surprises & Discoveries

`swap_credentials` remembers the `.claude.json` backup pair, but normal restore ignores that pair and restores by provider type. That makes the guard's exact backup list less authoritative than it looks.

## Decision Log

- Mechanical: Restore should consume the exact paths backed up by `AuthRestoreGuard`.
- Mechanical: Symlinks in credential profiles are a security boundary, not a convenience feature.
- User Challenge: Encryption-at-rest is out of scope because it changes recovery and key-management policy.
- Taste: Keep the quota usage error-surfacing test in this plan because it is in the same operator-trust surface, even though the targeted rerun passed during review.

## Outcomes & Retrospective

None yet. After implementation, record which restore paths were covered, whether stale backup files are removed, and whether the quota usage error-surfacing test stayed green or exposed a recurrence.

## Context and Orientation

Key files:

- `src/quota_exec.rs`: credential swap, backup, restore, quota open/select.
- `src/quota_config.rs`: account config and profile capture.
- `src/quota_state.rs`: account state and exhaustion/cooldown persistence.
- `src/quota_usage.rs`: usage refresh and the human-refresh error-surfacing test.
- `src/util.rs`: `write_0o600_if_unix`.

Definitions:

- Provider auth source: the active auth file or directory used by Codex or Claude.
- Profile: a saved account credential copy under quota router config.
- Restore guard: in-memory object that should put active credentials back even if the run fails.

## Plan of Work

Start with tests that model Claude active auth containing both `.claude/` and `.claude.json`. Prove the current code leaves `.claude.json` wrong or leaves backup files behind. Then change restore to use the guard's backup pairs directly. Add a safe copy helper that creates profile dirs with owner-only permissions where supported, copies regular files through bytes plus restricted write, rejects symlinks, and prunes stale profile contents before capture.

Keep the `quota_usage` human-refresh test in the targeted validation set. If it fails again after restore/profile edits, inspect the actual error text and preserve human context without leaking token bodies.

## Implementation Units

Unit 1 - Claude restore regression:

- Goal: Prove `.claude.json` is restored or removed exactly.
- Requirements advanced: R1.
- Dependencies: none.
- Files to create or modify: `src/quota_exec.rs`.
- Tests to add or modify: add quota exec unit tests for Claude success and error restore paths.
- Approach: create temp active auth, profile auth, and backup state; run restore path through helper-level tests.
- Specific test scenarios: selected profile `.claude.json` active during run; original `.claude.json` restored after success; original absence removes active file after restore; backups are removed.

Unit 2 - Safe profile capture:

- Goal: Make saved profiles owner-only, stale-free, and symlink-safe.
- Requirements advanced: R2, R3, R4.
- Dependencies: Unit 1 helper shape.
- Files to create or modify: `src/quota_config.rs`, maybe shared helper in `src/util.rs`.
- Tests to add or modify: profile capture tests for Codex file, Claude directory, stale destination pruning, and symlink rejection.
- Approach: remove raw `fs::copy` for credential files; use restricted writes; clear destination before copy.
- Specific test scenarios: stale `auth.json` removed when capture fails; symlink source returns a clear error; copied file mode is owner-only on Unix.

Unit 3 - State mutation lock audit:

- Goal: Ensure reset/select/status mutations cannot race with open runs.
- Requirements advanced: R5.
- Dependencies: Units 1 and 2.
- Files to create or modify: `src/quota_exec.rs`, `src/quota_status.rs`, `src/quota_state.rs` as needed.
- Tests to add or modify: focused state lock tests if a helper is introduced.
- Approach: use the existing provider lock or add a state lock around load-modify-save paths.
- Specific test scenarios: concurrent select/reset/open does not corrupt JSON and leases are released.

Unit 4 - Quota usage human refresh regression:

- Goal: Preserve a green quota usage error-surfacing baseline.
- Requirements advanced: R6.
- Dependencies: none.
- Files to create or modify: `src/quota_usage.rs`.
- Tests to add or modify: update or add tests around human refresh errors and secret scrubbing only if the existing targeted test regresses.
- Approach: rerun the targeted test after quota edits; if it fails, inspect the actual error text and preserve human context without leaking token bodies.
- Specific test scenarios: error text includes the expected human refresh reason and does not include JSON token fields.

## Concrete Steps

From the repository root:

    cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error -- --nocapture
    cargo test quota_exec::tests:: -- --list
    cargo test quota_config::tests:: -- --list

After adding tests:

    cargo test quota_exec::tests::claude_restore
    cargo test quota_config::tests::profile_capture
    cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error
    cargo test quota_

Expected observation: the newly added restore/profile tests fail before implementation and pass after. The quota usage human-refresh test remains passing; any recurrence is investigated with an updated assertion and evidence.

## Validation and Acceptance

Acceptance requires:

- Claude `.claude.json` is restored or removed correctly on success and failure.
- Backup files created by quota swaps are consumed or cleaned.
- Profile capture rejects symlinks or handles them through an explicit safe policy.
- Profile files are owner-only on Unix.
- Stale profile files do not survive a fresh capture.
- The quota usage human-refresh test passes.
- Targeted `cargo test quota_` passes.

## Idempotence and Recovery

All tests should use temp directories and never touch real provider credentials. If a manual test is needed, create throwaway provider homes and set environment variables or helper paths rather than using the operator's live auth. If implementation partially lands, revert only the quota files touched by this plan and keep root planning docs intact.

## Artifacts and Notes

Capture the failing and passing output for:

- `cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error`;
- new Claude restore tests;
- new safe profile capture tests.

## Interfaces and Dependencies

Interfaces changed or used:

- `AuthRestoreGuard` and restore helpers in `src/quota_exec.rs`;
- `copy_auth_to_profile` and recursive copy helpers in `src/quota_config.rs`;
- `write_0o600_if_unix` in `src/util.rs`;
- quota state/config files under the quota router config directory;
- provider auth sources for Codex and Claude.
