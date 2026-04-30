# Scheduler Completion Truth And Lane Resume

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This slice makes `auto parallel` safe to trust. The operator gains a scheduler that fails closed when current plan truth is unavailable, does not demote completed work because of contradictory ledger conventions, and refuses to resume stale lanes whose task body changed under the same id.

## Requirements Trace

- R1: Completion evidence conventions must be explicit and machine-checkable.
- R2: Checked rows with accepted archive/direct-review evidence must not mass-demote unexpectedly.
- R3: Production dispatch must fail closed on current plan refresh failure by default.
- R4: Last-good snapshot dispatch must be explicit recovery mode.
- R5: Lane resume must bind task id, task body, dependencies, verification text, base commit, and assignment hash.
- R6: `auto parallel status` must expose whether launch/resume/land is safe.

## Scope Boundaries

This plan does not implement new worker features. It does not change model prompts except where they must include new evidence-class or assignment-hash contracts. It does not remove stale-lane recovery; it makes recovery explicit and safer.

## Progress

- [x] 2026-04-30: Verified root rows are checked while `REVIEW.md` is empty.
- [x] 2026-04-30: Verified completion evidence checks require review handoff for full evidence.
- [x] 2026-04-30: Identified last-good plan fallback in scheduler refresh.
- [x] 2026-04-30: Identified lane resume keyed primarily around task identity and base context rather than a durable task-body hash.
- [ ] Add evidence-class policy and regression tests.
- [ ] Add fail-closed dispatch mode.
- [ ] Add lane assignment hash persistence and resume rejection.
- [ ] Add manifest-backed status summary.

## Surprises & Discoveries

- The root ledger cleanup is intentional, but runtime completion logic may not encode that convention.
- `auto parallel status` has rich stale recovery display, but the underlying truth is still spread across logs, lane dirs, pid files, and plan state.

## Decision Log

- Mechanical: Scheduler launch must rely on current plan truth by default.
- Mechanical: A checked row cannot mean both "complete" and "missing review handoff" without an explicit evidence class.
- Taste: Preserve recovery mode for last-good snapshots because it is useful during outages, but require explicit operator intent.
- User Challenge: Empty `REVIEW.md` can remain a valid convention only if the code recognizes where completion evidence lives instead.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Relevant files:

- `src/parallel_command.rs`: production scheduler, plan refresh, lane creation/resume, status rendering, landing.
- `src/loop_command.rs`: serial worker loop and completion demotion.
- `src/task_parser.rs`: parses `IMPLEMENTATION_PLAN.md` task rows, dependencies, artifacts, verification text.
- `src/completion_artifacts.rs`: checks review handoff, receipts, artifacts, and audit finding status.
- `src/verification_lint.rs`: validates command evidence shape.
- `tests/parallel_status.rs`: current integration status tests.
- `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`: root ledger truth.

Non-obvious terms:

- Evidence class: a machine-readable label explaining whether completion proof is a host receipt, review handoff, external blocker, archive record, or operator waiver.
- Last-good snapshot: a previous parse of the root queue used when current refresh fails.
- Assignment hash: digest of the task body and execution contract stored with a lane.

## Plan of Work

Define completion evidence classes and teach `completion_artifacts` to evaluate the current root-ledger convention. If checked rows can be backed by `ARCHIVED.md`, `COMPLETED.md`, receipts, or an explicit empty-review convention, encode that rule and test it. Otherwise update the root-ledger convention to keep minimal review handoffs.

Change `auto parallel` dispatch so current plan refresh failure blocks launch by default. Add an explicit recovery flag or mode for last-good snapshot dispatch, with loud output and no silent production scheduling.

Persist lane assignment metadata in each lane directory and in a run manifest. Include task id, task body hash, dependency text hash, verification text hash, base commit, branch, and assigned worker command. On resume, reject mismatch unless an operator performs a deliberate rebind.

Update `auto parallel status` to summarize safe/unsafe launch, stale plan snapshot state, stale lane state, and evidence drift from one manifest-backed source.

