# Plan 009 — Checkpoint: Consolidation complete

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

## Purpose / Big Picture

This plan is a decision gate, mirroring Plan 005. It does no implementation. It asserts a set of conditions that must hold before Phase 3 (Plans 010 through 012) can start.

Phase 2 delivered three things: a concrete security fix (credential file mode + error-body scrubbing, Plan 006), a structural refactor that extracts three duplicated helpers into `src/util.rs` (Plan 007), and a research note that decides whether a shared `LlmBackend` trait is warranted (Plan 008). This checkpoint verifies that all three landed and that the code still builds clean.

The purpose of the gate is to prevent Phase 3 (CI bootstrap, integration smoke tests, lifecycle reconciliation research) from starting against partially-landed Phase 2 work. CI that runs on top of unscrubbed credential errors, for example, would bake the bug into the first green signal the repo ever has.

The operator sees a short verification script, a line appended to `COMPLETED.md`, and a green light for Phase 3.

## Requirements Trace

- **R1.** Plan 006 is merged: credential files under `~/.config/quota-router` (or platform equivalent) are created with mode `0o600` on Unix, and error paths under `quota_usage.rs` / `quota_status.rs` no longer emit raw response bodies by default. An opt-in `AUTO_QUOTA_VERBOSE_ERRORS=1` preserves the prior diagnostic detail for debugging.
- **R2.** Plan 007 is merged: `resolve_working_branch`, `resolve_reference_repos`, and `log_prompt` are defined exactly once in `src/util.rs` and re-used across the former duplicate sites. A ripgrep scan across `src/` returns exactly one definition per symbol.
- **R3.** Plan 008 research is committed: `genesis/research/008-llm-backend-survey.md` exists, contains a recommendation, and the recommendation is classified in Plan 008's Decision Log.
- **R4.** `cargo build`, `cargo test`, `cargo clippy --all-targets --all-features -- -D warnings` all succeed on `main` after Phase 2 merges.
- **R5.** `COMPLETED.md` has a Phase 2 section summarizing the three plans and their commit hashes.
- **R6.** `genesis/PLANS.md` is updated if Plan 008 produced a follow-on (new plan declared) or deferral (note added).

## Scope Boundaries

- **Changing:** `COMPLETED.md` (append only); `genesis/PLANS.md` (only if Plan 008 requires it).
- **Not changing:** any file under `src/`, any test, any research note.
- **Not introducing:** a new plan beyond 010-012 already declared, unless Plan 008 explicitly required it.

## Progress

- [ ] R1 verified.
- [ ] R2 verified.
- [ ] R3 verified.
- [ ] R4 verified.
- [ ] R5 append authored.
- [ ] R6 reconciliation done.
- [ ] Plan marked complete. Phase 3 unblocked.

## Surprises & Discoveries

None yet. Potential surprises: Plan 007 extraction discovered a subtle behavioral difference between the three duplicate copies (they diverged silently), and the fix required more than a pure extraction.

## Decision Log

- **2026-04-21 — Gate covers both security AND structural work together.** Taste. Splitting into two gates would double the ceremony without adding safety. The two are independent, but they are both Phase 2 exits and belong to the same readiness statement.
- **2026-04-21 — R3 accepts "no trait" as a valid pass.** Mechanical. The research recommendation is the deliverable; its *content* is not gated by this plan. If Plan 008 says no-trait, R3 is still satisfied.
- **2026-04-21 — `AUTO_QUOTA_VERBOSE_ERRORS=1` is the only opt-in flag.** Taste. Scrubbing by default is the safer posture; the opt-in preserves developer debugging. A CLI flag would proliferate surface area across every quota command.

## Outcomes & Retrospective

None yet.

## Context and Orientation

- `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_usage.rs`, `src/quota_status.rs` — verification targets for R1.
- `src/util.rs` — verification target for R2 (exactly one definition each of the three helpers).
- `src/bug_command.rs`, `src/generation.rs`, `src/loop_command.rs`, `src/parallel_command.rs`, `src/review_command.rs`, `src/symphony_command.rs`, `src/steward_command.rs`, `src/main.rs` — verification targets for R2 (no duplicate definitions remain).
- `genesis/research/008-llm-backend-survey.md` — verification target for R3.
- `genesis/plans/008-llm-backend-trait-research.md` — verification target for R3 (Decision Log classification).
- `COMPLETED.md` — append target for R5.
- `genesis/PLANS.md` — reconciliation target for R6.
- Predecessor plans: `genesis/plans/006-quota-credential-hardening.md`, `007-shared-util-extraction.md`, `008-llm-backend-trait-research.md`.

## Plan of Work

1. Run the verification script below.
2. If any assertion fails, return to the relevant predecessor plan and complete it.
3. Append to `COMPLETED.md`.
4. Reconcile `genesis/PLANS.md` if Plan 008 required a follow-on.
5. Commit.

## Implementation Units

