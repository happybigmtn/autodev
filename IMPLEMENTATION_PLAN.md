# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `QSEC-001` Validate quota account slugs and profile containment

    Spec: `specs/300426-quota-backend-and-credential-safety.md`
    Why now: Raw quota account names still flow into profile paths before any slug or containment policy runs, so credential capture can touch paths outside the intended profile root before the config write fails.
    Codebase evidence: `src/quota_config.rs` builds `QuotaConfig::profile_dir(provider, name)` with `format!("{}-{name}", provider.label())`; `src/quota_accounts.rs` calls that helper before `copy_auth_to_profile`; `src/quota_config.rs` already has owner-only writes and symlink rejection that the slug check should reuse.
    Source of truth: `src/quota_config.rs`, `src/quota_accounts.rs`, `src/quota_state.rs`
    Runtime owner: `src/quota_config.rs`
    UI consumers: `auto quota accounts add`, `auto quota accounts remove`, `auto quota accounts capture`, `auto quota status`
    Generated artifacts: `.auto/symphony/verification-receipts/QSEC-001.json`
    Fixture boundary: production cannot import fixture/demo/sample account names or captured test profiles as credential truth; tests must use temp config/profile roots.
    Retired surfaces: raw account names as path components in quota profile construction
    Owns: `src/quota_config.rs`, `src/quota_accounts.rs`, `src/quota_state.rs`
    Integration touchpoints: `src/quota_selector.rs`, `src/quota_status.rs`, platform config dir quota TOML, quota state JSON
    Scope boundary: validate and contain account identity paths only; do not change provider selection, usage refresh, or backend model routing.
    Acceptance criteria: empty names, traversal names, absolute names, path separators, and non-slug punctuation fail before config, state, or profile filesystem mutation; resolved profile paths stay under `QuotaConfig::profiles_dir()`.
    Verification: `scripts/run-task-verification.sh QSEC-001 cargo test quota_config::tests::unsafe_account_names_fail_before_profile_mutation`; `scripts/run-task-verification.sh QSEC-001 cargo test quota_config::tests::profile_dir_stays_under_profiles_root`; `scripts/run-task-verification.sh QSEC-001 cargo test quota_state::tests::save_writes_owner_only`; `rg -n "profile_dir|validate_.*account|profiles_dir" src/quota_config.rs src/quota_accounts.rs src/quota_state.rs`
    Required tests: `quota_config::tests::unsafe_account_names_fail_before_profile_mutation`, `quota_config::tests::profile_dir_stays_under_profiles_root`, `quota_state::tests::save_writes_owner_only`
    Contract generation: `scripts/run-task-verification.sh QSEC-001 cargo test quota_config::tests::unsafe_account_names_fail_before_profile_mutation`
    Cross-surface tests: quota account fixture readback proves CLI-facing add/capture paths reject unsafe names before profile creation
    Review/closeout: reviewer creates a temp quota home, attempts `../x` and absolute account names through the runtime helper, and confirms no profile dir, config entry, or state entry is created.
    Completion artifacts: `.auto/symphony/verification-receipts/QSEC-001.json`, `REVIEW.md`
    Dependencies: none
    Estimated scope: M
    Completion signal: quota account identity becomes a validated slug before any credential/profile mutation.

- [ ] `QSEC-002` Stop quota failover after detected worker progress

    Spec: `specs/300426-quota-backend-and-credential-safety.md`
    Why now: `run_with_quota` detects progress in provider output but still logs that it is trying the next account, risking duplicate model-backed side effects after a partially completed worker run.
    Codebase evidence: `src/quota_exec.rs` calls `quota_output_has_agent_progress(&stderr_text)` and then continues on `QuotaVerdict::Exhausted`; existing tests cover progress-sentinel detection but not failover behavior.
    Source of truth: `src/quota_exec.rs`, `src/quota_state.rs`, `.auto/`
    Runtime owner: `src/quota_exec.rs`
    UI consumers: quota-router stderr lines, model-backed command logs, `auto quota status`
    Generated artifacts: `.auto/symphony/verification-receipts/QSEC-002.json`, `.auto/quota-recovery/**`
    Fixture boundary: production cannot treat fake provider output as live progress proof; tests must use synthetic provider stderr and temp `.auto/quota-recovery` roots.
    Retired surfaces: retry-after-progress log-only behavior that continues to another account
    Owns: `src/quota_exec.rs`, `src/quota_state.rs`
    Integration touchpoints: `src/quota_patterns.rs`, `src/quota_selector.rs`, quota-router stderr, provider credential restore guard
    Scope boundary: stop and record recovery after progress; preserve normal failover for quota exhaustion with no detected progress.
    Acceptance criteria: progress-detected quota exhaustion restores credentials, marks the account exhausted, writes or reports a recovery marker, returns a clear stop error, and never selects a second account in the same run.
    Verification: `scripts/run-task-verification.sh QSEC-002 cargo test quota_exec::tests::quota_exhaustion_after_progress_does_not_try_next_account`; `scripts/run-task-verification.sh QSEC-002 cargo test quota_exec::tests::immediate_quota_error_can_try_next_account`; `scripts/run-task-verification.sh QSEC-002 cargo test quota_exec::tests::detects_progress_sentinel_before_quota_failure`; `rg -n "quota_output_has_agent_progress|trying next|quota-recovery" src/quota_exec.rs src/quota_state.rs`
    Required tests: `quota_exec::tests::quota_exhaustion_after_progress_does_not_try_next_account`, `quota_exec::tests::immediate_quota_error_can_try_next_account`, `quota_exec::tests::detects_progress_sentinel_before_quota_failure`
    Contract generation: `scripts/run-task-verification.sh QSEC-002 cargo test quota_exec::tests::quota_exhaustion_after_progress_does_not_try_next_account`
    Cross-surface tests: quota-router stderr fixture and recovery marker readback show stop-vs-failover status without exposing secrets
    Review/closeout: reviewer checks the fixture proves the second account callback is not invoked after progress and that credentials restore before the command exits.
    Completion artifacts: `.auto/symphony/verification-receipts/QSEC-002.json`, `REVIEW.md`
    Dependencies: `QSEC-001`
    Estimated scope: S
    Completion signal: quota failover is safe after progress and still useful before progress.

- [ ] `QSEC-003` Decide Kimi and PI prompt transport plus quota migration policy

    Spec: `specs/300426-quota-backend-and-credential-safety.md`
    Why now: The generated quota spec flags Kimi and PI argv prompt delivery as a safety recommendation, but local code only proves `kimi_exec_args` uses `-p <prompt>` and does not prove stdin or file-prompt support.
    Codebase evidence: `src/kimi_backend.rs` builds `-p` argv prompt arguments; `src/codex_exec.rs` and `src/claude_exec.rs` already avoid argv prompts for their primary paths; no repo evidence proves Kimi or PI support a safer transport.
    Source of truth: `docs/decisions/quota-backend-prompt-transport.md`, `src/kimi_backend.rs`, `src/pi_backend.rs`
    Runtime owner: none
    UI consumers: README provider notes, quota-router stderr, backend command logs
    Generated artifacts: `.auto/symphony/verification-receipts/QSEC-003.json`
    Fixture boundary: production cannot import fixture CLI help output as provider truth; the decision must cite current local help output or primary provider documentation captured by the worker.
    Retired surfaces: undocumented unsafe argv prompt limitation for Kimi or PI
    Owns: `docs/decisions/quota-backend-prompt-transport.md`, `README.md`
    Integration touchpoints: `src/kimi_backend.rs`, `src/pi_backend.rs`, `src/codex_exec.rs`, `src/claude_exec.rs`, `src/backend_policy.rs`
    Scope boundary: decision and migration policy only; do not change backend invocation until the supported transport is proven.
    Acceptance criteria: decision doc records Kimi and PI prompt-delivery support, account slug display-name migration policy, unsafe-config handling, and whether implementation should move prompts off argv or expose an explicit operator limitation.
    Verification: `scripts/run-task-verification.sh QSEC-003 rg -n "Kimi|PI|argv|stdin|prompt file|migration" docs/decisions/quota-backend-prompt-transport.md`; `rg -n "kimi_exec_args|-p|parse_pi_error|resolve_pi_bin" src/kimi_backend.rs src/pi_backend.rs src/codex_exec.rs`
    Required tests: none
    Contract generation: `scripts/run-task-verification.sh QSEC-003 rg -n "prompt transport" docs/decisions/quota-backend-prompt-transport.md`
    Cross-surface tests: README provider-note grep agrees with the decision doc and does not claim unimplemented prompt transport
    Review/closeout: reviewer confirms every implementation recommendation in the decision cites local help output or primary documentation, not generated-spec hypothesis text.
    Completion artifacts: `docs/decisions/quota-backend-prompt-transport.md`, `.auto/symphony/verification-receipts/QSEC-003.json`
    Dependencies: `QSEC-001`
    Estimated scope: S
    Completion signal: backend prompt-transport and unsafe-account migration choices are explicit before code promises safer Kimi or PI behavior.

- [ ] `CHECK-001` Security and credential safety checkpoint

    Spec: `specs/300426-quota-backend-and-credential-safety.md`
    Why now: The quota slice touches credential paths and retry behavior, so the queue should stop and re-evaluate before planning-root and scheduler work expands the blast radius.
    Codebase evidence: `src/quota_config.rs`, `src/quota_exec.rs`, and `src/quota_state.rs` now own the highest-risk local filesystem mutations; `src/quota_usage.rs` already has sanitizer tests that should remain green.
    Source of truth: `REVIEW.md`, `.auto/symphony/verification-receipts/`
    Runtime owner: none
    UI consumers: `REVIEW.md`, quota-router stderr summaries
    Generated artifacts: `.auto/symphony/verification-receipts/CHECK-001.json`
    Fixture boundary: production cannot accept fixture credentials or synthetic provider output as live provider proof; checkpoint evidence is local code and temp-profile proof only.
    Retired surfaces: none
    Owns: `REVIEW.md`
    Integration touchpoints: `src/quota_config.rs`, `src/quota_exec.rs`, `src/quota_usage.rs`, `src/quota_status.rs`
    Scope boundary: checkpoint only; do not implement new quota features in this task.
    Acceptance criteria: `REVIEW.md` records slug containment proof, progress-stop proof, prompt-transport decision status, and any remaining quota backend blockers before the next cluster starts.
    Verification: `scripts/run-task-verification.sh CHECK-001 cargo test quota_usage::tests::claude_refresh_error_does_not_leak_body`; `scripts/run-task-verification.sh CHECK-001 cargo test quota_usage::tests::sanitize_quota_error_message_keeps_non_secret_context`; `rg -n "CHECK-001|QSEC-001|QSEC-002|QSEC-003" REVIEW.md`
    Required tests: `quota_usage::tests::claude_refresh_error_does_not_leak_body`, `quota_usage::tests::sanitize_quota_error_message_keeps_non_secret_context`
    Contract generation: `scripts/run-task-verification.sh CHECK-001 cargo test quota_usage::tests::claude_refresh_error_does_not_leak_body`
    Cross-surface tests: `REVIEW.md` readback proves the operator-facing quota status does not leak credential material
    Review/closeout: reviewer checks that no quota task is marked done without a matching receipt or explicit decision artifact.
    Completion artifacts: `REVIEW.md`, `.auto/symphony/verification-receipts/CHECK-001.json`
    Dependencies: `QSEC-001`, `QSEC-002`, `QSEC-003`
    Estimated scope: XS
    Completion signal: quota security work is reviewed before state and scheduler changes begin.

