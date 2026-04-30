# Release Decision Gate And Queue Promotion

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This final gate decides whether the production-readiness campaign is ready to become active root work and, eventually, parallel execution. The operator gains a clear GO/NO-GO decision backed by current corpus, root ledgers, receipts, tests, status, and release evidence.

## Requirements Trace

- R1: Plans 002-011 are closed or explicitly waived with risk ownership.
- R2: `genesis/` is complete and subordinate unless promoted.
- R3: Root queue promotion includes only accepted, evidence-backed slices.
- R4: `auto gen` or manual promotion must not overwrite active root truth without review.
- R5: `auto parallel` launch requires clean scheduler status, current receipts, and no high-severity blockers.
- R6: Release readiness requires current CI-equivalent local proof or a documented reason CI is the accepted proof.

## Scope Boundaries

This plan does not implement the earlier fixes. It does not force a release if gates fail. It does not promote the entire generated corpus automatically. It does not bypass operator sovereignty.

## Progress

- [x] 2026-04-30: Final gate positioned after safety, evidence, lifecycle, DX, and performance plans.
- [ ] Confirm all earlier plan outcomes.
- [ ] Decide root queue promotion strategy.
- [ ] Run generation or manual promotion in the selected mode.
- [ ] Decide whether `auto parallel` can launch.
- [ ] Record release GO/NO-GO.

## Surprises & Discoveries

None yet.

## Decision Log

- Mechanical: Queue promotion should be narrow because `genesis/` is not active truth by default.
- Mechanical: `auto parallel` launch requires a non-empty, valid root queue and scheduler status GO.
- Taste: Prefer snapshot/review before sync when using `auto gen` for production control changes.
- User Challenge: If the operator asks for immediate launch despite NO-GO blockers, record the challenge and require explicit acceptance.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Relevant files:

- This corpus under `genesis/`.
- Active root ledgers: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `WORKLIST.md`.
- Root specs and decision docs.
- Receipts under `.auto/symphony/verification-receipts/`.
- Release artifacts: `SHIP.md`, `QA.md`, `HEALTH.md`.
- CI workflow: `.github/workflows/ci.yml`.

Non-obvious terms:

- Queue promotion: moving accepted plan slices from generated corpus into active root `IMPLEMENTATION_PLAN.md` and related specs.
- Snapshot/review: producing generated outputs without immediately syncing them into root truth, so an operator can inspect first.
- GO/NO-GO: explicit release or execution decision with blockers and waivers.

## Plan of Work

Review outcomes from Plans 002-011. If all required gates pass, decide whether to use `auto gen --snapshot-only`, `auto steward`, or manual narrow edits to promote accepted slices. Because root ledgers are active truth, do not copy all generated plans wholesale. Promote the smallest coherent queue that advances production readiness and has machine-readable dependencies.

After promotion, run the shared schema validator, scheduler status, focused tests for touched modules, and release gate checks. If the root queue is valid and non-empty, run `auto parallel status` and decide whether to launch. If any high-severity blocker remains, record NO-GO and return to the owning plan.

## Implementation Units

- Unit 1: Earlier plan closeout review.
  - Goal: Verify closure or waivers for Plans 002-011.
  - Requirements advanced: R1.
  - Dependencies: Plans 002-011.
  - Files to create or modify: release decision artifact if promoted.
  - Tests to add or modify: none in this unit.
  - Approach: Read progress, outcomes, test evidence, and waivers.
  - Test scenarios: Test expectation: none -- this is evidence review.

- Unit 2: Promotion mode decision.
  - Goal: Choose `auto gen --snapshot-only`, `auto steward`, or manual promotion.
  - Requirements advanced: R2, R3, R4.
  - Dependencies: Unit 1.
  - Files to create or modify: root ledgers/specs only after decision.
  - Tests to add or modify: plan schema validation after promotion.
  - Approach: Prefer reviewable snapshots or narrow manual edits; avoid blind sync.
  - Test scenarios: promoted rows parse and have dependencies, artifacts, and verification fields.

- Unit 3: Root queue validation.
  - Goal: Confirm promoted queue is safe for execution.
  - Requirements advanced: R3, R5.
  - Dependencies: Unit 2.
  - Files to create or modify: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, specs if promoted.
  - Tests to add or modify: none unless validator gaps are found.
  - Approach: Run parser/schema checks, scheduler status, and focused tests.
  - Test scenarios: no missing dependency ids, no evidence-impossible rows, no stale completion drift.

- Unit 4: Release/execution GO decision.
  - Goal: Decide whether to run `auto parallel` and whether repo is release-ready.
  - Requirements advanced: R5, R6.
  - Dependencies: Unit 3.
  - Files to create or modify: `SHIP.md` or decision artifact if promoted.
  - Tests to add or modify: none in this gate.
  - Approach: Run CI-equivalent local proof or cite current CI; evaluate ship gate; record GO/NO-GO.
  - Test scenarios: Test expectation: none -- this is a release decision consuming prior tests.

## Concrete Steps

From the repository root:

    git status --short --branch
    find genesis -maxdepth 2 -type f | sort
    rg -n "^- \\[( |~|!)\\]" IMPLEMENTATION_PLAN.md REVIEW.md

If choosing snapshot generation:

    auto gen --snapshot-only

Expected observation: generated snapshot output is available for review without root sync.

If promoting manually or through reviewed sync, validate:

    cargo test task_parser
    cargo test generation
    cargo test parallel_status
    cargo test completion_artifacts
    cargo test ship
    cargo clippy --all-targets --all-features -- -D warnings

Before any parallel launch:

    auto parallel status

Expected observation: status says launch is safe, names a non-empty pending queue, and reports no stale plan or evidence blockers.

Release proof, when appropriate:

    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test
    cargo install --path . --locked --root "$PWD/.auto/install-proof"
    .auto/install-proof/bin/auto --version

Expected observation: CI-equivalent local proof passes, or the decision artifact explicitly cites current CI as accepted proof.

## Validation and Acceptance

GO requires all earlier blockers closed or waived, a complete subordinate corpus, a valid promoted root queue, safe scheduler status, current evidence, and release-gate proof. NO-GO is required if the queue is empty, stale, invalid, or blocked by high-severity security/state/evidence issues. The final decision must include the Not Doing list and any waivers.

## Idempotence and Recovery

Snapshot generation is safe to rerun. Manual promotion should be reviewed with `git diff` before execution. If `auto gen --sync-only` or another mutating promotion fails, recover from git diff and generated snapshots; do not reset unrelated user changes. If `.auto/install-proof` is created, it is generated state and can be removed after proof if desired.

## Artifacts and Notes

- Evidence to fill in: promoted root rows or decision not to promote.
- Evidence to fill in: scheduler status output.
- Evidence to fill in: CI/local release proof.
- Evidence to fill in: final GO/NO-GO decision and waivers.

## Interfaces and Dependencies

- Depends on Plans 002-011.
- Commands: `auto gen`, `auto steward`, `auto parallel status`, `auto parallel`, `auto ship`, Cargo validation commands.
- Files: `genesis/`, root ledgers, root specs, receipts, `SHIP.md`, `QA.md`, `HEALTH.md`.