## Implementation Units

- Unit 1: Completion evidence policy.
  - Goal: Make checked rows and review handoffs consistent.
  - Requirements advanced: R1, R2.
  - Dependencies: none.
  - Files to create or modify: `src/completion_artifacts.rs`, `src/task_parser.rs`, root ledger tests or fixtures.
  - Tests to add or modify: current checked-row plus empty `REVIEW.md` fixture; missing evidence fixture; accepted archive/direct-review fixture.
  - Approach: Add explicit evidence classes and update demotion logic to use them.
  - Test scenarios: current ledger convention does not mass-demote if accepted evidence exists.

- Unit 2: Fail-closed plan refresh.
  - Goal: Prevent dispatch from stale plan snapshots by default.
  - Requirements advanced: R3, R4.
  - Dependencies: Unit 1 optional.
  - Files to create or modify: `src/parallel_command.rs`, CLI args in `src/main.rs` if a recovery flag is needed.
  - Tests to add or modify: current plan unreadable blocks launch; explicit recovery flag allows last-good dispatch with warning.
  - Approach: Split status recovery from production dispatch.
  - Test scenarios: corrupt `IMPLEMENTATION_PLAN.md` fails before lane spawn.

- Unit 3: Lane assignment hashes.
  - Goal: Reject stale lane resumes after task changes.
  - Requirements advanced: R5.
  - Dependencies: task parser hash helper.
  - Files to create or modify: `src/parallel_command.rs`, maybe a new scheduler metadata module.
  - Tests to add or modify: same task id with changed body rejects resume; unchanged task resumes; explicit rebind records new hash.
  - Approach: Write metadata when assigning lane and compare on resume.
  - Test scenarios: edit dependency text while lane is paused; resume fails safely.

- Unit 4: Manifest-backed status.
  - Goal: Make launch/resume/land safety visible.
  - Requirements advanced: R6.
  - Dependencies: Units 2-3.
  - Files to create or modify: `src/parallel_command.rs`, `tests/parallel_status.rs`.
  - Tests to add or modify: manifest status for no run, live run, stale lane, refresh failure, safe launch.
  - Approach: Add `.auto/parallel/manifest.json` or equivalent with durable run state.
  - Test scenarios: `auto parallel status` prints safe/unsafe summary from manifest.

## Concrete Steps

From the repository root:

    rg -n "refresh_parallel_plan_or_last_good|is_fully_evidenced|has_review_handoff|resume|status" src/parallel_command.rs src/completion_artifacts.rs src/loop_command.rs tests

Expected observation: current refresh, evidence, resume, and status paths.

    cargo test parallel_status
    cargo test completion_artifacts

Expected observation before work: new split-brain and stale-resume tests fail.

After implementation:

    cargo test parallel_status
    cargo test completion_artifacts
    cargo test task_parser
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: scheduler truth tests pass and clippy is clean.

## Validation and Acceptance

Acceptance requires a fixture representing the current checked-row/empty-review state, a failing plan-refresh production dispatch test, a lane assignment hash mismatch test, and `auto parallel status` output that names launch safety. The scheduler must not silently run from stale truth.

## Idempotence and Recovery

Manifest writes should be idempotent and resumable. If metadata is missing for old lanes, status should mark them as legacy and require deliberate rebind or cleanup. If a plan refresh fails, rerunning after fixing the plan should proceed without manual cleanup. Recovery mode must leave a visible audit trail.

## Artifacts and Notes

- Evidence to fill in: current ledger fixture result before policy change.
- Evidence to fill in: `.auto/parallel/manifest.json` schema or equivalent.
- Evidence to fill in: status output before and after stale lane detection.

## Interfaces and Dependencies

- CLI: `auto parallel`, `auto parallel status`, `auto loop`, `auto super`.
- Files: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `.auto/parallel/`.
- Modules: `parallel_command`, `loop_command`, `task_parser`, `completion_artifacts`, `verification_lint`.
