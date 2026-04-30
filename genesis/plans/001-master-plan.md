# Master Production-Readiness Campaign

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This plan frames the full production-readiness campaign for `auto`. The operator gains a clear order of operations: protect state and credentials, make scheduler truth deterministic, bind evidence to current code, normalize lifecycle commands, then promote a root queue for parallel execution. They can see it working when the root ledgers, generated corpus, status output, receipts, and release gates all agree on whether execution is safe.

## Requirements Trace

- R1: Keep `genesis/` subordinate until operator promotion.
- R2: Preserve `auto corpus`, `auto gen`, `auto super`, and `auto parallel` as control primitives.
- R3: Do not launch parallel execution from an empty or unpromoted root queue.
- R4: Prioritize security, scheduler safety, resumability, receipts, first-run DX, and release readiness.
- R5: Include checkpoint gates after meaningful phase boundaries.

## Scope Boundaries

This master plan does not directly change Rust code. It defines sequencing and gates for the numbered plans. It does not replace `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, or root specs. It does not declare `genesis/` active execution truth.

## Progress

- [x] 2026-04-30: Reviewed root instructions, root ledgers, command surface, CI, key runtime modules, git history, and archived genesis snapshot.
- [x] 2026-04-30: Confirmed no root `PLANS.md` or root `plans/` directory exists.
- [x] 2026-04-30: Confirmed current `genesis/` needed outside review before promotion.
- [ ] Promote accepted slices into root `IMPLEMENTATION_PLAN.md` after operator review.
- [ ] Execute checkpoint plans before broader queue launch.

## Surprises & Discoveries

- The root queue is cleared, but runtime completion gates may not understand the empty-review convention.
- The archived genesis snapshot had useful ordering but some release-ledger claims are superseded by current root files.
- The authoring pass reported pre-refresh `genesis/` degradation; independent review should rely on current corpus shape and saved-state/code-path evidence.

## Decision Log

- Mechanical: Keep root ledgers as active truth because repo docs and file layout say so.
- Mechanical: Start with quota/state/scheduler safety because failures there can corrupt credentials, delete planning inputs, or dispatch false work.
- Taste: Use twelve plans with three checkpoint gates to keep ambition high without creating a flat backlog.
- User Challenge: Defer `auto parallel` launch until a root queue is promoted and safety blockers are closed.

## Outcomes & Retrospective

None yet. Fill this section after the operator accepts, promotes, or revises the campaign.

## Context and Orientation

Start at the repository root. The main CLI entry is `src/main.rs`. Planning generation lives in `src/generation.rs` and `src/corpus.rs`. Execution scheduling lives in `src/parallel_command.rs`, `src/loop_command.rs`, `src/super_command.rs`, `src/task_parser.rs`, and `src/completion_artifacts.rs`. Quota and backend execution live in `src/quota_*`, `src/codex_exec.rs`, `src/claude_exec.rs`, `src/kimi_backend.rs`, and `src/pi_backend.rs`. Release and quality gates live in `src/ship_command.rs`, `src/design_command.rs`, `src/qa_only_command.rs`, `src/health_command.rs`, `src/audit_everything.rs`, and `src/nemesis.rs`.

The active planning files are `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `WORKLIST.md`, and root `specs/`.

## Plan of Work

First, execute Plans 002-004 to close the highest-risk state, credential, and scheduler truth gaps. Then run Plan 005 as a checkpoint to decide whether the control plane is safe enough for evidence and release-contract work. Next, execute Plans 006-008 to unify receipts, harden release gates, and normalize execution schema. Run Plan 009 as the second checkpoint. Then execute Plans 010-011 for lifecycle truth, first-run DX, observability, and performance. Finish with Plan 012, which decides whether to promote root queue work and launch `auto parallel`.

## Implementation Units

- Unit 1: Campaign acceptance.
  - Goal: Decide whether this generated corpus becomes the next production-readiness input.
  - Requirements advanced: R1, R2, R3.
  - Dependencies: This corpus.
  - Files to create or modify: none unless promoted to root ledgers.
  - Tests to add or modify: none.
  - Approach: Review `GENESIS-REPORT.md`, `ASSESSMENT.md`, and this plan set.
  - Test scenarios: Test expectation: none -- this unit is a planning decision.

- Unit 2: Root queue promotion gate.
  - Goal: Convert accepted slices into active root work only after checkpoint acceptance.
  - Requirements advanced: R1, R3, R5.
  - Dependencies: Plans 002-005 accepted.
  - Files to create or modify: `IMPLEMENTATION_PLAN.md`, possibly root `specs/`.
  - Tests to add or modify: plan-validation checks appropriate to promoted rows.
  - Approach: Promote minimal, evidence-backed rows rather than copying the whole corpus.
  - Test scenarios: Run root plan parser/status checks after promotion.

- Unit 3: Production execution decision.
  - Goal: Decide whether `auto parallel` can launch.
  - Requirements advanced: R4, R5.
  - Dependencies: Plans 005, 009, and 012.
  - Files to create or modify: `REVIEW.md`, `SHIP.md`, receipts, root ledgers as needed.
  - Tests to add or modify: release and scheduler fixture tests from later plans.
  - Approach: Require current receipts, clean status, and no unresolved safety blockers.
  - Test scenarios: Launch only after `auto parallel status` and release gate proof agree.

## Concrete Steps

From the repository root:

    git status --short --branch

Expected observation: current branch and only intentional corpus changes.

    find genesis -maxdepth 2 -type f | sort

Expected observation: all mandatory corpus files and numbered plans exist.

    rg -n "^- \\[( |~|!)\\]" IMPLEMENTATION_PLAN.md REVIEW.md

Expected observation: no accidental open root queue rows unless deliberately promoted.

Review this corpus, then promote only accepted slices into root ledgers.

## Validation and Acceptance

Acceptance for this master plan is not code behavior. It is accepted when the operator can identify the active planning surface, see why `genesis/` is subordinate, understand the dependency order, and choose which slices to promote. The plan fails if it encourages launching `auto parallel` against an empty queue or treats archived snapshots as current truth.

## Idempotence and Recovery

Rereading or regenerating this master plan is safe. If promotion partially edits root ledgers, recover by comparing `git diff -- IMPLEMENTATION_PLAN.md REVIEW.md specs genesis` and keeping only accepted rows. If a later root `PLANS.md` is added, update this plan to reference its standard.

## Artifacts and Notes

- Corpus artifact: `genesis/GENESIS-REPORT.md`.
- Focus artifact: `genesis/FOCUS.md`.
- Plan index: `genesis/PLANS.md`.
- Historical context: `.auto/fresh-input/genesis-previous-20260430-180207/`.

## Interfaces and Dependencies

- Root ledgers: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `WORKLIST.md`.
- Generated corpus: `genesis/`.
- Scheduler: `auto parallel`, `auto super`, `auto loop`.
- Gates: `auto design`, `auto qa-only`, `auto health`, `auto ship`.
