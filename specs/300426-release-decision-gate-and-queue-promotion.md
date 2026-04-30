# Specification: Release Decision Gate And Queue Promotion

## Objective

Define the final GO/NO-GO gate for promoting the generated production-readiness corpus into active root work, then deciding whether `auto parallel` or release-oriented execution can start. The gate must protect operator sovereignty: generated corpus intent is input, while root ledgers, live source, receipts, and explicit waivers remain the active decision record.

## Source Of Truth

- Runtime owner modules/APIs: `src/generation.rs` owns generated snapshot and root-sync behavior; `src/state.rs` owns `.auto/state.json`; `src/parallel_command.rs` owns scheduler queue parsing, status, lane launch, and host reconciliation; `src/completion_artifacts.rs` owns task completion evidence; `src/ship_command.rs` owns mechanical release-gate checks and `SHIP.md` reporting.
- UI/presentation consumers: `auto gen`, `auto parallel status`, `auto parallel`, `auto ship`, root `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `WORKLIST.md`, `SHIP.md`, `QA.md`, and `HEALTH.md`.
- Generated artifacts: `gen-*/corpus/**`, `gen-*/specs/*.md`, `gen-*/IMPLEMENTATION_PLAN.md`, `.auto/state.json`, `.auto/symphony/verification-receipts/**`, `.auto/parallel/**`, and root specs/ledgers only after an explicit reviewed sync or manual promotion.
- Retired/superseded surfaces: old `gen-*` snapshots, stale generated plans, and checked root rows without current evidence must not be treated as runnable queue truth.

## Evidence Status

Verified facts grounded in code or commands:
- `genesis/plans/012-release-decision-gate-and-queue-promotion.md` defines the final gate as a review of Plans 002-011, promotion strategy, scheduler launch decision, and release GO/NO-GO.
- `src/generation.rs` saves `planning_root` and `latest_output_dir` through `save_generation_state(...)`, and `src/state.rs` persists those paths in `.auto/state.json` with `atomic_write`.
- `src/generation.rs` implements `--snapshot-only` by saving generator state without syncing root outputs, and tests this behavior in `snapshot_only_generation_does_not_sync_root_outputs`.
- `src/parallel_command.rs` marks root queue and review files as host-owned state, reports `auto parallel status`, prints blocker/frontier details, and demotes `[x]` tasks when `inspect_task_completion_evidence(...)` says repo-local evidence is incomplete.
- `src/completion_artifacts.rs` treats review handoff, verification receipts, declared completion artifacts, and unresolved audit findings as completion evidence.
- `src/ship_command.rs` evaluates a mechanical release gate, records gate verdicts in `SHIP.md`, supports `--bypass-release-gate`, and currently evaluates the initial gate before checkpoint/remote sync/model iteration in `run_ship(...)`.
- Command evidence from this gate pass: `rg -n "^- \\[ \\]" IMPLEMENTATION_PLAN.md` shows 30 unchecked root rows, and `auto parallel status` reports `health: healthy`, no running `autodev-parallel` tmux session, and no lane directories.
- Command evidence from this review pass: `find gen-20260430-184141/specs -maxdepth 1 -type f -name '*.md' | wc -l` reported `10`, including this release-decision spec and the production-control spec.

Recommendations for the intended system:
- Treat Plan 012 as a decision gate, not as another implementation slice that can be run before Plans 002-011 have proof or explicit waivers.
- Future campaigns should prefer `auto gen --snapshot-only` or a reviewed narrow manual promotion before mutating root specs and ledgers.
- Promote only accepted, evidence-backed rows into root `IMPLEMENTATION_PLAN.md`; this pass promotes the 14-day production-race queue and leaves `genesis/` as planning input.
- Require `auto parallel status` to report a non-empty runnable queue, no stale scheduler state, and no high-severity blocker before `auto parallel` launch.
- Require CI-equivalent local proof or a cited current CI run before a release GO decision.

Hypotheses / unresolved questions:
- Whether `ARCHIVED.md` should become a first-class machine-readable completion evidence source is unresolved.
- Whether `auto ship` should rerun the mechanical release gate after remote sync and after each model iteration is a recommended hardening point, not a verified current behavior.
- Whether queue promotion should be implemented as `auto gen --sync-only`, `auto steward`, or manual reviewed edits depends on operator choice after inspecting this snapshot.

## Runtime Contract

`src/generation.rs`, `src/parallel_command.rs`, `src/completion_artifacts.rs`, and `src/ship_command.rs` own canonical promotion and release readiness facts. If generated specs, generated plans, root ledgers, receipts, scheduler status, or release artifacts are absent or stale, runtime commands must fail closed by reporting NO-GO, leaving root truth unchanged, and naming the missing evidence. A bypass is not readiness; it must remain visible in `SHIP.md` or the decision artifact until replaced by proof.

## UI Contract

CLI output and markdown ledgers render runtime-owned promotion status. They must not duplicate scheduler eligibility rules, receipt freshness rules, artifact containment checks, release-gate classifications, queue completion logic, or fixture fallback truth in prose-only conventions. `auto parallel status` and any release decision report should display the runtime evidence source for GO/NO-GO, the current runnable queue count, the blocker frontier, and any explicit waivers.

Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- `gen-20260430-184141/specs/*.md` is the generated spec snapshot for this pass.
- `gen-20260430-184141/IMPLEMENTATION_PLAN.md` is present as the generated execution-plan snapshot for this pass; the reviewed root `IMPLEMENTATION_PLAN.md` copy is now the active worker queue.
- `.auto/state.json` records the latest planning root and output directory when generator flows save state.
- `.auto/symphony/verification-receipts/**` records verification evidence consumed by completion and release checks.
- `SHIP.md`, `QA.md`, and `HEALTH.md` are release-adjacent reports consumed by `src/ship_command.rs`.
- Refresh commands: `auto gen --snapshot-only`, `auto gen --sync-only` after reviewed acceptance, `auto parallel status`, `auto ship`, and the CI-equivalent Cargo commands listed in `AGENTS.md`.

## Fixture Policy

Synthetic generated snapshots, fake receipts, fake CI outputs, and temp-repo scheduler fixtures belong in tests only. Production release decisions must read the live root ledgers, live generated snapshot paths, current receipts, actual git state, and real release artifacts. Production code must not import fixture/demo/sample data to satisfy queue readiness, receipt freshness, release evidence, or GO/NO-GO status.

## Retired / Superseded Surfaces

- Archived `gen-*` directories are historical snapshots, not active truth.
- Deleted or renamed genesis plan files are superseded by the current numbered plans under `genesis/plans/`.
- Root checked rows without current review/receipt/artifact evidence are superseded by runtime completion evidence and must be demoted or waived explicitly.
- Any prose-only release checklist that bypasses `src/ship_command.rs` gate checks is superseded by the runtime release gate.

## Acceptance Criteria

- Plan 012 produces an explicit GO/NO-GO decision artifact or root ledger entry that cites every prerequisite plan from 002 through 011 as closed, waived, or blocking.
- Root queue promotion changes only reviewed root specs and ledgers, and `git diff --name-only` shows no unintended source or generated-state churn.
- A promoted root `IMPLEMENTATION_PLAN.md` contains a non-empty runnable queue only when each row has machine-readable dependencies, source of truth, runtime owner, generated artifacts, fixture boundary, completion artifacts, verification, and review/closeout fields.
- `auto parallel status` reports GO only when the promoted queue is non-empty, dependency-ready, and free of stale lane or evidence blockers.
- `auto parallel status` reports NO-GO or a blocker frontier when the root queue is empty, all rows are completed, all remaining rows are blocked, or completion evidence drift is detected.
- `auto ship` or the release decision artifact records a release GO only after current CI-equivalent local proof or cited current CI evidence is present.
- Any operator override records the waived requirement, owner, risk, and follow-up condition in `SHIP.md` or the release decision artifact.

## Verification

- `rg -n "^- \\[ \\]" IMPLEMENTATION_PLAN.md`
- `auto parallel status`
- `find gen-20260430-184141/specs -maxdepth 1 -type f -name '*.md' -printf '%f\n' | sort`
- `cargo test generation`
- `cargo test task_parser`
- `cargo test parallel_status`
- `cargo test completion_artifacts`
- `cargo test ship`
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo install --path . --locked --root "$PWD/.auto/install-proof"`
- `.auto/install-proof/bin/auto --version`
- `auto parallel status`

## Review And Closeout

A reviewer proves closeout by tracing the decision artifact back to each original corpus requirement: Plans 002-011 are listed with closeout or waiver evidence; promoted rows are grep-checked for required machine-readable fields; `auto parallel status` is captured after promotion; release proof is tied to current git state; and bypass language is absent unless an explicit waiver exists. The reviewer should also run `rg -n "genesis|gen-[0-9]" IMPLEMENTATION_PLAN.md REVIEW.md SHIP.md` and confirm historical snapshots are referenced only as evidence or provenance, not as active runtime truth.

## Open Questions

- Should the release decision artifact live in `SHIP.md`, `REVIEW.md`, a generated `gen-*/DECISION.md`, or a root spec/ledger section?
- Should `ARCHIVED.md` become accepted completion evidence, or should `REVIEW.md` remain the only machine-readable host handoff surface?
- Should `auto ship` rerun the mechanical gate after remote sync and after each model iteration before declaring success?
- Should `auto gen` refuse root sync when the current active root queue has no runnable rows but the generated queue has unreviewed tasks?
