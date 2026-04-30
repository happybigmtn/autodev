# Audit Nemesis And Report-Only Lifecycle Truth

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This slice makes the quality lifecycle honest. The operator gains report-only commands that really communicate their write boundaries, audit and nemesis flows with model-free fixture coverage, and lifecycle reports that cannot imply proof the host did not create.

## Requirements Trace

- R1: Report-only commands must declare and enforce their allowed write sets.
- R2: `auto nemesis --report-only` semantics must be renamed, tightened, or documented so root planning mutations are not surprising.
- R3: `auto nemesis --audit-passes` must either run a real loop or fail as unsupported with clear messaging.
- R4: `auto audit --everything` status and final review must use exact verdict and evidence contracts from earlier plans.
- R5: Critical lifecycle commands must have model-free fixture tests with stubbed backends.
- R6: Reports must distinguish host receipts, model observations, external blockers, and operator waivers.

## Scope Boundaries

This plan does not re-audit every source file. It does not change the audit quality rubric except where evidence labels or verdict parsing require it. It does not implement deployment.

## Progress

- [x] 2026-04-30: Verified `qa-only`, `health`, and `design` have real report-only boundary checks.
- [x] 2026-04-30: Verified `nemesis --report-only` can still sync root spec/plan artifacts.
- [x] 2026-04-30: Verified specs record `nemesis --audit-passes` as not fully implemented.
- [x] 2026-04-30: Verified audit lifecycle is mature but large and still model-report heavy.
- [ ] Decide report-only semantics for nemesis.
- [ ] Add lifecycle fixture harness.
- [ ] Apply shared evidence/verdict contracts to audit/nemesis reports.

## Surprises & Discoveries

- Report-only is strongest in `qa-only`, `health`, and `design`; nemesis is the outlier.
- `audit --everything` has the richest resumability model and can be used as a pattern for other lifecycle commands.

## Decision Log

- Mechanical: A command named report-only should not surprise operators with root plan/spec mutations.
- Mechanical: Advertised flags must either work or fail clearly.
- Taste: Prefer fixture tests with stubbed backends over live model tests for CI.
- User Challenge: Tightening report-only behavior may change an existing workflow that relies on nemesis writing planning artifacts without committing.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Relevant files:

- `src/qa_only_command.rs`, `src/health_command.rs`, `src/design_command.rs`: report-only boundary patterns.
- `src/nemesis.rs`: adversarial lifecycle, report-only behavior, audit-pass flags.
- `src/audit_everything.rs`, `src/audit_command.rs`: audit lifecycle and resumable manifest.
- `src/review_command.rs`, `src/qa_command.rs`, `src/book_command.rs`: quality/report flows.
- Root specs: quality pipeline and nemesis specs.

Non-obvious terms:

- Report-only: a mode that should only write reports/logs and not alter runtime or planning truth.
- Stubbed backend: a fake `codex` or `claude` command used in tests to produce deterministic model output without a live model.

## Plan of Work

Compare report-only write boundaries across `qa-only`, `health`, `design`, and `nemesis`. Decide whether nemesis report-only should become strictly output-only or be renamed/documented as planning-sync-without-commit. Update code, help text, and tests accordingly.

For `--audit-passes`, either implement the documented loop or change CLI/help/spec behavior to fail explicitly as unsupported until implemented. Do not leave inert flags that imply production behavior.

Build a model-free fixture harness for lifecycle commands. Use temp repositories and fake backend executables to test write boundaries, report requirements, verdict parsing, evidence labels, final status blocks, and queue transitions.

## Implementation Units

- Unit 1: Report-only semantics inventory.
  - Goal: Identify allowed writes for each report-only command.
  - Requirements advanced: R1, R2.
  - Dependencies: Plan 009.
  - Files to create or modify: notes, tests, possibly command help.
  - Tests to add or modify: none in this discovery unit.
  - Approach: Compare dirty-state guard patterns and write paths.
  - Test scenarios: Test expectation: none -- discovery only.

