# CI Fidelity And Installed-Binary Proof

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This plan aligns automated validation with the way operators actually use the CLI. Users gain confidence that source changes build, test, lint, and produce an installed `auto` binary whose help/version paths work.

The user can see it working when CI or local release checks include build/check proof, smoke tests, workflow lint where available, and installed-binary verification after source changes.

## Requirements Trace

- R1: CI should reflect local validation expectations from `AGENTS.md` and README.
- R2: Installed binary behavior should be proved after CLI-affecting changes.
- R3: Workflow syntax/security lint should be available locally or in CI when tools exist.
- R4: First-run smoke tests from Plan 010 should run in CI.
- R5: Exact tool versions or action pins must be treated as verified config or recommendations, not guessed.

## Scope Boundaries

This plan does not publish a release. It does not require adding external CI tools if the repo owner does not want them. It does not change source behavior except tests and workflow configuration.

## Progress

- [x] 2026-04-23: Existing CI identified: fmt, clippy, and tests.
- [x] 2026-04-23: Targeted rerun of the previously reported quota usage failure passed; full `cargo test` also passed with 377 tests.
- [ ] 2026-04-23: Add or update CI steps after targeted fixes pass.
- [ ] 2026-04-23: Add installed-binary proof.

## Surprises & Discoveries

The archived corpus's "no CI" claim is stale, but current CI still does not prove installed CLI behavior. Full `cargo test` is currently green, so CI fidelity work should focus on smoke coverage, installed-binary proof, and matching local docs to CI.

## Decision Log

- Mechanical: Do not claim CI fidelity from tests alone; installed-binary proof and smoke coverage are separate operator checks.
- Taste: Add `cargo check` and `cargo build` only if they provide useful separate feedback beyond clippy/tests for this repo.
- Mechanical: Installed-binary proof matters because operators run `auto`, not just unit tests.

## Outcomes & Retrospective

None yet. After implementation, record final CI steps and local command outputs.

## Context and Orientation

Relevant files:

- `.github/workflows/ci.yml`: current GitHub Actions workflow.
- `AGENTS.md`: build/validate expectations.
- `Cargo.toml`: binary name and package version.
- `README.md`: operator install examples.
- `tests/` if Plan 010 adds integration smoke tests.

Verified current CI facts:

- CI runs on push and pull request to `main`.
- It uses checkout with credentials persistence disabled.
- It installs stable Rust with rustfmt and clippy.
- It runs `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.

## Plan of Work

After Plans 003 and 010 have preserved quota coverage and added smoke coverage, update CI to run the smoke tests and installed-binary proof. Decide whether to add `cargo check` and `cargo build` as separate steps. Add local documentation saying when to run `cargo install --path . --root ~/.local` and how to verify `which auto` or `auto --version`.

## Implementation Units

Unit 1 - CI smoke integration:

- Goal: Run hermetic CLI smoke tests in CI.
- Requirements advanced: R1, R4.
- Dependencies: Plan 010.
- Files to create or modify: `.github/workflows/ci.yml`, smoke test files.
- Tests to add or modify: CI should run the smoke test target by default.
- Approach: keep smoke tests provider-free.
- Specific test scenarios: CI executes help/version/dry-run tests without secrets.

Unit 2 - Installed binary proof:

- Goal: Verify the packaged CLI entrypoint.
- Requirements advanced: R2.
- Dependencies: Unit 1.
- Files to create or modify: `.github/workflows/ci.yml`, README or AGENTS validation text.
- Tests to add or modify: CI step or local script for `cargo install --path . --root <temp>` followed by `auto --version`.
- Approach: install into a temporary root in CI to avoid touching user directories.
- Specific test scenarios: installed `auto --version` prints package version and build provenance.

Unit 3 - Workflow lint decision:

- Goal: Decide whether action lint/security lint belongs in CI.
- Requirements advanced: R3, R5.
- Dependencies: none.
- Files to create or modify: `.github/workflows/ci.yml` or docs only.
- Tests to add or modify: Test expectation: none -- this is CI configuration.
- Approach: if adding tools, pin versions or document them as optional local checks; do not guess latest versions.
- Specific test scenarios: workflow lint passes locally or is explicitly documented as optional.

Unit 4 - Validation docs alignment:

- Goal: Make `AGENTS.md` and README match CI.
- Requirements advanced: R1, R2.
- Dependencies: Units 1-3.
- Files to create or modify: `AGENTS.md`, `README.md`.
- Tests to add or modify: Test expectation: none -- docs only.
- Approach: update validation commands without overstating optional external tools.
- Specific test scenarios: `AGENTS.md` includes fmt if CI requires fmt.

## Concrete Steps

From the repository root:

    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test
    cargo install --path . --root target/autodev-install-smoke
    target/autodev-install-smoke/bin/auto --version

If workflow lint tools are installed:

    actionlint .github/workflows/ci.yml
    zizmor .github/workflows/ci.yml

Expected observation: all commands pass, including the currently green full test suite. If optional lint tools are not installed, do not make local validation depend on them without documenting installation.

## Validation and Acceptance

Acceptance requires:

- CI runs existing validation plus the smoke tests from Plan 010;
- installed binary proof works in CI or documented local release checks;
- `AGENTS.md` and README validation instructions match actual CI;
- exact action pins remain verified from workflow config rather than guessed;
- no secrets are required for CI.

## Idempotence and Recovery

CI edits are easy to rerun. If an added step is flaky or too slow, split it into a separate job or make it a documented release check. If installed-binary proof changes build artifacts, keep it under `target/` or a temp install root.

## Artifacts and Notes

Record CI run URL or local command output. Record installed binary path relative to the repository root or temp root; do not use absolute repository-root paths in docs.

## Interfaces and Dependencies

Interfaces touched:

- GitHub Actions workflow;
- Cargo build/test/install commands;
- README and AGENTS validation docs;
- smoke tests from Plan 010.