- [ ] `CSTATE-001` Resolve planning-root provenance and saved-state containment

    Spec: `specs/300426-corpus-state-and-planning-root-safety.md`
    Why now: `auto gen` and `auto reverse` trust raw `.auto/state.json` planning-root paths before printing why that root was selected, so a future corrupt saved path can still steer destructive generation behavior even when the current state points at this run.
    Codebase evidence: `src/generation.rs` selects explicit `--planning-root`, then saved state, then `genesis`; `src/state.rs` persists raw `PathBuf` values; `.auto/state.json` currently points at `genesis` and `gen-20260430-184141`.
    Source of truth: `src/generation.rs`, `src/state.rs`, `src/corpus.rs`
    Runtime owner: `src/generation.rs`
    UI consumers: `auto gen` stdout, `auto reverse` stdout, README planning-root guidance
    Generated artifacts: `.auto/state.json`, `.auto/symphony/verification-receipts/CSTATE-001.json`
    Fixture boundary: production cannot fall back to fixture corpora, archived `.auto/fresh-input` snapshots, or old `gen-*` outputs as active planning truth.
    Retired surfaces: silently trusted saved external planning roots
    Owns: `src/generation.rs`, `src/state.rs`, `src/corpus.rs`, `README.md`
    Integration touchpoints: `auto gen --snapshot-only`, `auto gen --sync-only`, `auto reverse`, `.auto/state.json`
    Scope boundary: planning-root selection and messaging only; do not change corpus authoring, generated spec validation, or root sync merge policy.
    Acceptance criteria: generation prints planning-root provenance as CLI, saved state, or default; saved outside-repo planning roots fail before output deletion or root sync; explicit CLI planning roots remain visibly operator-selected.
    Verification: `scripts/run-task-verification.sh CSTATE-001 cargo test generation::tests::saved_outside_repo_planning_root_is_rejected_before_generation`; `scripts/run-task-verification.sh CSTATE-001 cargo test generation::tests::planning_root_resolution_reports_cli_saved_or_default_source`; `rg -n "planning_root|latest_output_dir|provenance|saved state|default genesis" src/generation.rs src/state.rs README.md`
    Required tests: `generation::tests::saved_outside_repo_planning_root_is_rejected_before_generation`, `generation::tests::planning_root_resolution_reports_cli_saved_or_default_source`
    Contract generation: `scripts/run-task-verification.sh CSTATE-001 cargo test generation::tests::planning_root_resolution_reports_cli_saved_or_default_source`
    Cross-surface tests: `auto gen` stdout fixture and README grep both distinguish CLI, saved-state, and default planning roots
    Review/closeout: reviewer corrupts saved state in a temp repo and confirms generation fails before deleting or syncing any root files.
    Completion artifacts: `.auto/symphony/verification-receipts/CSTATE-001.json`, `REVIEW.md`
    Dependencies: `CHECK-001`
    Estimated scope: M
    Completion signal: saved state is a hint with provenance, not an untrusted source of active planning truth.

- [ ] `CSTATE-002` Reject empty primary corpora and make verify-only non-mutating

    Spec: `specs/300426-corpus-state-and-planning-root-safety.md`
    Why now: `load_planning_corpus` can return an empty `primary_plans` vector and `auto corpus --verify-only` currently runs sanitize-and-save code that can rewrite corpus files and `.auto/state.json`.
    Codebase evidence: `src/corpus.rs` treats any leading digit as primary and does not reject an empty primary set; `src/generation.rs` rejects zero numbered plans only after corpus authoring; `run_corpus` verify-only calls `sanitize_verify_and_save_corpus_outputs`.
    Source of truth: `src/corpus.rs`, `src/generation.rs`
    Runtime owner: `src/corpus.rs`, `src/generation.rs`
    UI consumers: `auto corpus --verify-only`, `auto gen`, generated corpus snapshots
    Generated artifacts: `.auto/symphony/verification-receipts/CSTATE-002.json`, `gen-*/corpus/**`
    Fixture boundary: production cannot import support-only fixture corpora or old generated snapshots as active primary plans.
    Retired surfaces: support-only `plans/` directories accepted as executable planning corpora
    Owns: `src/corpus.rs`, `src/generation.rs`
    Integration touchpoints: `emit_corpus_snapshot`, `verify_corpus_outputs`, `is_numbered_corpus_plan_file`, `.auto/state.json`
    Scope boundary: corpus loading and verify-only semantics only; do not change model authoring prompts.
    Acceptance criteria: corpus loading fails on zero `NNN-*.md` primary plans; corpus and generation agree on the primary-plan filename rule; verify-only validates current corpus shape without rewriting corpus markdown or saving state.
    Verification: `scripts/run-task-verification.sh CSTATE-002 cargo test corpus::tests::load_planning_corpus_rejects_empty_primary_plan_set`; `scripts/run-task-verification.sh CSTATE-002 cargo test corpus::tests::load_planning_corpus_rejects_support_only_plan_set`; `scripts/run-task-verification.sh CSTATE-002 cargo test generation::tests::corpus_verify_only_does_not_rewrite_corpus_files`; `rg -n "load_planning_corpus|primary_plans|is_primary_plan_file|verify_only" src/corpus.rs src/generation.rs`
    Required tests: `corpus::tests::load_planning_corpus_rejects_empty_primary_plan_set`, `corpus::tests::load_planning_corpus_rejects_support_only_plan_set`, `generation::tests::corpus_verify_only_does_not_rewrite_corpus_files`
    Contract generation: `scripts/run-task-verification.sh CSTATE-002 cargo test corpus::tests::load_planning_corpus_rejects_empty_primary_plan_set`
    Cross-surface tests: corpus verify-only fixture readback proves no corpus file or `.auto/state.json` mutation occurs
    Review/closeout: reviewer compares temp-repo file hashes before and after verify-only and confirms empty corpus errors are actionable.
    Completion artifacts: `.auto/symphony/verification-receipts/CSTATE-002.json`, `REVIEW.md`
    Dependencies: `CSTATE-001`
    Estimated scope: S
    Completion signal: corpus validation fails early and verify-only is a true no-mutation check.

- [ ] `CSTATE-003` Stage corpus refresh before replacing live planning input

    Spec: `specs/300426-corpus-state-and-planning-root-safety.md`
    Why now: `prepare_planning_root_for_corpus` archives and removes the existing planning root before replacement validation succeeds, so a failed corpus authoring pass can leave the operator without the prior valid `genesis/`.
    Codebase evidence: `src/generation.rs` copies the prior planning root into `.auto/fresh-input` and then removes the live planning root before the model writes new corpus files; `verify_corpus_outputs` runs only after model output.
    Source of truth: `src/generation.rs`, `src/util.rs`, `.auto/fresh-input/`
    Runtime owner: `src/generation.rs`
    UI consumers: `auto corpus` stdout, README corpus recovery guidance
    Generated artifacts: `.auto/fresh-input/**`, `.auto/symphony/verification-receipts/CSTATE-003.json`, `genesis/**`
    Fixture boundary: production cannot use fixture corpora as recovery truth; recovery must preserve the actual prior planning root until the new corpus validates.
    Retired surfaces: destructive in-place corpus refresh before replacement validation
    Owns: `src/generation.rs`, `src/util.rs`, `README.md`
    Integration touchpoints: `copy_tree`, `atomic_write`, `verify_corpus_outputs`, `sanitize_corpus_outputs`, `.auto/fresh-input`
    Scope boundary: corpus refresh transactionality only; do not change generated spec or implementation-plan output validation.
    Acceptance criteria: corpus authoring writes into a staged sibling root, validates required corpus artifacts there, then atomically swaps it into the requested planning root; a simulated authoring or validation failure leaves the previous corpus intact.
    Verification: `scripts/run-task-verification.sh CSTATE-003 cargo test generation::tests::corpus_refresh_failure_preserves_previous_planning_root`; `scripts/run-task-verification.sh CSTATE-003 cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs`; `rg -n "prepare_planning_root_for_corpus|fresh-input|staged|verify_corpus_outputs" src/generation.rs src/util.rs README.md`
    Required tests: `generation::tests::corpus_refresh_failure_preserves_previous_planning_root`, `generation::tests::snapshot_only_generation_does_not_sync_root_outputs`
    Contract generation: `scripts/run-task-verification.sh CSTATE-003 cargo test generation::tests::corpus_refresh_failure_preserves_previous_planning_root`
    Cross-surface tests: failed corpus fixture plus README recovery grep prove operators can recover without old corpus loss
    Review/closeout: reviewer checks `git status --short` and temp planning-root contents before and after a failing fixture.
    Completion artifacts: `.auto/symphony/verification-receipts/CSTATE-003.json`, `REVIEW.md`
    Dependencies: `CSTATE-002`
    Estimated scope: M
    Completion signal: corpus refresh behaves like a validated swap instead of a destructive rewrite.

- [ ] `CHECK-002` Corpus and planning-root safety checkpoint

    Spec: `specs/300426-production-control-and-planning-primacy.md`
    Why now: The generated snapshot is subordinate and `.auto/state.json` points at an older output dir, so state and corpus safety should be reviewed before schema or scheduler work consumes generated queue truth.
    Codebase evidence: `find gen-20260430-184141/specs -maxdepth 1 -type f -name '*.md'` shows ten generated specs; `gen-20260430-184141/IMPLEMENTATION_PLAN.md` is present as the generated execution-plan snapshot; root `IMPLEMENTATION_PLAN.md` now contains the promoted production-race queue with 26 priority rows and 4 follow-on rows.
    Source of truth: `REVIEW.md`, `gen-20260430-184141/`, `.auto/state.json`
    Runtime owner: none
    UI consumers: `REVIEW.md`, `auto gen` stdout, `auto parallel status`
    Generated artifacts: `.auto/symphony/verification-receipts/CHECK-002.json`
    Fixture boundary: production cannot treat generated snapshots, fixture corpora, or old `.auto/state.json` pointers as active root queue truth.
    Retired surfaces: none
    Owns: `REVIEW.md`
    Integration touchpoints: `src/generation.rs`, `src/corpus.rs`, `src/state.rs`, root `IMPLEMENTATION_PLAN.md`
    Scope boundary: checkpoint only; do not promote generated specs or queue rows.
    Acceptance criteria: `REVIEW.md` records planning-root provenance, current snapshot contents, promoted root queue shape, and whether corpus safety blockers remain before row validation work starts.
    Verification: `scripts/run-task-verification.sh CHECK-002 rg -n "CHECK-002|CSTATE-001|CSTATE-002|CSTATE-003" REVIEW.md`; `find gen-20260430-184141/specs -maxdepth 1 -type f -name '*.md' -printf '%f\n' | sort`; `rg -n "^- \\[ \\]" IMPLEMENTATION_PLAN.md`; `cargo run --quiet -- parallel status`
    Required tests: none
    Contract generation: `scripts/run-task-verification.sh CHECK-002 rg -n "CHECK-002" REVIEW.md`
    Cross-surface tests: snapshot file listing, root queue grep, parallel status, and review readback agree on promoted-root versus subordinate-snapshot status
    Review/closeout: reviewer confirms the checkpoint does not mutate root specs, root plan, or `.auto/state.json`.
    Completion artifacts: `REVIEW.md`, `.auto/symphony/verification-receipts/CHECK-002.json`
    Dependencies: `CSTATE-001`, `CSTATE-002`, `CSTATE-003`
    Estimated scope: XS
    Completion signal: corpus/state safety is reviewed and the generated snapshot remains explicitly subordinate.

