# Plan 011 — End-to-end smoke tests for `qa`, `health`, `ship`

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

## Purpose / Big Picture

Every test in the repo today lives inline in a `#[cfg(test)]` module inside the file under test. There are no integration tests; there is no `tests/` directory at the crate root. Twenty-five source files have inline tests. Nothing currently exercises a command end-to-end against a real fixture repository.

The risk this creates is not theoretical. Three of the most operator-facing commands -- `auto qa`, `auto health`, `auto ship` -- each write multi-file artifacts (`QA.md`, `HEALTH.md`, `SHIP.md`), consult Git state, consult other artifact files written by earlier commands, and bail on specific preconditions. A regression that breaks the artifact format, skips the precondition check, or misreads a Git state is invisible to unit tests because the unit tests don't execute the command.

This plan adds a small, fixture-based integration test suite at `tests/`. Each test sets up a temporary Git repo, writes a minimal set of input artifacts, runs the binary as a subprocess with a stubbed-out LLM backend (or a no-op backend mode), and asserts on the files produced and the exit code. The test count is modest -- one golden-path test per command, plus one precondition-failure test per command -- and the fixture helpers are shared.

The operator sees three new test files and a shared helper module. CI (Plan 010) runs them.

## Requirements Trace

- **R1.** A new `tests/` directory at the crate root contains at minimum: `tests/smoke_qa.rs`, `tests/smoke_health.rs`, `tests/smoke_ship.rs`, and a shared support module under `tests/support/mod.rs`.
- **R2.** Each smoke test file contains at minimum two tests: a golden-path test that sets up a valid state and asserts the command exits zero with the expected artifact(s) written, and a precondition-failure test that sets up an invalid state and asserts the command exits non-zero with a specific error message fragment.
- **R3.** Tests do not require a network connection, do not invoke any real LLM CLI, and complete in under 30 seconds total on a developer machine. If a command cannot be exercised without an LLM call, it is stubbed via an environment flag that short-circuits the backend invocation (add this flag if not already present, scoped to test-only; the production code path must remain the default).
- **R4.** Tests use `tempfile::TempDir` (already in the dep tree via `anyhow`'s tree, or add it explicitly if not) for sandbox isolation. No test touches the real working directory.
- **R5.** `cargo test` on CI (Plan 010) runs the new integration tests alongside unit tests without additional configuration.
- **R6.** A tests-for-tests check: the precondition-failure tests each start from a state where the golden-path test would pass, mutate one specific precondition, and verify the failure mode. This prevents the classic "both tests pass because neither precondition is actually checked" failure.
- **R7.** Test helpers in `tests/support/mod.rs` include: `make_fixture_repo()`, `write_artifact(path, contents)`, `run_auto(args, env)`, and `read_artifact(path)`. Each helper is documented with a one-line doc-comment describing what it does.

## Scope Boundaries

- **Creating:** `tests/smoke_qa.rs`, `tests/smoke_health.rs`, `tests/smoke_ship.rs`, `tests/support/mod.rs`.
- **Modifying (minimally):** possibly one or two source files if a test-only short-circuit env flag needs to be added to bypass LLM calls. The flag must be opt-in (environment variable, not a compile-time feature) and not advertised in user-facing docs.
- **Not covering in this plan:** `corpus`, `gen`, `loop`, `review`, `parallel`, `bug`, `nemesis`, `audit`, `steward`, `symphony`, `quota`. Smoke coverage of those commands belongs to follow-up plans.
- **Not covering:** property-based tests, mutation tests, coverage reports.
- **Not adding:** a Makefile or a `cargo xtask` runner. `cargo test` is sufficient.

## Progress

- [ ] Shared support module authored.
- [ ] QA smoke test authored (golden + precondition).
- [ ] Health smoke test authored (golden + precondition).
- [ ] Ship smoke test authored (golden + precondition).
- [ ] Test-only LLM short-circuit flag introduced, if required.
- [ ] `cargo test` passes locally including integration tests.
- [ ] CI run green (Plan 010 infrastructure must be in place).

## Surprises & Discoveries

None yet. Anticipated:
- `auto qa` may refuse to run inside a non-Git directory or a directory without an `IMPLEMENTATION_PLAN.md`; the fixture helper will need to seed both.
- `auto ship` reads `QA.md` and `HEALTH.md` as preconditions; the fixture must produce these. This is what makes the three tests a natural group: the ship fixture reuses the qa and health fixture.
- A command that has no test-only short-circuit will require one. Add it scoped under a specific env flag (e.g., `AUTO_SMOKE_STUB_BACKEND=1`) that causes the command to substitute a canned backend response and log that fact on stderr.

## Decision Log

- **2026-04-21 — Integration tests live in `tests/`, not in an inline module.** Taste. Integration tests that spin up the binary belong at the crate root per Rust convention. Keeping them alongside unit tests inside `src/` would confuse the two categories.
- **2026-04-21 — Stub via environment flag, not via a trait injection.** Mechanical. The codebase does not yet have a backend trait (Plan 008 is research-only). Retrofitting a trait for the purpose of testing is the wrong direction; the env-flag stub is a smaller, reversible intervention that can be replaced by a trait later if Plan 008 recommends one.
- **2026-04-21 — Only three commands in scope this pass.** Taste. Covering all thirteen-plus commands in a single plan is too large. Start with the post-QA lifecycle stretch (`qa` → `health` → `ship`) because that's where the multi-artifact interaction is richest and the most likely place for a silent regression.
- **2026-04-21 — No `tempfile` workspace-level pin if already transitively available.** Mechanical. If `cargo add --dev tempfile` is needed, do it; if it's already in the tree, prefer reuse.

## Outcomes & Retrospective

None yet.

## Context and Orientation

- `src/qa_command.rs` and/or `src/qa_only_command.rs` — commands under test; inspect their preconditions.
- `src/health_command.rs` — second command under test.
- `src/ship_command.rs` — third command under test; reads QA and HEALTH outputs.
- `src/main.rs` — for the `Qa`, `Health`, `Ship` variants and their argument shapes.
- `AGENTS.md` — documents the artifact filenames the commands read and write.
- `README.md` (after Plan 002) — confirms the validated behavior each command delivers.
- `genesis/SPEC.md` — artifact-shape table is the authoritative cross-reference for what a "valid" fixture looks like.

## Plan of Work

1. Read the current shape of `QaCommand`, `HealthCommand`, `ShipCommand` in `src/main.rs` and the corresponding command modules. Identify preconditions and produced artifacts.
2. Decide whether any of the three commands requires a test-only backend stub. Add the env-flag short-circuit if so, scoped narrowly.
3. Author `tests/support/mod.rs` with the four helpers listed under R7.
4. Author `tests/smoke_qa.rs` (golden + precondition-failure).
5. Author `tests/smoke_health.rs` (golden + precondition-failure).
6. Author `tests/smoke_ship.rs` (golden + precondition-failure; fixture is the post-qa-post-health state).
7. Run `cargo test`; iterate until green.
8. Commit.

## Implementation Units

**Unit 1 — Survey preconditions and artifacts.**
- Goal: a written list of what each of the three commands needs in the working dir and what it writes back.
- Requirements advanced: none directly; input to every other unit.
- Dependencies: none.
- Files to create or modify: none (scratch notes).
- Tests to add or modify: none.
- Approach: read the three command modules and note every `bail!`, every `fs::read_to_string`, every `fs::write`. Capture the precondition check and the output path.
- Test expectation: none.

**Unit 2 — Backend stub flag (if needed).**
- Goal: a narrowly-scoped env flag like `AUTO_SMOKE_STUB_BACKEND=1` that causes the command's backend dispatch to return a canned response without spawning a real CLI.
- Requirements advanced: R3.
- Dependencies: Unit 1.
- Files to create or modify: whichever backend entry point is shared across `qa`, `health`, `ship`; likely `src/codex_exec.rs` or a small helper that the commands call.
- Tests to add or modify: the new integration tests consume the flag.
- Approach: early return in the backend function when the env var is set; return a stub response that passes downstream parsers. Log `stub backend response` to stderr.
- Test expectation: unit-test footprint is zero; the flag is exercised only by the integration tests.

**Unit 3 — Support module.**
- Goal: `tests/support/mod.rs` exists and provides `make_fixture_repo()`, `write_artifact()`, `run_auto()`, `read_artifact()`.
- Requirements advanced: R1, R4, R7.
- Dependencies: Unit 1.
- Files to create or modify: `tests/support/mod.rs`.
- Tests to add or modify: none (support is consumed, not tested directly).
- Approach: use `tempfile::TempDir`; initialize a repo via `git init` (shelled out), write seed files, run the binary via `std::process::Command` pointed at `env!("CARGO_BIN_EXE_auto")`, capture stdout/stderr and exit status.
- Test expectation: helpers compile and are used by the three smoke files.

**Unit 4 — QA smoke test.**
- Goal: `tests/smoke_qa.rs` with two tests (`qa_golden_path_writes_qa_md`, `qa_bails_when_implementation_plan_missing` or the analogous precondition).
- Requirements advanced: R1, R2, R6.
- Dependencies: Units 2, 3.
- Files to create or modify: `tests/smoke_qa.rs`.
- Tests to add or modify: two new tests.
- Approach: golden sets up fixture, runs `auto qa --non-interactive` (or whichever argument set produces a deterministic run) with the stub flag; asserts `QA.md` exists and has expected headers. Precondition removes one seed file; asserts non-zero exit and specific error fragment.
- Test expectation: both tests pass.

**Unit 5 — Health smoke test.**
- Goal: `tests/smoke_health.rs` with two tests.
- Requirements advanced: R1, R2, R6.
- Dependencies: Unit 3, 4 (for shared fixture shape).
- Files to create or modify: `tests/smoke_health.rs`.
- Tests to add or modify: two new tests.
- Approach: same pattern; golden sets up the QA state and runs `auto health`; precondition removes a required input or dirties the Git state in a way the command rejects.
- Test expectation: both tests pass.

**Unit 6 — Ship smoke test.**
- Goal: `tests/smoke_ship.rs` with two tests.
- Requirements advanced: R1, R2, R6.
- Dependencies: Unit 3, 4, 5.
- Files to create or modify: `tests/smoke_ship.rs`.
- Tests to add or modify: two new tests.
- Approach: the golden fixture is the post-qa-post-health state. Assert `SHIP.md` is written. Precondition removes `QA.md` or `HEALTH.md` and asserts the command refuses.
- Test expectation: both tests pass.

**Unit 7 — CI pipeline verification.**
- Goal: Plan 010's CI workflow runs the new integration tests without additional config.
- Requirements advanced: R5.
- Dependencies: Units 3-6.
- Files to create or modify: none (if Plan 010's workflow already uses `cargo test`, it picks up integration tests automatically).
- Tests to add or modify: none.
- Approach: push the branch; observe CI run.
- Test expectation: CI exits green on the integration tests.

## Concrete Steps

From the repository root:

1. Survey preconditions/artifacts:
   ```
   rg -n 'bail!|fs::read_to_string|fs::write' src/qa_command.rs src/qa_only_command.rs src/health_command.rs src/ship_command.rs
   ```
   Note every path read and every path written. This is the fixture contract.
2. Determine if a stub is needed:
   ```
   rg -n 'run_codex_exec_with_env|spawn_claude|kimi_exec_args' src/qa_command.rs src/health_command.rs src/ship_command.rs
   ```
   If any of the three is reached, add the env flag short-circuit in Unit 2.
3. If adding the env flag: insert it at the nearest shared dispatch point. Prefer one change over three.
4. Create `tests/support/mod.rs`. Example skeleton (adapt to repo conventions):
   ```rust
   // Integration-test helpers shared across smoke tests.
   use std::path::Path;
   use std::process::{Command, Output};
   use tempfile::TempDir;

   pub(crate) fn make_fixture_repo() -> TempDir { /* init, git config, seed */ }
   pub(crate) fn write_artifact(dir: &Path, rel: &str, body: &str) { /* fs::write */ }
   pub(crate) fn run_auto(dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output { /* CARGO_BIN_EXE_auto */ }
   pub(crate) fn read_artifact(dir: &Path, rel: &str) -> String { /* fs::read_to_string */ }
   ```
5. Author the three smoke files. Each test should be around 30-60 lines -- enough to set up, run, and assert, not enough to obscure intent.
6. Run:
   ```
   cargo test --test smoke_qa
   cargo test --test smoke_health
   cargo test --test smoke_ship
   cargo test
   ```
7. Add `tempfile` as a dev-dependency if not present:
   ```
   cargo add --dev tempfile
   ```
8. Commit:
   ```
   git add tests/ src/ Cargo.toml Cargo.lock
   git commit -m "test: add end-to-end smoke tests for qa, health, ship"
   ```

## Validation and Acceptance

- **Observable 1.** `ls tests/` shows `smoke_qa.rs`, `smoke_health.rs`, `smoke_ship.rs`, and a `support/` directory containing `mod.rs`.
- **Observable 2.** `cargo test --test smoke_qa` reports at least 2 tests; likewise `smoke_health` and `smoke_ship`. Total integration tests >= 6.
- **Observable 3.** `cargo test` on a developer machine completes in under 30 seconds total (integration + unit).
- **Observable 4.** Removing the stub env flag while keeping the test as-is causes the golden-path tests to fail with a "LLM CLI not found" or equivalent error, proving the stub is the only reason they pass without a network.
- **Observable 5.** Reverting each of the three commands' precondition check one at a time causes the corresponding precondition-failure test to fail (proving the tests actually test the precondition).
- **Observable 6.** CI run (Plan 010) goes green on the PR that adds this plan's work.

## Idempotence and Recovery

- `cargo test` is idempotent. Integration tests use `tempfile::TempDir` and clean up their own sandboxes.
- If a test leaks a `TempDir` because of a panic, the next `cargo test` still runs to completion; the OS reaps temp dirs eventually.
- If the stub env flag was added and is later superseded by a backend trait (from a future Plan 008 follow-up), retire the flag as part of that follow-up -- do not leave both in place.
- If a test turns out to be flaky (e.g., because of Git environment variations), mark it `#[ignore]` with an inline comment pointing at a tracking task, rather than relaxing the assertion.

## Artifacts and Notes

- Precondition/artifact survey (Unit 1): to be filled at execution.
- Whether a stub flag was added (yes/no, and where): to be filled.
- Test counts per file: to be filled.
- Wall-clock for full `cargo test`: to be filled.
- Commit hash: to be filled.

## Interfaces and Dependencies

- **Depends on:** Plan 010 (CI) -- authored first so the new tests run automatically. Technically the tests can be written before CI, but they then sit unrun until CI lands; the practical ordering is 010 then 011.
- **Used by:** future smoke-coverage plans that extend to `corpus`, `gen`, `loop`, `review`, `audit`, `steward`. Those plans inherit the helpers in `tests/support/mod.rs`.
- **External:** `tempfile` (add if not present). No network. No real LLM CLI.
