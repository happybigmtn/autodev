# Plan 004 — `auto audit` verdict-application test harness

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

## Purpose / Big Picture

`auto audit` (module `src/audit_command.rs`, 1154 LOC) is one day old as of this corpus. It iterates repository files, runs a Kimi-backed auditor on each, writes per-file verdict JSON, and conditionally applies patches, worklist entries, or retire entries. The module has three tests (`glob_match` and `sha256_hex`); the verdict-application branch, manifest reconciliation, escalation handling, and resume-from-partial-run logic are untested.

This plan adds a bounded test harness that locks the current behavior of verdict application before any further feature work on the module. The harness does not exercise real `kimi-cli`; it substitutes pre-written `audit/files/<hash>/verdict.json` fixtures and asserts that the downstream handling (patch apply, worklist append, manifest update, retire-batch append) behaves correctly.

The operator gains confidence that `auto audit` will not silently change behavior when the module grows. The observable effect: `cargo test audit_command` runs at least ten new tests and all pass.

## Requirements Trace

- **R1.** At least one test exercises each of the six verdict categories handled by `audit_command.rs`: `CLEAN`, `DRIFT-SMALL`, `DRIFT-LARGE`, `SLOP`, `RETIRE`, `REFACTOR`.
- **R2.** At least one test asserts that a patch-applicable verdict (`DRIFT-SMALL`, `SLOP` when paired with a well-formed `patch.diff`) leads to a successful patch apply and manifest status transition to `Audited`.
- **R3.** At least one test asserts that a patch-applicable verdict with a malformed patch escalates to a worklist entry instead of corrupting the tree.
- **R4.** At least one test asserts that `RETIRE` verdicts append to the retire-batch artifact.
- **R5.** At least one test asserts that `DRIFT-LARGE` and `REFACTOR` write worklist entries and do not attempt patch application.
- **R6.** At least one test asserts that manifest reconciliation correctly identifies "unchanged since last audit" via matching `content_hash`, `doctrine_hash`, and `rubric_hash`, and skips re-auditing those files.
- **R7.** Tests run without a real `kimi-cli` invocation. All agent output is supplied via fixture files.
- **R8.** `FileVerdict::touched_paths` and `FileVerdict::escalate` fields (currently parsed but unused) are covered by at least one test each, documenting whether they are consumed or intentionally ignored. If ignored, the test pins the current behavior; a follow-on plan will decide whether to wire them up.

## Scope Boundaries

- **Changing:** `src/audit_command.rs` — only to (a) extract helper functions into testable shapes where necessary, without changing behavior, and (b) add `#[cfg(test)]` fixtures and tests.
- **Not changing:** `auto audit` command-line behavior, the verdict JSON schema, the manifest schema, the Kimi-CLI integration.
- **Not fixing:** the `FileVerdict::touched_paths` / `escalate` half-wired fields. Tests lock current behavior; wiring them up is out of scope.
- **Not adding:** new CLI flags, new verdict categories, new manifest fields.
- **Not introducing:** `mockall` or any external mocking library. Fixtures are plain JSON and plain test files.

## Progress

- [ ] Read `audit_command.rs` end-to-end and list every function involved in verdict application.
- [ ] Identify functions that can be tested directly, vs those that need a small refactor-for-testability.
- [ ] If refactor is needed, make it behavior-preserving and run all existing tests first.
- [ ] Write fixture JSON files under `src/audit_command_fixtures/` (or use `include_str!`-style inline fixtures).
- [ ] Add new `#[cfg(test)]` tests at the bottom of `audit_command.rs`.
- [ ] Run `cargo test audit_command` and assert at least 10 new tests pass.
- [ ] Commit.

## Surprises & Discoveries

None yet. Potential surprise worth logging: some verdict-application code paths turn out to be unreachable from any real agent output (dead-code-via-policy), which would change the test shape.

## Decision Log

- **2026-04-21 — Test via fixtures, not mocks.** Taste. The `#[cfg(test)]` blocks elsewhere in this repo (util.rs, generation.rs, review_command.rs) are all fixture-driven. Matching that idiom keeps the test shape consistent.
- **2026-04-21 — Refactor for testability only when unavoidable.** Mechanical. The extracted functions should be private with `pub(crate)` only if needed for a test outside the file.
- **2026-04-21 — Do not cover patch semantics beyond "does it apply cleanly."** Taste. Whether a patch improves the file is the auditor's judgment, not the test's. Tests assert the mechanical flow (apply succeeds, manifest transitions, worklist grows as expected).

## Outcomes & Retrospective

None yet.

## Context and Orientation