- [ ] `ROW-001` Extract one shared execution-row validator

    Spec: `specs/300426-execution-row-schema-parity.md`
    Why now: The rich task field list already exists in `task_parser`, and generation/spec/super enforce similar rules, but loop, parallel, review, and steward can still drift without one shared validator.
    Codebase evidence: `src/task_parser.rs` defines `PLAN_TASK_REQUIRED_FIELDS`; `src/generation.rs` and `src/super_command.rs` implement parallel scoped checks; `src/spec_command.rs` imports the shared fields; `src/review_command.rs` and `src/steward_command.rs` write queue truth.
    Source of truth: `src/task_parser.rs`, `src/verification_lint.rs`
    Runtime owner: `src/task_parser.rs`
    UI consumers: generated `IMPLEMENTATION_PLAN.md`, root `IMPLEMENTATION_PLAN.md`, `auto super` gate output, worker prompts
    Generated artifacts: `.auto/symphony/verification-receipts/ROW-001.json`, `gen-*/IMPLEMENTATION_PLAN.md`
    Fixture boundary: production must validate the live root or generated plan; tests may use synthetic valid and invalid row fixtures only.
    Retired surfaces: duplicated required-field validators that can accept different execution-row contracts
    Owns: `src/task_parser.rs`, `src/verification_lint.rs`
    Integration touchpoints: `src/generation.rs`, `src/spec_command.rs`, `src/super_command.rs`
    Scope boundary: shared validator API and fixture parity only; do not change markdown storage or scheduler selection semantics.
    Acceptance criteria: one validator reports task id, field, and invariant for missing fields, prose-only dependency gates, vague owners, broad verification, more than five required tests, and generated-artifact tasks with no contract check.
    Verification: `scripts/run-task-verification.sh ROW-001 cargo test task_parser::tests::execution_row_validator_accepts_rich_generated_contract`; `scripts/run-task-verification.sh ROW-001 cargo test task_parser::tests::execution_row_validator_rejects_missing_required_field_with_task_id`; `scripts/run-task-verification.sh ROW-001 cargo test task_parser::tests::execution_row_validator_rejects_prose_only_dependencies`; `rg -n "PLAN_TASK_REQUIRED_FIELDS|validate_.*execution|verify_commands_are_runnable" src/task_parser.rs src/verification_lint.rs`
    Required tests: `task_parser::tests::execution_row_validator_accepts_rich_generated_contract`, `task_parser::tests::execution_row_validator_rejects_missing_required_field_with_task_id`, `task_parser::tests::execution_row_validator_rejects_prose_only_dependencies`
    Contract generation: `scripts/run-task-verification.sh ROW-001 cargo test task_parser::tests::execution_row_validator_accepts_rich_generated_contract`
    Cross-surface tests: fixture row diagnostics are rendered with task id and field names usable by generated plans and CLI gates
    Review/closeout: reviewer compares the validator field list with the generated plan prompt and confirms there is no second authoritative required-field catalog.
    Completion artifacts: `.auto/symphony/verification-receipts/ROW-001.json`, `REVIEW.md`
    Dependencies: `CHECK-002`
    Estimated scope: M
    Completion signal: the execution-row schema has one runtime owner.

- [ ] `ROW-002` Enforce execution-row parity at dispatch and queue-write boundaries

    Spec: `specs/300426-execution-row-schema-parity.md`
    Why now: A shared validator is only useful if every command that selects, schedules, promotes, or writes rows calls it before workers see malformed queue truth.
    Codebase evidence: `src/parallel_command.rs` parses rows for dispatch; `src/loop_command.rs` owns serial task selection; `src/review_command.rs` and `src/steward_command.rs` reconcile queue files; `src/super_command.rs` already has a deterministic gate that should delegate to the shared validator.
    Source of truth: `src/task_parser.rs`, `src/parallel_command.rs`, `src/loop_command.rs`, `src/review_command.rs`, `src/steward_command.rs`
    Runtime owner: `src/task_parser.rs`
    UI consumers: `auto loop`, `auto parallel`, `auto review`, `auto steward`, `auto super`
    Generated artifacts: `.auto/symphony/verification-receipts/ROW-002.json`, `.auto/super/*/DETERMINISTIC-GATE.json`
    Fixture boundary: production dispatch must parse live root ledgers and never schedule fixture-normalized rows.
    Retired surfaces: command-specific acceptance of partial row shapes before worker dispatch
    Owns: `src/generation.rs`, `src/spec_command.rs`, `src/super_command.rs`, `src/loop_command.rs`, `src/parallel_command.rs`, `src/review_command.rs`, `src/steward_command.rs`
    Integration touchpoints: `verify_generated_implementation_plan`, `verify_parallel_ready_plan`, ready task selection, review/steward write paths
    Scope boundary: validator wiring only; do not add a database, change task statuses, or promote generated rows.
    Acceptance criteria: the same invalid row fixture is rejected by generation validation, spec insertion, super gate, loop selection, parallel selection, review write, and steward promotion with compatible diagnostics.
    Verification: `scripts/run-task-verification.sh ROW-002 cargo test super_command::tests::super_rejects_task_missing_runtime_ui_fields`; `scripts/run-task-verification.sh ROW-002 cargo test loop_command::tests::loop_rejects_invalid_execution_row`; `scripts/run-task-verification.sh ROW-002 cargo test review_command::tests::review_queue_write_rejects_invalid_execution_row`; `scripts/run-task-verification.sh ROW-002 cargo test steward_command::tests::steward_promotion_rejects_invalid_execution_row`; `rg -n "validate_.*execution|PLAN_TASK_REQUIRED_FIELDS" src/generation.rs src/spec_command.rs src/super_command.rs src/loop_command.rs src/parallel_command.rs src/review_command.rs src/steward_command.rs`
    Required tests: `super_command::tests::super_rejects_task_missing_runtime_ui_fields`, `loop_command::tests::loop_rejects_invalid_execution_row`, `review_command::tests::review_queue_write_rejects_invalid_execution_row`, `steward_command::tests::steward_promotion_rejects_invalid_execution_row`
    Contract generation: `scripts/run-task-verification.sh ROW-002 cargo test super_command::tests::super_rejects_task_missing_runtime_ui_fields`
    Cross-surface tests: one valid rich row fixture passes every consumer and one missing-field fixture fails every consumer
    Review/closeout: reviewer grep-checks that dispatch/write boundaries delegate to the shared validator or are covered by the parity fixture.
    Completion artifacts: `.auto/symphony/verification-receipts/ROW-002.json`, `REVIEW.md`
    Dependencies: `ROW-001`
    Estimated scope: M
    Completion signal: workers cannot receive rows that generation or super would reject.

- [ ] `CHECK-003A` Execution-row schema checkpoint

    Spec: `specs/300426-execution-row-schema-parity.md`
    Why now: Row validation changes every queue-writing and dispatch boundary, so evidence-model refactors should wait until the shared row contract has been reviewed independently.
    Codebase evidence: `src/task_parser.rs` owns the shared row fields; `src/generation.rs`, `src/spec_command.rs`, `src/super_command.rs`, `src/loop_command.rs`, `src/parallel_command.rs`, `src/review_command.rs`, and `src/steward_command.rs` are the consumers that must agree before later evidence work trusts their parsed rows.
    Source of truth: `REVIEW.md`, `.auto/symphony/verification-receipts/`
    Runtime owner: none
    UI consumers: `REVIEW.md`, `auto super`, `auto loop`, `auto parallel`, `auto review`, `auto steward`
    Generated artifacts: `.auto/symphony/verification-receipts/CHECK-003A.json`
    Fixture boundary: production cannot use fixture rows as active queue truth; checkpoint evidence must cite current validator tests, grep proof, and review readback.
    Retired surfaces: none
    Owns: `REVIEW.md`
    Integration touchpoints: `src/task_parser.rs`, `src/generation.rs`, `src/spec_command.rs`, `src/super_command.rs`, `src/loop_command.rs`, `src/parallel_command.rs`, `src/review_command.rs`, `src/steward_command.rs`
    Scope boundary: checkpoint only; do not implement receipt freshness, artifact containment, or scheduler launch behavior in this task.
    Acceptance criteria: `REVIEW.md` records the shared row-validator owner, every dispatch/write consumer checked, the valid-row fixture result, the missing-field fixture result, and any remaining row-schema blockers before evidence work starts.
    Verification: `scripts/run-task-verification.sh CHECK-003A rg -n "CHECK-003A|ROW-001|ROW-002" REVIEW.md`; `rg -n "validate_.*execution|PLAN_TASK_REQUIRED_FIELDS|verify_parallel_ready_plan|parse_tasks" src/task_parser.rs src/generation.rs src/spec_command.rs src/super_command.rs src/loop_command.rs src/parallel_command.rs src/review_command.rs src/steward_command.rs`
    Required tests: none
    Contract generation: `scripts/run-task-verification.sh CHECK-003A rg -n "CHECK-003A" REVIEW.md`
    Cross-surface tests: review readback names each row consumer and links the validator result to the CLI surface that consumes it
    Review/closeout: reviewer confirms evidence tasks do not begin while any row consumer still accepts a malformed rich execution row.
    Completion artifacts: `REVIEW.md`, `.auto/symphony/verification-receipts/CHECK-003A.json`
    Dependencies: `ROW-001`, `ROW-002`
    Estimated scope: XS
    Completion signal: execution-row schema truth is reviewed before receipt/artifact evidence policy changes consume it.

