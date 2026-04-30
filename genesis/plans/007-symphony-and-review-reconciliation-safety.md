# Symphony and Review Reconciliation Safety

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

Symphony and review reconciliation update the operator's source of truth. Operators gain confidence that branch mismatches and partial rows cannot silently corrupt `IMPLEMENTATION_PLAN.md` or review handoffs. They can see it working when branch validation happens before writes and `[~]` rows are preserved or transitioned only through explicit status logic.

## Requirements Trace

- R1: `auto review --branch` must validate branch intent before mutating plan or review files.
- R2: Symphony task marking must handle `[ ]`, `[~]`, `[!]`, and `[x]` without corrupting row text.
- R3: Git refs declared as completion artifacts must be treated as refs, not repo-relative files.
- R4: Review stale-batch follow-ups must be scheduler-visible task rows, not prose rows that disappear from parsed queues.
- R5: AD-014 evidence checkpoint must record hostile workflow render tests and zero-test receipt tests.
- R6: Tests must cover branch mismatch no-write, stale follow-up parser visibility, and partial-row reconciliation.

## Scope Boundaries

This plan does not redesign Symphony integration or require live Linear/Symphony credentials. It focuses on local reconciliation safety, root plan truth, and evidence recording.

## Progress

- [x] 2026-04-30: Found review writes before branch validation and Symphony partial-row corruption risk.
- [ ] 2026-04-30: Move branch validation before writes.
- [ ] 2026-04-30: Harden Symphony status transitions.
- [ ] 2026-04-30: Reconcile AD-014 and TASK-016 evidence.

## Surprises & Discoveries

- Current `TASK-016` is partial while the v0.2.0 tag already exists, and its declared completion artifact looks like a git ref.
- AD-014 appears more like an evidence checkpoint than a large implementation gap.
- Review stale-batch triage can move work out of `REVIEW.md` into a follow-up line that the shared parser does not schedule.

## Decision Log

- Mechanical: Plan/review writes must be validated before mutation.
- Taste: Treat git refs as a completion artifact type instead of asking authors to invent filesystem sentinel files.
- User Challenge: Live Linear/Symphony proof remains intentionally out of scope unless credentials and environment are available.

## Outcomes & Retrospective

None yet. Record whether AD-014 closes locally or remains an external-evidence blocker.

## Context and Orientation

Relevant files:

- `src/review_command.rs`: review harvesting, branch validation, queue writes.
- `src/symphony_command.rs`: plan reconciliation and workflow rendering.
- `src/completion_artifacts.rs`: completion artifact checks.
- `src/task_parser.rs`: artifact parsing and task status.
- `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `WORKLIST.md`: active root truth.

Non-obvious terms:

- Reconciliation: updating root plan/review files after a worker or review phase finishes.
- Git ref artifact: evidence such as `refs/tags/v0.2.0` that should be validated through git, not as a normal file path.

## Plan of Work

Move review branch validation to the beginning of the mutating path. Refactor Symphony task status updates to use parsed status markers and render a single correct marker. Make review stale-batch follow-ups emit scheduler-native task IDs and verify they are parsed. Teach completion artifact checks to recognize declared git refs and verify them with git. Then close AD-014 evidence locally by recording exact hostile workflow and zero-test receipt test outcomes, while leaving live external proof explicitly untested if it remains unavailable.

## Implementation Units

- Unit 1: Review no-write validation. Goal: branch mismatch cannot mutate files. Requirements advanced: R1. Dependencies: Plan 005 pass. Files: `src/review_command.rs`. Tests: branch mismatch leaves `IMPLEMENTATION_PLAN.md` and review queues unchanged. Approach: validate before completed-plan harvesting. Scenarios: wrong branch; no branch flag.
- Unit 2: Symphony status rendering. Goal: avoid corrupted `[x] - [~]` rows. Requirements advanced: R2. Dependencies: shared parser. Files: `src/symphony_command.rs`, `src/task_parser.rs` if needed. Tests: mark pending/blocked/partial/done rows. Approach: parsed marker replacement. Scenarios: partial row remains well formed.
- Unit 3: Review stale follow-up rows. Goal: stale review work remains schedulable. Requirements advanced: R4, R6. Dependencies: Unit 1. Files: `src/review_command.rs`, `src/task_parser.rs` if ID rules change. Tests: stale triage writes a backticked generated ID that parser returns as pending. Approach: generate a stable task-like ID or use the existing task ID when available. Scenarios: stale item removed from review queue appears in parsed implementation queue.
- Unit 4: Git ref completion artifacts. Goal: release tag evidence can close correctly. Requirements advanced: R3. Dependencies: Unit 2 optional. Files: `src/completion_artifacts.rs`, `src/task_parser.rs`. Tests: existing tag satisfies `refs/tags/...`; missing tag blocks. Approach: detect `refs/` prefix and call git. Scenarios: v0.2.0 tag exists.
- Unit 5: AD-014 handoff. Goal: record actual local evidence. Requirements advanced: R5, R6. Dependencies: Units 1-4. Files: `REVIEW.md`, maybe `IMPLEMENTATION_PLAN.md` when promoted. Tests: named AD-014 tests. Approach: run exact tests and record outcomes. Scenarios: hostile branch/model tests pass; zero-test receipt tests pass.

## Concrete Steps

From the repository root:

    rg -n "mark_tasks_done_in_plan|mark_task_header_done|stale|branch|completion artifacts|refs/tags|AD-014|TASK-016" src/review_command.rs src/symphony_command.rs src/completion_artifacts.rs src/task_parser.rs IMPLEMENTATION_PLAN.md REVIEW.md
    cargo test symphony_command::tests::workflow_render_rejects_hostile_branch
    cargo test symphony_command::tests::workflow_render_rejects_hostile_model_and_effort
    cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests
    cargo test review_command::tests
    cargo test symphony_command::tests
    cargo test completion_artifacts::tests

Expected observations after implementation: no malformed task markers, no branch-mismatch writes, and git refs can satisfy completion artifacts.

## Validation and Acceptance

Acceptance:

- Branch mismatch in `auto review` produces no plan/review writes.
- Symphony marking never creates duplicate status markers.
- Stale review follow-ups remain visible to the shared task parser and schedulers.
- Declared git refs are validated with git.
- AD-014 local evidence is recorded with exact test names and results.
- Live external proof remains clearly marked as untested if not run.

## Idempotence and Recovery

Reconciliation should be idempotent: rerunning a completed status update should leave a single marker. If a malformed marker already exists, repair it through parser-rendered output and record the before/after line in the handoff.

## Artifacts and Notes

Fill in AD-014 evidence, branch validation tests, partial-row test names, and git ref validation output when promoted.

## Interfaces and Dependencies

Interfaces: review command, Symphony command, task parser, completion artifacts, git refs, root plan/review files. Dependencies: receipt freshness work improves but does not block local reconciliation tests.
