# IMPLEMENTATION_PLAN

## Priority Work

- [x] `P0-001` Restore the validation baseline and public command surface

    Spec: `specs/220526-validation-command-surface-baseline.md`
    Why now: This is the first P0 because workers cannot interpret later red tests if formatting and the live Clap command list are already failing; it also gives operators a truthful first-run command surface before deeper validator or release work.
    Codebase evidence: `cargo fmt --check` currently reports diffs in `src/spec_command.rs`, `src/super_command.rs`, and `src/task_parser.rs`; `cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture` currently fails because live `auto --help` includes `audit-harvest` while the test and README omit it; `.github/workflows/ci.yml` smokes `auto --help` but omits `auto doctor --help` and `auto audit-harvest --help`.
    Source of truth: `src/main.rs` Clap `Command` enum and dispatch are canonical for public CLI commands.
    Runtime owner: `src/main.rs`
    UI consumers: `auto --help`, `auto audit-harvest --help`, `README.md`, `.github/workflows/ci.yml`, `src/doctor_command.rs`
    Generated artifacts: none
    Fixture boundary: Command-surface tests may use Clap parse fixtures; production help and CI smoke must read the live binary, not copied help text or generated snapshots.
    Retired surfaces: README's stale `twenty-one commands` count if `audit-harvest` remains public; `tests::top_level_command_surface_matches_live_enum` expected list without `audit-harvest`; CI installed-binary smoke that skips `auto doctor --help` and an intended public `auto audit-harvest --help`.
    Owns: src/main.rs, src/spec_command.rs, src/super_command.rs, src/task_parser.rs, README.md, .github/workflows/ci.yml
    Integration touchpoints: `cargo fmt --check`, `auto --help`, `auto doctor --help`, `auto audit-harvest --help`
    Scope boundary: Decide only the current public/hidden status of `audit-harvest`; do not add a top-level `auto status`, rework doctor readiness categories, or change model-backed command behavior.
    Acceptance criteria: `cargo fmt --check` exits 0; the command-surface test matches the live public Clap command list; README command count/list and CI smoke agree with the chosen public surface; `auto doctor --help` is included in first-run help proof; `audit-harvest` is either documented and smoked as public or hidden with a test proving it is not listed.
    Verification: Run current red proof first with `cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture`; after changes run `cargo fmt --check`, `cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture`, `cargo test tests::doctor_command_is_parseable -- --nocapture`, `cargo run -- --help`, and `cargo run -- audit-harvest --help` if public.
    Required tests: `tests::top_level_command_surface_matches_live_enum`, `tests::doctor_command_is_parseable`, `doctor_command::tests::doctor_checks_expected_help_surfaces`
    Contract generation: none -- no generated contract
    Cross-surface tests: `cargo run -- --help` and `cargo run -- audit-harvest --help` must read back the same public surface documented in `README.md` and smoked in `.github/workflows/ci.yml`.
    Review/closeout: Reviewer checks `rg -n "audit-harvest|twenty-one commands|twenty-two commands|auto doctor --help" README.md .github/workflows/ci.yml src/main.rs` plus the command-surface test so the original command drift cannot return silently.
    Completion artifacts: none
    Dependencies: none
    Estimated scope: S
    Completion signal: Local formatting and the public command-surface regression are green, with README and CI smoke aligned to live Clap output.

- [x] `P0-002` Re-tighten shared proof command lint

    Spec: `specs/220526-generated-plan-proof-validators.md`
    Why now: This is the second P0 because generated and spec-authored task rows feed parallel execution; it outranks status polish and quota hardening because weak proof commands can dispatch multiple workers on unprovable tasks.
    Codebase evidence: `src/verification_lint.rs::verify_cargo_test_command` is currently a no-op; `cargo test spec_command::tests::auto_spec_plan_validation_rejects_multi_filter_verification_commands -- --nocapture`, `cargo test generation::tests::generated_plan_rejects_multiple_cargo_test_filters -- --nocapture`, and `cargo test generation::tests::generated_plan_rejects_bin_only_cargo_lib_verification -- --nocapture` currently fail; `cargo test spec_command::tests::auto_spec_plan_validation_rejects_malformed_grep_verification_commands -- --nocapture` passes, proving grep lint still works for `auto spec`.
    Source of truth: `src/verification_lint.rs` owns runnable proof-command linting; `src/task_parser.rs` owns shared execution-row validation; `src/generation.rs` and `src/spec_command.rs` own stricter generated/spec plan gates.
    Runtime owner: `src/verification_lint.rs`
    UI consumers: `auto spec` stdout, `auto gen` stdout, generated `IMPLEMENTATION_PLAN.md`, `auto super` deterministic gate, `auto parallel` worker prompts
    Generated artifacts: `gen-*/IMPLEMENTATION_PLAN.md`, `gen-*/specs/*.md`
    Fixture boundary: Validator fixtures must stay in Rust tests or temp dirs; production validators must not accept copied sample proof commands, old snapshots, zero-test filters, or fixture specs as current queue truth.
    Retired surfaces: Loosened `cargo test --lib` acceptance for this bin-only crate; multi-filter cargo-test proof as one generated-plan verification command; package-wide cargo-test verification with no concrete filter; malformed non-recursive directory `grep` diagnostics that do not name the malformed proof.
    Owns: src/verification_lint.rs, src/task_parser.rs, src/spec_command.rs, src/generation.rs
    Integration touchpoints: `src/super_command.rs::verify_parallel_ready_plan`, `src/completion_artifacts.rs::verification_plan`, `scripts/run-task-verification.sh`
    Scope boundary: Restore command-shape policy only; do not redesign receipt JSON, alter completion evidence freshness, or broaden generated-plan task schemas.
    Acceptance criteria: Shared lint rejects multi-filter cargo-test commands, stale `cargo test --lib` proof in this bin-only crate, package-wide cargo-test rows without a concrete filter, and malformed directory `grep`; existing quoted-command receipt extraction and zero-test receipt rejection remain green.
    Verification: Run current red proof first with `cargo test spec_command::tests::auto_spec_plan_validation_rejects_multi_filter_verification_commands -- --nocapture`; after changes run `cargo test spec_command::tests::auto_spec_plan_validation_rejects_multi_filter_verification_commands -- --nocapture`, `cargo test generation::tests::generated_plan_rejects_multiple_cargo_test_filters -- --nocapture`, `cargo test generation::tests::generated_plan_rejects_bin_only_cargo_lib_verification -- --nocapture`, `cargo test generation::tests::generated_plan_rejects_malformed_directory_grep_verification -- --nocapture`, and `cargo test completion_artifacts::tests::verification_plan_extracts_backtick_commands_without_bare_flags -- --nocapture`.
    Required tests: `spec_command::tests::auto_spec_plan_validation_rejects_multi_filter_verification_commands`, `generation::tests::generated_plan_rejects_multiple_cargo_test_filters`, `generation::tests::generated_plan_rejects_bin_only_cargo_lib_verification`, `generation::tests::generated_plan_rejects_malformed_directory_grep_verification`, `completion_artifacts::tests::verification_plan_extracts_backtick_commands_without_bare_flags`
    Contract generation: none -- no generated contract
    Cross-surface tests: `cargo test spec_command::tests::auto_spec_plan_validation_rejects_multi_filter_verification_commands -- --nocapture` proves `auto spec` plan output consumes the same shared lint used by generated plans.
    Review/closeout: Reviewer checks `rg -n "multi-filter cargo test|cargo test --lib|malformed grep verification|verify_cargo_test_command" src/verification_lint.rs src/spec_command.rs src/generation.rs` and confirms the previously red validator tests now fail closed for the original weak commands.
    Completion artifacts: none
    Dependencies: `P0-001`
    Estimated scope: S
    Completion signal: Shared proof-command lint is strict again across `auto spec`, generated plans, and receipt extraction safety tests.