- [ ] `EVID-001` Reject completion artifact paths outside the repo

    Spec: `specs/300426-receipt-artifact-and-evidence-binding.md`
    Why now: Completion and ship evidence both resolve declared artifact strings with `repo_root.join(relative)`, which needs canonical repo containment before hashing or accepting any markdown-declared path.
    Codebase evidence: `src/completion_artifacts.rs` and `src/ship_command.rs` each implement `declared_artifact_path`; both special-case verification receipt paths and otherwise join paths under the repo root.
    Source of truth: `src/completion_artifacts.rs`, `src/task_parser.rs`
    Runtime owner: `src/completion_artifacts.rs`
    UI consumers: `auto parallel` completion reconciliation, `auto ship` release blockers, `REVIEW.md`
    Generated artifacts: `.auto/symphony/verification-receipts/EVID-001.json`
    Fixture boundary: production cannot accept fixture artifact paths, absolute host paths, or copied receipt excerpts as current completion proof.
    Retired surfaces: artifact path acceptance that can hash arbitrary host paths
    Owns: `src/completion_artifacts.rs`, `src/ship_command.rs`, `src/task_parser.rs`
    Integration touchpoints: `verification_receipt_root`, declared completion artifacts parsing, ship receipt loading
    Scope boundary: path containment and diagnostics only; do not consolidate receipt freshness logic in this task.
    Acceptance criteria: absolute paths, traversal paths, symlink escapes, and declared directories outside the repo fail with clear diagnostics in both task completion and ship gate contexts.
    Verification: `scripts/run-task-verification.sh EVID-001 cargo test completion_artifacts::tests::completion_artifact_paths_reject_parent_escape`; `scripts/run-task-verification.sh EVID-001 cargo test ship_command::tests::ship_gate_rejects_outside_declared_artifact_path`; `rg -n "declared_artifact_path|canonicalize|repo root" src/completion_artifacts.rs src/ship_command.rs src/task_parser.rs`
    Required tests: `completion_artifacts::tests::completion_artifact_paths_reject_parent_escape`, `ship_command::tests::ship_gate_rejects_outside_declared_artifact_path`
    Contract generation: `scripts/run-task-verification.sh EVID-001 cargo test completion_artifacts::tests::completion_artifact_paths_reject_parent_escape`
    Cross-surface tests: the same outside-artifact fixture blocks completion evidence and ship release readiness
    Review/closeout: reviewer verifies no code path hashes a declared artifact before containment is checked.
    Completion artifacts: `.auto/symphony/verification-receipts/EVID-001.json`, `REVIEW.md`
    Dependencies: `CHECK-003A`
    Estimated scope: S
    Completion signal: declared artifacts are repo-contained before they can satisfy completion or release proof.

- [ ] `EVID-002` Consolidate receipt freshness into one shared inspector

    Spec: `specs/300426-receipt-artifact-and-evidence-binding.md`
    Why now: `completion_artifacts` and `ship_command` already check the same receipt freshness concepts, but duplicate structs and functions can drift as receipt fields evolve.
    Codebase evidence: both `src/completion_artifacts.rs` and `src/ship_command.rs` define `verification_receipt_freshness_problem`, current commit, dirty-state fingerprint, plan hash, expected argv, declared artifact hashes, and zero-test handling.
    Source of truth: `src/completion_artifacts.rs`, `scripts/verification_receipt.py`
    Runtime owner: `src/completion_artifacts.rs`
    UI consumers: `auto parallel`, `auto loop`, `auto symphony`, `auto ship`, `REVIEW.md`, `SHIP.md`
    Generated artifacts: `.auto/symphony/verification-receipts/*.json`, `.auto/symphony/verification-receipts/EVID-002.json`, `SHIP.md`
    Fixture boundary: production cannot treat model narrative, fixture receipts, or historical copied JSON as host-observed verification receipts.
    Retired surfaces: duplicate receipt freshness logic in `src/ship_command.rs`
    Owns: `src/completion_artifacts.rs`, `src/ship_command.rs`, `scripts/verification_receipt.py`
    Integration touchpoints: `scripts/run-task-verification.sh`, ship release gate, completion evidence gap classification
    Scope boundary: shared inspector and parity tests only; defer formal schema files and directory hash budgets to a follow-on task.
    Acceptance criteria: completion and ship gates return the same freshness result for commit, dirty state, plan hash, expected argv, failed commands, superseded commands, zero-test status, and declared artifact hashes.
    Verification: `scripts/run-task-verification.sh EVID-002 cargo test completion_artifacts::tests::shared_receipt_inspector_rejects_stale_commit`; `scripts/run-task-verification.sh EVID-002 cargo test ship_command::tests::ship_gate_uses_shared_receipt_inspector`; `scripts/run-task-verification.sh EVID-002 cargo test completion_artifacts::tests::inspect_task_completion_evidence_accepts_explicitly_superseded_failed_attempt`; `rg -n "verification_receipt_freshness_problem|SharedReceipt|zero_test|supersedes" src/completion_artifacts.rs src/ship_command.rs scripts/verification_receipt.py`
    Required tests: `completion_artifacts::tests::shared_receipt_inspector_rejects_stale_commit`, `ship_command::tests::ship_gate_uses_shared_receipt_inspector`, `completion_artifacts::tests::inspect_task_completion_evidence_accepts_explicitly_superseded_failed_attempt`
    Contract generation: `scripts/run-task-verification.sh EVID-002 cargo test completion_artifacts::tests::shared_receipt_inspector_rejects_stale_commit`
    Cross-surface tests: stale receipt fixture produces compatible blocker text in completion reconciliation and ship gate output
    Review/closeout: reviewer grep-confirms receipt freshness code has one owner after the refactor or a single parity suite covers every release field.
    Completion artifacts: `.auto/symphony/verification-receipts/EVID-002.json`, `REVIEW.md`
    Dependencies: `EVID-001`
    Estimated scope: M
    Completion signal: receipt freshness policy cannot silently diverge between completion and release.

- [ ] `EVID-003` Model external evidence and waiver classes explicitly

    Spec: `specs/300426-receipt-artifact-and-evidence-binding.md`
    Why now: Some tasks require live or external evidence that cannot be represented by local receipts, but the current completion model mainly distinguishes local verification repairs and external follow-ups without a durable waiver/evidence contract.
    Codebase evidence: `src/completion_artifacts.rs` has `assess_task_completion_gap` and external-verification detection; `src/ship_command.rs` records bypass reasons in `SHIP.md`; `REVIEW.md` is the host handoff surface.
    Source of truth: `src/completion_artifacts.rs`, `REVIEW.md`, `SHIP.md`
    Runtime owner: `src/completion_artifacts.rs`
    UI consumers: `auto parallel` completion reconciliation, `auto ship`, `REVIEW.md`, `SHIP.md`
    Generated artifacts: `.auto/symphony/verification-receipts/EVID-003.json`, `SHIP.md`
    Fixture boundary: production cannot promote fixture external evidence or model-written command claims as host receipts.
    Retired surfaces: review prose that calls model observations a host receipt
    Owns: `src/completion_artifacts.rs`, `src/ship_command.rs`, `REVIEW.md`
    Integration touchpoints: task markdown `Verification:`, `Completion artifacts:`, ship bypass sections, review handoff rendering
    Scope boundary: evidence classification and readback only; do not add external service integrations.
    Acceptance criteria: host receipt, model narrative, external evidence, operator waiver, archive record, and blocked evidence states are explicit in completion/readiness output; waiver records include owner, risk, and expiry or follow-up condition.
    Verification: `scripts/run-task-verification.sh EVID-003 cargo test completion_artifacts::tests::checked_row_empty_review_uses_explicit_evidence_class`; `scripts/run-task-verification.sh EVID-003 cargo test completion_artifacts::tests::archive_backed_checked_row_is_fully_evidenced`; `scripts/run-task-verification.sh EVID-003 cargo test ship_command::tests::ship_gate_bypass_records_operator_reason`; `rg -n "host receipt|model narrative|external evidence|operator waiver|archive record" src/completion_artifacts.rs src/ship_command.rs REVIEW.md`
    Required tests: `completion_artifacts::tests::checked_row_empty_review_uses_explicit_evidence_class`, `completion_artifacts::tests::archive_backed_checked_row_is_fully_evidenced`, `ship_command::tests::ship_gate_bypass_records_operator_reason`
    Contract generation: `scripts/run-task-verification.sh EVID-003 cargo test completion_artifacts::tests::checked_row_empty_review_uses_explicit_evidence_class`
    Cross-surface tests: completion evidence output and `SHIP.md` bypass output use the same evidence-class names
    Review/closeout: reviewer confirms narrative-only proof cannot satisfy executable verification and that waivers remain visible until replaced by proof.
    Completion artifacts: `.auto/symphony/verification-receipts/EVID-003.json`, `REVIEW.md`
    Dependencies: `EVID-002`
    Estimated scope: M
    Completion signal: non-local evidence is visible and never confused with host receipts.

- [ ] `CHECK-003` Row schema and evidence checkpoint

    Spec: `specs/300426-execution-row-schema-parity.md`
    Why now: Shared row validation and evidence binding change the scheduler truth model, so the queue should stop before dispatch/resume behavior is tightened.
    Codebase evidence: `src/task_parser.rs`, `src/completion_artifacts.rs`, and `src/ship_command.rs` now own the contracts that later scheduler work will consume.
    Source of truth: `REVIEW.md`, `.auto/symphony/verification-receipts/`
    Runtime owner: none
    UI consumers: `REVIEW.md`, `auto super`, `auto parallel`, `auto ship`
    Generated artifacts: `.auto/symphony/verification-receipts/CHECK-003.json`
    Fixture boundary: production cannot use fixture rows or receipts as active queue truth; checkpoint evidence must be current test receipts and review readback.
    Retired surfaces: none
    Owns: `REVIEW.md`
    Integration touchpoints: `src/task_parser.rs`, `src/completion_artifacts.rs`, `src/ship_command.rs`, `src/super_command.rs`
    Scope boundary: checkpoint only; do not launch scheduler work or promote generated rows.
    Acceptance criteria: `REVIEW.md` records row-validator parity, artifact containment, shared receipt inspector status, explicit evidence classes, and any remaining schema/evidence blockers.
    Verification: `scripts/run-task-verification.sh CHECK-003 rg -n "CHECK-003|ROW-001|ROW-002|EVID-001|EVID-002|EVID-003" REVIEW.md`; `rg -n "validate_.*execution|declared_artifact_path|shared receipt|operator waiver" src/task_parser.rs src/completion_artifacts.rs src/ship_command.rs`
    Required tests: none
    Contract generation: `scripts/run-task-verification.sh CHECK-003 rg -n "CHECK-003" REVIEW.md`
    Cross-surface tests: review readback links validator and evidence names to the CLI surfaces that consume them
    Review/closeout: reviewer confirms no scheduler task starts while schema or evidence blockers remain unresolved.
    Completion artifacts: `REVIEW.md`, `.auto/symphony/verification-receipts/CHECK-003.json`
    Dependencies: `CHECK-003A`, `EVID-001`, `EVID-002`, `EVID-003`
    Estimated scope: XS
    Completion signal: execution-row and evidence contracts are stable enough for scheduler hardening.

