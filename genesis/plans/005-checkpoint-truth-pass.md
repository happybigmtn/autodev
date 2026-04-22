# Plan 005 — Checkpoint: Truth pass complete

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

## Purpose / Big Picture

This plan is a decision gate. It does no implementation work. It asserts a set of conditions that must be true before Phase 2 (Plans 006 through 008) can start. The purpose is to prevent the common failure mode where a corpus refresh moves onto structural changes while the surface-level drift that seeded the corpus remains unfixed.

The operator gains a short verification script and a documented state. Anyone picking up the repo after this gate passes can see that the README is honest, the audit command has a minimum-viable test harness, and the dead tmux code is gone.

## Requirements Trace

- **R1.** Plan 002 is merged on `main` (or at least on the active working branch) and `grep -c 'thirteen commands' README.md` is `0`.
- **R2.** Plan 003 is merged and `head -5 src/codex_exec.rs` does not contain `#![allow(dead_code)]`.
- **R3.** Plan 004 is merged and `cargo test audit_command` reports at least 13 tests passing.
- **R4.** `cargo build`, `cargo test`, `cargo clippy --all-targets --all-features -- -D warnings` all succeed on `main`.
- **R5.** A line is appended to `COMPLETED.md` summarizing the truth pass.

## Scope Boundaries

- **Changing:** `COMPLETED.md` — one appended section summarizing the truth pass.
- **Not changing:** source, other docs, CI config.
- **Not introducing:** new plans beyond 006-008 already declared.

## Progress

- [ ] R1 verified.
- [ ] R2 verified.
- [ ] R3 verified.
- [ ] R4 verified.
- [ ] `COMPLETED.md` append authored.
- [ ] Plan marked complete. Phase 2 unblocked.

## Surprises & Discoveries

None yet.

## Decision Log

- **2026-04-21 — Gate is hard, not soft.** Taste. Allowing Phase 2 to start with Phase 1 items only partially done produces a situation where the security fix (Plan 006) is reviewed against a docs-stale README, which makes review harder and confusing. Hard gate is worth the small friction.
- **2026-04-21 — Gate lives as a plan file, not as a CI rule.** Mechanical. CI does not exist yet (Plan 010 introduces it). The gate is operator-driven.

## Outcomes & Retrospective

None yet.

## Context and Orientation

- `README.md` — verification target for R1.
- `src/codex_exec.rs` — verification target for R2.
- `src/audit_command.rs` — verification target for R3.
- `Cargo.toml` — standard build config.
- `AGENTS.md` — names the validate commands.
- `COMPLETED.md` — append target for R5.
- `genesis/plans/002-readme-command-inventory-sync.md`, `003-codex-exec-tmux-deadcode-removal.md`, `004-audit-verdict-test-harness.md` — predecessor plans.

## Plan of Work

1. Run the validation script below.
2. If any check fails, return to the relevant predecessor plan and finish it.
3. When all checks pass, write the append block into `COMPLETED.md` and commit.

## Implementation Units

**Unit 1 — Verification script run.**
- Goal: every assertion in the Validation section returns the expected value.
- Requirements advanced: R1, R2, R3, R4.
- Dependencies: Plans 002, 003, 004 complete.
- Files to create or modify: none.
- Tests to add or modify: none.
- Approach: run the script below; fix any failing predecessor plan before proceeding.
- Test expectation: none -- this is a gate, not a code change.

**Unit 2 — Completion ledger update.**
- Goal: `COMPLETED.md` records the truth pass as done, with a short list of the three predecessor plans and their commit hashes.
- Requirements advanced: R5.
- Dependencies: Unit 1.
- Files to create or modify: `COMPLETED.md`.
- Tests to add or modify: none.
- Approach: append a new `## Phase 1 truth pass (YYYY-MM-DD)` section with bullet points per plan.
- Test expectation: none -- docs-only.

## Concrete Steps

From the repository root:

1. Check R1:
   ```
   grep -c 'thirteen commands' README.md
   grep -cE '^- `auto (steward|audit|symphony)`' README.md
   grep -c '### `auto steward`' README.md
   grep 'finder: Kimi' README.md
   ```
2. Check R2:
   ```
   head -5 src/codex_exec.rs
   wc -l src/codex_exec.rs
   rg 'ensure_tmux_lanes|render_tmux_codex_script|wait_for_tmux_completion' src/
   ```
3. Check R3:
   ```
   cargo test audit_command -- --list | wc -l
   cargo test audit_command
   ```
4. Check R4:
   ```
   cargo build
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   ```
5. If everything passes, append to `COMPLETED.md`:
   - a `## Phase 1 truth pass (YYYY-MM-DD)` heading
   - one bullet per plan with title and commit hash
   - a note "Phase 2 (genesis/plans/006 through 008) unblocked."
6. Commit:
   ```
   git add COMPLETED.md
   git commit -m "docs: mark Phase 1 truth pass complete"
   ```

## Validation and Acceptance

- **Observable 1.** R1 grep returns `0` for the obsolete count, `3` for the new inventory rows, `1` each for the three new detailed-guide sections.
- **Observable 2.** `codex_exec.rs` is under 400 lines and does not contain `#![allow(dead_code)]`.
- **Observable 3.** `cargo test audit_command` reports at least 13 passing tests.
- **Observable 4.** `cargo build`, `cargo test`, `cargo clippy -D warnings` all exit `0`.
- **Observable 5.** `git log -1 COMPLETED.md` shows the truth-pass commit.

## Idempotence and Recovery

- Rerunning the validation script is safe and reflects current state.
- If the `COMPLETED.md` append is committed twice, the second commit is empty or duplicative and should be squashed.
- If any predecessor plan has been partially reverted, the grep assertions fail and the gate does not pass. Fix the predecessor first.

## Artifacts and Notes

- Baseline verification output: (to be filled after run).
- `COMPLETED.md` append block: (to be filled after run).
- Commit hashes for Plans 002, 003, 004: (to be filled).

## Interfaces and Dependencies

- **Depends on:** Plans 002, 003, 004.
- **Used by:** Plans 006, 007, 008 start only after this gate passes.
- **External:** `cargo`. No agent CLI, no network.
