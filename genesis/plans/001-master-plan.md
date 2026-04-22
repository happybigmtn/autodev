# Plan 001 — Master plan and sequencing

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, reconcile this plan against it.

## Purpose / Big Picture

Make `autodev` honest about itself in its docs, safer for its operators in its quota subsystem, cleaner in its internal structure, and enforceable via CI. The operator gains a tool whose README matches the binary, whose credential handling matches the security posture expected of a local CLI, and whose test and CI scaffolding actually runs. An external observer can see it working by comparing README command inventory to `auto --help`, by inspecting `ls -l ~/.config/quota-router/profiles/*/auth.json` and seeing `0o600`, and by watching a green GitHub Actions run on the next push.

This plan is the index for Plans 002 through 012. It does no implementation work itself; it asserts sequencing and dependency order, declares decision gates, and records what the bundle must achieve before the next corpus pass.

## Requirements Trace

- **R1.** The generated corpus under `genesis/` is the active planning surface for this pass and takes precedence over any ad-hoc plan file elsewhere in the tree until a root `PLANS.md` or `plans/` is created. Contract: `genesis/PLANS.md` and `genesis/plans/NNN-*.md` are the canonical list; `IMPLEMENTATION_PLAN.md` at the repository root remains empty until `auto gen` promotes plans into it.
- **R2.** Plans in this corpus execute in the declared Phase 1 → Phase 2 → Phase 3 sequence. Each phase ends with a checkpoint plan (005, 009) that blocks the next phase until its acceptance is met.
- **R3.** No plan in this corpus introduces a new CLI command, a new artifact file shape, or a rewrite of `parallel_command.rs` / `generation.rs` / `bug_command.rs` beyond extracting shared helpers.
- **R4.** Every numbered plan under `genesis/plans/` is self-contained. A novice with only the current working tree and the single plan file can execute it without reading other plans.

## Scope Boundaries

- **Not changing** in this plan: source files under `src/`, configuration under `.auto/`, the existing README or AGENTS.md, anything under `specs/` or `bug/` or `nemesis/`.
- **Not changing** via this corpus at all: the `Symphony` or `Steward` feature sets (their reconciliation is researched in Plan 012 and implementation is deferred).
- **Deferred** to a future corpus pass: encryption at rest for quota credentials, full refactor of `parallel_command.rs`, command retirement.

## Progress

- [x] 2026-04-21 — Corpus authored. `ASSESSMENT.md`, `SPEC.md`, `DESIGN.md`, `PLANS.md`, `GENESIS-REPORT.md`, `plans/001-master-plan.md` through `plans/012-*.md` written.
- [ ] Phase 1 (Plans 002, 003, 004) complete.
- [ ] Plan 005 checkpoint passed.
- [ ] Phase 2 (Plans 006, 007, 008) complete.
- [ ] Plan 009 checkpoint passed.
- [ ] Phase 3 (Plans 010, 011, 012) complete.

## Surprises & Discoveries

- 2026-04-21 — The repo has sixteen commands, not thirteen. Three (`steward`, `audit`, `symphony`) are undocumented in the README.
- 2026-04-21 — `src/codex_exec.rs` carries ~400 lines of dead tmux scaffolding fronted by `#![allow(dead_code)]`.
- 2026-04-21 — Quota credentials are stored in plaintext under `~/.config/quota-router/` with no `chmod` call anywhere in the subsystem.

## Decision Log

- **2026-04-21 — Active planning surface = `genesis/`.** Mechanical. Repo has no root `PLANS.md`, no root `plans/`, no `CLAUDE.md`. `AGENTS.md` does not designate an alternate surface.
- **2026-04-21 — Agent-instruction file convention = `AGENTS.md`.** Mechanical. File already exists; this is a Codex-first repo based on its validation commands.
- **2026-04-21 — Docs truth-pass before security fix.** Taste. Both matter; docs affect every first-run operator, security affects only quota users. Operator may reorder by running Plan 006 before Plans 002-004 if the quota risk is more pressing in their threat model.
- **2026-04-21 — `LlmBackend` trait and command-lifecycle reconciliation are research-only.** Taste / User Challenge. Two callers do not universally justify an abstraction; command retirement is a product decision.
- **2026-04-21 — Phase-boundary checkpoints (Plans 005, 009) are mandatory.** Mechanical. The corpus explicitly requests decision gates at meaningful phase boundaries.

## Outcomes & Retrospective

None yet. To be filled as each phase completes.

## Context and Orientation

Relevant files and starting points for a novice operator picking up this corpus:

- `README.md` at the repository root — current tool-facing prose. Known stale.
- `AGENTS.md` at the repository root — authoritative build / validate / essentials block.
- `src/main.rs` — command enum and dispatch. Lines 52-96 enumerate all sixteen commands.
- `src/util.rs` — shared git, checkpoint, and atomic-write helpers. Starting point for Plan 007 extractions.
- `src/codex_exec.rs` — contains dead tmux scaffolding (Plan 003 target).
- `src/audit_command.rs` — `auto audit` implementation (Plan 004 target).
- `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_usage.rs`, `src/quota_status.rs` — quota subsystem (Plan 006 target).
- `genesis/ASSESSMENT.md` — full review findings with file:line citations.
- `genesis/SPEC.md` — behavioral contract and current-state truth table.
- `genesis/DESIGN.md` — CLI surface and artifact design.
- `genesis/PLANS.md` — index of numbered plans with dependency order.
- `genesis/GENESIS-REPORT.md` — refresh summary and decision audit trail.

The repo builds with `cargo build`. Validate with `cargo test` and `cargo clippy --all-targets --all-features -- -D warnings`. There is no pre-existing CI; introducing it is Plan 010's scope.

## Plan of Work

The master plan expresses the bundle as three phases with two decision gates:

- **Phase 1 (Plans 002 → 003 → 004) — Truth pass.** Small, high-signal, unblock everything else. README inventory, dead-code removal, audit-command test harness.
- **Plan 005 — Phase 1 gate.** Assert: `cargo test`, `cargo clippy -D warnings`, and `auto --help` match README inventory before Phase 2 begins.
- **Phase 2 (Plans 006 → 007 → 008) — Consolidation.** Security hardening on quota, shared-utility extraction, LlmBackend research.
- **Plan 009 — Phase 2 gate.** Assert: credential files are `0o600`, duplication metrics have dropped, and a backend-trait decision is recorded.
- **Phase 3 (Plans 010 → 011 → 012) — Foundation.** CI, integration smoke tests, command-lifecycle research.

## Implementation Units

This plan is an index; it produces no code. It produces this document and the twelve sibling files under `genesis/plans/`.

**Unit 1 — Corpus authorship.**
- Goal: author `ASSESSMENT.md`, `SPEC.md`, `DESIGN.md`, `PLANS.md`, `GENESIS-REPORT.md`, and `plans/001-master-plan.md` through `plans/012-*.md` inside `genesis/`.
- Requirements advanced: R1, R4.
- Dependencies: none.
- Files to create: `genesis/ASSESSMENT.md`, `genesis/SPEC.md`, `genesis/DESIGN.md`, `genesis/PLANS.md`, `genesis/GENESIS-REPORT.md`, `genesis/plans/001-master-plan.md` through `012-command-lifecycle-reconciliation-research.md`.
- Tests to add or modify: none.
- Approach: read repo source and control docs; write a corpus that treats code as truth and docs as claims.
- Test expectation: none — this is the planning corpus itself; no code behavior changes.

## Concrete Steps

1. From the repository root, confirm the corpus is in place:
   ```
   ls genesis/
   ls genesis/plans/
   ```
2. Open `genesis/GENESIS-REPORT.md` and read the Decision audit trail section end to end.
3. Proceed to Plan 002. After each plan completes, return to this plan and update the Progress section.

## Validation and Acceptance

Acceptance for this plan:

- `genesis/` contains `ASSESSMENT.md`, `SPEC.md`, `DESIGN.md`, `PLANS.md`, `GENESIS-REPORT.md`, and `plans/001-master-plan.md` through `plans/012-command-lifecycle-reconciliation-research.md`.
- Each plan under `genesis/plans/` includes every section listed in the corpus ExecPlan requirements (Purpose / Big Picture, Requirements Trace, Scope Boundaries, Progress, Surprises & Discoveries, Decision Log, Outcomes & Retrospective, Context and Orientation, Plan of Work, Implementation Units, Concrete Steps, Validation and Acceptance, Idempotence and Recovery, Artifacts and Notes, Interfaces and Dependencies).
- No plan file contains absolute repository-root paths in prose.

Observable verification:
```
find genesis -name '*.md' -type f | sort
grep -c '^## ' genesis/plans/001-master-plan.md
```
The second command should print at least 15.

## Idempotence and Recovery

Rerunning this plan means re-authoring the corpus. If the corpus already exists and is correct, re-execution is a no-op. If a plan file is accidentally deleted, recover via `git checkout -- genesis/plans/NNN-*.md` from the commit that last contained it. If the entire corpus needs to be rebuilt, `auto corpus` is the reproducer; the operator may choose to rerun it or to hand-edit.

## Artifacts and Notes

- Corpus files enumerated in the Validation section.
- Commit: `0b59aec` was HEAD at corpus-authoring time.
- Tool versions observed: Rust 2021 edition, `clap` 4, `tokio` with `full` features, `reqwest` 0.12, `chrono` 0.4.

## Interfaces and Dependencies

- **Depends on:** nothing. Pure authoring.
- **Used by:** Plans 002 through 012.
- **External:** none. No agent CLI, no network call, no build.