- [ ] `SCHED-001` Fail closed on current plan refresh failure

    Spec: `specs/300426-scheduler-completion-and-lane-resume.md`
    Why now: Production dispatch currently continues from a last-good queue snapshot when current plan refresh fails, which can schedule stale work after root queue or dependency truth changes.
    Codebase evidence: `src/parallel_command.rs` implements `refresh_parallel_plan_or_last_good` and logs that it is continuing with the last good snapshot; scheduler tests already cover ready/blocked parsing and stale status display.
    Source of truth: `src/parallel_command.rs`, `src/task_parser.rs`
    Runtime owner: `src/parallel_command.rs`
    UI consumers: `auto parallel`, `auto parallel status`, `.auto/parallel/live.log`
    Generated artifacts: `.auto/parallel/live.log`, `.auto/symphony/verification-receipts/SCHED-001.json`
    Fixture boundary: production scheduler code must inspect the live root plan and must not rely on fixture or last-good queue data without explicit recovery mode.
    Retired surfaces: default last-good queue dispatch for production mode
    Owns: `src/parallel_command.rs`
    Integration touchpoints: Linear auto-sync fallback, `inspect_loop_plan`, `ready_parallel_tasks`, `.auto/parallel/live.log`
    Scope boundary: dispatch fail-closed and explicit recovery mode only; do not change task parsing or lane metadata.
    Acceptance criteria: if current `IMPLEMENTATION_PLAN.md` refresh or parse fails, `auto parallel` spawns no new lanes unless explicit recovery mode is selected; recovery mode logs that it is using a last-good snapshot.
    Verification: `scripts/run-task-verification.sh SCHED-001 cargo test parallel_command::tests::parallel_launch_fails_closed_when_plan_refresh_fails`; `scripts/run-task-verification.sh SCHED-001 cargo test parallel_command::tests::parallel_recovery_mode_allows_last_good_snapshot_with_warning`; `rg -n "refresh_parallel_plan_or_last_good|last good queue snapshot|recovery mode" src/parallel_command.rs`
    Required tests: `parallel_command::tests::parallel_launch_fails_closed_when_plan_refresh_fails`, `parallel_command::tests::parallel_recovery_mode_allows_last_good_snapshot_with_warning`
    Contract generation: `scripts/run-task-verification.sh SCHED-001 cargo test parallel_command::tests::parallel_launch_fails_closed_when_plan_refresh_fails`
    Cross-surface tests: live-log fixture and stdout fixture both say fail-closed or explicit recovery using the same wording
    Review/closeout: reviewer checks the failing-refresh fixture creates no lane directories and no worker prompt.
    Completion artifacts: `.auto/symphony/verification-receipts/SCHED-001.json`, `REVIEW.md`
    Dependencies: `CHECK-003`
    Estimated scope: M
    Completion signal: stale queue snapshots are opt-in recovery, not normal production dispatch.

- [ ] `SCHED-002` Persist lane assignment metadata and reject stale resume

    Spec: `specs/300426-scheduler-completion-and-lane-resume.md`
    Why now: Resume candidates are keyed by task id and prompt fallback, so changed task bodies, dependencies, verification text, branch, or base commit can be resumed as if they were the same assignment.
    Codebase evidence: `src/parallel_command.rs` writes `lane-*/task-id`, infers task id from prompt names, discovers resume candidates by current pending/partial task id, and already computes lane base commits for landing.
    Source of truth: `src/parallel_command.rs`, `src/task_parser.rs`
    Runtime owner: `src/parallel_command.rs`
    UI consumers: `auto parallel`, `auto parallel status`, lane prompts, `.auto/parallel/live.log`
    Generated artifacts: `.auto/parallel/lanes/lane-*/assignment.json`, `.auto/symphony/verification-receipts/SCHED-002.json`
    Fixture boundary: production lane resume must inspect live lane repos and live root plan metadata; fixture metadata belongs only in tests.
    Retired surfaces: lane resume keyed only by `task-id`
    Owns: `src/parallel_command.rs`
    Integration touchpoints: `write_lane_task_id`, `discover_resume_candidates`, `prepare_parallel_lane_assignment`, lane prompt generation
    Scope boundary: assignment metadata and resume rejection only; do not change landing/cherry-pick recovery semantics.
    Acceptance criteria: assignment metadata records task id, task body hash, dependency hash, verification hash, branch, base commit, worker command/model, and assignment hash; resume rejects mismatches with the stale field name.
    Verification: `scripts/run-task-verification.sh SCHED-002 cargo test parallel_command::tests::lane_assignment_metadata_rejects_changed_task_body`; `scripts/run-task-verification.sh SCHED-002 cargo test parallel_command::tests::lane_assignment_metadata_rejects_changed_dependencies`; `scripts/run-task-verification.sh SCHED-002 cargo test parallel_command::tests::lane_assignment_metadata_rejects_changed_verification_text`; `rg -n "assignment.json|assignment hash|LANE_TASK_ID_FILE|discover_resume_candidates" src/parallel_command.rs`
    Required tests: `parallel_command::tests::lane_assignment_metadata_rejects_changed_task_body`, `parallel_command::tests::lane_assignment_metadata_rejects_changed_dependencies`, `parallel_command::tests::lane_assignment_metadata_rejects_changed_verification_text`
    Contract generation: `scripts/run-task-verification.sh SCHED-002 cargo test parallel_command::tests::lane_assignment_metadata_rejects_changed_task_body`
    Cross-surface tests: `auto parallel status` fixture reports stale assignment metadata instead of active progress
    Review/closeout: reviewer mutates one assignment field in a fixture and confirms resume fails with the exact field name.
    Completion artifacts: `.auto/symphony/verification-receipts/SCHED-002.json`, `REVIEW.md`
    Dependencies: `SCHED-001`
    Estimated scope: M
    Completion signal: lane resume is bound to the current task contract, not just a task id.

- [ ] `SCHED-003` Add scheduler safety verdict and loop-parallel ready-set parity

    Spec: `specs/300426-scheduler-completion-and-lane-resume.md`
    Why now: Operators need one launch/resume/land safety verdict, and serial and parallel execution should choose the same ready task set from the same validated root plan.
    Codebase evidence: `src/parallel_command.rs` already prints health, frontier, stale recovery, and lane state; `src/parallel_command.rs` contains ready task logic; `src/loop_command.rs` owns serial loop selection.
    Source of truth: `src/parallel_command.rs`, `src/loop_command.rs`, `src/task_parser.rs`, `src/completion_artifacts.rs`
    Runtime owner: `src/parallel_command.rs`, `src/loop_command.rs`
    UI consumers: `auto parallel status`, `auto parallel`, `auto loop`, `.auto/parallel/live.log`
    Generated artifacts: `.auto/parallel/live.log`, `.auto/symphony/verification-receipts/SCHED-003.json`
    Fixture boundary: production status must inspect live plan, lane dirs, pids, receipts, and tmux state; synthetic run roots belong only in tests.
    Retired surfaces: status output that lists raw state without safe/unsafe next action
    Owns: `src/parallel_command.rs`, `src/loop_command.rs`, `tests/parallel_status.rs`
    Integration touchpoints: `format_parallel_blocker_frontier`, `render_parallel_health_summary`, ready task selection, completion evidence classification
    Scope boundary: status verdict and ready-set parity only; do not add stale cleanup automation.
    Acceptance criteria: `auto parallel status` prints a single safety verdict for launch/resume/landing; loop and parallel derive identical ready task ids from a shared fixture plan after validation.
    Verification: `scripts/run-task-verification.sh SCHED-003 cargo test parallel_command::tests::parallel_status_prints_launch_resume_land_safety_verdict`; `scripts/run-task-verification.sh SCHED-003 cargo test loop_command::tests::loop_and_parallel_ready_sets_match_for_schema_fixture`; `scripts/run-task-verification.sh SCHED-003 cargo test --test parallel_status parallel_status_reports_stale_lane_recovery_without_live_host`; `cargo run --quiet -- parallel status`
    Required tests: `parallel_command::tests::parallel_status_prints_launch_resume_land_safety_verdict`, `loop_command::tests::loop_and_parallel_ready_sets_match_for_schema_fixture`, `parallel_status_reports_stale_lane_recovery_without_live_host`
    Contract generation: `scripts/run-task-verification.sh SCHED-003 cargo test parallel_command::tests::parallel_status_prints_launch_resume_land_safety_verdict`
    Cross-surface tests: fixture status output and live `auto parallel status` readback both include the safety verdict
    Review/closeout: reviewer confirms degraded state is labeled stale, blocked, unsafe, or safe with a current evidence source.
    Completion artifacts: `.auto/symphony/verification-receipts/SCHED-003.json`, `REVIEW.md`
    Dependencies: `SCHED-001`, `SCHED-002`
    Estimated scope: M
    Completion signal: scheduler status tells operators whether to launch, resume, recover, or stop.

- [ ] `CHECK-004` Scheduler dispatch and resume checkpoint

    Spec: `specs/300426-scheduler-completion-and-lane-resume.md`
    Why now: Scheduler changes can strand or resume lane work incorrectly, so release/verdict work should wait until dispatch, resume, and status safety are reviewed.
    Codebase evidence: `src/parallel_command.rs`, `src/loop_command.rs`, and `tests/parallel_status.rs` now own fail-closed dispatch, assignment hashes, and safety verdict readback.
    Source of truth: `REVIEW.md`, `.auto/parallel/live.log`
    Runtime owner: none
    UI consumers: `REVIEW.md`, `auto parallel status`
    Generated artifacts: `.auto/symphony/verification-receipts/CHECK-004.json`, `.auto/parallel/live.log`
    Fixture boundary: production cannot use stale lane fixtures as current progress; checkpoint evidence must cite live status or mark it as fixture-only.
    Retired surfaces: none
    Owns: `REVIEW.md`
    Integration touchpoints: `src/parallel_command.rs`, `src/loop_command.rs`, `tests/parallel_status.rs`
    Scope boundary: checkpoint only; do not prune, reset, or resume lanes.
    Acceptance criteria: `REVIEW.md` records plan-refresh fail-closed proof, assignment metadata proof, ready-set parity proof, and current live `auto parallel status` safety verdict.
    Verification: `scripts/run-task-verification.sh CHECK-004 rg -n "CHECK-004|SCHED-001|SCHED-002|SCHED-003" REVIEW.md`; `cargo run --quiet -- parallel status`; `rg -n "safety verdict|assignment.json|last good queue snapshot" src/parallel_command.rs src/loop_command.rs`
    Required tests: none
    Contract generation: `scripts/run-task-verification.sh CHECK-004 rg -n "CHECK-004" REVIEW.md`
    Cross-surface tests: live status readback and review handoff agree on safe or unsafe scheduler state
    Review/closeout: reviewer confirms no stale recovery lane is described as active progress.
    Completion artifacts: `REVIEW.md`, `.auto/symphony/verification-receipts/CHECK-004.json`
    Dependencies: `SCHED-001`, `SCHED-002`, `SCHED-003`
    Estimated scope: XS
    Completion signal: scheduler state is ready for release-gate hardening or explicitly blocked.