**Unit 1 — Verification script run.**
- Goal: every assertion in the Validation section returns the expected value.
- Requirements advanced: R1, R2, R3, R4.
- Dependencies: Plans 006, 007, 008 complete.
- Files to create or modify: none.
- Tests to add or modify: none.
- Approach: run the script below; fix any failing predecessor plan before proceeding.
- Test expectation: none -- this is a gate, not a code change.

**Unit 2 — Completion ledger update.**
- Goal: `COMPLETED.md` records Phase 2 as done, with per-plan commit hashes.
- Requirements advanced: R5.
- Dependencies: Unit 1.
- Files to create or modify: `COMPLETED.md`.
- Tests to add or modify: none.
- Approach: append a `## Phase 2 consolidation (YYYY-MM-DD)` section with one bullet per plan.
- Test expectation: none.

**Unit 3 — PLANS.md reconciliation.**
- Goal: if Plan 008 produced a "yes, trait" recommendation, a follow-on plan is declared in `genesis/PLANS.md`; if "no trait", a deferral note is added.
- Requirements advanced: R6.
- Dependencies: Unit 1.
- Files to create or modify: `genesis/PLANS.md` (only if required).
- Tests to add or modify: none.
- Approach: read the Plan 008 recommendation; decide which branch applies; make the smallest possible edit.
- Test expectation: none -- docs only.

## Concrete Steps

From the repository root:

1. Check R1 (credential hardening):
   ```
   rg -n 'fs::set_permissions|PermissionsExt::set_mode\(' src/quota_config.rs src/quota_state.rs src/quota_usage.rs
   rg -n 'AUTO_QUOTA_VERBOSE_ERRORS' src/
   rg -n 'response\.text|response_text|body\.to_string' src/quota_usage.rs src/quota_status.rs
   ```
   - Expect: at least one `set_permissions` call per quota-state writer on Unix-gated code.
   - Expect: `AUTO_QUOTA_VERBOSE_ERRORS` referenced and its default behavior is scrubbed output.
   - Expect: raw body extraction is wrapped by a redactor helper; bare body returns flagged for removal.
2. Check R2 (shared utility extraction):
   ```
   rg -nE 'fn resolve_working_branch\b' src/
   rg -nE 'fn resolve_reference_repos\b' src/
   rg -nE 'fn log_prompt\b' src/
   ```
   - Expect: exactly one definition per symbol, in `src/util.rs`.
3. Check R3 (Plan 008 research):
   ```
   ls genesis/research/008-llm-backend-survey.md
   rg -n '^## Recommendation' genesis/research/008-llm-backend-survey.md
   rg -n 'Mechanical|Taste|User Challenge' genesis/plans/008-llm-backend-trait-research.md
   ```
   - Expect: survey file exists; recommendation section present; classification in Decision Log.
4. Check R4 (build/test/clippy):
   ```
   cargo build
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   ```
5. Append to `COMPLETED.md`:
   - `## Phase 2 consolidation (YYYY-MM-DD)` heading
   - three bullets (Plan 006, 007, 008) with title and commit hash
   - note: "Phase 3 (genesis/plans/010 through 012) unblocked."
6. If Plan 008 recommended a follow-on trait plan: append a row to the `genesis/PLANS.md` numbered plan table and commit together. If "no trait": append a one-line deferral note under the table.
7. Commit:
   ```
   git add COMPLETED.md genesis/PLANS.md
   git commit -m "docs: mark Phase 2 consolidation complete"
   ```

## Validation and Acceptance

- **Observable 1.** R1 ripgrep shows credential-file writers call `set_permissions` (Unix-gated); raw body extraction is wrapped by a redactor.
- **Observable 2.** R2 ripgrep shows exactly one definition per shared helper, all in `src/util.rs`.
- **Observable 3.** Plan 008 research file exists and contains a recommendation section.
- **Observable 4.** `cargo build`, `cargo test`, `cargo clippy -D warnings` all exit 0.
- **Observable 5.** `git log -1 COMPLETED.md` shows the Phase 2 commit.
- **Observable 6.** If Plan 008 required it, `genesis/PLANS.md` has been updated.

## Idempotence and Recovery

- Rerunning the verification script is safe; it reflects current state.
- If the `COMPLETED.md` append is committed twice, the second commit is empty or duplicative and should be squashed.
- If a predecessor plan is partially reverted, assertions fail and the gate does not pass. Fix the predecessor.

## Artifacts and Notes

- Baseline verification output: (to be filled).
- `COMPLETED.md` append block: (to be filled).
- Commit hashes for Plans 006, 007, 008: (to be filled).
- Plan 008 recommendation classification: (to be filled — `full | scoped | no-trait`).

## Interfaces and Dependencies

- **Depends on:** Plans 006, 007, 008.
- **Used by:** Plans 010, 011, 012 start only after this gate passes.
- **External:** `cargo`. No agent CLI, no network.
