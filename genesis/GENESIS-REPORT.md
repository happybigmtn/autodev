# GENESIS-REPORT — autodev corpus refresh

## Refresh summary

This corpus was authored in one planning pass against the working tree at commit `0b59aec` (HEAD on `main`). The repo already has a `genesis/` directory but it was effectively empty (only `genesis/plans/` with no contents). No prior `genesis/` snapshot existed to archive, so no `.auto/fresh-input/` archival was performed for this pass.

No operator focus seed (`--focus`) and no operator idea seed (`--idea`) were supplied. The corpus was shaped purely by repo-state review.

## Major findings

1. **README command inventory drift.** The README claims "thirteen commands"; `src/main.rs:52-96` has sixteen. `steward`, `audit`, and `symphony` are absent from the README inventory and detailed guide. This is the single most operator-visible drift.
2. **Dead tmux scaffolding in `codex_exec.rs`.** Around 400 of the module's 685 lines are marked `#![allow(dead_code)]` and unreferenced. The real tmux logic for `auto parallel` lives in `parallel_command.rs`; the dead code is a stale plan artifact.
3. **Plaintext credentials in quota subsystem.** `quota_config.rs`, `quota_state.rs`, and `quota_usage.rs` all use `fs::write` with default umask; no `chmod 0o600` anywhere. Profile directories under `~/.config/quota-router/profiles/*` contain raw `auth.json` and `.claude.json`.
4. **Model-default drift.** README:39 names MiniMax as the `auto bug` finder default; commit `639d953` made Kimi primary.
5. **`auto audit` has no PI fallback.** Bails at `audit_command.rs:1023` without `--use-kimi-cli`; the doc-comment on the `Audit` variant does not say Kimi is mandatory.
6. **`FileVerdict::touched_paths` and `escalate` are half-wired.** Parsed from verdict JSON, never consumed downstream.
7. **Duplicated logic across commands.** Branch resolution, reference-repo discovery, and prompt logging each reappear in 4-6 modules.
8. **No CI.** `.github/` does not exist; `AGENTS.md` Validate block is aspirational only.
9. **Quota error messages can leak token-refresh body content** on failure paths (`quota_usage.rs:125-126, 245`; `quota_status.rs:75`).
10. **`IMPLEMENTATION_PLAN.md` in the repo is an empty skeleton.** The tool that uses a plan-queue lifecycle does not use one for itself.

## Recommended direction

Close the doc/code gap first, then consolidate. Specifically:

1. Update README so the command inventory, per-command sections, and default-model claims match code reality.
2. Delete the dead tmux scaffolding in `codex_exec.rs`.
3. Add a test harness around `auto audit` verdict application before the command gains further features.
4. Fix quota credential file permissions; scrub error messages that can leak token content.
5. Extract shared helpers (branch resolution, reference-repo discovery, prompt logging) into `util.rs` or a new small module.
6. Research-only: evaluate a `LlmBackend` trait across `bug_command` and `nemesis`; evaluate whether `steward` should supersede `corpus + gen` for mid-flight repos.
7. Introduce a minimal GitHub Actions CI (`cargo test`, `cargo clippy -D warnings`, `cargo fmt --check`).
8. Add end-to-end smoke tests for `qa`, `health`, `ship`, and `audit` against a fixture repo.

## Top next priorities (in order)

1. **Plan 002** — README command inventory sync.
2. **Plan 003** — Delete dead tmux code in `codex_exec.rs`.
3. **Plan 004** — Test harness for `auto audit` verdict application.
4. **Plan 005** — Truth-pass decision gate.
5. **Plan 006** — Quota credential file permissions + log scrubbing.

## Not Doing

- Rewriting the crate or splitting into a workspace.
- Adding a seventeenth command.
- Rewriting `parallel_command.rs` or `generation.rs` beyond extracting shared helpers.
- Building a web UI, TUI, or JSON API front end.
- Replacing `anyhow` with a typed error scheme.
- Encrypting quota credentials at rest in this pass. (Restricting file permissions is in scope; encryption is a separate product decision.)
- Cross-repo refactoring beyond what `--reference-repo` already does.
- Retiring `steward`, `audit`, or `symphony` without an explicit operator decision. Plan 012 researches the question; retirement is not a default.

## Focus-seed response

No `--focus` seed was provided. The recommended priority order above is derived from repo-state review alone, weighting (a) operator-visible discrepancy (README vs. code), (b) concrete security gap (quota plaintext credentials), and (c) structural debt that grows with every new feature (duplicated helpers, no CI, no tests for the newest command). If a focus seed had been provided that pointed away from (a) or (b), those items would still have outranked the focus because they affect every first-run operator experience.

## Decision audit trail

