# Specification: Execution Row Schema Parity

## Objective

Make every command that writes, validates, selects, reviews, or schedules root plan rows enforce the same machine-readable execution-row contract.

## Source Of Truth

- Runtime owners: `src/generation.rs`, `src/spec_command.rs`, `src/super_command.rs`, `src/task_parser.rs`, `src/parallel_command.rs`, `src/loop_command.rs`, `src/review_command.rs`, `src/steward_command.rs`, `src/verification_lint.rs`.
- Queue owners: generated `gen-*/IMPLEMENTATION_PLAN.md`, root `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`.
- UI consumers: generated plan docs, `auto super` execution gate output, `auto parallel` lane prompts, `auto loop` prompts, `auto review` and `auto steward` reconciliation output.
- Generated artifacts: `gen-*/IMPLEMENTATION_PLAN.md`, root `IMPLEMENTATION_PLAN.md`, `.auto/super/*/DETERMINISTIC-GATE.json`, review/steward artifacts.
- Retired/superseded surfaces: legacy row shapes missing source-of-truth/runtime/UI/generated/fixture/retire/contract/cross-surface/closeout fields, prose-only dependencies, broad verification, and ambiguous completion artifacts.

## Evidence Status

Verified facts grounded in code or primary repo files:

- `generation.rs` validates generated implementation plans for required root sections and many required task fields, verified by `rg -n "Generated implementation plan validation|must include|verify_generated_implementation_plan|verify_generated_plan_task" src/generation.rs`.
- The generation prompt requires `Source of truth:`, `Runtime owner:`, `UI consumers:`, `Generated artifacts:`, `Fixture boundary:`, `Retired surfaces:`, `Contract generation:`, `Cross-surface tests:`, and `Review/closeout:`, verified by `rg -n "Source of truth:|Runtime owner:|Cross-surface tests:|Review/closeout:" src/generation.rs`.
- `verification_lint` rejects malformed proof commands for generated task fields, verified by `rg -n "verify_commands_are_runnable|verify_cargo_test_command|verify_grep_command" src/verification_lint.rs`.
- `super_command` has deterministic gate logic for pre-parallel readiness, verified by `rg -n "verify_parallel_ready_plan|verify_super_task|DeterministicGateSummary" src/super_command.rs`.
- `task_parser` parses statuses, dependencies, completion artifacts, and verification text, verified by `rg -n "parse_tasks|parse_task_dependencies|parse_completion_artifacts|verification_text" src/task_parser.rs`.
- `parallel_command`, `loop_command`, `review_command`, and `steward_command` all touch root plan truth, verified by `rg -n "IMPLEMENTATION_PLAN.md|parse_tasks|review|steward" src/parallel_command.rs src/loop_command.rs src/review_command.rs src/steward_command.rs`.

Recommendations for the intended system:

- Extract one shared execution-row validator for generated plan validation, auto spec insertion, super gate, loop selection, parallel selection, review writes, and steward writes.
- Make invalid rows fail with row id and exact missing/malformed field.
- Keep markdown as the storage format for this campaign; enforce schema with parser/validator tests instead of adding a database.
- Normalize prose gate dependencies into `Dependencies:` so scheduler inputs remain machine-readable.

Hypotheses / unresolved questions:

- The validator module name and public API are undecided.
- Backward compatibility for existing root rows may require an explicit normalization pass.
- Some research/design tasks need a lighter implementation contract but still need closeout artifacts and verification.

## Runtime Contract

- `task_parser` owns row parsing.
- A shared validator owns row schema acceptance and must be called before generation sync, super execution, loop/parallel dispatch, review queue reconciliation, and steward promotion.
- `verification_lint` owns command-shape checks within `Verification:` and `Required tests:`.
- If a row lacks required fields, uses broad/unrunnable verification, has prose-only dependencies, or references missing specs, it must be rejected before worker dispatch.

## UI Contract

- Generated plans and root plans must expose the same field names so workers and reviewers do not translate between formats.
- Error output must name the row id, field, and failed invariant.
- Worker prompts must render row fields rather than inventing missing runtime/UI/fixture/retired-surface facts.
- Docs and README must not publish a different row schema than the runtime validator enforces.
- Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- `gen-*/IMPLEMENTATION_PLAN.md`.
- Root `IMPLEMENTATION_PLAN.md` after sync/promotion.
- `.auto/super/*/DETERMINISTIC-GATE.json`.
- Review/steward write artifacts and prompt logs.
- Future fixture rows under tests if a shared validator is introduced.

## Fixture Policy

- Invalid and valid row fixtures belong in Rust unit tests or temp generated plans.
- Production code must validate the live root queue before dispatch; it must not schedule from fixture-normalized rows.
- Fixture tasks must use synthetic ids that cannot collide with active production rows.

## Retired / Superseded Surfaces

- Retire validator copies that check different field sets.
- Retire generated or reviewed task rows that omit runtime/UI/generated/fixture/retired/contract/closeout fields.
- Retire broad package-wide `cargo test` as a generated task verification command unless scoped by a concrete filter or accepted exception.

## Acceptance Criteria

- One shared validator or one shared fixture parity suite proves generation, spec, super, loop, parallel, review, and steward accept/reject the same row.
- Missing required fields fail with row-specific messages.
- Prose mentions of gated prerequisites fail unless the referenced task ids also appear in `Dependencies:`.
- Valid rich generated rows pass super and scheduler readiness without ad hoc translation.
- Research/design rows can be represented truthfully without pretending implementation ownership exists before evidence.

## Verification

- `cargo test generation::tests`
- `cargo test spec_command::tests`
- `cargo test super_command::tests`
- `cargo test task_parser::tests`
- `cargo test parallel_command::tests`
- `cargo test loop_command::tests`
- `cargo test review_command::tests`
- `cargo test steward_command::tests`
- `rg -n "Source of truth:|Runtime owner:|UI consumers:|Contract generation:|Cross-surface tests:|Review/closeout:" src/generation.rs src/spec_command.rs src/super_command.rs src/task_parser.rs`

## Review And Closeout

- A reviewer runs one valid generated-row fixture through every consumer and records that all accept it.
- A reviewer runs one missing-field fixture through every consumer and records that all reject it.
- Grep proof must show no command-specific required-field list can drift silently; either calls go through the shared validator or parity tests cover the callsite.
- Closeout updates README/help if the row contract changes.

## Open Questions

- Should the shared validator return structured diagnostics for UI rendering?
- Should old root rows be auto-normalized or rejected until manually fixed?
- Should `auto review` be allowed to write partial schema rows for follow-up-only items?
