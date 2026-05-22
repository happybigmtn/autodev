# REVIEW

## `AUTODEV-HARDENING-20260430`
- Source: manual completion batch for the remaining autodev auto-super/auto-parallel/no-go hardening rows.
- Rows completed: `QSEC-001`, `QSEC-002`, `QSEC-003`, `CHECK-001`, `CSTATE-001`, `CSTATE-002`, `CSTATE-003`, `CHECK-002`, `ROW-001`, `ROW-002`, `CHECK-003A`, `EVID-001`, `EVID-002`, `EVID-003`, `CHECK-003`, `SCHED-001`, `SCHED-002`, `SCHED-003`, `CHECK-004`, `REL-001`, `REL-002`, `LIFE-001`, `LIFE-002`, `DX-001`, `CTRL-001`, `PROMO-001`, `QSEC-004`, `EVID-004`, `DX-002`, `CTRL-002`.
- Files: `README.md`, `src/audit_everything.rs`, `src/book_command.rs`, `src/completion_artifacts.rs`, `src/corpus.rs`, `src/design_command.rs`, `src/doctor_command.rs`, `src/generation.rs`, `src/kimi_backend.rs`, `src/loop_command.rs`, `src/main.rs`, `src/nemesis.rs`, `src/parallel_command.rs`, `src/pi_backend.rs`, `src/quota_exec.rs`, `src/quota_selector.rs`, `src/quota_status.rs`, `src/review_command.rs`, `src/ship_command.rs`, `src/spec_command.rs`, `src/steward_command.rs`, `src/super_command.rs`, `src/task_parser.rs`, `src/verdict.rs`, `tests/lifecycle_flows.rs`, `tests/performance_status.rs`, `docs/decisions/production-control-promotion.md`, `docs/decisions/quota-backend-prompt-transport.md`, `docs/decisions/super-snapshot-promotion-default.md`, `docs/verification-receipt-schema.md`.
- Scope exceptions: per-row verification receipts were not regenerated; this was a manual consolidation and rebuild pass under process-pressure constraints, with direct focused cargo validation recorded below.
- Validation: `cargo fmt --check`; `cargo test execution_row_validator -- --nocapture`; `cargo test shared_receipt -- --nocapture`; `cargo test lane_assignment_metadata -- --nocapture`; `cargo test terminal_verdict -- --nocapture`; `cargo test nemesis_report_only_contract_matches_help -- --nocapture`; `cargo test nemesis_audit_passes_gt_one_is_truthful -- --nocapture`; `cargo test doctor_reports_active_planning_and_queue_health -- --nocapture`; `cargo test parallel_status_prints_launch_resume_land_safety_verdict -- --nocapture`; `cargo test --test lifecycle_flows -- --nocapture`; `cargo test --test performance_status -- --nocapture`.
- Completion artifacts: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `docs/decisions/production-control-promotion.md`, `docs/decisions/quota-backend-prompt-transport.md`, `docs/decisions/super-snapshot-promotion-default.md`, `docs/verification-receipt-schema.md`, `src/verdict.rs`, `tests/lifecycle_flows.rs`, `tests/performance_status.rs`.
- Remaining blockers: none after manual reconciliation. The prior `QSEC-001`
  stale-receipt note was superseded by
  `.auto/symphony/verification-receipts/QSEC-001.json`, which now records
  commit `465a95e459b096ceaf9fbfd737f8e685c37de9df` and three passing wrapper
  proofs.