| Decision | Classification | Rationale |
|---|---|---|
| Use `genesis/` as the active planning surface, not a root `plans/` dir | **Mechanical** | Repo has no root `PLANS.md` and no root `plans/`; `AGENTS.md` does not designate one. Creating a corpus under `genesis/` matches what `auto corpus` itself writes. |
| Use `AGENTS.md` as the instruction file convention | **Mechanical** | `AGENTS.md` already exists; no `CLAUDE.md` at the repo root. Codex-first repo per its validation commands. |
| Treat "thirteen commands" in README as a drift issue, not a product statement | **Mechanical** | `main.rs:52-96` has 16 enum variants; `steward` and `audit` have full `Command` doc comments. The README is out of date. |
| Put docs truth-pass (Plan 002) before security fix (Plan 006) | **Taste** | Both matter; docs affect every first-run, security affects only quota users. Alternative would be to do security first; chosen to do docs first because the corpus itself is easier to maintain against a truthful README. Operator may reorder. |
| Delete dead tmux code rather than complete it | **Taste** | `parallel_command.rs` already provides the tmux integration that operators use. The dead code in `codex_exec.rs` is speculative. Could go either way; deletion is simpler and reversible via git. |
| Include `auto audit` tests (Plan 004) in Phase 1 rather than Phase 2 | **Taste** | Test coverage is usually a later phase, but `audit` is one day old and gaining features fast. Locking behavior via tests first pays off quickly. |
| `LlmBackend` trait (Plan 008) as research-only | **Taste** | Two callers do not universally justify an abstraction; Rule-of-three says wait until the third use case appears. Research first, implement later. |
| Command-lifecycle reconciliation (Plan 012) as research-only | **User Challenge** | Whether `steward` replaces `corpus + gen` for mid-flight repos is a product direction the operator should weigh in on. Not safe to auto-decide. |
| Do not refactor `parallel_command.rs` in this pass | **Taste** | 7853 LOC, most-used command, high risk. Earliest legitimate time to consider it is after Plans 005 and 009 gates. |
| Do not introduce encryption at rest for quota credentials | **User Challenge** | Encryption vs. file-permission-only is a product decision with operator-facing implications (key management, CLI surface). This corpus proposes `chmod 0o600` only; encryption remains a backlog item. |
| Not add a new command in this pass | **Mechanical** | "This repo should stay small." Recent churn (two new commands on 2026-04-21) is already testing that. |
| Not tackle the `FileVerdict::touched_paths` / `escalate` half-wired fields in Plan 004 | **Taste** | Scope-of-Plan-004 is test coverage, not feature completion. Escalation resolution belongs in a later plan once tests lock existing behavior. |

## Failure modes and rescue paths

1. **Plan 002 reveals that the README needs a larger rewrite than a line-count fix.** Rescue: bail from 002 with a narrower target (inventory table only) and open a follow-on plan for the detailed-guide rewrite.
2. **Plan 003 breaks a caller that was actually wired and the analyst missed.** Rescue: `cargo build` catches it immediately; revert the deletion and recategorize the code as live. Git keeps the branch recoverable.
3. **Plan 004 requires mocking `kimi-cli` and is hard to test hermetically.** Rescue: split Plan 004 into two sub-plans — pure-function tests for `glob_match` / hash logic (already exist), and a fixture-driven test that supplies a pre-recorded `kimi-cli` stdout. If the fixture approach proves brittle, downgrade Plan 004 to "apply tests to the pure-parse surface" and open a new plan for integration-level coverage.
4. **Plan 006 file-permission change breaks on non-Unix systems.** Rescue: gate the `chmod` call behind `#[cfg(unix)]`. Windows support is already tacit (the code uses `std::fs`, not `nix`), so a `cfg` gate is consistent.
5. **Plan 010 CI fails on `cargo clippy -D warnings` because the current code has warnings.** Rescue: run locally first; Plan 010 becomes "make warnings zero, then add CI."
6. **Plan 011 integration smoke tests reveal a real bug in `qa` / `health` / `ship`.** Rescue: pause 011, open an incident plan, resume 011 once the bug is fixed.
7. **Plan 012 research concludes that `steward` should fully replace `corpus + gen`.** Rescue: open a follow-on plan for retiring `corpus` / `gen`; this is a significant scope change and must go through the `User Challenge` decision path with the operator.

## Glossary (for corpus readers unfamiliar with this repo)

- **ExecPlan.** A numbered plan file under `genesis/plans/` with the full section set defined in this corpus. Self-contained and novice-friendly.
- **Decision gate plan.** A plan that does no implementation work itself; it says what must be true before later plans start.
- **Lane.** A single worker inside `auto parallel`, typically one tmux window with its own git worktree.
- **Verdict (for `auto audit`).** One of `CLEAN`, `DRIFT-SMALL`, `DRIFT-LARGE`, `SLOP`, `RETIRE`, `REFACTOR`. Written by the auditor into `audit/files/<hash-prefix>/verdict.json`.
- **Checkpoint.** A machine-authored git commit used to protect in-progress state before a potentially destructive operation.
- **Sibling repo / reference repo.** A neighboring git repo under the same parent directory that the tool treats as a valid implementation or review surface when a task points there.
- **Planning corpus.** The contents of `genesis/` — disposable planning artifacts (this document is one of them).
- **Quota router.** The `quota_*.rs` modules that select and swap provider credentials across multiple named accounts.