- [ ] `REL-001` Share exact terminal verdict parsing across report gates

    Spec: `specs/300426-release-gate-and-verdict-parser.md`
    Why now: Design, audit, and book reports currently use local any-line checks for positive verdicts, so mixed or duplicate verdict reports can pass in one surface while failing in another.
    Codebase evidence: `src/design_command.rs` accepts any line exactly `Verdict: GO`; `src/audit_everything.rs` uses `final_review_is_go` and `first_verdict_line`; `src/book_command.rs` accepts any line `Verdict: PASS`.
    Source of truth: `src/verdict.rs`, `src/design_command.rs`, `src/audit_everything.rs`, `src/book_command.rs`
    Runtime owner: `src/verdict.rs`
    UI consumers: `.auto/design/*/DESIGN-REPORT.md`, `.auto/audit-everything/*/FINAL-REVIEW.md`, `CODEBASE-BOOK/BOOK-QUALITY-REVIEW.md`, terminal gate output
    Generated artifacts: `.auto/symphony/verification-receipts/REL-001.json`
    Fixture boundary: production cannot treat fixture reports as release or design truth; tests must use synthetic mixed-verdict reports only.
    Retired surfaces: any-line verdict scans
    Owns: `src/verdict.rs`, `src/design_command.rs`, `src/audit_everything.rs`, `src/book_command.rs`
    Integration touchpoints: design resolve gate, audit final review, book quality review, report prompts
    Scope boundary: parser and callsite delegation only; do not alter report content prompts beyond final verdict format wording.
    Acceptance criteria: exactly one terminal verdict in the expected final location is accepted; missing, mixed, duplicated, or contradicted verdicts fail closed in design, audit, and book gates.
    Verification: `scripts/run-task-verification.sh REL-001 cargo test design_command::tests::design_report_rejects_mixed_verdicts`; `scripts/run-task-verification.sh REL-001 cargo test audit_everything::tests::final_review_rejects_mixed_verdicts`; `scripts/run-task-verification.sh REL-001 cargo test book_command::tests::quality_review_rejects_duplicate_verdicts`; `rg -n "Verdict:|parse_.*verdict|final_review_is_go|quality_review_is_pass" src/verdict.rs src/design_command.rs src/audit_everything.rs src/book_command.rs`
    Required tests: `design_command::tests::design_report_rejects_mixed_verdicts`, `audit_everything::tests::final_review_rejects_mixed_verdicts`, `book_command::tests::quality_review_rejects_duplicate_verdicts`
    Contract generation: `scripts/run-task-verification.sh REL-001 cargo test design_command::tests::design_report_rejects_mixed_verdicts`
    Cross-surface tests: the same mixed-verdict fixture fails design, audit, and book report gates
    Review/closeout: reviewer grep-confirms old local verdict helpers delegate to the shared parser or are removed.
    Completion artifacts: `.auto/symphony/verification-receipts/REL-001.json`, `REVIEW.md`
    Dependencies: `CHECK-004`
    Estimated scope: M
    Completion signal: report verdicts fail closed consistently across model-backed gates.

- [ ] `REL-002` Rerun ship readiness after sync and model iterations

    Spec: `specs/300426-release-gate-and-verdict-parser.md`
    Why now: `auto ship` evaluates the release gate before checkpoint/remote sync and does not rerun it after model-driven edits, so current-tree release readiness can go stale inside the ship flow.
    Codebase evidence: `src/ship_command.rs` calls `evaluate_ship_gate` before `auto_checkpoint_if_needed` or `sync_branch_with_remote`; after ship iterations it pushes/checkpoints without a final mechanical gate readback.
    Source of truth: `src/ship_command.rs`, `SHIP.md`
    Runtime owner: `src/ship_command.rs`
    UI consumers: `auto ship` stdout, `SHIP.md`, release reports
    Generated artifacts: `SHIP.md`, `.auto/ship/*`, `.auto/symphony/verification-receipts/REL-002.json`
    Fixture boundary: production release must read live reports, live receipts, live git state, and current branch refs; fixture reports cannot satisfy release proof.
    Retired surfaces: release readiness evaluated only before sync or before model edits
    Owns: `src/ship_command.rs`
    Integration touchpoints: `evaluate_ship_gate`, `record_ship_gate_blockers`, `record_ship_gate_bypass`, branch sync helpers, ship prompt logs
    Scope boundary: gate ordering and final status block only; do not automate deployment or remove explicit bypass support.
    Acceptance criteria: ship syncs/checkpoints current branch/base truth before the first gate, reruns the gate after each model iteration that changes the tree, blocks on stale receipts/reports, and writes a final status block to stdout and `SHIP.md`.
    Verification: `scripts/run-task-verification.sh REL-002 cargo test ship_command::tests::ship_gate_runs_after_remote_sync_before_model`; `scripts/run-task-verification.sh REL-002 cargo test ship_command::tests::ship_gate_reruns_after_model_iteration_changes`; `scripts/run-task-verification.sh REL-002 cargo test ship_command::tests::ship_gate_reports_stale_qa_or_health`; `rg -n "evaluate_ship_gate|sync_branch_with_remote|push_branch_with_remote_sync|final status" src/ship_command.rs`
    Required tests: `ship_command::tests::ship_gate_runs_after_remote_sync_before_model`, `ship_command::tests::ship_gate_reruns_after_model_iteration_changes`, `ship_command::tests::ship_gate_reports_stale_qa_or_health`
    Contract generation: `scripts/run-task-verification.sh REL-002 cargo test ship_command::tests::ship_gate_runs_after_remote_sync_before_model`
    Cross-surface tests: `auto ship` fixture stdout and `SHIP.md` fixture both show final status, blockers, and bypass state
    Review/closeout: reviewer traces a red-gate and green-gate fixture through sync, model iteration, post-iteration gate, and final status.
    Completion artifacts: `.auto/symphony/verification-receipts/REL-002.json`, `REVIEW.md`
    Dependencies: `REL-001`, `EVID-002`
    Estimated scope: M
    Completion signal: `auto ship` readiness is evaluated against the current tree at every release decision point.

- [ ] `LIFE-001` Settle Nemesis report-only and audit-pass contract

    Spec: `specs/300426-audit-nemesis-and-report-only-lifecycle.md`
    Why now: `auto nemesis --report-only` skips implementation but can still sync root specs and append root plan rows, while `--audit-passes` is advertised without a tested multi-pass or fail-fast contract.
    Codebase evidence: `src/nemesis.rs` gates implementation on `report_only` but still reaches root sync/append paths; `src/main.rs` exposes `audit_passes`; existing Nemesis tests focus backend selection, output verification, and plan append helpers.
    Source of truth: `src/nemesis.rs`, `src/main.rs`, `README.md`
    Runtime owner: `src/nemesis.rs`
    UI consumers: terminal help, README Nemesis command docs, root `specs/`, root `IMPLEMENTATION_PLAN.md`
    Generated artifacts: `nemesis/**`, `.auto/symphony/verification-receipts/LIFE-001.json`
    Fixture boundary: production cannot use fake Nemesis model output as current audit truth; tests must use temp repos and stubbed outputs.
    Retired surfaces: implicit root planning mutation from a command named report-only
    Owns: `src/nemesis.rs`, `src/main.rs`, `README.md`
    Integration touchpoints: `src/task_parser.rs`, `src/spec_command.rs`, root plan append helpers
    Scope boundary: Nemesis lifecycle flags only; do not redesign audit, bug, review, or steward pipelines.
    Acceptance criteria: report-only behavior is explicitly named, tested, and documented; `--audit-passes 2` either produces pass-specific artifacts or fails before model work with actionable unsupported messaging.
    Verification: `scripts/run-task-verification.sh LIFE-001 cargo test nemesis::tests::nemesis_report_only_contract_matches_help`; `scripts/run-task-verification.sh LIFE-001 cargo test nemesis::tests::nemesis_audit_passes_gt_one_is_truthful`; `rg -n "report_only|audit_passes|sync_nemesis_spec_to_root|append_nemesis_plan_to_root" src/nemesis.rs src/main.rs README.md`
    Required tests: `nemesis::tests::nemesis_report_only_contract_matches_help`, `nemesis::tests::nemesis_audit_passes_gt_one_is_truthful`
    Contract generation: `scripts/run-task-verification.sh LIFE-001 cargo test nemesis::tests::nemesis_report_only_contract_matches_help`
    Cross-surface tests: Nemesis fixture root-plan readback and README/help grep agree on whether report-only mutates root truth
    Review/closeout: reviewer confirms command help, README, runtime writes, and report artifacts describe the same Nemesis contract.
    Completion artifacts: `.auto/symphony/verification-receipts/LIFE-001.json`, `REVIEW.md`
    Dependencies: `REL-001`
    Estimated scope: M
    Completion signal: Nemesis lifecycle flags no longer surprise operators.

- [ ] `LIFE-002` Add model-free lifecycle fixture and evidence-label smoke coverage

    Spec: `specs/300426-audit-nemesis-and-report-only-lifecycle.md`
    Why now: Lifecycle commands should prove what they read, wrote, skipped, and blocked without relying on live model behavior or unlabeled model observations.
    Codebase evidence: `src/qa_only_command.rs`, `src/health_command.rs`, and `src/design_command.rs` already share report-only dirty-state helpers; `src/audit_everything.rs` already writes manifest-backed `RUN-STATUS.md`; Nemesis and book/review paths still need broader fixture coverage.
    Source of truth: `src/audit_everything.rs`, `src/nemesis.rs`, `src/review_command.rs`, `src/book_command.rs`
    Runtime owner: `src/audit_everything.rs`, `src/nemesis.rs`
    UI consumers: `RUN-STATUS.md`, `FINAL-REVIEW.md`, `REVIEW.md`, Nemesis reports, CODEBASE-BOOK review
    Generated artifacts: `.auto/audit-everything/**`, `audit/**`, `nemesis/**`, `.auto/symphony/verification-receipts/LIFE-002.json`
    Fixture boundary: production cannot import fixture reports as QA, health, audit, book, or Nemesis truth; fixture model binaries stay in test temp dirs.
    Retired surfaces: unlabeled model observations presented as host proof
    Owns: `tests/lifecycle_flows.rs`, `src/audit_everything.rs`, `src/nemesis.rs`, `src/book_command.rs`
    Integration touchpoints: `src/qa_only_command.rs`, `src/health_command.rs`, `src/design_command.rs`, `src/review_command.rs`
    Scope boundary: fixture harness and evidence labels only; do not invoke live model providers.
    Acceptance criteria: one model-free lifecycle fixture proves allowed write sets, required report presence, evidence labels, and failure messages for critical report-only or lifecycle paths.
    Verification: `scripts/run-task-verification.sh LIFE-002 cargo test --test lifecycle_flows lifecycle_fixture_rejects_unlabeled_or_unauthorized_lifecycle_claims`; `scripts/run-task-verification.sh LIFE-002 cargo test audit_everything::tests::run_status_markdown_records_pause_paths_and_task_counts`; `scripts/run-task-verification.sh LIFE-002 cargo test health_command::tests::health_report_only_rejects_disallowed_dirty_state`; `rg -n "Evidence Class|report-only|RUN-STATUS|lifecycle_fixture" src/audit_everything.rs src/nemesis.rs tests/lifecycle_flows.rs`
    Required tests: `lifecycle_fixture_rejects_unlabeled_or_unauthorized_lifecycle_claims`, `audit_everything::tests::run_status_markdown_records_pause_paths_and_task_counts`, `health_command::tests::health_report_only_rejects_disallowed_dirty_state`
    Contract generation: `scripts/run-task-verification.sh LIFE-002 cargo test --test lifecycle_flows lifecycle_fixture_rejects_unlabeled_or_unauthorized_lifecycle_claims`
    Cross-surface tests: fixture report artifacts and terminal-status assertions use the same evidence labels
    Review/closeout: reviewer opens the fixture artifacts and confirms failures name disallowed writes or unlabeled evidence rather than generic model errors.
    Completion artifacts: `tests/lifecycle_flows.rs`, `.auto/symphony/verification-receipts/LIFE-002.json`, `REVIEW.md`
    Dependencies: `LIFE-001`, `EVID-003`
    Estimated scope: M
    Completion signal: lifecycle honesty has model-free regression coverage.

