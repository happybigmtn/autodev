# Quota Backend And Credential Safety

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This slice makes model account routing safe enough for production execution. The operator gains confidence that quota account names cannot escape the profile directory, credential state is not corrupted by partial writes, prompts are not leaked through avoidable argv paths, and failover will not duplicate side effects after a worker has already made progress.

## Requirements Trace

- R1: Quota account names must be validated as safe account slugs.
- R2: Profile directories must remain contained under the quota profile root.
- R3: Account config and state writes must be atomic and owner-only.
- R4: Quota failover must stop or require manual recovery after detected worker progress.
- R5: Provider error text must pass through a shared sanitizer before display.
- R6: Kimi/PI prompt delivery must avoid argv where feasible or document a hard production limitation.

## Scope Boundaries

This plan does not change task scheduling, corpus generation, or release gates. It does not change which providers are supported. It does not remove Kimi or PI support unless research proves no safe production path exists.

## Progress

- [x] 2026-04-30: Verified `QuotaConfig::profile_dir` interpolates raw account names into paths.
- [x] 2026-04-30: Verified `run_with_quota` continues after quota exhaustion even when progress is detected.
- [x] 2026-04-30: Verified credential source copying rejects symlinks and writes owner-only files.
- [ ] Add validation and containment helpers.
- [ ] Add failover-after-progress behavior and tests.
- [ ] Add atomic owner-only config/state writes.
- [ ] Research and harden Kimi/PI prompt delivery.

## Surprises & Discoveries

- Existing credential copying is stronger than account/profile naming.
- The failover code detects progress but only changes the log message before retrying.
- Generation and backend execution still have multiple invocation paths with different logging and quota behavior.

## Decision Log

- Mechanical: Account names must be treated as untrusted input because they become filesystem paths.
- Mechanical: Retry-after-progress is a production blocker because workers can edit files before quota output appears.
- Taste: Use a strict ASCII slug instead of trying to preserve arbitrary display names; display labels can be added later if needed.
- User Challenge: If Kimi/PI cannot accept prompts over stdin, production use should require an explicit unsafe-prompt-argv acknowledgement.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Relevant files:

- `src/quota_config.rs`: provider enum, account config, profile path construction, credential copy helpers.
- `src/quota_accounts.rs`: `auto quota accounts add/list/remove/capture`.
- `src/quota_exec.rs`: quota-aware provider selection, credential swap, failover, progress detection.
- `src/quota_state.rs`: quota state persistence.
- `src/quota_usage.rs`, `src/quota_status.rs`: provider refresh and displayed error text.
- `src/kimi_backend.rs`, `src/pi_backend.rs`, `src/codex_exec.rs`: backend command construction.
- `src/main.rs`: quota command arguments and backend defaults.

Non-obvious terms:

- Profile root: the directory returned by `QuotaConfig::profiles_dir()`.
- Account slug: a filesystem-safe account identifier such as `work-codex-1`.
- Retry-after-progress: running the same model prompt again after the previous attempt may already have changed files.

## Plan of Work

Add a shared account-name validation helper in `quota_config.rs`, then route all account entry points through it. Replace direct profile path construction with a helper that canonicalizes or syntactically validates containment under `profiles_dir()`. Add config-load validation so an existing unsafe config fails closed with a remediation message.

Update quota config/state persistence to use an atomic owner-only helper that writes a temporary file with restrictive permissions and renames it into place. Reject destination symlinks where supported.

Change `run_with_quota` so quota exhaustion or availability failure after detected progress returns a manual-recovery error instead of retrying. Preserve retry behavior when no progress is detected.

Unify error sanitization through one helper for provider refresh, status, and quota execution. Research whether Kimi/PI can receive prompts on stdin; if yes, move prompt delivery off argv. If no, add explicit production warning and timeout/preflight parity.

## Implementation Units

- Unit 1: Account slug validation.
  - Goal: Reject unsafe account names before config or profile mutation.
  - Requirements advanced: R1, R2.
  - Dependencies: none.
  - Files to create or modify: `src/quota_config.rs`, `src/quota_accounts.rs`, `src/main.rs` tests if needed.
  - Tests to add or modify: reject empty, whitespace, `/`, `..`, absolute-like, control characters, shell-looking names, and overly long names; accept conservative slugs.
  - Approach: Add `validate_account_name` and call it from add, remove, capture, select, status, config load, and state load paths.
  - Test scenarios: `auto quota accounts add ../bad codex` fails before touching profile files.