- [x] `P0-003` Checkpoint the local validator baseline

    Spec: `specs/220526-generated-plan-proof-validators.md`
    Why now: This checkpoint follows two high-risk baseline fixes so the next workers do not widen scope on top of an ambiguous formatter, command-surface, or proof-lint state; it outranks new feature work until the execution queue can prove itself.
    Codebase evidence: `cargo fmt --check`, command-surface tests, and three proof-lint rejection tests were red in the live checkout before this plan; `src/generation.rs::verify_generated_implementation_plan` and `src/spec_command.rs::verify_plan_output` are both downstream of the shared lint.
    Source of truth: `src/generation.rs::verify_generated_implementation_plan`, `src/spec_command.rs::verify_plan_output`, and `src/verification_lint.rs::verify_commands_are_runnable`
    Runtime owner: `src/generation.rs`
    UI consumers: generated `IMPLEMENTATION_PLAN.md`, `auto gen` stdout, `auto spec` stdout, `auto super` deterministic gate
    Generated artifacts: `gen-*/IMPLEMENTATION_PLAN.md`
    Fixture boundary: Checkpoint proof must run against local tests and live source; production validators cannot import fixture plans or sample generated rows.
    Retired surfaces: none
    Owns: src/generation.rs, src/spec_command.rs, src/verification_lint.rs
    Integration touchpoints: `src/task_parser.rs`, `src/super_command.rs`
    Scope boundary: Verification-only checkpoint; do not add new validation policy beyond proving `P0-001` and `P0-002`.
    Acceptance criteria: The targeted formatter, command-surface, spec-plan, generated-plan, and receipt-extraction tests all pass locally with no zero-test filters.
    Verification: Run `cargo fmt --check`, `cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture`, `cargo test spec_command::tests::auto_spec_plan_validation_rejects_multi_filter_verification_commands -- --nocapture`, `cargo test generation::tests::generated_plan_rejects_multiple_cargo_test_filters -- --nocapture`, and `cargo test completion_artifacts::tests::verification_plan_extracts_backtick_commands_without_bare_flags -- --nocapture`.
    Required tests: `tests::top_level_command_surface_matches_live_enum`, `spec_command::tests::auto_spec_plan_validation_rejects_multi_filter_verification_commands`, `generation::tests::generated_plan_rejects_multiple_cargo_test_filters`, `completion_artifacts::tests::verification_plan_extracts_backtick_commands_without_bare_flags`
    Contract generation: none -- no generated contract
    Cross-surface tests: `cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture` plus `cargo run -- --help` read back the runtime-to-CLI command surface.
    Review/closeout: Reviewer confirms the checkpoint commands include concrete filters and would fail if formatter drift, public command drift, or weak generated proof commands returned.
    Completion artifacts: none
    Dependencies: `P0-001`, `P0-002`
    Estimated scope: XS
    Completion signal: Baseline validator cluster is green enough for snapshot and receipt workers to proceed.

- [x] `P0-004` Make default `auto super` snapshot-first and promotion-gated

    Spec: `specs/220526-super-snapshot-first-runtime.md`
    Why now: This is the next P0 because default `auto super` currently mutates root queue truth before operator review; it outranks release polish because operator sovereignty and root ledger safety are prerequisites for any model-backed macro run.
    Codebase evidence: `src/super_command.rs:207-220` calls `generation::run_gen` with `snapshot_only: false` and `sync_only: false`; `src/generation.rs::finalize_verified_generation_outputs` already supports snapshot-only state saving; `cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs -- --nocapture` passes; `src/super_command.rs:311` currently verifies root `IMPLEMENTATION_PLAN.md` before parallel.
    Source of truth: `src/generation.rs` owns generation mode semantics; `src/super_command.rs` owns super orchestration and must consume those modes without redefining them.
    Runtime owner: `src/super_command.rs`
    UI consumers: `auto super` stdout, `auto super --dry-run` stdout, `.auto/super/<run-id>/manifest.json`, `.auto/super/<run-id>/DETERMINISTIC-GATE.json`, README lifecycle prose
    Generated artifacts: `gen-*/corpus/**`, `gen-*/specs/*.md`, `gen-*/IMPLEMENTATION_PLAN.md`, `.auto/state.json`, `.auto/super/<run-id>/manifest.json`, `.auto/super/<run-id>/DETERMINISTIC-GATE.json`
    Fixture boundary: Super tests may use temp repos and synthetic generated snapshots; production super must not treat fixture snapshots or old `gen-*` dirs as active root truth without explicit operator promotion.
    Retired surfaces: Default `auto super` root sync before snapshot review; execution-gate prompt text that permits editing root specs or root `IMPLEMENTATION_PLAN.md` while default super is in snapshot mode; default parallel launch from unpromoted generated rows.
    Owns: src/super_command.rs, src/generation.rs, README.md
    Integration touchpoints: `auto gen --sync-only --output-dir <gen-dir>`, `src/state.rs`, `src/generation.rs::run_gen`, `src/super_command.rs::verify_parallel_ready_plan`, `src/parallel_command.rs`
    Scope boundary: Keep explicit promotion on the existing `auto gen --sync-only --output-dir <gen-dir>` path; do not add a new super promotion flag unless the worker proves it is required for this slice.
    Acceptance criteria: Default `auto super` invokes generation snapshot-only; root `specs/*.md` and root `IMPLEMENTATION_PLAN.md` are unchanged by the gen stage; super stdout and manifest distinguish snapshot output from root queue truth; deterministic gate reads the generated plan while in snapshot mode; default parallel execution is skipped or blocked with a clear promotion command until the snapshot is explicitly promoted.
    Verification: Run `cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs -- --nocapture`, add and run `cargo test super_command::tests::super_default_generation_is_snapshot_only -- --nocapture`, add and run `cargo test super_command::tests::super_deterministic_gate_reads_generated_snapshot_plan -- --nocapture`, add and run `cargo test super_command::tests::super_skips_parallel_until_snapshot_is_promoted -- --nocapture`, and run `cargo run -- super --dry-run --no-execute "snapshot proof"`.
    Required tests: `generation::tests::snapshot_only_generation_does_not_sync_root_outputs`, `super_command::tests::super_default_generation_is_snapshot_only`, `super_command::tests::super_deterministic_gate_reads_generated_snapshot_plan`, `super_command::tests::super_skips_parallel_until_snapshot_is_promoted`
    Contract generation: none -- no generated contract
    Cross-surface tests: `cargo run -- super --dry-run --no-execute "snapshot proof"` must show snapshot staging and explicit promotion without implying root queue mutation.
    Review/closeout: Reviewer checks `rg -n "snapshot_only: true|sync-only|snapshot|root plan:   unchanged|DETERMINISTIC-GATE" src/super_command.rs README.md` and confirms no default path still launches `auto parallel` from an unpromoted generated plan.
    Completion artifacts: none
    Dependencies: `P0-003`
    Estimated scope: M
    Completion signal: Default super creates reviewable snapshot output and cannot silently replace or execute root queue truth.

