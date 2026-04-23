# Security Checkpoint Gate

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This checkpoint decides whether the repo is safe enough to proceed from Phase 1 into execution-contract changes. Users gain a clear go/no-go moment after planning truth, quota credentials, and Symphony workflow rendering are addressed.

The gate is visible when targeted tests pass, root docs reflect the current state, and any remaining security risks are recorded as explicit follow-ups rather than hidden assumptions.

## Requirements Trace

- R1: Root planning truth has been reconciled.
- R2: Quota credential restore and profile capture are tested and safe.
- R3: Symphony workflow rendering validates and quotes hostile values.
- R4: Checkpoint staging risk around `genesis/` and sensitive files has a documented decision.
- R5: The quota usage human-refresh test passes or any recurrence has a documented blocker.

## Scope Boundaries

This checkpoint does not implement new behavior. It verifies Plans 002 through 004 and records whether later plans may proceed. It does not require broad refactors or release tagging.

## Progress

- [x] 2026-04-23: Gate created after the first three implementation slices.
- [ ] 2026-04-23: Run after Plans 002, 003, and 004 are implemented.
- [ ] 2026-04-23: Record go/no-go decision in root planning docs.

## Surprises & Discoveries

None yet. This section should capture any new security risk discovered while implementing the first phase, especially around checkpoint staging and quota profile directories.

## Decision Log

- Mechanical: Do not proceed to verification/parser refactors while credential restore remains broken.
- Taste: Include checkpoint-staging policy here because `genesis/` tracking status affects both security and planning truth.
- User Challenge: If fixing dangerous-mode defaults is proposed here, pause for operator decision rather than silently changing defaults.

## Outcomes & Retrospective

None yet. At gate time, record whether the phase passed, failed, or passed with documented exceptions.

## Context and Orientation

This gate depends on:

- Plan 002: root planning truth.
- Plan 003: quota credential restore/profile hardening.
- Plan 004: Symphony workflow rendering hardening.

Relevant files:

- `IMPLEMENTATION_PLAN.md`, `ARCHIVED.md`, `WORKLIST.md`, `specs/`.
- `src/quota_exec.rs`, `src/quota_config.rs`, `src/quota_usage.rs`.
- `src/symphony_command.rs`, `src/main.rs`.
- `src/util.rs` for checkpoint exclusions.

## Plan of Work

Confirm each Phase 1 plan produced targeted tests and docs. Run targeted tests first, then the full local validation commands if targeted tests pass. Inspect checkpoint exclusions and decide whether `genesis/` should remain stageable, be ignored, or require explicit opt-in for auto-checkpoints.

## Implementation Units

Unit 1 - Verify planning reconciliation:

- Goal: Confirm root docs no longer contain stale active claims.
- Requirements advanced: R1.
- Dependencies: Plan 002.
- Files to create or modify: root planning docs only if evidence is missing.
- Tests to add or modify: Test expectation: none -- documentation gate only.
- Approach: run stale-claim greps and inspect active task statuses.
- Specific test scenarios: stale strings such as old test counts and old command counts do not appear as current claims.

Unit 2 - Verify quota security:

- Goal: Confirm credential restore/copy tests pass.
- Requirements advanced: R2, R5.
- Dependencies: Plan 003.
- Files to create or modify: none unless test evidence is missing.
- Tests to add or modify: none at gate time.
- Approach: run the targeted quota tests added by Plan 003.
- Specific test scenarios: Claude restore success/failure, symlink rejection, stale pruning, owner-only file mode, and quota usage human error test pass.

Unit 3 - Verify Symphony rendering:

- Goal: Confirm hostile render inputs are safe.
- Requirements advanced: R3.
- Dependencies: Plan 004.
- Files to create or modify: none unless test evidence is missing.
- Tests to add or modify: none at gate time.
- Approach: run targeted Symphony render tests.
- Specific test scenarios: semicolon, quote, `$()`, whitespace, and newline payloads are rejected or inert.

Unit 4 - Decide checkpoint staging policy:

- Goal: Prevent auto-checkpoints from silently committing surprising generated or secret-looking paths.
- Requirements advanced: R4.
- Dependencies: Plans 002 and 003.
- Files to create or modify: `IMPLEMENTATION_PLAN.md` for decision record; `src/util.rs` only if this gate is expanded into implementation.
- Tests to add or modify: Test expectation: none -- this gate records the decision; implementation belongs in a follow-up if needed.
- Approach: inspect `CHECKPOINT_EXCLUDE_RULES`, current dirty state, and generated path policy.
- Specific test scenarios: decision states whether `genesis/` remains stageable or needs an explicit safeguard.

## Concrete Steps

From the repository root:

    rg -n "333 tests|no \\.github|thirteen commands|0\\.1\\.0" IMPLEMENTATION_PLAN.md specs README.md
    cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error
    cargo test quota_exec::tests::
    cargo test quota_config::tests::
    cargo test symphony_command::tests::
    rg -n "CHECKPOINT_EXCLUDE_RULES|genesis" src/util.rs IMPLEMENTATION_PLAN.md

If targeted tests pass:

    cargo test
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: targeted tests pass before full validation. If full validation fails elsewhere, record the exact failing test or lint.

## Validation and Acceptance

Gate passes only if:

- root planning docs are reconciled;
- quota restore/profile tests pass;
- quota usage human-refresh coverage passes or any recurrence is documented as an external blocker;
- Symphony hostile scalar tests pass;
- checkpoint staging policy is documented;
- any remaining security risks are listed with owner plans before Phase 2 begins.

## Idempotence and Recovery

This gate can be rerun after any Phase 1 fix. If a test fails, do not proceed to Plan 006; return to the owning plan and update this gate's Progress. If root docs drift again, repeat Plan 002 before re-gating.

## Artifacts and Notes

Paste concise command results into root planning or the implementation commit message. Avoid storing secrets or full provider auth paths in notes.

## Interfaces and Dependencies

This gate depends on local Cargo validation, root docs, quota modules, Symphony rendering, and checkpoint policy in `src/util.rs`.
