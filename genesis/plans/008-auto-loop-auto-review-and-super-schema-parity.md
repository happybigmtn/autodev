# Auto Loop, Auto Review, and Super Schema Parity

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

Autodev should not enforce one task contract in `auto gen` and a looser one in `auto super`, `auto loop`, or review flows. Operators gain semantic consistency: a task that is valid for generation is valid for execution, and a task that lacks runtime/source/evidence fields is rejected before workers see it. They can see it working when schema tests use one shared contract.

## Requirements Trace

- R1: `auto super` deterministic gate must enforce the same rich task fields as `auto gen` where applicable.
- R2: `auto loop` must use dependency-safe first-actionable selection from Plan 004.
- R3: Review-generated plan changes must preserve the current task contract.
- R4: Schema errors must identify missing fields and source files clearly.
- R5: Tests must cover parity across generation, super, loop, and review command paths.

## Scope Boundaries

This plan does not redesign the task format. It consolidates existing expectations around source of truth, runtime owner, UI consumers, generated artifacts, fixture boundary, verification, completion artifacts, and dependencies. It does not force old archived tasks to be rewritten unless they are active.

## Progress

- [x] 2026-04-30: Found schema drift between `auto super` legacy gate and `auto gen` rich validation.
- [ ] 2026-04-30: Extract or reuse shared task-contract validation.
- [ ] 2026-04-30: Add parity tests for super/loop/review paths.

## Surprises & Discoveries

- `auto gen` has moved faster than downstream consumers, which is a good sign for quality but a risk for execution parity.

## Decision Log

- Mechanical: One autonomous workflow needs one task contract.
- Taste: Prefer shared validation helpers over copying field lists between commands.
- User Challenge: Richer validation may reject legacy active tasks; root plan reconciliation should update those tasks rather than weakening the contract.

## Outcomes & Retrospective

None yet. Record which fields are required for all active implementation rows and which are only required for generated rows.

## Context and Orientation

Relevant files:

- `src/generation.rs`: current rich plan validation and task contract.
- `src/super_command.rs`: deterministic gate and workflow orchestration.
- `src/loop_command.rs`: task selection and prompt generation.
- `src/review_command.rs`: completed plan harvesting and review queue writes.
- `src/task_parser.rs`: shared parse model.

Non-obvious term:

- Task contract: the required markdown fields that make a task executable by an autonomous worker without guessing source truth, scope, or validation.

## Plan of Work

Identify the authoritative validation helper in `src/generation.rs` or extract a small shared module. Use it from `auto super` before execution, from review-generated plan ingestion before writes, and from loop task selection before prompt dispatch. Ensure errors are precise and preserve current `auto gen` behavior. Add tests proving equivalent input is accepted or rejected consistently across command paths.

## Implementation Units

- Unit 1: Shared contract helper. Goal: avoid duplicated schema drift. Requirements advanced: R1, R3. Dependencies: Plan 004 parser fixes. Files: `src/generation.rs`, possible new `src/task_contract.rs`, callers. Tests: existing generation validators still pass. Approach: extract minimal helper without broad refactor. Scenarios: missing Source of truth; missing Verification; broad Required tests.
- Unit 2: Super parity. Goal: enforce rich fields before super execution. Requirements advanced: R1, R4. Dependencies: Unit 1. Files: `src/super_command.rs`. Tests: super rejects legacy minimal task. Approach: call shared validator and format actionable errors. Scenarios: old task shape rejected with field list.
- Unit 3: Loop/review parity. Goal: prevent loose downstream execution. Requirements advanced: R2, R3, R5. Dependencies: Unit 1. Files: `src/loop_command.rs`, `src/review_command.rs`. Tests: loop skips invalid task; review refuses to write invalid generated plan. Approach: validate parsed blocks before prompt/write. Scenarios: dependency-ready but schema-invalid task remains blocked.

## Concrete Steps

From the repository root:

    rg -n "validate_generated_plan|Required tests|Source of truth|Runtime owner|first actionable|deterministic gate" src/generation.rs src/super_command.rs src/loop_command.rs src/review_command.rs src/task_parser.rs
    cargo test generation::tests
    cargo test super_command::tests
    cargo test loop_command::tests
    cargo test review_command::tests
    auto gen --help

Expected observations after implementation: the same malformed task fails in every execution path with a comparable error.

## Validation and Acceptance

Acceptance:

- `auto super` rejects active implementation rows missing required execution fields.
- `auto loop` does not dispatch schema-invalid rows.
- `auto review` cannot write generated plan changes that fail the current contract.
- Existing valid generated tasks still pass.
- Error messages name the row ID and missing fields.

## Idempotence and Recovery

Rerunning validation should be deterministic. If active root tasks fail the richer contract, update those task blocks in the root planning surface when promoted rather than bypassing validation.

## Artifacts and Notes

Record rejected sample rows and final validator location in the task handoff.

## Interfaces and Dependencies

Interfaces: generation validator, task parser, super command, loop command, review command. Dependencies: Plan 004 dependency truth and current generated task field names.
