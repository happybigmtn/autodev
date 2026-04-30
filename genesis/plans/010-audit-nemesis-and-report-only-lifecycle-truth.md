# Audit, Nemesis, and Report-Only Lifecycle Truth

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

Quality commands should be honest about what they read, write, and prove. Operators gain predictable audit, nemesis, health, design, and report-only behavior. They can see it working when status commands do not create runs, dry-run/report-only modes have tested write boundaries, and public flags such as `--audit-passes` either work or are rejected/deprecated clearly.

## Requirements Trace

- R1: `auto audit --everything status/pause/unpause` must not create new run state.
- R2: Audit remediation must not dispatch tasks with unmet dependencies.
- R3: `nemesis --audit-passes` must match runtime behavior or be removed/reframed.
- R4: Report-only and dry-run commands must document and test their write boundaries.
- R5: Health/design/qa/review report surfaces must produce durable, valid artifacts or clear no-op output.
- R6: `auto design --resolve` final NO-GO paths must promote or preserve unresolved plan items before failing.

## Scope Boundaries

This plan does not change the core audit methodology or the content of nemesis findings. It aligns lifecycle truth and operator contracts. It does not require a live production application target.

## Progress

- [x] 2026-04-30: Found status/pause/unpause run-creation risk and nemesis flag drift.
- [ ] 2026-04-30: Fix audit status lifecycle and cycle-breaker dependency safety.
- [ ] 2026-04-30: Reconcile nemesis flags and report-only semantics.

## Surprises & Discoveries

- `qa-only` already has stronger dirty-state guardrails than some neighboring quality commands.
- `design` verifies required artifacts, while `health` relies more heavily on prompt discipline.
- Final-pass design NO-GO handling can fail after generating unresolved plan items unless those rows are promoted or preserved.

## Decision Log

- Mechanical: Public command flags must not be aspirational.
- Taste: Normalize write-boundary behavior command by command rather than abstracting before behavior is clear.
- User Challenge: Some "dry-run" commands may intentionally write prompt logs; if retained, the output must say so plainly.

## Outcomes & Retrospective

None yet. Record each command's final read/write contract.

## Context and Orientation

Relevant files:

- `src/audit_everything.rs`: professional audit runs, status/pause/unpause, remediation.
- `src/audit_command.rs`: audit command behavior.
- `src/nemesis.rs`: nemesis lifecycle and public flags.
- `src/qa_only_command.rs`, `src/health_command.rs`, `src/design_command.rs`, `src/review_command.rs`: report-only and quality surfaces.
- `.auto/audit-everything/`, `nemesis/`, `QA.md`, `HEALTH.md`, `REVIEW.md`: output surfaces.

Non-obvious terms:

- Report-only: a mode that should generate report artifacts without modifying product code.
- Lifecycle truth: the guarantee that status, pause, resume, dry-run, and report-only mean what the command says.

## Plan of Work

Adjust audit status/pause/unpause so they inspect existing runs without creating new state. Ensure audit remediation uses the dependency-safe eligibility helper from Plan 004. Decide whether nemesis should implement multiple audit passes or reject the flag as unsupported. Document and test write boundaries for health/design/qa-only/review/dry-run paths, focusing on whether they write only report/prompt artifacts or no files. Require expected `QA.md` and `HEALTH.md` outputs before success, and make final-pass `auto design --resolve` NO-GO behavior preserve or promote unresolved plan rows before returning failure.

## Implementation Units

- Unit 1: Audit lifecycle. Goal: non-mutating status controls. Requirements advanced: R1, R2. Dependencies: Plans 004 and 009. Files: `src/audit_everything.rs`. Tests: status/pause/unpause with no run creates no directories; remediation skips blocked tasks. Approach: branch phase handling before run creation. Scenarios: no audit run; paused run; missing dependency.
- Unit 2: Nemesis flag truth. Goal: public CLI matches behavior. Requirements advanced: R3. Dependencies: Unit 1 optional. Files: `src/nemesis.rs`, `src/main.rs` if args change, README/spec docs when promoted. Tests: `--audit-passes 2` either performs two passes or errors clearly. Approach: implement or deprecate with explicit message. Scenarios: default pass; invalid pass count.
- Unit 3: Report-only contracts and outputs. Goal: consistent operator expectations. Requirements advanced: R4, R5. Dependencies: Plan 009. Files: `src/qa_only_command.rs`, `src/health_command.rs`, `src/design_command.rs`, `src/review_command.rs`, docs when promoted. Tests: dirty-state/write-boundary tests per command plus missing or empty `QA.md`/`HEALTH.md` failures. Approach: compare pre/post git status and allowed files, then assert expected report artifacts exist and are non-empty. Scenarios: health report only; design dry-run; review dry-run; model exits zero without report.
- Unit 4: Design final NO-GO preservation. Goal: unresolved design work is not dropped. Requirements advanced: R6. Dependencies: Unit 3 optional. Files: `src/design_command.rs`, `src/task_parser.rs` if needed. Tests: final pass NO-GO appends/promotes unresolved plan items before failing. Approach: move promotion before final failure or persist a recovery artifact with explicit instructions. Scenarios: last resolve pass remains NO-GO; generated rows are parser-visible.

## Concrete Steps

From the repository root:

    rg -n "status|pause|unpause|audit_passes|report-only|dry-run|HEALTH.md|QA.md|DESIGN|NO-GO|resolve" src/audit_everything.rs src/nemesis.rs src/qa_only_command.rs src/health_command.rs src/design_command.rs src/review_command.rs src/main.rs
    cargo test audit_everything::tests
    cargo test nemesis::tests
    cargo test qa_only_command::tests
    cargo test health_command::tests
    cargo test design_command::tests
    cargo test review_command::tests
    auto audit --everything status
    auto nemesis --help

Expected observations after implementation: status commands do not create run state, and report-only commands state exactly what they wrote.

## Validation and Acceptance

Acceptance:

- Audit status/pause/unpause are non-mutating when no run exists.
- Audit remediation never dispatches dependency-blocked tasks.
- `nemesis --audit-passes` behavior is implemented or rejected with clear help.
- Report-only/dry-run command tests assert allowed write sets.
- `QA.md` and `HEALTH.md` are required before successful report exits.
- Final-pass design NO-GO keeps unresolved plan rows available to the operator.
- Operator-facing output names artifacts and next steps.

## Idempotence and Recovery

Status and dry-run commands should be safe to repeat. If a command creates an artifact by design, rerunning should overwrite or append predictably and say where. If stale audit state exists, archive it rather than mixing it with new runs.

## Artifacts and Notes

Record the final write-boundary matrix for audit, nemesis, qa-only, health, design, and review in the promoted handoff.

## Interfaces and Dependencies

Interfaces: audit run state, nemesis output, quality reports, git status checks, task eligibility helper. Dependencies: Plans 004 and 009.