- [x] `P1-005` Use exact terminal verdict parsing in the super execution gate

    Spec: `specs/220526-release-gates-and-verdict-readiness.md`
    Why now: This is paired with snapshot-first super because a permissive model gate can approve execution from ambiguous report prose; it outranks broader release reports because the shared exact parser already exists and is a narrow fail-closed fix.
    Codebase evidence: `src/verdict.rs::exact_terminal_verdict` and `terminal_verdict_is` exist and `cargo test verdict -- --nocapture` passes; `src/super_command.rs:739` still accepts any line equal to `Verdict: GO`.
    Source of truth: `src/verdict.rs` owns model terminal verdict parsing.
    Runtime owner: `src/super_command.rs`
    UI consumers: `.auto/super/<run-id>/EXECUTION-GATE.md`, `auto super` stdout
    Generated artifacts: `.auto/super/<run-id>/EXECUTION-GATE.md`
    Fixture boundary: Verdict tests may write synthetic gate reports in temp dirs; production gate parsing must consume the actual model-authored `EXECUTION-GATE.md`.
    Retired surfaces: Any-line `Verdict: GO` scans in `src/super_command.rs`
    Owns: src/super_command.rs, src/verdict.rs
    Integration touchpoints: `src/super_command.rs::run_super_execution_gate`, `.auto/super/<run-id>/EXECUTION-GATE.md`
    Scope boundary: Replace only super execution-gate verdict acceptance; do not redesign deterministic gate JSON, ship gate blockers, or model prompts beyond required wording for a single terminal verdict.
    Acceptance criteria: Super execution gate accepts exactly one allowed terminal verdict line; mixed `Verdict: GO` and `Verdict: NO-GO`, duplicate verdicts, missing verdicts, and invalid verdict lines fail closed with actionable errors.
    Verification: Run `cargo test verdict -- --nocapture`, add and run `cargo test super_command::tests::super_execution_gate_rejects_mixed_verdicts -- --nocapture`, add and run `cargo test super_command::tests::super_execution_gate_rejects_duplicate_verdicts -- --nocapture`, and add and run `cargo test super_command::tests::super_execution_gate_accepts_single_go_verdict -- --nocapture`.
    Required tests: `verdict::tests::exact_terminal_verdict_rejects_mixed_verdicts`, `verdict::tests::terminal_verdict_is_requires_exact_single_line`, `super_command::tests::super_execution_gate_rejects_mixed_verdicts`, `super_command::tests::super_execution_gate_rejects_duplicate_verdicts`, `super_command::tests::super_execution_gate_accepts_single_go_verdict`
    Contract generation: none -- no generated contract
    Cross-surface tests: `super_command::tests::super_execution_gate_accepts_single_go_verdict` must prove the `EXECUTION-GATE.md` report format maps to runtime gate acceptance.
    Review/closeout: Reviewer checks `rg -n "exact_terminal_verdict|Verdict: GO|lines\\(\\).*any" src/super_command.rs src/verdict.rs` and confirms super no longer has an any-line verdict scan.
    Completion artifacts: none
    Dependencies: `P0-004`
    Estimated scope: S
    Completion signal: Super model gate uses the shared verdict parser and fails closed on ambiguous gate reports.

- [x] `P0-006` Checkpoint snapshot and gate readiness

    Spec: `specs/220526-super-snapshot-first-runtime.md`
    Why now: This checkpoint follows the risky super behavior change so receipt, lane, and quota workers do not proceed while the macro command might still mutate root queue truth or approve execution ambiguously.
    Codebase evidence: The snapshot-only generation unit test is already green, but `src/super_command.rs` currently root-syncs generation and scans any `Verdict: GO`; both surfaces must be proven together before widening scope.
    Source of truth: `src/super_command.rs` orchestration and `src/generation.rs` generation-mode semantics
    Runtime owner: `src/super_command.rs`
    UI consumers: `auto super --dry-run` stdout, `.auto/super/<run-id>/manifest.json`, `.auto/super/<run-id>/EXECUTION-GATE.md`, `.auto/super/<run-id>/DETERMINISTIC-GATE.json`
    Generated artifacts: `.auto/super/<run-id>/manifest.json`, `.auto/super/<run-id>/EXECUTION-GATE.md`, `.auto/super/<run-id>/DETERMINISTIC-GATE.json`, `gen-*/IMPLEMENTATION_PLAN.md`
    Fixture boundary: Checkpoint proof may use temp super roots; production super must read live generated output and root queue truth only through runtime owners.
    Retired surfaces: none
    Owns: src/super_command.rs, src/generation.rs
    Integration touchpoints: `auto gen --sync-only --output-dir <gen-dir>`, `src/state.rs`, `src/verdict.rs`
    Scope boundary: Verification-only checkpoint; do not add new promotion UX or release-gate behavior.
    Acceptance criteria: Snapshot-first, promotion-gated execution and exact super verdict parsing are both covered by targeted tests and dry-run readback.
    Verification: Run `cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs -- --nocapture`, `cargo test super_command::tests::super_default_generation_is_snapshot_only -- --nocapture`, `cargo test super_command::tests::super_skips_parallel_until_snapshot_is_promoted -- --nocapture`, `cargo test super_command::tests::super_execution_gate_rejects_mixed_verdicts -- --nocapture`, and `cargo run -- super --dry-run --no-execute "snapshot proof"`.
    Required tests: `generation::tests::snapshot_only_generation_does_not_sync_root_outputs`, `super_command::tests::super_default_generation_is_snapshot_only`, `super_command::tests::super_skips_parallel_until_snapshot_is_promoted`, `super_command::tests::super_execution_gate_rejects_mixed_verdicts`
    Contract generation: none -- no generated contract
    Cross-surface tests: `cargo run -- super --dry-run --no-execute "snapshot proof"` must match the tested snapshot/promotion contract in terminal output.
    Review/closeout: Reviewer confirms the checkpoint would fail if default `auto super` reintroduced root sync, unpromoted parallel dispatch, or any-line `Verdict: GO` acceptance.
    Completion artifacts: none
    Dependencies: `P0-004`, `P1-005`
    Estimated scope: XS
    Completion signal: Super snapshot sovereignty and gate parsing are green as a cluster.

