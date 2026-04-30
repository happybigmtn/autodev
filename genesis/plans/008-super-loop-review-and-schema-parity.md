# Super Loop Review And Schema Parity

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This slice makes generated execution contracts consistent across `auto super`, `auto gen`, `auto loop`, `auto review`, and `auto parallel`. The operator gains a single task-row shape that all execution paths understand, so a generated plan row cannot pass one gate and fail or be misread by another.

## Requirements Trace

- R1: Generated plan rows must include machine-readable task id, dependencies, verification, artifacts, and ownership.
- R2: `auto super` execution gate and `auto parallel` scheduler must validate the same required fields.
- R3: `auto loop` must not select dependency-blocked or evidence-impossible rows.
- R4: `auto review` and steward reconciliation must not write queue rows that violate scheduler schema.
- R5: Schema errors must produce actionable messages naming the row and missing field.

## Scope Boundaries

This plan does not redesign the root ledger format. It does not require YAML or a database. It tightens the existing markdown contract and shares validation across command paths.

## Progress

- [x] 2026-04-30: Verified `src/generation.rs` has rich generated-plan validators.
- [x] 2026-04-30: Verified `src/super_command.rs` has deterministic execution gate logic.
- [x] 2026-04-30: Verified scheduler, loop, review, and steward all touch root plan truth.
- [ ] Inventory all row validators and parser assumptions.
- [ ] Extract shared execution-row validation.
- [ ] Add parity tests across super/gen/loop/review/parallel.

## Surprises & Discoveries

- The generator has more plan-shape validation than some runtime consumers.
- Strong prompts exist, but the production guarantee must come from host-side shared validation.

## Decision Log

- Mechanical: One markdown row shape should have one host-side validator.
- Mechanical: A row that can be scheduled must have evidence and dependency fields all consumers understand.
- Taste: Keep markdown as the source format for now; add explicit contracts instead of inventing a new storage layer.
- User Challenge: If existing plans are too loose, the operator may need to accept a one-time queue normalization.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Relevant files:

- `src/generation.rs`: corpus and generated plan validation.
- `src/super_command.rs`: execution gate before parallel launch.
- `src/task_parser.rs`: root plan row parser.
- `src/parallel_command.rs`: scheduler eligibility.
- `src/loop_command.rs`: serial task selection.
- `src/review_command.rs`, `src/steward_command.rs`: queue reconciliation and review flows.
- `IMPLEMENTATION_PLAN.md`, `REVIEW.md`: root execution ledgers.

Non-obvious terms:

- Execution-row schema: the set of markdown fields a task row must carry to be safely scheduled.
- Schema parity: all command paths reject or accept the same row for the same reasons.

## Plan of Work

Inventory existing generated-plan validation, super execution gate checks, task parser fields, scheduler eligibility checks, and review/steward row-writing behavior. Extract a shared validation function or module that produces structured errors. Call it from generation sync, super execution gate, parallel preflight, loop selection, review/steward queue writes, and any plan promotion command.

Update prompts only after host validation is in place, so model output is guided but not trusted. Add fixtures for valid rows and invalid rows covering missing dependencies, missing artifacts, prose-only verification, unknown dependency ids, duplicated ids, and unsafe evidence paths.

## Implementation Units

- Unit 1: Validator inventory.
  - Goal: Identify every row validator and writer.
  - Requirements advanced: R1, R2, R4.
  - Dependencies: none.
  - Files to create or modify: documentation notes or code comments only if useful.
  - Tests to add or modify: none in this unit.
  - Approach: Use `rg` and record callsites before extraction.
  - Test scenarios: Test expectation: none -- this is discovery for the shared module.

- Unit 2: Shared execution-row validator.
  - Goal: Give all command paths one schema contract.
  - Requirements advanced: R1, R2, R5.
  - Dependencies: Unit 1.
  - Files to create or modify: `src/task_parser.rs` or new `src/task_schema.rs`, `src/generation.rs`, `src/super_command.rs`.
  - Tests to add or modify: valid row passes; missing fields fail with row id; unknown dependency fails.
  - Approach: Return structured validation errors and preserve current parser output.
  - Test scenarios: generated row without artifacts is rejected before sync.

- Unit 3: Runtime consumer parity.
  - Goal: Ensure loop, parallel, review, and steward use the shared contract.
  - Requirements advanced: R2, R3, R4.
  - Dependencies: Unit 2.
  - Files to create or modify: `src/parallel_command.rs`, `src/loop_command.rs`, `src/review_command.rs`, `src/steward_command.rs`.
  - Tests to add or modify: loop skips dependency-blocked row; review write rejects invalid row; parallel preflight reports same errors as super.
  - Approach: Call shared validator at selection/write boundaries.
  - Test scenarios: a row with missing dependency id blocks dispatch with a precise error.

- Unit 4: Prompt and docs alignment.
  - Goal: Ensure model prompts ask for exactly the host-enforced row contract.
  - Requirements advanced: R1, R5.
  - Dependencies: Units 2-3.
  - Files to create or modify: prompt strings in `src/generation.rs`, `src/super_command.rs`, `src/parallel_command.rs`, README if needed.
  - Tests to add or modify: prompt snapshot/string tests where existing patterns use them.
  - Approach: Replace prose-only guidance with the shared field names and failure language.
  - Test scenarios: generated prompt mentions required fields and evidence classes.

## Concrete Steps

From the repository root:

    rg -n "Dependencies:|Verification:|Completion artifacts|validate|ready task|task row|IMPLEMENTATION_PLAN" src/generation.rs src/super_command.rs src/task_parser.rs src/parallel_command.rs src/loop_command.rs src/review_command.rs src/steward_command.rs

Expected observation: all row contract and parser paths.

    cargo test task_parser
    cargo test generation
    cargo test parallel
    cargo test super

Expected observation before work: new parity tests fail.

After implementation:

    cargo test task_parser
    cargo test generation
    cargo test parallel
    cargo test super
    cargo test review
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: row schema behavior is consistent across producers and consumers.

## Validation and Acceptance

Acceptance requires one shared schema contract invoked by generation, super, parallel, loop, review, and steward paths. Invalid rows must fail with precise row-level errors. A valid generated row must be accepted by all consumers without ad hoc translation.

## Idempotence and Recovery

Schema validation is read-only unless invoked during a writer flow. Existing invalid root rows should produce blockers, not automatic rewrites, unless the operator runs a deliberate normalization step. Prompt updates are safe to rerun.

## Artifacts and Notes

- Evidence to fill in: validator callsite list.
- Evidence to fill in: invalid row fixture names.
- Evidence to fill in: before/after error messages for one malformed row.

## Interfaces and Dependencies

- Commands: `auto gen`, `auto super`, `auto parallel`, `auto loop`, `auto review`, `auto steward`.
- Files: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, generated specs/plans.
- Modules: `generation`, `super_command`, `task_parser`, `parallel_command`, `loop_command`, `review_command`, `steward_command`.