- Unit 2: Nemesis report-only decision.
  - Goal: Remove surprise from nemesis behavior.
  - Requirements advanced: R2.
  - Dependencies: Unit 1.
  - Files to create or modify: `src/nemesis.rs`, `src/main.rs`, README/specs as needed.
  - Tests to add or modify: report-only write-boundary test; planning-sync mode test if renamed.
  - Approach: Either restrict writes to report/logs or rename/add an explicit flag for plan/spec sync.
  - Test scenarios: `auto nemesis --report-only` leaves root specs/plans untouched unless explicit sync mode is used.

- Unit 3: Nemesis audit-passes contract.
  - Goal: Make advertised flag behavior truthful.
  - Requirements advanced: R3.
  - Dependencies: none.
  - Files to create or modify: `src/nemesis.rs`, `src/main.rs`, specs.
  - Tests to add or modify: nonzero audit passes either run expected loop or fail with unsupported message.
  - Approach: Choose implementation or explicit unsupported gate based on feasibility.
  - Test scenarios: `auto nemesis --audit-passes 2` has observable behavior matching help.

- Unit 4: Lifecycle fixture harness.
  - Goal: Test critical model-backed flows without live models.
  - Requirements advanced: R5.
  - Dependencies: fake backend utilities.
  - Files to create or modify: tests under `tests/` or module tests.
  - Tests to add or modify: `qa-only`, `health`, `review`, `design --resolve`, `nemesis --report-only`, `ship`, `audit --everything` smoke fixtures.
  - Approach: Create temp repo fixtures and fake `codex`/`claude` executables that write deterministic reports.
  - Test scenarios: report-only commands fail if fake backend writes unauthorized files.

- Unit 5: Evidence labels in lifecycle reports.
  - Goal: Prevent model prose from masquerading as host proof.
  - Requirements advanced: R4, R6.
  - Dependencies: Plans 006-007.
  - Files to create or modify: `src/audit_everything.rs`, `src/audit_command.rs`, `src/nemesis.rs`, `src/review_command.rs`.
  - Tests to add or modify: report with command claim but no receipt is labeled narrative or blocked.
  - Approach: Reuse evidence classes and verdict parser from earlier plans.
  - Test scenarios: audit final review with mixed verdict or unlabeled proof fails.

## Concrete Steps

From the repository root:

    rg -n "report_only|audit_passes|allowed|dirty|Verdict|receipt|final status" src/qa_only_command.rs src/health_command.rs src/design_command.rs src/nemesis.rs src/audit_everything.rs src/audit_command.rs src/review_command.rs src/ship_command.rs

Expected observation: report-only write boundaries and lifecycle gate points.

    cargo test qa_only
    cargo test health
    cargo test design
    cargo test nemesis
    cargo test audit_everything

Expected observation before work: new lifecycle fixture tests fail or are absent.

After implementation:

    cargo test qa_only
    cargo test health
    cargo test design
    cargo test nemesis
    cargo test audit_everything
    cargo test --test lifecycle_flows
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: report-only and lifecycle fixture tests pass.

## Validation and Acceptance

Acceptance requires `nemesis --report-only` semantics to be honest, `--audit-passes` behavior to match help, and at least one model-free fixture path for each critical lifecycle command family. Reports must use exact verdict parsing and evidence labels from earlier plans.

## Idempotence and Recovery

Report-only tests should create temp repositories and clean up after themselves. If nemesis behavior changes, preserve a migration note in docs/specs. If fixture backend scripts fail, the tests should show backend stderr paths for diagnosis.

## Artifacts and Notes

- Evidence to fill in: nemesis semantics decision.
- Evidence to fill in: lifecycle fixture test names.
- Evidence to fill in: before/after report-only dirty-state proof.

## Interfaces and Dependencies

- Commands: `auto qa-only`, `auto health`, `auto design`, `auto review`, `auto nemesis`, `auto audit`, `auto audit --everything`, `auto ship`.
- Files: reports, root specs, root ledgers, `.auto/` run roots.
- Modules: quality, audit, nemesis, review, ship command modules.