- [x] `P1-007` Reject stale release JSON receipts in the ship gate

    Spec: `specs/220526-receipt-and-lane-evidence-contract.md`
    Why now: This is the highest-leverage receipt slice because release readiness is currently red and can accept stale JSON proof; it outranks lane metadata cleanup because operators need ship blockers to mean the same thing as completion evidence before any release decision.
    Codebase evidence: `cargo test ship_command::tests::ship_gate_uses_shared_receipt_inspector -- --nocapture` currently fails because no `commit mismatch` blocker is produced; `cargo test ship_command::tests::ship_gate_rejects_stale_completion_receipt -- --nocapture` currently fails because `report.is_blocked()` is false; `src/ship_command.rs::load_verification_receipts` reads JSON receipts from `.auto/symphony/verification-receipts`; `docs/verification-receipt-schema.md` says JSON receipts are staging/compatibility and durable proof travels in commit footers.
    Source of truth: `src/completion_artifacts.rs` owns receipt freshness semantics; `src/ship_command.rs::evaluate_ship_gate` owns release-readiness blockers.
    Runtime owner: `src/ship_command.rs`
    UI consumers: `auto ship` stdout, `SHIP.md`, `.auto/ship/**`
    Generated artifacts: `.auto/symphony/verification-receipts/<TASK>.json`, `Auto-Verification-Receipt-*` commit footers, `SHIP.md`
    Fixture boundary: Receipt tests may write temp JSON and synthetic commits; production release gates must not trust hand-edited or stale JSON as durable proof when footer freshness or current-tree metadata is missing.
    Retired surfaces: Ancestor JSON receipts satisfying release-required commands after branch, plan, or dirty-state drift; release-gate code paths that bypass shared receipt freshness.
    Owns: src/completion_artifacts.rs, src/ship_command.rs, docs/verification-receipt-schema.md
    Integration touchpoints: `scripts/run-task-verification.sh`, `scripts/verification_receipt.py`, `src/parallel_command.rs::commit_task_closeout`, `src/loop_command.rs::reconcile_loop_task_completion_evidence`
    Scope boundary: Fix ship-gate stale receipt acceptance and document the JSON-versus-footer boundary; do not redesign lane receipt propagation or assignment metadata in this slice.
    Acceptance criteria: Stale JSON receipts block ship with shared freshness diagnostics; failed or zero-test receipts still block ship; commit-footer receipts remain the durable preferred proof; existing loop demotion behavior remains green.
    Verification: Run current red proof first with `cargo test ship_command::tests::ship_gate_uses_shared_receipt_inspector -- --nocapture`; after changes run `cargo test ship_command::tests::ship_gate_uses_shared_receipt_inspector -- --nocapture`, `cargo test ship_command::tests::ship_gate_rejects_stale_completion_receipt -- --nocapture`, `cargo test completion_artifacts::tests::inspect_task_completion_evidence_accepts_commit_footer_receipts -- --nocapture`, `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_non_ancestor_json_receipt -- --nocapture`, and `cargo test loop_command::tests::loop_marks_task_partial_when_completion_evidence_missing -- --nocapture`.
    Required tests: `ship_command::tests::ship_gate_uses_shared_receipt_inspector`, `ship_command::tests::ship_gate_rejects_stale_completion_receipt`, `completion_artifacts::tests::inspect_task_completion_evidence_accepts_commit_footer_receipts`, `completion_artifacts::tests::inspect_task_completion_evidence_rejects_non_ancestor_json_receipt`, `loop_command::tests::loop_marks_task_partial_when_completion_evidence_missing`
    Contract generation: none -- no generated contract
    Cross-surface tests: `cargo test ship_command::tests::ship_gate_rejects_stale_completion_receipt -- --nocapture` must prove stale runtime receipt facts appear as operator-visible ship blockers.
    Review/closeout: Reviewer checks `rg -n "shared_receipt_freshness_problem|Auto-Verification-Receipt|verification-receipts|stale validation receipt|commit mismatch" src/completion_artifacts.rs src/ship_command.rs docs/verification-receipt-schema.md` and confirms stale JSON cannot satisfy release-required proof.
    Completion artifacts: none
    Dependencies: `P0-006`
    Estimated scope: S
    Completion signal: The two currently failing ship-gate stale receipt tests are green and release blockers match shared receipt freshness semantics.

- [x] `P1-008` Resolve operator and evidence lane routing

    Spec: `specs/220526-receipt-and-lane-evidence-contract.md`
    Why now: This is the next operator-visible evidence slice because `Lane kind:` is parsed but routing is contradictory; it outranks lane assignment metadata because a user first needs status and dispatch to agree on whether work is autonomous, evidence-only, or operator-owned.
    Codebase evidence: `src/task_parser.rs::LaneKind` parses `operator` and `evidence`; `src/parallel_command.rs::is_operator_task` currently returns false for all tasks; `cargo test parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks -- --nocapture` currently fails because the status verdict does not contain `code lanes ready: CODE-001`; `src/parallel_command.rs::write_operator_actions_for_ready_tasks` already exists.
    Source of truth: `src/task_parser.rs::LaneKind` owns row metadata; `src/parallel_command.rs` owns dispatch/status routing.
    Runtime owner: `src/parallel_command.rs`
    UI consumers: `auto parallel status` stdout, `.auto/parallel/operator-actions.md`, lane assignment logs
    Generated artifacts: `.auto/parallel/operator-actions.md`, `.auto/parallel/**/assignment.json`
    Fixture boundary: Lane routing tests may use synthetic plan rows; production routing must read live `IMPLEMENTATION_PLAN.md` rows and must not infer operator work from sample wording or fixtures.
    Retired surfaces: `Lane kind: operator` rows silently treated as code lanes; status text that disagrees with dispatch routing; stale docs implying operator queue semantics that runtime ignores.
    Owns: src/task_parser.rs, src/parallel_command.rs, docs/decisions/parallel-host-reconciliation-policy.md
    Integration touchpoints: `src/loop_command.rs::parse_tasks`, `auto parallel status`, `.auto/parallel/operator-actions.md`
    Scope boundary: Resolve routing and status semantics only; do not add assignment hashes, worker command metadata, or receipt footer redesign.
    Acceptance criteria: `Lane kind: operator` tasks route to operator actions and do not consume code workers; `Lane kind: evidence` tasks render as evidence queue; normal code rows remain dispatchable; mainnet/autonomous gates remain code tasks unless explicitly tagged operator.
    Verification: Run current red proof first with `cargo test parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks -- --nocapture`; after changes run `cargo test parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks -- --nocapture`, `cargo test parallel_command::tests::inferred_mainnet_autonomous_gate_remains_dispatchable_code -- --nocapture`, and `cargo test parallel_command::tests::operator_actions_file_records_full_task_contract -- --nocapture`.
    Required tests: `parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks`, `parallel_command::tests::inferred_mainnet_autonomous_gate_remains_dispatchable_code`, `parallel_command::tests::operator_actions_file_records_full_task_contract`
    Contract generation: none -- no generated contract
    Cross-surface tests: `parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks` must prove parsed runtime lane kind appears consistently in status output.
    Review/closeout: Reviewer checks `rg -n "Lane kind|is_operator_task|operator-actions|evidence queue|code lanes ready" src/task_parser.rs src/parallel_command.rs docs/decisions/parallel-host-reconciliation-policy.md` and confirms status, dispatch, and docs use one routing contract.
    Completion artifacts: none
    Dependencies: `P1-007`
    Estimated scope: S
    Completion signal: Operator, evidence, and code rows route and render according to `LaneKind`.

