# Re-Tighten Generated Plan Proof Validators

## Priority Decision

P0. This is the second implementation slice after the formatting and command-surface baseline. Score: very high operator value, high design clarity, high engineering leverage, direct evidence, and high parallel executability. It outranks `auto super`, status UX, quota hardening, and documentation because untrusted generated task rows make future `auto parallel` runs unproductive.

## User / Operator Outcome

An operator can run `auto gen` and receive an implementation plan whose rows reject weak proof, malformed verification commands, vague ownership, broad cargo filters, and inconsistent execution-row fields before workers are dispatched.

## Evidence

- `cargo test generation::tests::generated_plan_rejects_multiple_cargo_test_filters -- --nocapture` failed in the independent corpus review.
- The authoring pass also observed failing generation/spec validation tests around bold fields, estimated scope, bin-only cargo verification, malformed directory grep verification, multiple cargo test filters, and tag-only ownership prose.
- `src/generation.rs`, `src/spec_command.rs`, `src/task_parser.rs`, and `src/verification_lint.rs` all participate in generated task validation.
- `WORKLIST.md` still records required hardening for generated verification command synthesis and false-positive proof.

## Scope Boundary

Do not fix receipt freshness, ship gates, lane routing, `auto super` promotion behavior, quota persistence, or README command lists here. Do not accept broad proof just to make tests pass. If a test is stale because behavior intentionally changed, update the test with a narrow rationale and add a replacement assertion for the new contract.

## Implementation Slice

Goal: restore strict generated-plan and spec-task proof validation.

Dependencies: plan 002 should be complete or its formatting/command-surface failures should be isolated.

Files likely to modify:

- `src/generation.rs`
- `src/spec_command.rs`
- `src/task_parser.rs`
- `src/verification_lint.rs`
- targeted tests embedded in those modules.

Tests to add or modify:

- Existing failing generation/spec validation tests should pass with behavior that rejects weak proof.
- Add focused tests for exactly one accepted behavior per validator change.
- Keep `Required tests:` and `Verification:` parsing consistent between generated plans, spec output, review, and loop consumers.

Approach:

1. Run the targeted failing generation/spec validation tests and capture the first real contract mismatch.
2. Re-tighten generated-plan validation so broad verification, ambiguous ownership, malformed grep proof, stale `cargo --lib` guidance, and multiple cargo filters are handled according to the current intended policy.
3. Normalize bold/plain required field parsing so accepted formatting styles do not bypass validation.
4. Keep error ordering useful: generated-plan-specific failures should not be hidden behind a less precise shared-row error when the generated-plan validator promises a sharper diagnostic.
5. Run the full generation/spec/task-parser verification cluster before widening.

## Verification

From the repository root:

    cargo test generation::tests::generated_plan_accepts_bold_fields_and_required_tests_none_explanation
    cargo test generation::tests::generated_plan_rejects_large_active_scope
    cargo test generation::tests::generated_plan_rejects_bin_only_cargo_lib_verification
    cargo test generation::tests::generated_plan_rejects_malformed_directory_grep_verification
    cargo test generation::tests::generated_plan_rejects_multiple_cargo_test_filters
    cargo test generation::tests::generated_plan_rejects_tag_only_owns_prose_with_helpful_message
    cargo test spec_command::tests::auto_spec_plan_validation_rejects_multi_filter_verification_commands
    cargo test spec_command::tests::auto_spec_plan_validation_rejects_prose_dependencies
    cargo test task_parser::tests
    cargo test verification_lint::tests

Expected observation: generated/spec task validators reject weak proof consistently and still accept explicitly supported row formatting.

## Deferred

- Receipt freshness and ship gate semantics.
- Lane-kind routing.
- `auto super` snapshot-first default.
- Operator status UX.
- Quota persistence hardening.