- [ ] `DX-001` Extend doctor with active planning and queue health

    Spec: `specs/300426-first-run-dx-observability-and-performance.md`
    Why now: `auto doctor` is already a no-model first success, but it stops before reporting whether the active planning root, corpus shape, root queue, and generated snapshot state are usable.
    Codebase evidence: `src/doctor_command.rs` builds repo/help/tool checks and prints no-model guarantees; `src/task_parser.rs` and `src/corpus.rs` already expose the data needed for queue and corpus summaries; README already describes first-run flows.
    Source of truth: `src/doctor_command.rs`, `src/corpus.rs`, `src/task_parser.rs`, `README.md`
    Runtime owner: `src/doctor_command.rs`
    UI consumers: `auto doctor` stdout, README quickstart, CI smoke logs
    Generated artifacts: `.auto/symphony/verification-receipts/DX-001.json`
    Fixture boundary: production doctor output must read live repo files; tests may use temp repos with synthetic `genesis/` and `IMPLEMENTATION_PLAN.md`.
    Retired surfaces: first-run output that proves binary health but not planning-surface health
    Owns: `src/doctor_command.rs`, `README.md`
    Integration touchpoints: `src/corpus.rs`, `src/task_parser.rs`, `.github/workflows/ci.yml`
    Scope boundary: read-only checks only; do not invoke models, network APIs, Docker, browsers, tmux, Linear, or GitHub.
    Acceptance criteria: doctor reports planning-root provenance, corpus primary plan count, root queue pending/blocked/completed counts, generated snapshot status, and a safe next action without mutating files.
    Verification: `scripts/run-task-verification.sh DX-001 cargo test doctor_command::tests::doctor_reports_active_planning_and_queue_health`; `scripts/run-task-verification.sh DX-001 cargo test doctor_command::tests::doctor_checks_expected_help_surfaces`; `cargo run --quiet -- doctor`; `rg -n "auto doctor|planning root|queue health|snapshot-only|sync-only" src/doctor_command.rs README.md .github/workflows/ci.yml`
    Required tests: `doctor_command::tests::doctor_reports_active_planning_and_queue_health`, `doctor_command::tests::doctor_checks_expected_help_surfaces`
    Contract generation: `scripts/run-task-verification.sh DX-001 cargo test doctor_command::tests::doctor_reports_active_planning_and_queue_health`
    Cross-surface tests: doctor stdout fixture and README quickstart grep agree on the first non-mutating success path
    Review/closeout: reviewer runs local `auto doctor` and verifies required failures, optional warnings, and planning next action without leaking secrets.
    Completion artifacts: `.auto/symphony/verification-receipts/DX-001.json`, `REVIEW.md`
    Dependencies: `CHECK-002`, `SCHED-003`
    Estimated scope: M
    Completion signal: first-run doctor reduces uncertainty before model-backed workflows start.

- [ ] `CTRL-001` Choose the production-control promotion artifact

    Spec: `specs/300426-production-control-and-planning-primacy.md`
    Why now: The generated snapshot has been reviewed into root queue truth for this execution gate, but future production-control campaigns still need a durable promotion artifact policy so implementation does not assume root edits, `PROMOTION.md`, or `.auto/super` manifests without a recorded decision.
    Codebase evidence: `gen-20260430-184141/specs/` contains ten specs; `gen-20260430-184141/IMPLEMENTATION_PLAN.md` is present as the generated snapshot; root `IMPLEMENTATION_PLAN.md` now contains 26 priority rows and 4 follow-on rows; `.auto/state.json` points at `gen-20260430-184141`.
    Source of truth: `docs/decisions/production-control-promotion.md`, `src/generation.rs`, `src/super_command.rs`
    Runtime owner: none
    UI consumers: `auto gen`, `auto super`, `auto parallel status`, `auto doctor`, README lifecycle prose
    Generated artifacts: `.auto/symphony/verification-receipts/CTRL-001.json`
    Fixture boundary: production cannot import generated snapshots as active queue truth before reviewed promotion.
    Retired surfaces: generated snapshots treated as active doctrine without explicit promotion
    Owns: `docs/decisions/production-control-promotion.md`, `README.md`
    Integration touchpoints: `src/generation.rs`, `src/super_command.rs`, `src/parallel_command.rs`, `.auto/state.json`
    Scope boundary: decision and docs only; do not promote rows, sync root specs, or change `auto super` defaults.
    Acceptance criteria: decision doc chooses the promotion artifact, documents waiver shape, explains whether stale root specs are tombstoned at sync or separately, and defines how `auto doctor` or status surfaces report planning primacy.
    Verification: `scripts/run-task-verification.sh CTRL-001 rg -n "promotion artifact|root ledger|generated snapshot|waiver|tombstone" docs/decisions/production-control-promotion.md`; `rg -n "genesis/|gen-<timestamp>|snapshot-only|sync-only" README.md src/generation.rs src/super_command.rs`
    Required tests: none
    Contract generation: `scripts/run-task-verification.sh CTRL-001 rg -n "promotion artifact" docs/decisions/production-control-promotion.md`
    Cross-surface tests: README lifecycle grep agrees with the decision doc and does not present `genesis/` or `gen-*` as active queue truth
    Review/closeout: reviewer checks the decision does not silently override operator promotion choice and lists any waivers separately from readiness.
    Completion artifacts: `docs/decisions/production-control-promotion.md`, `.auto/symphony/verification-receipts/CTRL-001.json`
    Dependencies: `DX-001`, `CHECK-004`
    Estimated scope: S
    Completion signal: production-control promotion has a reviewed artifact before queue promotion happens.

