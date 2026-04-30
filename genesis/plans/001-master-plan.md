# Master Plan for Autodev Production Readiness

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

This plan turns the genesis assessment into an ordered production-readiness program. Operators gain a clear path from current repository truth to a safe `auto parallel` launch and release gate. They can see it working when quota credentials are isolated, corpus generation is rollback-safe, scheduling refuses blocked work, receipts prove the current tree, and `auto ship` refuses stale evidence.

## Requirements Trace

- R1: Preserve `auto corpus` and `auto gen` as control primitives while making them safe.
- R2: Close high-severity security and scheduler risks before throughput work.
- R3: Keep active root planning truth in `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `ARCHIVED.md`, `REVIEW.md`, and `specs/` unless the operator promotes a new surface.
- R4: Split work into independently verifiable slices with checkpoint gates.
- R5: Avoid running `auto parallel` until the queue and evidence model are trustworthy.

## Scope Boundaries

This master plan does not implement code by itself. It governs the generated plan set and names the order in which work should land. It does not replace root planning files, does not rewrite old specs directly, and does not launch parallel workers.

## Progress

- [x] 2026-04-30: Reviewed root instructions, current code, active plans, previous snapshot, CI, receipts, and history.
- [x] 2026-04-30: Identified the high-priority production risks and grouped them into numbered ExecPlans.
- [ ] 2026-04-30: Promote selected slices into the active root queue after operator approval.

## Surprises & Discoveries

- `genesis/` was empty/deleted in the working tree, which reproduced the corpus-root risk in current state.
- The most urgent risks are not missing product features; they are trust-boundary failures in quota, corpus, dependencies, and receipts.

## Decision Log

- Mechanical: No root `PLANS.md` or root `plans/` exists, so this corpus is subordinate to root control docs.
- Mechanical: Plans include a terminal/operator design slice because the CLI is a user-facing product.
- Taste: Two checkpoint plans divide the queue into safety and execution-contract phases.
- User Challenge: The operator focus mentions implementing the approved queue with `auto parallel`, but this plan defers launch until safety gates pass.

## Outcomes & Retrospective

None yet. Fill this in after the operator promotes slices or after the first checkpoint gate is run.

## Context and Orientation

Relevant files:

- `src/main.rs` defines the `auto` command surface.
- `src/corpus.rs` and `src/generation.rs` own corpus and generation behavior.
- `src/quota_config.rs` and `src/quota_exec.rs` own quota-backed credential handling.
- `src/task_parser.rs`, `src/parallel_command.rs`, and `src/completion_artifacts.rs` own queue, dependencies, lanes, and completion evidence.
- `src/ship_command.rs`, `scripts/run-task-verification.sh`, and `scripts/verification_receipt.py` own release proof.
- Root planning truth currently lives in `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `ARCHIVED.md`, `REVIEW.md`, and `specs/`.

## Plan of Work

Execute the queue in four phases. First, close quota security, corpus atomicity, and dependency truth. Second, run a security/state checkpoint. Third, bind receipts, repair reconciliation paths, and align schema consumers. Fourth, run an execution-contract checkpoint, then normalize audit/DX and decide whether to promote the queue into active root execution.

## Implementation Units

- Unit 1: Safety tranche. Goal: remove immediate production blockers. Requirements advanced: R1, R2. Dependencies: none. Files: Plans 002-004. Tests: quota path/lease tests, corpus failure tests, parser/scheduler tests. Approach: implement isolated fixes by module ownership. Scenarios: malicious account name rejected; failed corpus preserves previous root; missing dependency blocks scheduling.
- Unit 2: First checkpoint. Goal: decide whether evidence supports moving into execution-contract work. Requirements advanced: R4, R5. Dependencies: Unit 1. Files: Plan 005 and checkpoint notes. Tests: targeted command suite from Plans 002-004. Approach: run review and record go/no-go. Scenarios: all safety tests pass or blockers are listed.
- Unit 3: Execution-contract tranche. Goal: make evidence, reconciliation, and command schemas consistent. Requirements advanced: R2, R4. Dependencies: Unit 2. Files: Plans 006-008. Tests: receipt freshness, Symphony/review no-write, loop/super schema tests. Approach: add shared helpers before command-specific behavior. Scenarios: stale receipt rejected; partial rows preserved; blocked loop row skipped.
- Unit 4: Release tranche. Goal: normalize audit/DX and decide queue promotion. Requirements advanced: R3, R5. Dependencies: Unit 3. Files: Plans 009-012. Tests: report-only write boundaries, doctor/help smoke, release gate. Approach: close stale root truth and record decision. Scenarios: `auto parallel status` is green or explicit blockers remain.

## Concrete Steps

From the repository root:

    git status --short
    cargo test -- --list
    rg -n "AD-014|TASK-016|WORKLIST|Dependencies:" IMPLEMENTATION_PLAN.md WORKLIST.md
    auto doctor

After each implementation tranche, run its plan-specific tests and update root planning files only when the operator promotes the work.

## Validation and Acceptance

Acceptance for this master plan is a coherent plan set, not code behavior. The generated files must exist, use the required ExecPlan headings, and rank security/control-plane blockers before parallel execution. Later acceptance belongs to the child plans and checkpoint gates.

## Idempotence and Recovery

Rerun this planning pass by regenerating `genesis/` from current code and root plan truth. If partial plan edits occur, compare `git status --short`, restore only the affected `genesis/` files, and keep unrelated user changes intact.

## Artifacts and Notes

- This file indexes the overall program.
- `genesis/PLANS.md` is the human-readable plan index.
- `genesis/GENESIS-REPORT.md` records the decision audit trail.

## Interfaces and Dependencies

This plan depends on the current Rust CLI architecture, root planning docs, CI workflow, receipt scripts, and active git state. It does not introduce external services.
