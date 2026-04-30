# Quota Profile Path and Credential Lease Hardening

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

Parallel model execution is only production-ready if quota account profiles cannot escape their storage directory and one lane cannot swap credentials out from under another. Operators gain safe quota-backed execution for `auto corpus`, `auto gen`, `auto parallel`, and related model phases. They can see it working through rejected unsafe account names and tests that prove credential leases span the child process lifetime.

## Requirements Trace

- R1: Account names must be validated before they become filesystem paths.
- R2: Credential profile operations must stay within the provider profile directory.
- R3: A quota lease must protect the active global credential swap for the full model child process.
- R4: Claude activation must not leave mixed credentials from a previous account.
- R5: Tests must fail before the fix for path traversal and lease lifetime gaps.

## Scope Boundaries

This plan does not change model selection defaults, quota pricing, or account exhaustion heuristics except where needed to preserve credential safety. It does not replace quota with per-process credential homes unless the lease design proves insufficient.

## Progress

- [x] 2026-04-30: Identified path traversal and process-lifetime credential risks in quota code.
- [ ] 2026-04-30: Add path-safe account name validation and tests.
- [ ] 2026-04-30: Hold provider lock or isolated credential context through child execution.
- [ ] 2026-04-30: Add mixed Claude credential cleanup coverage.

## Surprises & Discoveries

- Existing symlink rejection and owner-only writes are real progress, but they do not bound raw account names.
- The lock currently protects reservation and swap, not the full model execution.

## Decision Log

- Mechanical: This is the first implementation slice because unsafe credentials can corrupt unrelated lanes.
- Taste: Prefer a small validation and locking change before larger credential-home redesign.
- User Challenge: Unlimited parallelism remains a goal, but global credential swaps require either serialization or true isolation.

## Outcomes & Retrospective

None yet. Record whether the final design serializes per provider, isolates homes per process, or uses a different safe mechanism.

## Context and Orientation

Relevant files:

- `src/quota_config.rs`: profile path construction and credential capture/copy helpers.
- `src/quota_accounts.rs`: account add/remove/list behavior.
- `src/quota_exec.rs`: account selection, credential activation, restore guard, and command execution.
- `src/main.rs`: quota command CLI arguments.
- `src/codex_exec.rs` and `src/claude_exec.rs`: shared execution paths that call quota wrappers.

Non-obvious terms:

- Provider: a model family such as Codex or Claude with separate credential files.
- Profile: the stored credential copy for one quota account.
- Lease: the temporary reservation of an account while a command runs.

## Plan of Work

Add a single account-name validator that permits stable slug characters and rejects separators, parent-directory components, empty names, and platform-dangerous path characters. Use it before any profile path is constructed or any add/remove/capture/select command proceeds. Then change quota execution so the provider lock or an equivalent lease guard remains alive until after the child process exits and credentials are restored. Finally, ensure Claude activation clears active credential files that are absent from the selected profile before persisting selection.

## Implementation Units

- Unit 1: Account-name validation. Goal: prevent profile path escape. Requirements advanced: R1, R2. Dependencies: none. Files: `src/quota_config.rs`, `src/quota_accounts.rs`, `src/main.rs`. Tests: reject `../x`, `a/b`, empty, and separator-containing names; accept existing simple names. Approach: central helper returning sanitized error. Scenarios: add/remove/capture/select fail before touching disk.
- Unit 2: Process-lifetime lease. Goal: prevent concurrent credential swaps. Requirements advanced: R3. Dependencies: Unit 1. Files: `src/quota_exec.rs`. Tests: simulate two provider runs and assert the second cannot swap until first child completes, or assert isolated env paths differ. Approach: keep guard ownership around spawn/wait and restore. Scenarios: long child holds active credentials.
- Unit 3: Claude active cleanup. Goal: prevent mixed credentials. Requirements advanced: R4. Dependencies: Unit 2. Files: `src/quota_exec.rs`. Tests: active directory has extra file, selected profile lacks it, select removes or quarantines extra file. Approach: replace active provider credential set atomically. Scenarios: no stale `.claude` file remains.

## Concrete Steps

From the repository root:

    rg -n "profile_dir|run_with_quota|reserve_account_and_swap|copy_profile_to_active_auth" src/quota_*.rs src/main.rs
    cargo test quota_config::tests
    cargo test quota_exec::tests
    cargo test quota_accounts::tests
    cargo test quota

Expected observations after implementation: unsafe account names return an error before filesystem mutation, and concurrent quota-backed commands cannot observe mixed credentials.

## Validation and Acceptance

Acceptance:

- `auto quota add codex ../bad ...` and equivalent unsafe names are rejected.
- Tests prove profile paths remain under the provider profile directory.
- A child process keeps the selected credential context until it exits.
- Claude selection cannot leave stale active credential files from another account.
- Existing symlink rejection and owner-only write tests continue to pass.

## Idempotence and Recovery

Validation is idempotent because rejected names should not create files. If a partial run leaves temporary credential backups, use the existing restore guard behavior and inspect only provider-specific test directories, never real user credentials.

## Artifacts and Notes

Record test names and any intentional serialization tradeoff in `REVIEW.md` or the active task handoff when promoted.

## Interfaces and Dependencies

Interfaces: quota CLI subcommands, quota config helpers, shared Codex/Claude execution wrappers, filesystem profile layout, provider lock files. External dependencies: local model CLIs and credential files, but tests should use temp directories.