- Unit 2: Profile containment.
  - Goal: Make profile path construction impossible to escape.
  - Requirements advanced: R2.
  - Dependencies: Unit 1.
  - Files to create or modify: `src/quota_config.rs`.
  - Tests to add or modify: profile paths remain under `profiles_dir()` for all valid providers and names.
  - Approach: Replace `profile_dir(provider, name)` internals with `safe_profile_dir` semantics and use syntactic component checks before filesystem existence.
  - Test scenarios: unsafe existing config is rejected with a clear message.

- Unit 3: Atomic account state writes.
  - Goal: Avoid truncated config/state files after interruption.
  - Requirements advanced: R3.
  - Dependencies: none.
  - Files to create or modify: `src/util.rs`, `src/quota_config.rs`, `src/quota_state.rs`.
  - Tests to add or modify: symlink destination rejection where possible, temp-file permission check, rename behavior.
  - Approach: Add or reuse an owner-only atomic write helper and make quota config/state use it.
  - Test scenarios: simulated write failure leaves previous TOML/JSON intact.

- Unit 4: Failover after progress.
  - Goal: Prevent duplicate side effects.
  - Requirements advanced: R4.
  - Dependencies: none.
  - Files to create or modify: `src/quota_exec.rs`, quota tests.
  - Tests to add or modify: exhausted account with progress returns manual recovery and does not call next account; exhaustion without progress still retries.
  - Approach: Convert progress detection from log suffix into control-flow branch.
  - Test scenarios: fake backend stderr containing progress markers and quota exhaustion produces a non-retry result.

- Unit 5: Backend prompt and error hygiene.
  - Goal: Reduce secret leakage and inconsistent backend failures.
  - Requirements advanced: R5, R6.
  - Dependencies: backend CLI capability research.
  - Files to create or modify: `src/kimi_backend.rs`, `src/pi_backend.rs`, `src/codex_exec.rs`, `src/quota_usage.rs`, `src/quota_status.rs`, docs if needed.
  - Tests to add or modify: argv construction tests, sanitizer tests, timeout/preflight tests.
  - Approach: Prefer stdin prompt delivery; otherwise print explicit warning and require production opt-in for unsafe argv delivery.
  - Test scenarios: fake backend receives prompt through stdin or command fails with a clear unsafe-mode message.

## Concrete Steps

From the repository root:

    rg -n "profile_dir|run_with_quota|quota_output_has_agent_progress|write_0o600|refresh_codex_with_cli|kimi|pi" src

Expected observation: all callsites that must be covered.

    cargo test quota -- --nocapture

Expected observation before work: missing validation coverage or failing new tests.

After implementation:

    cargo test quota
    cargo test backend_policy
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: quota/backend tests pass and clippy is clean.

## Validation and Acceptance

Acceptance requires observable failures for unsafe account names, path containment tests, no retry after detected progress, atomic write tests, and sanitized provider error output. A manual check should confirm `auto quota accounts add safe-name codex` still works when credentials exist, while unsafe names fail before filesystem mutation.

## Idempotence and Recovery

Validation helpers are safe to rerun. Existing unsafe configs should fail closed with instructions to rename or remove unsafe entries manually. If atomic write migration partially fails, previous config/state should remain intact. If Kimi/PI prompt hardening cannot be completed, leave a documented production warning and a failing research gate rather than silently accepting argv leakage.

## Artifacts and Notes

- Evidence to fill in: failing unsafe-name test before implementation.
- Evidence to fill in: passing quota tests after implementation.
- Evidence to fill in: backend prompt delivery decision for Kimi/PI.

## Interfaces and Dependencies

- CLI: `auto quota accounts add`, `list`, `remove`, `capture`, provider-backed model execution.
- Filesystem: quota config directory, quota profiles directory, provider auth sources.
- Modules: `quota_config`, `quota_accounts`, `quota_exec`, `quota_state`, `quota_usage`, `quota_status`, `kimi_backend`, `pi_backend`, `codex_exec`.