- [ ] `P1-009` Checkpoint evidence and lane contracts

    Spec: `specs/220526-receipt-and-lane-evidence-contract.md`
    Why now: This checkpoint comes after receipt freshness and lane routing because both feed release and parallel status decisions; it prevents quota/security work from hiding unresolved proof semantics.
    Codebase evidence: Ship receipt tests and lane-kind routing tests are currently red in the live checkout; `docs/verification-receipt-schema.md` and `docs/decisions/parallel-host-reconciliation-policy.md` already state the intended durable-proof and operator-queue boundaries.
    Source of truth: `src/completion_artifacts.rs`, `src/ship_command.rs`, `src/task_parser.rs::LaneKind`, and `src/parallel_command.rs`
    Runtime owner: `src/completion_artifacts.rs`
    UI consumers: `auto ship` stdout, `auto parallel status` stdout, `.auto/parallel/operator-actions.md`, `SHIP.md`
    Generated artifacts: `.auto/symphony/verification-receipts/<TASK>.json`, `Auto-Verification-Receipt-*` commit footers, `.auto/parallel/operator-actions.md`
    Fixture boundary: Checkpoint tests may use temp receipts and synthetic task rows; production proof cannot rely on fixture receipt JSON or sample plans.
    Retired surfaces: none
    Owns: src/completion_artifacts.rs, src/ship_command.rs, src/task_parser.rs, src/parallel_command.rs
    Integration touchpoints: `scripts/run-task-verification.sh`, `.auto/parallel/**`, `.auto/symphony/verification-receipts/**`
    Scope boundary: Verification-only checkpoint; do not change lane metadata breadth, host-owned queue constants, or loop footer policy.
    Acceptance criteria: Stale release receipts block ship, footer receipts still satisfy completion evidence, zero-test receipts are rejected, and lane-kind routing is green.
    Verification: Run `cargo test ship_command::tests::ship_gate_uses_shared_receipt_inspector -- --nocapture`, `cargo test ship_command::tests::ship_gate_rejects_stale_completion_receipt -- --nocapture`, `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests -- --nocapture`, `cargo test completion_artifacts::tests::inspect_task_completion_evidence_accepts_quoted_command_receipts_with_argv -- --nocapture`, and `cargo test parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks -- --nocapture`.
    Required tests: `ship_command::tests::ship_gate_uses_shared_receipt_inspector`, `ship_command::tests::ship_gate_rejects_stale_completion_receipt`, `completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests`, `completion_artifacts::tests::inspect_task_completion_evidence_accepts_quoted_command_receipts_with_argv`, `parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks`
    Contract generation: none -- no generated contract
    Cross-surface tests: Ship-gate and parallel-status tests must prove runtime evidence/lane facts are rendered to operator surfaces.
    Review/closeout: Reviewer confirms the checkpoint would fail if stale JSON receipts were accepted again or `Lane kind: operator` stopped rendering as operator work.
    Completion artifacts: none
    Dependencies: `P1-007`, `P1-008`
    Estimated scope: XS
    Completion signal: Evidence and lane contracts are consistent enough for quota/security hardening to proceed.

- [ ] `P1-010` Reject unsafe persisted quota account names on load

    Spec: `specs/220526-quota-persistence-and-credential-hardening.md`
    Why now: This is the first quota P1 because unsafe persisted account identity can reach selection, cooldown, profile, or credential mutation before any new write happens; it outranks atomic-save work because invalid loaded state must fail closed before it is used.
    Codebase evidence: `src/quota_config.rs::load` parses TOML without calling `validate_account_names`; `src/quota_state.rs::load` parses JSON without validating account map keys; mutation methods already call `validate_account_name`; `cargo test quota_config::tests::save_writes_owner_only -- --nocapture` and `cargo test quota_state::tests::save_writes_owner_only -- --nocapture` pass, proving save permissions are partially covered.
    Source of truth: `src/quota_config.rs::QuotaConfig` owns configured account identity; `src/quota_state.rs::QuotaState` owns quota account state.
    Runtime owner: `src/quota_config.rs`
    UI consumers: `auto quota status` stdout, `auto quota select` stderr, quota-router stderr
    Generated artifacts: platform config `quota-router/config.toml`, platform config `quota-router/state.json`
    Fixture boundary: Quota tests may use temp config homes and synthetic config/state files; production quota commands must read the live platform config and reject unsafe persisted names rather than normalizing or importing fixtures.
    Retired surfaces: Persisted unsafe account names accepted after TOML/JSON parse; cooldown refresh or selection paths that operate on unvalidated persisted keys.
    Owns: src/quota_config.rs, src/quota_state.rs
    Integration touchpoints: `src/quota_accounts.rs`, `src/quota_exec.rs`, `src/quota_status.rs`
    Scope boundary: Validate persisted account names on load only; do not change write atomicity, credential copy behavior, or provider prompt transport.
    Acceptance criteria: Loading config or state with unsafe account names fails with an actionable error before any selection, cooldown refresh, profile path, or credential mutation; valid persisted config/state still load.
    Verification: Add and run `cargo test quota_config::tests::load_rejects_unsafe_account_names -- --nocapture`, add and run `cargo test quota_state::tests::load_rejects_unsafe_account_names -- --nocapture`, and run `cargo test quota_config::tests::profile_dir_rejects_unsafe_names -- --nocapture`.
    Required tests: `quota_config::tests::load_rejects_unsafe_account_names`, `quota_state::tests::load_rejects_unsafe_account_names`, `quota_config::tests::profile_dir_rejects_unsafe_names`
    Contract generation: none -- no generated contract
    Cross-surface tests: `quota_config::tests::load_rejects_unsafe_account_names` must prove runtime load rejection produces the sanitized error consumed by quota CLI/status surfaces.
    Review/closeout: Reviewer checks `rg -n "validate_account_names|load_rejects_unsafe_account_names|validate_account_name" src/quota_config.rs src/quota_state.rs` and confirms persisted invalid keys cannot reach runtime mutation paths.
    Completion artifacts: none
    Dependencies: `P1-009`
    Estimated scope: S
    Completion signal: Quota config/state loads fail closed on unsafe persisted account names.

- [ ] `P1-011` Make quota config and state writes atomic owner-only

    Spec: `specs/220526-quota-persistence-and-credential-hardening.md`
    Why now: This follows load validation because account state is now trusted enough to persist; it outranks credential refresh hardening because config/state truncation can corrupt the router's canonical account truth during ordinary quota operations.
    Codebase evidence: `src/util.rs::write_0o600_if_unix` opens targets with truncate semantics; `src/util.rs::atomic_write` exists but does not set owner-only mode; `src/quota_config.rs::save` and `src/quota_state.rs::save` use `write_0o600_if_unix`.
    Source of truth: `src/util.rs` owns filesystem write helpers; `src/quota_config.rs` and `src/quota_state.rs` own quota persistence.
    Runtime owner: `src/util.rs`
    UI consumers: `auto quota status` stdout, `auto quota accounts add/list/remove/capture` stdout/stderr, quota-router stderr
    Generated artifacts: platform config `quota-router/config.toml`, platform config `quota-router/state.json`
    Fixture boundary: Persistence tests may use temp config homes and symlink fixtures; production quota saves must never import fixture files or follow symlinked state/config destinations.
    Retired surfaces: Direct truncate writes for quota config/state; owner-only writes that are not atomic; symlinked quota config/state destinations accepted as save targets.
    Owns: src/util.rs, src/quota_config.rs, src/quota_state.rs
    Integration touchpoints: `src/quota_accounts.rs`, `src/quota_exec.rs`, `src/quota_status.rs`
    Scope boundary: Add or adapt a shared atomic owner-only write helper and use it for quota config/state only; do not migrate unrelated report writes in this slice.
    Acceptance criteria: Quota config and state saves write via a complete temp replacement, leave final files `0o600` on Unix, clean temp files on failure, and reject symlinked destinations; existing owner-only save tests remain green.
    Verification: Add and run `cargo test util::tests::atomic_write_0o600_if_unix_preserves_owner_only_mode -- --nocapture`, add and run `cargo test quota_config::tests::save_is_atomic_owner_only_and_rejects_destination_symlink -- --nocapture`, add and run `cargo test quota_state::tests::save_is_atomic_owner_only_and_rejects_destination_symlink -- --nocapture`, and run `cargo test util::tests::atomic_write_removes_temp_file_after_rename_failure -- --nocapture`.
    Required tests: `util::tests::atomic_write_0o600_if_unix_preserves_owner_only_mode`, `quota_config::tests::save_is_atomic_owner_only_and_rejects_destination_symlink`, `quota_state::tests::save_is_atomic_owner_only_and_rejects_destination_symlink`, `util::tests::atomic_write_removes_temp_file_after_rename_failure`
    Contract generation: none -- no generated contract
    Cross-surface tests: none -- no UI/runtime boundary
    Review/closeout: Reviewer checks `rg -n "atomic_write_0o600|write_0o600_if_unix|QuotaConfig::save|QuotaState::save|symlink" src/util.rs src/quota_config.rs src/quota_state.rs` and confirms quota persistence no longer truncates canonical files directly.
    Completion artifacts: none
    Dependencies: `P1-010`
    Estimated scope: S
    Completion signal: Quota config/state persistence is atomic, owner-only, and symlink-refusing.

