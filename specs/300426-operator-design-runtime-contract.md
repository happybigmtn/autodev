# Specification: Operator Design Runtime Contract

## Objective

- Make autodev's terminal/operator surfaces a coherent, runtime-backed product interface.
- Prevent design claims, generated plans, and report-only commands from drifting away from source-owned command behavior.
- Ensure `auto design` and `auto super` can block or release downstream execution based on concrete runtime/UI contracts.

## Source Of Truth

- Runtime owners: `src/main.rs`, `src/design_command.rs`, `src/super_command.rs`, `src/generation.rs`, `src/spec_command.rs`, `src/task_parser.rs`, `src/parallel_command.rs`, `src/review_command.rs`, `src/qa_only_command.rs`, `src/health_command.rs`, `src/doctor_command.rs`, `src/ship_command.rs`, `src/audit_everything.rs`, and `src/nemesis.rs`.
- Planning owners: `DESIGN.md`, `README.md`, `AGENTS.md`, `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, root `specs/`, and `genesis/` as pre-generation input.
- UI consumers: terminal help, stdout summaries, prompt logs, Markdown reports, generated implementation plans, QA/health/design/review/audit reports, release gates, and tmux/CI log readers.
- Generated artifacts: `.auto/design/*`, `.auto/super/*`, `.auto/parallel/*`, `.auto/logs/*`, `gen-*`, `bug/`, `nemesis/`, generated specs, generated `IMPLEMENTATION_PLAN.md`, and verification receipts.
- Retired or superseded surfaces: any old command count, legacy task field list, stale dry-run/report-only claim, or generated plan shape that conflicts with runtime code.

## Evidence Status

- This spec is grounded in the live 2026-04-30 checkout and the design gate artifacts under `.auto/super/20260430-133225/design/pass-01`.
- A static frontend scan found no browser app, package manifest, route tree, component tree, or design-token stylesheet. The active design surface is terminal/operator UX.
- Narrow runtime QA covered `auto design --help`, `auto super --help`, `auto doctor`, `cargo test design_command::tests`, and `cargo test super_command::tests`.
- The pass-02 repair gate rechecked the same terminal/operator surface and found `auto parallel status` degraded by stale lane/recovery state plus older host warnings; root `IMPLEMENTATION_PLAN.md` also had open/partial rows without the full rich task contract required by the current deterministic gate.
- The pass-03 repair gate found the pass-02 runtime items mostly implemented and receipt-backed, but the operator ledgers still contradict each other: `TASK-016` remains partial in the active queue while `ARCHIVED.md` says it passed, and `REVIEW.md` still lists completed design items as blocked. This is product-state drift even though the relevant tests now pass.

## Runtime Contract

- `src/main.rs` owns the command inventory and public help labels.
- `src/design_command.rs` owns required design artifacts, verdict parsing, resolve behavior, and design-plan promotion.
- `src/super_command.rs` owns the production campaign sequence and pre-parallel deterministic gate.
- `src/generation.rs` and `src/spec_command.rs` own generated spec and plan contract validation.
- `src/task_parser.rs` owns task status, dependency, verification, and completion-artifact parsing.
- Report-only and dry-run commands must enforce and print their write boundaries through runtime checks, not prompt-only discipline.
- Gate commands must fail closed when required reports, receipts, generated artifacts, or source-owned fields are absent.
- Parallel/status and release/readiness surfaces must classify stale lane state, recovery work, dependency frontier, review handoff gaps, and receipt drift as operator-visible product state rather than burying them in logs.
- Review, archive, completion, active queue, receipt, and tag ledgers must be reconciled before a design gate can return GO. A green runtime fix with stale handoff rows is still a misleading operator interface.

## UI Contract

- The terminal UI renders runtime facts and must not duplicate scheduler status, dependency readiness, receipt validity, model routing, release readiness, or credential state in docs or prompts without a runtime owner.
- Every operator-facing long command should end with a stable final status block: status, files written, receipts, blockers, and next command.
- Help and docs must describe required tools by workflow, separating no-model preflight from model-backed execution and GitHub/Symphony integrations.
- Queue, review, archive, completion, tag, and receipt ledgers must not contradict each other at gate time. If they do, the UI must surface a concrete reconciliation task or blocker.
- Any future web UI must consume the same runtime-owned contracts and generated bindings. Fake dashboards, fixture fallbacks, manual catalogs, or copied task logic are out of scope for production acceptance.

## Generated Artifacts

- `auto design` generates six required Markdown artifacts plus optional resolve status and parallel artifacts.
- `auto super` generates CEO, functional review, design, execution gate, deterministic gate, branch reconciliation, and final sanity artifacts.
- `auto gen` generates dated specs and `IMPLEMENTATION_PLAN.md` from `genesis/`.
- Verification receipts must identify the current commit, dirty state, plan identity, and relevant artifact hashes when used as completion or release proof.
- `.auto/parallel/*` status, salvage, and host-warning artifacts are generated evidence; they are not completion proof until reconciled with root queue/review state and current receipts.

## Fixture Policy

- Fixture, demo, sample, and synthetic data may be used for tests and local command smoke checks only.
- Production command output must not fall back to fixture task state, sample queue rows, fake receipts, or mock UI facts.
- Browser or preview artifacts are non-authoritative unless wired to runtime-owned data and covered by readback proof.

## Retired / Superseded Surfaces

- Supersede docs or specs that describe an old command count, old model defaults, old task field list, or report-only semantics that the current runtime no longer implements.
- Tombstone or update stale generated design/planning claims before `auto gen` can amplify them.
- Root `DESIGN.md` is the durable design doctrine; `genesis/DESIGN.md` remains planning input.

## Acceptance Criteria

- Root `DESIGN.md` exists and describes terminal/operator UX as the real product interface.
- `auto super` and `auto gen` enforce compatible task-contract fields for source of truth, runtime owner, UI consumers, generated artifacts, fixture boundary, contract generation, cross-surface proof, and closeout review.
- Report-only and dry-run commands either enforce their allowed write sets or clearly fail with the changed paths.
- `auto design --resolve` preserves or promotes unresolved NO-GO design items before it exits.
- CLI help and doctor output have tests or smoke checks for high-value operator surfaces.
- `auto parallel status` reports stale/recovery lanes and warning freshness in a way that lets the operator decide whether to reset, resume, or ignore old run state.
- Receipt-backed completion evidence rejects stale or mismatched receipts when commit, dirty-state, plan, or declared artifacts no longer match current repo truth.
- Active queue, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, verification receipts, and release tags agree on every design/release item before generation or execution is unblocked.

## Verification

- `cargo test design_command::tests`
- `cargo test super_command::tests`
- `cargo test task_parser::tests::dependencies_none_and_multiline_notes_are_stable`
- `cargo run --quiet -- doctor`
- `cargo run --quiet -- design --help`
- `cargo run --quiet -- super --help`
- `rg -n "Operator Design System|DESIGN-00|Verdict:" DESIGN.md IMPLEMENTATION_PLAN.md .auto/super/20260430-133225/design/pass-01`
- `cargo run --quiet -- parallel status`

## Review And Closeout

- Review must compare runtime command behavior against `DESIGN.md`, this spec, and generated plan rows.
- Closeout must include command output proof, task-contract proof, write-boundary proof, and design NO-GO promotion proof.
- A simple compile is not enough when the original risk is runtime/design drift.

## Open Questions

- Should final status blocks be centralized in a new helper module or stabilized command by command first?
- Should `auto doctor` and `auto parallel status` gain JSON output in the first design tranche, or wait until text labels are stable?
- Should CI run installed binary smoke for every command help surface or only high-value commands?