- `src/audit_command.rs` — main implementation.
  - Lines 129-138: `FileVerdict` struct with `touched_paths` and `escalate` fields parsed but unused downstream.
  - Lines 140-478: `run_audit` orchestration.
  - Lines 307, 331, 372, 440, 452: manifest-write points.
  - Lines 341-342: file-artifact dir wiped on restart (`fs::remove_dir_all(&file_dir).ok()`).
  - Lines 769-861: verdict application logic. Branches on verdict variant.
  - Lines 800: patch application site.
  - Lines 945-977: retire-batch append.
  - Lines 1020-1109: Kimi/Pi backend routing.
  - Lines 1023: bails if `--use-kimi-cli` is not set.
  - Lines 1123-1154: existing tests (`glob_match` and `sha256_hex`).
- `docs/audit-doctrine-template.md` — the doctrine-file shape. Tests can supply a fake doctrine as fixture.
- `src/util.rs::atomic_write` — used for manifest writes; behavior is already tested in `util.rs`.

Commands for orientation:
```
grep -nE '^(pub |fn |async fn |struct |enum )' src/audit_command.rs
rg 'touched_paths|escalate' src/audit_command.rs
rg 'verdict.verdict|verdict.patch|verdict.worklist_entry|verdict.retire_reason' src/audit_command.rs
```

## Plan of Work

1. **Catalogue verdict handling.** Identify the single function (or contiguous code region) that takes a parsed `FileVerdict` plus the file path and decides what to do.
2. **Isolate into a pure function if possible.** The ideal shape: `fn apply_verdict(verdict: &FileVerdict, repo_ctx: &RepoCtx) -> VerdictOutcome` where `RepoCtx` is a small struct containing the manifest, the worklist path, and the retire-batch path. If the current implementation interleaves filesystem operations with decision logic, do a minimal behavior-preserving refactor.
3. **Write fixtures.** Six verdict JSON blobs: CLEAN, DRIFT-SMALL-with-good-patch, DRIFT-SMALL-with-bad-patch, DRIFT-LARGE, SLOP-with-patch, RETIRE, REFACTOR. Plus a manifest fixture.
4. **Write tests.** Each test sets up a temp repo (`tempfile::TempDir` is not in Cargo.toml — use `std::env::temp_dir()` with a unique subdir and explicit cleanup), writes the fixture, calls the target function, asserts on the resulting filesystem and return value.
5. **Run `cargo test audit_command`.** Confirm at least 10 new tests pass; assert the pre-existing 3 tests still pass.
6. **Commit.**

Expected fixture layout (inside the temp repo created by each test):
```
<temp-root>/
  audit/
    DOCTRINE.md            (fixture doctrine)
    MANIFEST.json          (fixture manifest)
    files/
      <hash-prefix>/
        verdict.json       (the fixture verdict under test)
        patch.diff         (when applicable)
  src/
    example.rs             (the file being audited, matching the fixture's content_hash)
  WORKLIST.md              (written by the test as needed)
```

## Implementation Units

**Unit 1 — Behavior-preserving refactor (only if needed) to isolate `apply_verdict`.**
- Goal: verdict handling is callable from tests without invoking `kimi-cli`.
- Requirements advanced: R2, R3, R4, R5.
- Dependencies: none.
- Files to create or modify: `src/audit_command.rs`.
- Tests to add or modify: existing tests must still pass.
- Approach: extract the pure decision/effect logic into a function that accepts a parsed verdict and a small context struct. Do not rename the existing entry points.
- Test scenarios: `cargo test audit_command` still shows 3 passes (existing tests).
- Test expectation: existing tests unchanged.

**Unit 2 — Fixture authorship.**
- Goal: six verdict fixtures and one manifest fixture exist and parse.
- Requirements advanced: R1, R7.
- Dependencies: none.
- Files to create or modify: add new `#[cfg(test)]` constants inside `src/audit_command.rs`, or add fixture files under `src/audit_command_fixtures/` imported via `include_str!`.
- Tests to add or modify: none yet.
- Approach: hand-write JSON mirroring `FileVerdict` and `ManifestEntry` schemas verified against the struct definitions at lines 129-138 and the manifest-entry definition.
- Test scenarios: a new test `verdict_fixtures_all_parse` deserializes each fixture and asserts no errors.
- Test expectation: `cargo test verdict_fixtures_all_parse` passes.

