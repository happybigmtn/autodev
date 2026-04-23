# First-Run Doctor And Hermetic Smoke Tests

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This plan gives new operators and contributors a no-model success path. A user should be able to build the CLI, run a local preflight, and see honest command-specific requirements before trusting `auto` with live model calls, credentials, commits, or pushes.

The user can see it working when hermetic tests prove `auto --help`, `auto --version`, dry-run corpus behavior, incomplete corpus errors, and command-specific missing dependency messages without calling real provider CLIs.

## Requirements Trace

- R1: Provide a no-model first-run success path.
- R2: Make runtime requirements command-specific rather than globally overstated.
- R3: Add hermetic smoke tests for CLI help/version and selected dry-run/error paths.
- R4: Surface archive/recovery paths before destructive planning-root changes.
- R5: Keep examples honest about live model calls and repo mutation.

## Scope Boundaries

This plan does not add live provider integration tests. It does not require a new public command if extending `auto health` is simpler. It does not change execution behavior except preflight/error clarity and tests.

## Progress

- [x] 2026-04-23: First-run friction and missing smoke tests identified.
- [ ] 2026-04-23: Decide whether to extend `auto health` or add a doctor subcommand.
- [ ] 2026-04-23: Add hermetic CLI smoke tests.
- [ ] 2026-04-23: Update docs with honest first-run path.

## Surprises & Discoveries

`auto corpus` can archive and clear an existing planning root before invoking a model. The archive path is recoverable, but first-run copy should make that behavior explicit before operators run it.

## Decision Log

- User Challenge: Adding a new `auto doctor` command changes public surface; extending `auto health` may be preferable.
- Mechanical: Smoke tests must not call live model CLIs.
- Taste: Use standard library process tests first; add test dependencies only if necessary.

## Outcomes & Retrospective

None yet. After implementation, record the chosen preflight shape and how long the first-run path takes locally.

## Context and Orientation

Relevant files:

- `src/main.rs`: command help and argument parsing.
- `src/health_command.rs`: existing health command.
- `src/generation.rs`: corpus dry-run, verify-only, and planning-root archive behavior.
- `README.md`: onboarding and command docs.
- `AGENTS.md`: build and validation commands.
- `.github/workflows/ci.yml`: future CI target for smoke tests.

Terms:

- Hermetic test: a test that does not require real provider auth, network APIs, or model calls.
- First-run success moment: a command a new operator can run that proves the tool is installed and understands the repo without side effects.

## Plan of Work

Start by choosing the smallest public surface: either add a local doctor mode under `auto health` or add a dedicated `auto doctor` only if the command grouping justifies it. Then add integration smoke tests using the built binary or a test harness. Tests should cover help/version, missing/incomplete corpus behavior, corpus dry-run, and clear errors for missing dependencies without invoking live models.

Update README onboarding to show a no-model path first, then clearly mark commands that can call live models, mutate files, commit, push, or swap credentials.

## Implementation Units

Unit 1 - Preflight surface decision:

- Goal: Choose `auto health` extension or new doctor command.
- Requirements advanced: R1, R2.
- Dependencies: Plan 009.
- Files to create or modify: `src/main.rs`, `src/health_command.rs`, `README.md`, root plan note.
- Tests to add or modify: help text tests for the chosen surface.
- Approach: prefer extending existing health unless the CLI becomes confusing.
- Specific test scenarios: help text names local preflight and does not imply live model calls.

Unit 2 - Hermetic CLI smoke tests:

- Goal: Prove core invocation paths without live providers.
- Requirements advanced: R1, R3.
- Dependencies: Unit 1.
- Files to create or modify: top-level `tests/` or existing inline tests if integration tests are deferred.
- Tests to add or modify: `auto --help`, `auto --version`, selected command help, corpus dry-run, incomplete corpus error.
- Approach: use temp repos and fake environment values; avoid network and provider CLI calls.
- Specific test scenarios: `auto --help` lists all commands; `auto corpus --dry-run` reports planned action; `auto gen --sync-only` with missing corpus gives a clear error.

Unit 3 - Archive/recovery preflight polish:

- Goal: Make destructive generated-directory behavior visible before model invocation.
- Requirements advanced: R4.
- Dependencies: Unit 2.
- Files to create or modify: `src/generation.rs`, README.
- Tests to add or modify: dry-run or failure-path test for archive path messaging.
- Approach: print archive/recovery path before clearing a corpus root when possible.
- Specific test scenarios: simulated model launch failure still leaves or reports archived corpus recovery location.

Unit 4 - Honest onboarding docs:

- Goal: Make copy-paste examples accurate.
- Requirements advanced: R2, R5.
- Dependencies: Units 1-3.
- Files to create or modify: `README.md`, possibly `AGENTS.md`.
- Tests to add or modify: Test expectation: none -- docs only.
- Approach: list command-specific requirements and mark mutating commands.
- Specific test scenarios: `rg -n "valid origin is required"` should not present origin as universal if only some workflows need it.

## Concrete Steps

From the repository root:

    cargo run -- --help
    cargo run -- --version
    cargo run -- corpus --dry-run --planning-root genesis
    cargo run -- gen --sync-only --planning-root genesis

After tests are added:

    cargo test cli_smoke
    cargo test health_command::tests::
    cargo test generation::tests::corpus

Expected observation: smoke tests pass without provider auth or network access.

## Validation and Acceptance

Acceptance requires:

- a no-model first-run path exists and is documented;
- command-specific requirements are clearer in README/help;
- hermetic smoke tests cover help/version and at least two planning error/dry-run paths;
- corpus archive/recovery behavior is visible before or during failure;
- tests do not require live model CLIs.

## Idempotence and Recovery

Smoke tests should use temp directories and should not depend on the user's repo state. If an integration-test dependency is added and proves heavy, revert to standard library `Command` tests. If a new command is proposed and rejected, keep the underlying preflight logic under `auto health`.

## Artifacts and Notes

Record final first-run command sequence and expected output snippets. Keep snippets short and avoid absolute paths.

## Interfaces and Dependencies

Interfaces touched:

- CLI help/version;
- health or doctor preflight;
- generation dry-run/error paths;
- README onboarding;
- CI test command list after Plan 011.