- [ ] `P1-012` Harden Claude credential refresh copy

    Spec: `specs/220526-quota-persistence-and-credential-hardening.md`
    Why now: This closes the remaining credential movement gap after canonical config/state hardening; it outranks quota UX polish because raw credential copy can follow unsafe paths and loosen secret-file permissions.
    Codebase evidence: `src/quota_exec.rs::sync_newer_claude_credentials` still uses raw `fs::copy`; `src/quota_exec.rs` already has symlink-refusing credential copy helpers for other swap paths; `cargo test quota_exec::tests::sync_newer_claude_credentials_updates_stale_profile -- --nocapture` currently passes while exercising the raw-copy refresh path.
    Source of truth: `src/quota_exec.rs` owns credential swap, restore, and profile refresh behavior.
    Runtime owner: `src/quota_exec.rs`
    UI consumers: quota-router stderr, `auto quota select` stderr, backend command logs
    Generated artifacts: platform config `quota-router/profiles/<provider>-<name>/**`, platform config `quota-router/backup/**`, `.auto/quota-recovery/**`
    Fixture boundary: Tests may create temp Claude credential files and symlinks; production refresh must read live Claude credential paths and must not follow symlinked source or destination credentials.
    Retired surfaces: Raw `fs::copy` in Claude credential refresh; refreshed profile credentials with non-owner-only mode; symlinked active or profile Claude credentials accepted during refresh.
    Owns: src/quota_exec.rs
    Integration touchpoints: `src/quota_config.rs::copy_auth_to_profile`, `src/quota_exec.rs::swap_credentials`, `src/util.rs::write_0o600_if_unix`
    Scope boundary: Replace only the newer-Claude-credential refresh copy path; do not change OAuth parsing, account selection, Codex isolated-home behavior, or PI/Kimi prompt transport.
    Acceptance criteria: Refreshing stale Claude profile credentials refuses symlinked active/profile paths, writes owner-only refreshed credentials, preserves newer profile credentials, and keeps existing swap/restore behavior green.
    Verification: Run `cargo test quota_exec::tests::sync_newer_claude_credentials_updates_stale_profile -- --nocapture`, add and run `cargo test quota_exec::tests::sync_newer_claude_credentials_rejects_symlinked_profile_credentials -- --nocapture`, add and run `cargo test quota_exec::tests::sync_newer_claude_credentials_rejects_symlinked_active_credentials -- --nocapture`, add and run `cargo test quota_exec::tests::sync_newer_claude_credentials_preserves_owner_only_mode -- --nocapture`, and run `cargo test quota_exec::tests::sync_newer_claude_credentials_keeps_newer_profile -- --nocapture`.
    Required tests: `quota_exec::tests::sync_newer_claude_credentials_updates_stale_profile`, `quota_exec::tests::sync_newer_claude_credentials_rejects_symlinked_profile_credentials`, `quota_exec::tests::sync_newer_claude_credentials_rejects_symlinked_active_credentials`, `quota_exec::tests::sync_newer_claude_credentials_preserves_owner_only_mode`, `quota_exec::tests::sync_newer_claude_credentials_keeps_newer_profile`
    Contract generation: none -- no generated contract
    Cross-surface tests: none -- no UI/runtime boundary
    Review/closeout: Reviewer checks `rg -n "sync_newer_claude_credentials|fs::copy|symlinked credential path|0o600" src/quota_exec.rs` and confirms the only Claude refresh copy path uses the hardened helper.
    Completion artifacts: none
    Dependencies: `P1-011`
    Estimated scope: S
    Completion signal: Claude profile refresh no longer uses raw copy and fails closed on symlinked credential paths.

- [ ] `P1-013` Checkpoint quota credential hardening

    Spec: `specs/220526-quota-persistence-and-credential-hardening.md`
    Why now: This checkpoint follows three credential/state safety changes so later quota routing or provider-transport work does not hide a security regression in persistence or secret movement.
    Codebase evidence: Load validation, atomic owner-only saves, and Claude refresh copy are independent hardening surfaces across `src/quota_config.rs`, `src/quota_state.rs`, `src/util.rs`, and `src/quota_exec.rs`.
    Source of truth: `src/quota_config.rs`, `src/quota_state.rs`, `src/quota_exec.rs`, and `src/util.rs`
    Runtime owner: `src/quota_exec.rs`
    UI consumers: `auto quota status` stdout, quota-router stderr
    Generated artifacts: platform config `quota-router/config.toml`, platform config `quota-router/state.json`, platform config `quota-router/profiles/**`, platform config `quota-router/backup/**`, `.auto/quota-recovery/**`
    Fixture boundary: Checkpoint proof may use temp config homes; production quota code must never import fixture credentials or sample account data.
    Retired surfaces: none
    Owns: src/quota_config.rs, src/quota_state.rs, src/quota_exec.rs, src/util.rs
    Integration touchpoints: `src/quota_accounts.rs`, `src/quota_status.rs`, `src/quota_exec.rs::run_with_quota`
    Scope boundary: Verification-only checkpoint; do not expand into provider prompt transport, new account UX, or broad quota refactors.
    Acceptance criteria: Persisted unsafe names are rejected, config/state saves are atomic owner-only, symlinked save/refresh paths fail closed, and existing quota status still renders sanitized state.
    Verification: Run `cargo test quota_config::tests::load_rejects_unsafe_account_names -- --nocapture`, `cargo test quota_state::tests::load_rejects_unsafe_account_names -- --nocapture`, `cargo test quota_config::tests::save_is_atomic_owner_only_and_rejects_destination_symlink -- --nocapture`, `cargo test quota_state::tests::save_is_atomic_owner_only_and_rejects_destination_symlink -- --nocapture`, and `cargo test quota_exec::tests::sync_newer_claude_credentials_rejects_symlinked_profile_credentials -- --nocapture`.
    Required tests: `quota_config::tests::load_rejects_unsafe_account_names`, `quota_state::tests::load_rejects_unsafe_account_names`, `quota_config::tests::save_is_atomic_owner_only_and_rejects_destination_symlink`, `quota_state::tests::save_is_atomic_owner_only_and_rejects_destination_symlink`, `quota_exec::tests::sync_newer_claude_credentials_rejects_symlinked_profile_credentials`
    Contract generation: none -- no generated contract
    Cross-surface tests: `cargo run -- quota status` may be used as a manual readback after tests to confirm status renders sanitized account state without exposing credentials.
    Review/closeout: Reviewer confirms the checkpoint would fail if unsafe persisted names, direct truncating quota saves, or raw Claude credential refresh copies returned.
    Completion artifacts: none
    Dependencies: `P1-010`, `P1-011`, `P1-012`
    Estimated scope: XS
    Completion signal: Quota persistence and credential movement are hardened as a cluster.

