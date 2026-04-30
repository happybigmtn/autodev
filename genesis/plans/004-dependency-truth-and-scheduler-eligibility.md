# Dependency Truth and Scheduler Eligibility

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

Autonomous execution is only safe when every scheduler sees the same dependency truth. Operators gain confidence that `auto parallel`, `auto loop`, audit remediation, and Symphony workers will not start blocked rows. They can see it working when bare dependency references are parsed, external blockers stay external, missing dependency IDs block scheduling, and status output explains why a row is not eligible.

## Requirements Trace

- R1: Parse dependency IDs in backticked and bare forms such as `Dependencies: TASK-011`.
- R2: Treat missing dependency IDs as unresolved blockers, not as resolved work.
- R3: Keep external dependencies distinct from internal dependency IDs so unsynced external blockers cannot become silently satisfied work.
- R4: Share eligibility rules across `auto parallel`, `auto loop`, audit remediation, and generated task consumers.
- R5: Preserve blocked, partial, pending, and done task status semantics.
- R6: Tests must cover dependency extraction, missing IDs, external dependencies, and first-actionable selection.

## Scope Boundaries

This plan does not rewrite the entire task schema and does not change the visible markdown status markers. It focuses on dependency truth and scheduling eligibility. Broader schema parity is handled in Plan 008.

## Progress

- [x] 2026-04-30: Found that bare dependency IDs are not parsed and missing IDs are not treated as blockers.
- [ ] 2026-04-30: Extend parser tests for bare and missing dependencies.
- [ ] 2026-04-30: Route scheduler eligibility through the shared parser.

## Surprises & Discoveries

- Root `IMPLEMENTATION_PLAN.md` contains a dependency format that current parsing can miss.
- A missing dependency ID is more dangerous than a completed dependency because it may indicate stale plan truth.
- `External dependency:` lines are parsed near ordinary dependencies but later consumers do not consistently use the structured external blocker list.

## Decision Log

- Mechanical: Dependency truth is a prerequisite for safe parallel execution.
- Taste: Keep markdown human-readable; improve parser tolerance instead of forcing every old plan line to be rewritten first.
- User Challenge: If a dependency ID is missing from the current plan, the scheduler should block and ask for reconciliation, even if that slows execution.

## Outcomes & Retrospective

None yet. Record any legacy dependency formats intentionally accepted or rejected.

## Context and Orientation

Relevant files:

- `src/task_parser.rs`: shared task parser and dependency extraction.
- `src/parallel_command.rs`: queue parsing, unresolved dependency filtering, status, lane assignment.
- `src/loop_command.rs`: first actionable task selection.
- `src/audit_everything.rs`: remediation queue and cycle-breaker behavior.
- `IMPLEMENTATION_PLAN.md` and `WORKLIST.md`: current human-authored dependency examples.

Non-obvious terms:

- Eligible task: a pending or partial task whose dependencies are done and whose blockers are not external.
- Missing dependency: a dependency ID referenced by a task but absent from the parsed plan.

## Plan of Work

First, add parser tests for bare dependency IDs, comma-separated IDs, backticked IDs, external dependency lines, and missing dependency references. Then update the dependency model to return known unresolved dependencies, missing dependency IDs, and external blockers as separate concepts. Update `auto parallel status`, lane assignment, `auto loop`, Symphony relation sync, and audit remediation to use this shared eligibility result. Finally, add user-facing status text that explains missing dependencies and external blockers.

## Implementation Units

- Unit 1: Parser expansion. Goal: parse common dependency forms. Requirements advanced: R1, R5. Dependencies: none. Files: `src/task_parser.rs`. Tests: dependency extraction with bare IDs, backticks, punctuation, and external dependency lines. Approach: structured line parsing rather than ad hoc grep. Scenarios: `Dependencies: TASK-011`; `Dependencies: TASK-011, AD-014`; `External dependency: Linear issue`.
- Unit 2: Missing and external dependency blockers. Goal: prevent false readiness. Requirements advanced: R2, R3. Dependencies: Unit 1. Files: `src/task_parser.rs`, `src/parallel_command.rs`, `src/symphony_command.rs`. Tests: task with unknown dependency is blocked; external-only blocker is visible and unscheduled unless explicitly waived. Approach: compare referenced IDs to parsed task map and carry external blockers separately. Scenarios: missing ID appears in status output; unsynced external issue is not silently skipped.
- Unit 3: Shared eligibility consumers. Goal: align scheduler behavior. Requirements advanced: R4, R6. Dependencies: Unit 2. Files: `src/parallel_command.rs`, `src/loop_command.rs`, `src/audit_everything.rs`. Tests: first actionable skips blocked row; audit cycle-breaker does not dispatch blocked row. Approach: call shared helper. Scenarios: partial row with missing dependency remains blocked.

## Concrete Steps

From the repository root:

    rg -n "Dependencies:|External dependency|unresolved|first actionable|cycle-breaker|parse" src/task_parser.rs src/parallel_command.rs src/loop_command.rs src/audit_everything.rs src/symphony_command.rs IMPLEMENTATION_PLAN.md WORKLIST.md
    cargo test task_parser::tests
    cargo test parallel_command::tests
    cargo test loop_command::tests
    cargo test audit_everything::tests
    auto parallel status

Expected observations after implementation: status output names missing dependencies and no scheduler selects those rows.

## Validation and Acceptance

Acceptance:

- Bare dependency IDs parse the same as backticked IDs.
- Missing dependency IDs block execution and are visible in status.
- External dependencies remain visible blockers until resolved or explicitly waived.
- `auto loop` does not present a dependency-blocked row as first actionable.
- Audit remediation does not use a cycle-breaker to dispatch blocked work.
- Existing blocked `[!]`, partial `[~]`, pending `[ ]`, and done `[x]` behavior remains intact.

## Idempotence and Recovery

Parser and scheduler changes are deterministic. If a plan becomes newly blocked because an old dependency was previously ignored, update the root plan truth rather than weakening the parser. Rerun status after edits to confirm the queue state.

## Artifacts and Notes

Record before/after `auto parallel status` snippets in the task handoff, especially rows that move from ready to blocked because of missing dependencies.

## Interfaces and Dependencies

Interfaces: shared task parser, queue status, lane assignment, loop prompt preparation, audit remediation. Dependencies: current markdown task format and root plan files.