**Unit 3 — Verdict-branch coverage.**
- Goal: tests exist for each of CLEAN, DRIFT-SMALL (good patch), DRIFT-SMALL (malformed patch), DRIFT-LARGE, SLOP, RETIRE, REFACTOR.
- Requirements advanced: R1, R2, R3, R4, R5.
- Dependencies: Units 1, 2.
- Files to create or modify: `src/audit_command.rs`.
- Tests to add or modify: add seven tests named after their verdict, e.g., `apply_verdict_clean_updates_manifest_only`, `apply_verdict_drift_small_applies_patch_and_marks_audited`, `apply_verdict_drift_small_malformed_patch_escalates_to_worklist`, etc.
- Approach: each test constructs a tempdir, seeds the fixtures, calls the extracted function, asserts on the result and the filesystem.
- Test scenarios:
  - CLEAN: manifest entry `status = Audited`, no worklist or retire writes, repo tree unchanged.
  - DRIFT-SMALL + good patch: file mutated, manifest `Audited`, no worklist write.
  - DRIFT-SMALL + malformed patch: file unchanged, manifest `ApplyFailed`, worklist appended.
  - DRIFT-LARGE: file unchanged, manifest `Escalated`, worklist appended.
  - SLOP + good patch: file mutated, manifest `Audited`.
  - RETIRE: retire-batch appended, manifest `Audited`.
  - REFACTOR: file unchanged, manifest `Escalated`, worklist appended.
- Test expectation: all seven pass.

**Unit 4 — Manifest reconciliation test.**
- Goal: a test asserts that an unchanged file (matching hashes) is skipped in the next run.
- Requirements advanced: R6.
- Dependencies: Units 1, 2.
- Files to create or modify: `src/audit_command.rs`.
- Tests to add or modify: `manifest_skips_unchanged_file`.
- Approach: write a manifest entry for a fixture file; call the selection/reconciliation routine; assert it is excluded from the pending list.
- Test scenarios: content_hash + doctrine_hash + rubric_hash all match → skip. Any one differs → include.
- Test expectation: the test passes.

**Unit 5 — `touched_paths` and `escalate` pinning tests.**
- Goal: current behavior of these parsed-but-unused fields is pinned.
- Requirements advanced: R8.
- Dependencies: Units 1, 2.
- Files to create or modify: `src/audit_command.rs`.
- Tests to add or modify: `touched_paths_parsed_but_not_consumed`, `escalate_flag_parsed_but_not_consumed`.
- Approach: each test supplies a verdict with the field populated, invokes the application pathway, and asserts the field's current effect. If the field genuinely has no effect, the test asserts that no side effect tied to the field occurs.
- Test scenarios: `touched_paths = ["src/other.rs"]` does not cause `src/other.rs` to be touched; `escalate = true` does not cause automatic worklist escalation (pins today's behavior).
- Test expectation: the tests pass and document the current behavior for a follow-on plan.

## Concrete Steps

From the repository root:

1. Read the full module once:
   ```
   wc -l src/audit_command.rs
   grep -nE '^(pub |fn |async fn |struct |enum )' src/audit_command.rs
   rg 'FileVerdict' src/audit_command.rs
   ```
2. Run the existing tests as a baseline:
   ```
   cargo test audit_command
   ```
3. If refactoring, make a minimal behavior-preserving extraction. Commit this refactor on its own if it is large enough to deserve a review pass.
4. Author fixtures and tests at the bottom of `src/audit_command.rs` inside the `#[cfg(test)] mod tests` block.
5. Run the tests and iterate:
   ```
   cargo test audit_command -- --nocapture
   ```
6. When all tests pass, run the full suite and clippy:
   ```
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   ```
7. Commit:
   ```
   git add src/audit_command.rs
   git commit -m "test(audit_command): cover verdict application and manifest reconcile"
   ```

## Validation and Acceptance

- **Observable 1.** `cargo test audit_command` reports at least 13 passing tests (3 existing + 10 new minimum).
- **Observable 2.** Each verdict category has at least one named test:
  ```
  cargo test audit_command -- --list | grep -E 'clean|drift_small|drift_large|slop|retire|refactor'
  ```
- **Observable 3.** `cargo clippy --all-targets --all-features -- -D warnings` remains clean after the change.
- **Observable 4.** No `kimi-cli` invocation appears in any test (`rg 'kimi-cli' src/audit_command.rs` returns only production-code lines, not test lines).

Fail-before-fix: on the baseline commit, none of the new test names exist and verdict-application behavior is effectively untested.

## Idempotence and Recovery

- Tests live inside the module; rerunning `cargo test` simply reruns them.
- Fixture data is self-contained per test via tempdirs.
- If a test flakes due to filesystem races (unlikely, since tempdirs are per-test), rerun. No cross-test state.
- If the refactor of Unit 1 breaks existing tests, `git checkout -- src/audit_command.rs` reverts and the plan can restart from Unit 1 with a smaller scope.

## Artifacts and Notes

- Pre-change test count (Unit 0 baseline): 3.
- Post-change test count: (to be filled).
- Commit hash for refactor (if separated): (to be filled).
- Commit hash for tests: (to be filled).

## Interfaces and Dependencies

- **Depends on:** `src/audit_command.rs` at its current behavior. `util.rs::atomic_write` for manifest writes (no changes required).
- **Used by:** Plan 005 gate, which confirms the audit module has durable tests before further feature work.
- **External:** none. No `kimi-cli`, no network.