## Follow-On Work

- [x] `FOL-001` Split doctor baseline readiness from execution readiness

    Spec: `specs/220526-operator-status-and-first-run-truth.md`
    Why now: This is deferred until the validation baseline is green because doctor output should explain a trustworthy command surface; it remains important for zero-friction onboarding but does not outrank red formatter/proof/command tests.
    Codebase evidence: `src/doctor_command.rs::build_doctor_report` mixes repo layout, planning health, queue counts, optional tools, and help surfaces into one report; `README.md` says optional model tools are capability warnings, while `AGENTS.md` currently says `claude`, `codex`, `pi`, and `gh` are required on PATH.
    Source of truth: `src/doctor_command.rs` owns no-model first-run readiness; `src/parallel_command.rs` owns parallel state; `src/quota_status.rs` owns quota state.
    Runtime owner: `src/doctor_command.rs`
    UI consumers: `auto doctor` stdout, README quickstart, AGENTS.md operator instructions
    Generated artifacts: none
    Fixture boundary: Doctor tests may use temp repos and fake PATH tools; production doctor must read the live checkout and live PATH without invoking model providers or network APIs.
    Retired surfaces: AGENTS.md wording that treats model tools as required for no-model first-run success; doctor output that leaves baseline readiness and execution readiness in one ambiguous bucket.
    Owns: src/doctor_command.rs, README.md, AGENTS.md
    Integration touchpoints: `src/main.rs`, `src/parallel_command.rs::run_parallel_status`, `src/quota_status.rs`
    Scope boundary: Improve doctor categories and docs only; do not add a new `auto status` command or duplicate parallel/quota runtime logic.
    Acceptance criteria: Doctor output separately labels baseline checkout/binary readiness and execution/model readiness; missing model tools remain warnings, not baseline failures; README and AGENTS agree on first-run expectations.
    Verification: Add and run `cargo test doctor_command::tests::doctor_distinguishes_baseline_from_execution_readiness -- --nocapture`, run `cargo test doctor_command::tests::doctor_reports_missing_optional_tools_without_panicking -- --nocapture`, run `cargo test doctor_command::tests::doctor_reports_active_planning_and_queue_health -- --nocapture`, and run `cargo run -- doctor`.
    Required tests: `doctor_command::tests::doctor_distinguishes_baseline_from_execution_readiness`, `doctor_command::tests::doctor_reports_missing_optional_tools_without_panicking`, `doctor_command::tests::doctor_reports_active_planning_and_queue_health`
    Contract generation: none -- no generated contract
    Cross-surface tests: `cargo run -- doctor` must render the same first-run model-tool policy documented in README.md and AGENTS.md.
    Review/closeout: Reviewer checks `rg -n "Required tools on PATH|capability warnings|baseline|execution readiness|model/network" AGENTS.md README.md src/doctor_command.rs` and confirms no-model first run is not blocked by missing optional tools.
    Completion artifacts: none
    Dependencies: `P0-001`
    Estimated scope: S
    Completion signal: Operators can run `auto doctor` and distinguish baseline readiness from model-backed workflow readiness.

- [ ] `FOL-002` Prove ship gate ordering around checkpoint and remote sync

    Spec: `specs/220526-release-gates-and-verdict-readiness.md`
    Why now: This is deferred until stale receipt semantics are fixed because gate ordering is only useful when the gate itself rejects stale proof; it matters before release prep can be considered ready.
    Codebase evidence: `src/ship_command.rs::evaluate_ship_gate` runs before model execution; `src/ship_command.rs::run_ship` currently evaluates the gate before checkpoint/remote-sync code paths, while `cargo test ship_command::tests::ship_gate_runs_after_remote_sync_before_model -- --nocapture` passes only the helper-level expectation.
    Source of truth: `src/ship_command.rs::evaluate_ship_gate` owns release readiness; `src/util.rs::auto_checkpoint_if_needed` and remote sync helpers own branch synchronization.
    Runtime owner: `src/ship_command.rs`
    UI consumers: `auto ship` stdout, `SHIP.md`, `.auto/ship/**`
    Generated artifacts: `SHIP.md`, `.auto/ship/codex.stderr.log`, `.auto/logs/ship-*-prompt.md`
    Fixture boundary: Ship tests may create temp git repos and synthetic reports; production ship must evaluate the live branch state after any checkpoint or remote sync that changes the proof surface.
    Retired surfaces: Ship-gate checks that describe pre-sync branch state while release-prep continues on a different synced branch state.
    Owns: src/ship_command.rs
    Integration touchpoints: `src/util.rs::auto_checkpoint_if_needed`, `src/util.rs::sync_branch_with_remote`, `src/completion_artifacts.rs`
    Scope boundary: Reorder or rerun mechanical gate checks only; do not add new release blocker categories or model prompt requirements.
    Acceptance criteria: `auto ship` evaluates release blockers after any checkpoint/remote sync that can change branch proof; if model prep changes release artifacts, the gate reruns before claiming readiness or records a bypass reason.
    Verification: Add and run `cargo test ship_command::tests::ship_gate_runs_after_checkpoint_before_model -- --nocapture`, run `cargo test ship_command::tests::ship_gate_runs_after_remote_sync_before_model -- --nocapture`, add and run `cargo test ship_command::tests::ship_gate_reruns_after_model_iteration_changes -- --nocapture`, and run `cargo test ship_command::tests::ship_gate_bypass_records_operator_reason -- --nocapture`.
    Required tests: `ship_command::tests::ship_gate_runs_after_checkpoint_before_model`, `ship_command::tests::ship_gate_runs_after_remote_sync_before_model`, `ship_command::tests::ship_gate_reruns_after_model_iteration_changes`, `ship_command::tests::ship_gate_bypass_records_operator_reason`
    Contract generation: none -- no generated contract
    Cross-surface tests: Ship tests must prove runtime gate blockers are recorded in `SHIP.md` or stdout after the synchronized branch state is known.
    Review/closeout: Reviewer checks `rg -n "evaluate_ship_gate|auto_checkpoint_if_needed|sync_branch_with_remote|bypass-release-gate" src/ship_command.rs` and confirms no model release-prep path can skip a current mechanical gate.
    Completion artifacts: none
    Dependencies: `P1-007`
    Estimated scope: S
    Completion signal: Ship readiness gate facts describe the current branch state, not a stale pre-sync state.