- [ ] `PROMO-001` Gate generated queue promotion and release decision

    Spec: `specs/300426-release-decision-gate-and-queue-promotion.md`
    Why now: The current generated snapshot has been promoted into the root implementation queue for worker launch, and release readiness remains NO-GO until the safety, evidence, scheduler, lifecycle, and DX clusters are closed or waived.
    Codebase evidence: root `IMPLEMENTATION_PLAN.md` now has 26 unchecked priority rows and 4 unchecked follow-on rows, `auto parallel status` reports `health: healthy` with no running `autodev-parallel` tmux session, `SHIP.md`, `QA.md`, and `HEALTH.md` are absent, and `src/ship_command.rs` blocks missing release evidence.
    Source of truth: `gen-20260430-184141/IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `SHIP.md`, `src/generation.rs`, `src/parallel_command.rs`, `src/ship_command.rs`
    Runtime owner: `src/generation.rs`, `src/parallel_command.rs`, `src/ship_command.rs`
    UI consumers: `auto gen --sync-only`, `auto parallel status`, `auto ship`, root ledgers, `SHIP.md`
    Generated artifacts: `.auto/symphony/verification-receipts/PROMO-001.json`, `SHIP.md`
    Fixture boundary: production release decisions must read live root ledgers, receipts, reports, git state, and explicit waivers; fixture release reports cannot satisfy GO.
    Retired surfaces: generated specs or plans treated as active execution truth before promotion
    Owns: `REVIEW.md`, `SHIP.md`, `IMPLEMENTATION_PLAN.md`
    Integration touchpoints: `src/generation.rs`, `src/parallel_command.rs`, `src/completion_artifacts.rs`, `src/ship_command.rs`, `.auto/state.json`
    Scope boundary: decision and promotion gate only; do not implement remaining queue tasks inside this gate.
    Acceptance criteria: decision artifact lists every prerequisite cluster as closed, waived, or blocking; any promoted root rows pass the shared validator; `auto parallel status` reports a non-empty safe queue before launch or a clear NO-GO; release GO requires current local or CI-equivalent proof and current `SHIP.md`, `QA.md`, and `HEALTH.md`.
    Verification: `scripts/run-task-verification.sh PROMO-001 cargo test generation::tests::generated_plan_rejects_missing_spec_refs`; `scripts/run-task-verification.sh PROMO-001 cargo test ship_command::tests::ship_gate_fails_without_installed_binary_proof`; `cargo run --quiet -- parallel status`; `rg -n "^- \\[ \\]" IMPLEMENTATION_PLAN.md`; `find gen-20260430-184141/specs -maxdepth 1 -type f -name '*.md' -printf '%f\n' | sort`
    Required tests: `generation::tests::generated_plan_rejects_missing_spec_refs`, `ship_command::tests::ship_gate_fails_without_installed_binary_proof`
    Contract generation: `scripts/run-task-verification.sh PROMO-001 cargo test generation::tests::generated_plan_rejects_missing_spec_refs`
    Cross-surface tests: root queue grep, `auto parallel status`, and `auto ship` gate fixtures all report the same GO/NO-GO state
    Review/closeout: reviewer traces the decision back to QSEC, CSTATE, ROW, EVID, SCHED, REL, LIFE, DX, and CTRL tasks and confirms bypasses are visible and time-bounded.
    Completion artifacts: `.auto/symphony/verification-receipts/PROMO-001.json`, `REVIEW.md`, `SHIP.md`
    Dependencies: `CHECK-001`, `CHECK-002`, `CHECK-003`, `CHECK-004`, `REL-002`, `LIFE-002`, `CTRL-001`
    Estimated scope: M
    Completion signal: generated planning can be promoted or rejected from evidence, not momentum.

## Follow-On Work

- [ ] `QSEC-004` Implement decided Kimi and PI prompt transport policy

    Spec: `specs/300426-quota-backend-and-credential-safety.md`
    Why now: Prompt transport is high-risk but depends on the documented provider capability decision from `QSEC-003`; implementation should follow that evidence instead of guessing.
    Codebase evidence: `src/kimi_backend.rs` currently uses `-p <prompt>`; `src/pi_backend.rs` parses PI errors but local code has not proven stdin or file-prompt support; Codex and Claude paths already avoid raw prompt argv in their primary wrappers.
    Source of truth: `docs/decisions/quota-backend-prompt-transport.md`, `src/kimi_backend.rs`, `src/pi_backend.rs`
    Runtime owner: `src/kimi_backend.rs`, `src/pi_backend.rs`
    UI consumers: backend command logs, quota-router stderr, README provider notes
    Generated artifacts: `.auto/symphony/verification-receipts/QSEC-004.json`
    Fixture boundary: production cannot use fake CLI help as provider capability; implementation tests must reflect the decision artifact and fixture command construction only.
    Retired surfaces: unsafe argv prompt transport for providers that support stdin or prompt files
    Owns: `src/kimi_backend.rs`, `src/pi_backend.rs`, `src/codex_exec.rs`, `README.md`
    Integration touchpoints: `src/backend_policy.rs`, `src/quota_usage.rs`, `src/quota_status.rs`
    Scope boundary: Kimi/PI prompt transport and shared displayed-error sanitization only; do not change default model routing.
    Acceptance criteria: providers with verified stdin/file support move prompts off argv; providers without support show an explicit documented limitation; displayed provider errors pass through the shared sanitizer before terminal or durable operator logs.
    Verification: `scripts/run-task-verification.sh QSEC-004 cargo test kimi_backend::tests::exec_args_use_decided_prompt_transport`; `scripts/run-task-verification.sh QSEC-004 cargo test pi_backend::tests::pi_prompt_transport_matches_decision`; `scripts/run-task-verification.sh QSEC-004 cargo test quota_status::tests::renders_sanitized_provider_errors`; `rg -n "prompt transport|sanitize_quota_error_message|kimi_exec_args|parse_pi_error" src/kimi_backend.rs src/pi_backend.rs src/quota_status.rs README.md`
    Required tests: `kimi_backend::tests::exec_args_use_decided_prompt_transport`, `pi_backend::tests::pi_prompt_transport_matches_decision`, `quota_status::tests::renders_sanitized_provider_errors`
    Contract generation: `scripts/run-task-verification.sh QSEC-004 cargo test kimi_backend::tests::exec_args_use_decided_prompt_transport`
    Cross-surface tests: README provider note and backend argv fixture agree on the implemented transport
    Review/closeout: reviewer confirms prompt bodies do not appear in argv fixtures when safer transport is supported.
    Completion artifacts: `.auto/symphony/verification-receipts/QSEC-004.json`, `REVIEW.md`
    Dependencies: `QSEC-003`
    Estimated scope: M
    Completion signal: Kimi/PI prompt handling follows documented provider capability instead of speculation.

- [ ] `EVID-004` Formalize receipt schema and directory artifact hash limits

    Spec: `specs/300426-receipt-artifact-and-evidence-binding.md`
    Why now: Receipt fields are now important to both completion and release gates, but the JSON contract is implicit in Rust structs and the writer script, and directory hashing has no measured bound.
    Codebase evidence: `scripts/verification_receipt.py` writes receipt JSON; `src/completion_artifacts.rs` and `src/ship_command.rs` parse receipts; directory artifact hashing walks files without a documented maximum.
    Source of truth: `docs/verification-receipt-schema.md`, `scripts/verification_receipt.py`, `src/completion_artifacts.rs`
    Runtime owner: `src/completion_artifacts.rs`
    UI consumers: `auto parallel`, `auto ship`, `REVIEW.md`, `SHIP.md`
    Generated artifacts: `docs/verification-receipt-schema.md`, `.auto/symphony/verification-receipts/EVID-004.json`
    Fixture boundary: production cannot accept fixture receipt schemas or copied excerpts as current proof; schema fixtures must remain under tests or docs examples.
    Retired surfaces: implicit receipt schema known only by source readers
    Owns: `docs/verification-receipt-schema.md`, `scripts/verification_receipt.py`, `src/completion_artifacts.rs`
    Integration touchpoints: `scripts/run-task-verification.sh`, release gate receipt loading, declared artifact hashing
    Scope boundary: schema documentation, writer/parser conformance, and directory hash size limits only; do not add external evidence services.
    Acceptance criteria: receipt schema docs list required and optional fields, parser/writer conformance tests reject missing required fields, and directory artifact hashing has a measured max file-count or byte limit with clear blocker text.
    Verification: `scripts/run-task-verification.sh EVID-004 cargo test completion_artifacts::tests::receipt_schema_requires_current_metadata`; `scripts/run-task-verification.sh EVID-004 cargo test completion_artifacts::tests::directory_artifact_hashing_respects_documented_limit`; `rg -n "commit|dirty_state|plan_hash|expected_argv|declared_artifacts|directory limit" docs/verification-receipt-schema.md scripts/verification_receipt.py src/completion_artifacts.rs`
    Required tests: `completion_artifacts::tests::receipt_schema_requires_current_metadata`, `completion_artifacts::tests::directory_artifact_hashing_respects_documented_limit`
    Contract generation: `scripts/run-task-verification.sh EVID-004 cargo test completion_artifacts::tests::receipt_schema_requires_current_metadata`
    Cross-surface tests: schema fixture is accepted by completion and ship receipt readers through the shared inspector
    Review/closeout: reviewer compares a live receipt to the schema and confirms every release-required field is documented.
    Completion artifacts: `docs/verification-receipt-schema.md`, `.auto/symphony/verification-receipts/EVID-004.json`, `REVIEW.md`
    Dependencies: `EVID-002`
    Estimated scope: S
    Completion signal: receipt JSON is a documented contract, not an incidental writer shape.

- [ ] `DX-002` Add measured large-status performance fixtures

    Spec: `specs/300426-first-run-dx-observability-and-performance.md`
    Why now: The first-run spec asks for scale evidence, but exact performance targets should come only after deterministic large-plan and audit-status measurements exist.
    Codebase evidence: `src/parallel_command.rs` renders plan/frontier/status output; `src/audit_everything.rs` prints manifest-backed status; no current `tests/performance_status.rs` file exists in the repo file list.
    Source of truth: `tests/performance_status.rs`, `src/parallel_command.rs`, `src/audit_everything.rs`
    Runtime owner: `src/parallel_command.rs`, `src/audit_everything.rs`
    UI consumers: `auto parallel status`, `auto audit --everything --everything-phase status`, README performance notes
    Generated artifacts: `.auto/symphony/verification-receipts/DX-002.json`
    Fixture boundary: production cannot treat synthetic scale fixtures as live production evidence; performance numbers must be labeled as observations with input size and machine/context.
    Retired surfaces: unmeasured performance targets
    Owns: `tests/performance_status.rs`, `README.md`
    Integration touchpoints: `src/task_parser.rs`, `.auto/parallel/**`, `.auto/audit-everything/**`
    Scope boundary: deterministic status fixture and documentation only; do not optimize code unless the fixture exposes a clear regression.
    Acceptance criteria: large queue and large audit manifest fixtures render status under a recorded observation, and README labels the number as observation rather than a release target.
    Verification: `scripts/run-task-verification.sh DX-002 cargo test --test performance_status large_plan_status_renders_under_measured_observation`; `scripts/run-task-verification.sh DX-002 cargo test --test performance_status large_audit_status_renders_under_measured_observation`; `rg -n "performance observation|large plan|audit status" tests/performance_status.rs README.md`
    Required tests: `large_plan_status_renders_under_measured_observation`, `large_audit_status_renders_under_measured_observation`
    Contract generation: `scripts/run-task-verification.sh DX-002 cargo test --test performance_status large_plan_status_renders_under_measured_observation`
    Cross-surface tests: fixture stdout snippets match the status labels documented in README
    Review/closeout: reviewer records command, input size, elapsed observation, machine/context, and confirms no target is claimed without future decision.
    Completion artifacts: `tests/performance_status.rs`, `.auto/symphony/verification-receipts/DX-002.json`, `REVIEW.md`
    Dependencies: `DX-001`, `SCHED-003`
    Estimated scope: S
    Completion signal: first-run and status performance claims are grounded in measured fixtures.

- [ ] `CTRL-002` Decide whether super defaults should snapshot before root sync

    Spec: `specs/300426-production-control-and-planning-primacy.md`
    Why now: `auto super` currently runs the production-race pipeline through generation and gates, while the generated spec recommends treating snapshots as subordinate until reviewed promotion; changing the default needs an operator-facing decision.
    Codebase evidence: `src/super_command.rs` orchestrates corpus, design, functional review, generation, deterministic gate, and parallel launch; `src/generation.rs` supports `--snapshot-only` and `--sync-only`.
    Source of truth: `docs/decisions/super-snapshot-promotion-default.md`, `src/super_command.rs`, `src/generation.rs`
    Runtime owner: none
    UI consumers: `auto super`, `auto gen`, README lifecycle prose
    Generated artifacts: `.auto/symphony/verification-receipts/CTRL-002.json`
    Fixture boundary: production cannot use fixture super manifests or generated snapshots as active root queue truth.
    Retired surfaces: undocumented `auto super` root-sync semantics during production-control campaigns
    Owns: `docs/decisions/super-snapshot-promotion-default.md`, `README.md`
    Integration touchpoints: `src/super_command.rs`, `src/generation.rs`, `src/parallel_command.rs`
    Scope boundary: decision only; do not change `auto super` execution defaults until the decision is accepted.
    Acceptance criteria: decision doc states whether `auto super` should default to snapshot/review before sync, what flag preserves current behavior, and how deterministic gate output should explain the chosen mode.
    Verification: `scripts/run-task-verification.sh CTRL-002 rg -n "auto super|snapshot|sync-only|promotion|deterministic gate" docs/decisions/super-snapshot-promotion-default.md`; `rg -n "snapshot_only|sync_only|run_super|verify_parallel_ready_plan" src/super_command.rs src/generation.rs README.md`
    Required tests: none
    Contract generation: `scripts/run-task-verification.sh CTRL-002 rg -n "snapshot" docs/decisions/super-snapshot-promotion-default.md`
    Cross-surface tests: README super-mode prose and decision doc agree on root-sync behavior
    Review/closeout: reviewer confirms the decision preserves operator sovereignty and does not silently change execution semantics.
    Completion artifacts: `docs/decisions/super-snapshot-promotion-default.md`, `.auto/symphony/verification-receipts/CTRL-002.json`
    Dependencies: `CTRL-001`
    Estimated scope: S
    Completion signal: super promotion behavior is a deliberate product decision rather than an accidental default.

## Completed / Already Satisfied

- [x] `SAT-001` Report-only write-boundary helpers already exist for QA-only, health, and design surfaces.
  Evidence: `src/qa_only_command.rs` owns `report_only_dirty_state_report`; `src/health_command.rs` imports `require_nonempty_report` and dirty-state helpers; `src/design_command.rs` has `design_report_only_rejects_disallowed_dirty_state`.

- [x] `SAT-002` Rich plan-task required fields already have a shared catalog.
  Evidence: `src/task_parser.rs` defines `PLAN_TASK_REQUIRED_FIELDS`, `PLAN_TASK_PROCESS_FIELDS`, and `TASK_FIELD_BOUNDARIES`; `src/generation.rs`, `src/spec_command.rs`, and `src/super_command.rs` consume those fields.

- [~] `SAT-003` Verification receipt freshness already records important current-tree metadata.
  Evidence: `src/completion_artifacts.rs` checks commit, dirty-state fingerprint, plan hash, expected argv, failed commands, superseded failures, zero-test summaries, and declared artifact hashes; `src/ship_command.rs` has matching release-gate checks that `EVID-002` will consolidate.

- [~] `SAT-004` `auto doctor` already provides a no-model first-run proof for layout, binary metadata, help surfaces, and optional tools.
  Evidence: `src/doctor_command.rs` prints required checks, capabilities, and a no-model/network guarantee; `.github/workflows/ci.yml` smokes the installed binary help surfaces.

- [x] `SAT-005` Professional audit status and file-quality gates already exist.
  Evidence: `src/audit_everything.rs` owns `.auto/audit-everything`, `RUN-STATUS.md`, pause/unpause/status controls, final-review evidence classification, file-quality accept score `9.0`, and target score `10.0`.

- [~] `SAT-006` The planning corpus remains subordinate to root queue truth.
  Evidence: `gen-20260430-184141/specs/` contains ten generated specs and `gen-20260430-184141/corpus/**` contains the copied corpus, while the reviewed root `IMPLEMENTATION_PLAN.md` now contains the active promoted worker queue and references `gen-*` only as provenance or promotion evidence.
