# PLANS - generated corpus index

This file indexes the generated `genesis/plans/` ExecPlans. It is not the root implementation queue and it is not a replacement for root planning docs.

## Active Planning Surface

No root `PLANS.md` file exists in this repository today, and no root `plans/` directory exists. The active control surface remains `IMPLEMENTATION_PLAN.md`, `ARCHIVED.md`, `WORKLIST.md`, and `specs/`. The generated `genesis/` corpus is subordinate to those root docs. Its numbered plans are recommendations and ready-to-run slices, not automatic queue authority until promoted into the root implementation plan.

If a root `PLANS.md` standard is later added, future generated numbered plans should explicitly conform to it.

## Sequencing Principles

The chosen order prioritizes trust and recovery before expansion:

1. Reconcile the planning truth surface so operators stop executing stale work.
2. Fix credential and generated workflow safety risks.
3. Gate after security fixes before changing execution contracts.
4. Harden verification evidence and shared task parsing.
5. Gate again before changing first-run and CI surfaces.
6. Improve first-run DX and release proof.
7. Leave lifecycle research as a decision gate rather than silently changing product direction.

The obvious alternative would be to start with modular refactors. That was rejected because current risks are behavioral and operator-facing: credential restore, shell/YAML rendering, false verification proof, and stale planning truth. Current `cargo test` is green, but it does not cover the higher-risk credential restore and workflow-rendering gaps.

## Plan Index

| Plan | Title | Type | Depends on | Why now |
|---|---|---|---|---|
| 001 | Master Plan | Coordination | none | Orients the whole corpus and keeps later slices focused |
| 002 | Root Planning Truth Reconciliation | Implementation | 001 | Prevents stale root docs from steering execution |
| 003 | Quota Credential Restore And Profile Hardening | Implementation | 001 | Fixes the highest-severity credential risk and preserves quota usage error-surfacing coverage |
| 004 | Symphony Workflow Rendering Hardening | Implementation | 001 | Hardens generated executable shell/YAML before more Symphony use |
| 005 | Security Checkpoint Gate | Checkpoint | 002, 003, 004 | Stops later execution-contract work until the security baseline is proved |
| 006 | Verification Command And Receipt Hardening | Implementation | 005 | Closes known false-proof worklist items |
| 007 | Shared Task Parser And Blocked-Task Preservation | Implementation | 005 | Aligns generation, loop, parallel, review, and Symphony on one task contract |
| 008 | Backend Invocation Policy Research | Research gate | 005 | Designs a shared execution policy before refactoring live runners |
| 009 | Execution Contract Checkpoint Gate | Checkpoint | 006, 007, 008 | Confirms evidence and parser changes before DX/CI changes |
| 010 | First-Run Doctor And Hermetic Smoke Tests | Implementation | 009 | Gives operators a no-model success path and contributors integration proof |
| 011 | CI Fidelity And Installed-Binary Proof | Implementation | 009, 010 | Aligns CI with real operator commands and installed CLI expectations |
| 012 | Release Readiness And Command Lifecycle Decision Gate | Checkpoint/research | 010, 011 | Decides whether current lifecycle is ready for release and whether `steward` changes product direction |

## Phase Boundaries

Phase 1: Plans 002-004.

Goal: root truth and security. This phase should end with Plan 005.

Phase 2: Plans 006-008.

Goal: execution evidence and shared contracts. This phase should end with Plan 009.

Phase 3: Plans 010-011.

Goal: first-run confidence and CI fidelity. This phase should end with Plan 012.

## Promotion Guidance

To promote a `genesis/plans/` plan into active work, copy its chosen slice into `IMPLEMENTATION_PLAN.md` using the root plan's existing task style, then keep the `genesis/` plan as supporting detail. Do not execute every generated plan automatically.