- [ ] `FOL-003` Complete lane assignment metadata and host-owned queue constants

    Spec: `specs/220526-receipt-and-lane-evidence-contract.md`
    Why now: This is deferred until lane-kind routing is coherent; after that, assignment metadata and protected-file constants can remove remaining drift without blocking the core operator queue semantics.
    Codebase evidence: `src/parallel_command.rs::LaneAssignmentMetadata` includes task id, branch, base commit, and hashes but no assignment hash or worker command/model metadata; `SHARED_QUEUE_FILES` and `HOST_QUEUE_STATE_FILES` are not identical, with `ARCHIVED.md` present in one surface and absent in another.
    Source of truth: `src/parallel_command.rs` owns lane assignment metadata and host-owned queue state.
    Runtime owner: `src/parallel_command.rs`
    UI consumers: lane prompts, `.auto/parallel/**/assignment.json`, `auto parallel status` stdout, lane logs
    Generated artifacts: `.auto/parallel/**/assignment.json`, `.auto/parallel/operator-actions.md`
    Fixture boundary: Tests may use synthetic plan rows and temp lane roots; production assignment metadata must describe live task rows and live worker settings, not fixture command strings.
    Retired surfaces: Divergent host-owned queue file lists; assignment metadata that cannot detect base-commit or worker-command drift.
    Owns: src/parallel_command.rs, AGENTS.md
    Integration touchpoints: `src/task_parser.rs`, `src/completion_artifacts.rs`, `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `RECEIPTS-DRIFT.md`
    Scope boundary: Extend metadata and constants only; do not change lane-kind routing, receipt freshness policy, or parallel worker implementation strategy.
    Acceptance criteria: Assignment metadata includes enough stable data to detect stale task body, dependency, verification, base commit, worker model, and command drift; host-owned queue file constants are unified or intentionally documented with tests.
    Verification: Run `cargo test parallel_command::tests::lane_assignment_metadata_rejects_changed_task_body -- --nocapture`, `cargo test parallel_command::tests::lane_assignment_metadata_rejects_changed_dependencies -- --nocapture`, `cargo test parallel_command::tests::lane_assignment_metadata_rejects_changed_verification_text -- --nocapture`, add and run `cargo test parallel_command::tests::lane_assignment_metadata_rejects_changed_base_commit -- --nocapture`, and add and run `cargo test parallel_command::tests::worker_prompt_lists_host_owned_queue_files -- --nocapture`.
    Required tests: `parallel_command::tests::lane_assignment_metadata_rejects_changed_task_body`, `parallel_command::tests::lane_assignment_metadata_rejects_changed_dependencies`, `parallel_command::tests::lane_assignment_metadata_rejects_changed_verification_text`, `parallel_command::tests::lane_assignment_metadata_rejects_changed_base_commit`, `parallel_command::tests::worker_prompt_lists_host_owned_queue_files`
    Contract generation: none -- no generated contract
    Cross-surface tests: `parallel_command::tests::worker_prompt_lists_host_owned_queue_files` must prove runtime protected files are rendered into worker-facing prompt text.
    Review/closeout: Reviewer checks `rg -n "LaneAssignmentMetadata|SHARED_QUEUE_FILES|HOST_QUEUE_STATE_FILES|ARCHIVED.md|assignment.json" src/parallel_command.rs AGENTS.md` and confirms assignment drift cannot go unnoticed.
    Completion artifacts: none
    Dependencies: `P1-008`
    Estimated scope: M
    Completion signal: Lane assignment metadata and host-owned queue prompts describe the same protected runtime contract.

- [x] `FOL-004` Add a stable final status block to audit status

    Spec: `specs/220526-operator-status-and-first-run-truth.md`
    Why now: This is deferred because it improves operator clarity but does not unblock the core queue, snapshot, receipt, or quota safety loops; it should reuse existing status formatting instead of creating a new audit report campaign.
    Codebase evidence: `src/audit_everything.rs::run_status` prints audit status facts and writes `RUN-STATUS.md`; `src/qa_only_command.rs::format_final_status_block` already provides a reusable status/files/blockers/next-step pattern consumed by other commands.
    Source of truth: `src/audit_everything.rs` owns audit status facts; `src/qa_only_command.rs` owns the reusable final-status block helper if generalized.
    Runtime owner: `src/audit_everything.rs`
    UI consumers: `auto audit --everything status` stdout, `.auto/audit-everything/<run-id>/RUN-STATUS.md`
    Generated artifacts: `.auto/audit-everything/<run-id>/RUN-STATUS.md`
    Fixture boundary: Audit status tests may use temp audit run dirs and manifests; production status must read live audit run state and not import sample manifests as truth.
    Retired surfaces: Audit status output that omits a stable status/files/blockers/next-step closeout block.
    Owns: src/audit_everything.rs, src/qa_only_command.rs
    Integration touchpoints: `.auto/audit-everything/**`, `auto health`, `auto design`
    Scope boundary: Add final status rendering only; do not change audit finding scoring, remediation, or harvest behavior.
    Acceptance criteria: Audit status stdout and `RUN-STATUS.md` include status, files/artifacts, blockers, and next step; large status rendering remains within existing performance expectations.
    Verification: Add and run `cargo test audit_everything::tests::audit_status_prints_final_status_block_and_next_step -- --nocapture`, run `cargo test audit_everything::tests::run_status_markdown_records_pause_paths_and_task_counts -- --nocapture`, and run `cargo test --test performance_status large_audit_status_renders_under_measured_observation -- --nocapture`.
    Required tests: `audit_everything::tests::audit_status_prints_final_status_block_and_next_step`, `audit_everything::tests::run_status_markdown_records_pause_paths_and_task_counts`, `large_audit_status_renders_under_measured_observation`
    Contract generation: none -- no generated contract
    Cross-surface tests: `audit_everything::tests::run_status_markdown_records_pause_paths_and_task_counts` must prove runtime status facts are written to the operator-visible markdown readback.
    Review/closeout: Reviewer checks `rg -n "final status|next step|RUN-STATUS|format_final_status_block" src/audit_everything.rs src/qa_only_command.rs` and confirms audit status has the same actionable closeout shape as other no-model status surfaces.
    Completion artifacts: none
    Dependencies: `P0-001`
    Estimated scope: S
    Completion signal: Audit status has an actionable final block without changing audit runtime truth.

## Completed / Already Satisfied

- `src/generation.rs` already has snapshot-only generation support in `finalize_verified_generation_outputs`, and `cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs -- --nocapture` exits 0 in the live checkout.
- `src/verdict.rs` already implements `exact_terminal_verdict` and `terminal_verdict_is`; `cargo test verdict -- --nocapture` exits 0.
- `src/doctor_command.rs` already treats missing `codex`, `claude`, `pi`, and `gh` as capability warnings in doctor tests; `cargo test doctor_command::tests::doctor_reports_missing_optional_tools_without_panicking -- --nocapture` exits 0.
- `src/completion_artifacts.rs` already rejects zero-test cargo receipts and accepts quoted command receipts with argv matching; `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests -- --nocapture` and `cargo test completion_artifacts::tests::inspect_task_completion_evidence_accepts_quoted_command_receipts_with_argv -- --nocapture` exit 0.
- Quota save permission checks already exist for current non-atomic writes; `cargo test quota_config::tests::save_writes_owner_only -- --nocapture` and `cargo test quota_state::tests::save_writes_owner_only -- --nocapture` exit 0.
