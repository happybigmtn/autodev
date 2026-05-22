# Specification: Generated Plan Proof Validators

## Objective

Restore strict generated-plan and `auto spec` task validation so `auto gen` cannot hand weak, malformed, or broad proof rows to `auto parallel`.

This is P0 because untrusted generated rows can waste multiple workers at once. It outranks status polish, quota hardening, and release reports because those surfaces depend on a truthful execution queue.

## Source Of Truth

- Runtime owner modules/APIs: `src/generation.rs::verify_generated_implementation_plan`, `src/generation.rs::verify_generated_plan_task_is_scoped`, `src/spec_command.rs::verify_plan_output`, `src/task_parser.rs::validate_execution_row`, and `src/verification_lint.rs::verify_commands_are_runnable`.
- UI consumers: `auto gen` stdout, `auto spec` stdout, generated `IMPLEMENTATION_PLAN.md`, `auto super` deterministic gate, `auto parallel` worker prompts, `auto review` queue validation.
- Generated artifacts: `gen-*/specs/*.md`, `gen-*/IMPLEMENTATION_PLAN.md`, root `specs/*.md` and root `IMPLEMENTATION_PLAN.md` when explicit sync is used.
- Retired/superseded surfaces: generated plan rows whose proof is "See spec", broad workspace commands, package-wide cargo tests, stale bin-only `cargo test --lib` guidance for this binary crate, malformed directory `grep` proof, tag-only ownership prose that does not name a concrete owned surface.

## Evidence Status

Verified facts grounded in code or commands:

- `src/generation.rs:145-158` defines required generated spec sections including `## Source Of Truth`, `## Evidence Status`, `## Runtime Contract`, `## UI Contract`, `## Generated Artifacts`, `## Fixture Policy`, `## Retired / Superseded Surfaces`, `## Acceptance Criteria`, `## Verification`, `## Review And Closeout`, and `## Open Questions`.
- `src/generation.rs:159-172` defines required corpus priority plan sections.
- `src/generation.rs:2940-3004` reads and normalizes `gen-*/IMPLEMENTATION_PLAN.md`, validates shared execution rows, validates generated-task scope, checks spec references, and rewrites normalized output.
- `src/generation.rs:3007-3054` applies generated-plan-specific scope checks for decomposition placeholders, `Estimated scope`, `Required tests`, `Verification`, completion artifacts, process fields, ownership, and prose gates.
- `src/generation.rs:3151-3187` rejects vague or overlarge `Required tests:` content and requires concrete test names or explicit `none`.
- `src/generation.rs:3252-3277` rejects broad workspace/all verification and package-wide cargo test verification in generated plans.
- `src/task_parser.rs:298-326` defines required execution-row fields shared by generated plans and runtime queue consumers.
- `src/task_parser.rs:397-423` validates headers, required fields, dependencies, estimated scope, completion artifacts, process fields, commands, ownership, lane kind, and field boundaries.
- `src/verification_lint.rs:4-37` is the shared runnable-command lint entry point.
- `src/verification_lint.rs:45-59` currently contains comments that deliberately loosen `cargo test --lib` and multiple filter handling.
- `WORKLIST.md:3-5` records required follow-up for stale `cargo --lib`, malformed grep, zero-test false proof, and ambiguous corrected task receipts.
- Command evidence from this generation pass: `cargo test generation::tests::generated_plan_rejects_multiple_cargo_test_filters -- --nocapture` exited 101 because the test expected a multi-filter failure but validation accepted the plan path.

Recommendations for the intended system:

- Keep shared execution-row validation strict enough for all consumers, then layer generated-plan-specific diagnostics in `src/generation.rs` where the generator promises sharper proof quality.
- Reject multi-filter cargo test commands in generated verification unless an explicit policy and test says multi-filter commands are acceptable.
- Use `rg` for recursive grep proof against directories; reject non-recursive `grep` against directory-like operands.
- Preserve support for bold field labels only if they normalize into the same required-field contract.

Hypotheses / unresolved questions:

- It is unresolved whether multiple cargo test filters are intentionally allowed by runtime policy or only accidentally allowed by `src/verification_lint.rs`.
- It is unresolved whether `cargo test --lib <filter>` should be rejected for this crate because `Cargo.toml:8-10` declares only `[[bin]] auto`, or whether future library targets should make that check dynamic.

## Runtime Contract

`src/task_parser.rs` owns the canonical execution-row schema. `src/generation.rs` owns stricter generated-plan proof gates before generated rows can become root queue work.

If required sections, required task fields, runnable proof commands, concrete ownership, task dependencies, or referenced specs are absent, generation must fail closed before syncing root outputs or dispatching workers.

If a generated proof command cannot be shown to exercise the target regression, the validator must reject it with a diagnostic that names the task and field.

## UI Contract

Generated `IMPLEMENTATION_PLAN.md` is the operator-facing UI for worker dispatch. It must not duplicate runtime constants, risk classifications, eligibility rules, or verification policy in prose that bypasses `task_parser` and `verification_lint`.

`auto gen`, `auto spec`, and `auto super` should present validator failures as actionable task-row errors, not as model-quality blame or generic parse failures.

Production UI must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; it must consume runtime helpers/generated contracts or render an explicit unavailable/error state when runtime truth is missing.

## Generated Artifacts

- `gen-*/specs/*.md`
- `gen-*/IMPLEMENTATION_PLAN.md`
- Root `specs/*.md` and root `IMPLEMENTATION_PLAN.md` only through explicit sync paths
- Prompt and model logs under `.auto/logs/`

Refresh commands:

```bash
auto gen --snapshot-only
auto gen --sync-only --output-dir <gen-dir>
auto spec "<prompt>"
```

## Fixture Policy

Validator fixtures must live in Rust unit tests or temp directories created by tests. Production generation must not accept fixture specs, sample rows, old `gen-*` snapshots, or copied proof commands as current queue truth.

## Retired / Superseded Surfaces

- Broad verification such as `cargo test --workspace` or `cargo test --all` for one bounded generated task.
- Package-wide cargo tests without a concrete test filter for generated task proof.
- `Required tests: See spec`, `TBD`, `TODO`, or empty proof fields.
- Non-recursive `grep` against directory-like operands as verification proof.
- Tag-only ownership prose without concrete path-like ownership or an explicit git ref contract.

## Acceptance Criteria

- Generated plans with missing required spec sections fail before root sync.
- Generated plans with missing required execution-row fields fail before root sync.
- Generated plans reject broad workspace/all cargo verification for one task.
- Generated plans reject package-wide cargo test verification without a concrete filter.
- Generated plans reject malformed directory `grep` verification.
- Generated plans reject stale bin-only `cargo test --lib` guidance unless target detection proves a lib target exists.
- Generated plans reject multiple cargo test filters unless the policy is intentionally changed and all tests/docs are updated.
- Generated plans accept supported bold/plain field labels only when they normalize into the same task schema.
- `auto spec` uses the same shared row validator as `auto gen` for appended task rows.

## Verification

Run from `/home/r/coding/autodev`:

```bash
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
```

Grep proof:

```bash
rg -n "cargo --lib|multi-filter|grep|Required tests|validate_execution_row|verify_commands_are_runnable" src/generation.rs src/spec_command.rs src/task_parser.rs src/verification_lint.rs WORKLIST.md
```

## Review And Closeout

A reviewer should create or inspect one accepted generated task row and one rejected row for each validator class. The proof must show a validator would fail if the original weak command returned.

Closeout must include the currently failing multi-filter test and at least one grep/assertion proof that the loosened behavior in `src/verification_lint.rs` has either been tightened or intentionally documented with replacement tests.

## Open Questions

- Should multi-filter cargo test commands ever be allowed in generated plan proof?
- Should target-aware validation inspect Cargo metadata before rejecting `cargo test --lib`, or should this binary crate keep a simple fail-closed rule?
- Should zero-test detection be enforced in the generation validator, receipt validator, or both?