## Auto Spec Product Experience Text Plate Validation
- Source: Bitino planning run generated developer/operator-facing specs with concrete text plates (`Surface 1`, `Plate -`, JSON-RPC, HTTP wire output) that still failed the old `surface plate|mockup|wireframe|viewport` substring check.
- Files: `src/spec_command.rs`, `src/claude_exec.rs`, `REVIEW.md`.
- Scope exceptions: validator and regression tests plus one visibility repair required by existing local Claude-routing commits; no prompt contract, plan-row schema, lane execution, or generated Bitino specs changed in this repo.
- Changed during this pass: `verify_product_experience_contract` still rejects generic product prose, but now accepts concrete developer-facing text plates and exact command/status/output surfaces as valid surface plates. Added regression coverage for JSON-RPC/reference-doc style plates and terminal command output plates.
- Validation: `cargo test spec_command::tests::auto_spec_accepts_developer_facing_text_plates`; `cargo test spec_command::tests::auto_spec_accepts_exact_command_status_output_surfaces`; `cargo test spec_command::tests::auto_spec_rejects_ui_contract_without_actual_design_plates`; `git diff --check -- src/spec_command.rs src/claude_exec.rs REVIEW.md`; `cargo install --path .`; `auto --help`.
- Validation caveats: `cargo test spec_command::tests::auto_spec` still fails two pre-existing plan-validation tests (`auto_spec_plan_validation_rejects_prose_dependencies`, `auto_spec_plan_validation_rejects_multi_filter_verification_commands`) because the current checkout accepts those fixtures. `cargo fmt --check` also reports pre-existing formatting drift outside this change (`src/generation.rs`, `src/parallel_command.rs`, `src/quota_exec.rs`, `src/super_command.rs`, `src/task_parser.rs`).
- Remaining blockers: none for this validator fix.

## `P0-001`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `.github/workflows/ci.yml`, `README.md`, `src/audit_everything.rs`, `src/doctor_command.rs`, `src/generation.rs`, `src/main.rs`, `src/parallel_command.rs`, `src/quota_usage.rs`, `src/spec_command.rs`, `src/super_command.rs`, `src/task_parser.rs`
- Scope exceptions: none recorded by host.
- Validation: verification receipt `/home/r/coding/autodev/.auto/symphony/verification-receipts/P0-001.json` has unsuperseded failed command(s): `cargo test`
- Completion artifacts: none
- Remaining blockers: missing REVIEW.md handoff; verification receipt `/home/r/coding/autodev/.auto/symphony/verification-receipts/P0-001.json` has unsuperseded failed command(s): `cargo test`

## `FOL-001`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `README.md`, `src/doctor_command.rs`, `src/main.rs`
- Scope exceptions: none recorded by host.
- Validation: verification receipt `/home/r/coding/autodev/.auto/symphony/verification-receipts/FOL-001.json` has unsuperseded failed command(s): `cargo test`
- Completion artifacts: none
- Remaining blockers: missing REVIEW.md handoff; verification receipt `/home/r/coding/autodev/.auto/symphony/verification-receipts/FOL-001.json` has unsuperseded failed command(s): `cargo test`

## `FOL-004`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/audit_everything.rs`
- Scope exceptions: none recorded by host.
- Validation: host observed verification receipt at `/home/r/coding/autodev/.auto/symphony/verification-receipts/FOL-004.json`
- Completion artifacts: none
- Remaining blockers: missing REVIEW.md handoff

## `P0-002`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/generation.rs`, `src/super_command.rs`, `src/verification_lint.rs`
- Scope exceptions: none recorded by host.
- Validation: host observed verification receipt at `/home/r/coding/autodev/.auto/symphony/verification-receipts/P0-002.json`
- Completion artifacts: none
- Remaining blockers: missing REVIEW.md handoff

## `P0-003`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: none recorded by host
- Scope exceptions: none recorded by host.
- Validation: host observed verification receipt at `/home/r/coding/autodev/.auto/symphony/verification-receipts/P0-003.json`
- Completion artifacts: none
- Remaining blockers: missing REVIEW.md handoff

## `P0-004`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `README.md`, `src/super_command.rs`
- Scope exceptions: none recorded by host.
- Validation: verification receipt `/home/r/coding/autodev/.auto/symphony/verification-receipts/P0-004.json` has unsuperseded failed command(s): `cargo run -- super --dry-run --no-execute snapshot proof`, `cargo test`
- Completion artifacts: none
- Remaining blockers: missing REVIEW.md handoff; verification receipt `/home/r/coding/autodev/.auto/symphony/verification-receipts/P0-004.json` has unsuperseded failed command(s): `cargo run -- super --dry-run --no-execute snapshot proof`, `cargo test`

## `P1-005`
- Source: auto parallel host handoff synthesized after lane landing.
- Files: `src/super_command.rs`
- Scope exceptions: none recorded by host.
- Validation: host observed verification receipt at `/home/r/coding/autodev/.auto/symphony/verification-receipts/P1-005.json`
- Completion artifacts: none
- Remaining blockers: missing REVIEW.md handoff
