# Plan 003 — Retire dead tmux scaffolding in codex_exec.rs

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

## Purpose / Big Picture

`src/codex_exec.rs` is fronted by `#![allow(dead_code)]` at line 1 with a comment describing the module as "staged for CLI integration but not yet wired." Roughly 400 of its 685 lines implement a tmux-based lane invocation that nothing calls. The real tmux orchestration that operators use lives in `src/parallel_command.rs`. Leaving dead code in place makes `auto parallel` harder to reason about, misleads future planners into thinking a tmux-per-Codex-call feature is in progress, and bloats a security-sensitive module.

After this plan, `codex_exec.rs` contains only the entry points that are actually called. The file becomes roughly 300 lines instead of 685, `#![allow(dead_code)]` disappears, and `cargo build` / `cargo test` / `cargo clippy -D warnings` remain green.

An operator observing the effect sees the file shrink, sees `cargo clippy` report no dead-code warnings, and sees nothing else change because nothing else was using the code.

## Requirements Trace

- **R1.** Every unreferenced tmux-related public or private function in `src/codex_exec.rs` is removed.
- **R2.** The `#![allow(dead_code)]` attribute is removed from `src/codex_exec.rs`. If any remaining item triggers a dead-code warning, it is either removed or documented with a specific inline `#[allow(dead_code)]` and a one-line justification.
- **R3.** `cargo build`, `cargo test`, and `cargo clippy --all-targets --all-features -- -D warnings` all pass after the change.
- **R4.** No caller of `codex_exec` is changed. Entry points `run_codex_exec`, `run_codex_exec_with_env`, `spawn_codex`, and any other live function retain their signatures.
- **R5.** The `auto parallel` command continues to function. Verified by `cargo test` (`parallel_command` has ~57 tests) and by manual smoke-test reading of `parallel_command.rs` to confirm it does not depend on the removed helpers.

## Scope Boundaries

- **Changing:** `src/codex_exec.rs` only.
- **Not changing:** `src/parallel_command.rs`, `src/loop_command.rs`, `src/main.rs`, any test files, any other module.
- **Not moving:** no function is relocated to another module. If a helper is kept, it stays in `codex_exec.rs`.
- **Not renaming:** no public symbol is renamed.

## Progress

- [ ] Identify every symbol in `codex_exec.rs`.
- [ ] For each symbol, run a repo-wide reference check.
- [ ] Produce a draft list of symbols to delete and symbols to keep.
- [ ] Remove the draft list.
- [ ] Remove the crate-level `#![allow(dead_code)]`.
- [ ] Run `cargo build`, `cargo test`, `cargo clippy -D warnings`.
- [ ] Commit.

## Surprises & Discoveries

None yet. Potential surprise worth logging during execution: a tmux helper turns out to be called from a test module or a macro that the naive `rg` scan missed.

## Decision Log

- **2026-04-21 — Delete rather than extract to a separate file.** Taste. The dead code is genuinely unused; keeping it "just in case" is the same anti-pattern the operator's global CLAUDE.md flags as a "phantom feature." Git history preserves it if future work wants to resurrect.
- **2026-04-21 — Do not rewrite the live entry points during this plan.** Mechanical. This plan is a deletion, not a refactor. Cleaning up signatures and error handling on the remaining functions belongs in a separate plan if it becomes necessary.

## Outcomes & Retrospective

None yet.

## Context and Orientation

- `src/codex_exec.rs` — 685 lines total.
  - Line 1: `#![allow(dead_code)]` (the artifact to remove).
  - Lines 30-53: `run_codex_exec` — live entry point.
  - Lines 56-119: `run_codex_exec_with_env` — live entry point, called by the command modules.
  - Lines 122-397: `run_codex_exec_in_tmux_with_env`, `spawn_codex_in_tmux`, `ensure_tmux_lanes`, `render_tmux_codex_script`, `wait_for_tmux_completion`, plus their helpers — the dead zone.
  - Lines 206-287: `spawn_codex` — live; used by `run_codex_exec_with_env`.
  - Lines 656-685: two tests for stdout progress detection.
- `src/parallel_command.rs` — the module operators actually use for multi-lane tmux execution. Should not be touched.
- `src/loop_command.rs`, `src/qa_command.rs`, `src/qa_only_command.rs`, `src/review_command.rs`, `src/ship_command.rs`, `src/health_command.rs`, `src/bug_command.rs`, `src/nemesis.rs`, `src/audit_command.rs`, `src/steward_command.rs` — callers of `run_codex_exec_with_env`. Do not touch.

## Plan of Work

1. Enumerate every item defined in `src/codex_exec.rs`. Produce a working list.
2. For each item, grep the rest of `src/` for references. An item used only in `codex_exec.rs` and not used by the two tests or by the live entry points is a candidate for removal.
3. Pay specific attention to: `ensure_tmux_lanes`, `render_tmux_codex_script`, `wait_for_tmux_completion`, `spawn_codex_in_tmux`, `run_codex_exec_in_tmux_with_env`, any constants or helper enums introduced for tmux support.
4. Delete the dead items in one bounded edit. Prefer deleting contiguous blocks to avoid accidental pinhole retention.
5. Remove `#![allow(dead_code)]` from line 1.
6. Run `cargo build` and respond to any unresolved references.
7. Run `cargo test` and `cargo clippy --all-targets --all-features -- -D warnings`. Address any warning by removing the offending item (preferred) or adding a narrow `#[allow(dead_code)]` with a one-line justification (fallback).
8. Commit.

## Implementation Units

**Unit 1 — Reference survey.**
- Goal: produce a list of items in `codex_exec.rs` with their reference counts outside the file.
- Requirements advanced: R1, R4 (ensures live entry points are not touched).
- Dependencies: none.
- Files to create or modify: none yet (survey is working notes).
- Tests to add or modify: none.
- Approach: `grep -nE '^(pub |fn |struct |enum |const )' src/codex_exec.rs` to list all items; then for each item, `rg '<item_name>' src/` to count references.
- Test scenarios: none.
- Test expectation: none -- research step, no code behavior changes.

**Unit 2 — Delete dead tmux helpers.**
- Goal: `codex_exec.rs` no longer contains unreferenced tmux helpers; `#![allow(dead_code)]` is removed.
- Requirements advanced: R1, R2.
- Dependencies: Unit 1.
- Files to create or modify: `src/codex_exec.rs`.
- Tests to add or modify: the existing 2 tests at the bottom of the file are retained. If a test references a deleted helper, keep the test's expectation but rewrite against the live entry point, or remove the test if its only purpose was to cover dead code.
- Approach: `apply_patch`, deleting contiguous functions.
- Test scenarios:
  - `cargo build` succeeds.
  - `cargo test codex_exec` succeeds and the remaining tests cover stdout progress detection.
  - `cargo clippy --all-targets --all-features -- -D warnings` produces no new warnings in `codex_exec.rs`.
- Test expectation: existing tests stay green; no new tests added.

**Unit 3 — Validate no external caller broken.**
- Goal: confirm no other module has a stale reference.
- Requirements advanced: R4, R5.
- Dependencies: Unit 2.
- Files to create or modify: none.
- Tests to add or modify: none.
- Approach: full `cargo build` and full `cargo test`. If a compile error surfaces, it points at a real caller; decide whether to restore the item or fix the caller (preferred: restore the item and investigate, because the initial survey claimed it was unreferenced).
- Test scenarios: full test run succeeds.
- Test expectation: existing tests stay green.

## Concrete Steps

From the repository root:

1. Survey items:
   ```
   grep -nE '^(pub |fn |async fn |struct |enum |const |static )' src/codex_exec.rs
   ```
2. For each function name listed, check outside references:
   ```
   rg 'ensure_tmux_lanes|render_tmux_codex_script|wait_for_tmux_completion|spawn_codex_in_tmux|run_codex_exec_in_tmux_with_env' src/
   ```
   Any match limited to `src/codex_exec.rs` itself is a removal candidate.
3. Open the file and delete the dead functions. Remove line 1's `#![allow(dead_code)]`.
4. Build and test:
   ```
   cargo build
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   ```
5. If clippy or rustc flag items as dead, either delete them too or add a narrow attribute with a justification comment.
6. Stage and commit:
   ```
   git add src/codex_exec.rs
   git commit -m "chore(codex_exec): retire unreferenced tmux scaffolding"
   ```

## Validation and Acceptance

- **Observable 1.** `wc -l src/codex_exec.rs` after is substantially smaller than before. (Before: 685. Target after: under 400.)
- **Observable 2.** `head -5 src/codex_exec.rs` does not contain `#![allow(dead_code)]`.
- **Observable 3.** `cargo build` succeeds.
- **Observable 4.** `cargo test` passes all existing tests.
- **Observable 5.** `cargo clippy --all-targets --all-features -- -D warnings` reports no warnings.
- **Observable 6.** `rg 'ensure_tmux_lanes|render_tmux_codex_script|wait_for_tmux_completion|spawn_codex_in_tmux|run_codex_exec_in_tmux_with_env' src/` returns no matches.

Fail-before-fix check: prior to the edit, `rg 'ensure_tmux_lanes' src/` returns hits only in `src/codex_exec.rs` — demonstrating the dead-code claim.

## Idempotence and Recovery

- If the build fails mid-plan, `git checkout -- src/codex_exec.rs` restores the file and the plan can restart.
- If the deletion is too aggressive (accidentally removed a live helper), `cargo build` fails at the caller; revert the specific deletion.
- The edit is committed once and rerunning the plan is a no-op.

## Artifacts and Notes

- Pre-change `wc -l src/codex_exec.rs`: 685.
- Post-change `wc -l src/codex_exec.rs`: (to be filled).
- Clippy output before and after: (to be filled).
- Commit hash: (to be filled).

## Interfaces and Dependencies

- **Depends on:** nothing external. Pure source-file deletion.
- **Used by:** callers of `run_codex_exec_with_env` (every command module). They are not modified; their dependency on the live entry points is preserved.
- **External:** none. No agent CLI, no network call, but `cargo build` and `cargo test` must run locally.
