# Receipt Artifact And Release Evidence Binding

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This slice makes evidence durable and current. The operator gains proof that receipts and declared artifacts belong to the current repository state, cannot point outside the repository, and are evaluated consistently by completion and release gates.

## Requirements Trace

- R1: Declared artifacts must be repository-contained unless they are explicitly allowed external evidence.
- R2: Receipt freshness checks must bind commit, dirty state, plan hash, command argv, status, zero-test outcome, and declared artifact hashes.
- R3: Completion and release gates must share evidence inspection code rather than drifting copies.
- R4: Narrative proof must be labeled separately from host-created receipts.
- R5: Missing, failed, stale, superseded, or zero-test receipts must block receipt-required completion.

## Scope Boundaries

This plan does not decide release go/no-go by itself. It does not require every command in every report to have a receipt; it requires receipt-required claims to be explicit and consistently checked. It does not change the external receipt storage location unless needed for containment.

## Progress

- [x] 2026-04-30: Verified `completion_artifacts.rs` contains strong receipt checks.
- [x] 2026-04-30: Verified `ship_command.rs` has release-gate evidence logic with ordering concerns.
- [x] 2026-04-30: Identified declared artifact path containment risk.
- [x] 2026-04-30: Independent review verified completion and ship code already check current commit, dirty fingerprint, plan hash, expected argv, zero-test status, and declared artifact hashes through duplicated logic.
- [ ] Add root-contained artifact path validation.
- [ ] Unify completion and ship evidence inspectors.
- [ ] Add evidence-class labels for narrative proof.

## Surprises & Discoveries

- The receipt layer is already substantial; the risk is more about containment, shared ownership, parser semantics, and release ordering than absence.
- Some commands still trust model-written markdown for "commands run" claims.

## Decision Log

- Mechanical: Artifact paths from plans/reviews must be treated as untrusted input.
- Mechanical: Duplicate receipt freshness logic is a drift risk in a release gate.
- Taste: Add explicit external-evidence labels instead of banning non-file evidence, because some operational blockers are real but not local.
- User Challenge: Requiring receipts everywhere may slow report-only workflows; this plan only requires clear evidence classes.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Relevant files:

- `src/completion_artifacts.rs`: task completion evidence and receipt freshness.
- `src/ship_command.rs`: release gate, ship notes, receipt checks, QA/health freshness.
- `src/verification_lint.rs`: command extraction and evidence linting.
- `src/parallel_command.rs`: worker prompts and receipt expectations.
- `.auto/symphony/verification-receipts/`: current receipt location.
- `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `SHIP.md`, `QA.md`, `HEALTH.md`: evidence consumers.

Non-obvious terms:

- Declared artifact: a file or directory listed in task/review evidence as proof of work.
- Host-created receipt: machine-written JSON proof of a command invocation and result.
- External evidence: proof that cannot be represented as a local file or command receipt, such as a credential blocker or live service status.

## Plan of Work

Create or extract a shared evidence inspector that both completion and ship gates use. Add a path parser that rejects absolute paths, parent traversal, and paths outside the repository unless marked with an explicit external-evidence class. Ensure receipt freshness checks include current commit, dirty fingerprint, plan hash, command argv, status, zero-test status, superseded status, and declared artifact hashes.

Update task/review parsing to distinguish host receipts, declared artifacts, narrative proof, and external evidence. Update worker prompts and release output so the required evidence class is visible.

## Implementation Units

- Unit 1: Artifact path containment.
  - Goal: Prevent evidence paths from escaping the repository root.
  - Requirements advanced: R1.
  - Dependencies: none.
  - Files to create or modify: `src/completion_artifacts.rs`, `src/ship_command.rs`, tests.
  - Tests to add or modify: reject `/absolute/path`, `../outside`, symlink escape if feasible; accept normal repo-relative paths and receipt aliases.
  - Approach: Normalize syntactic components before reading files and canonicalize existing paths when possible.
  - Test scenarios: declared artifact `../secret` is rejected, not hashed.

- Unit 2: Shared evidence inspector.
  - Goal: Remove completion/ship drift.
  - Requirements advanced: R2, R3, R5.
  - Dependencies: Unit 1.
  - Files to create or modify: `src/completion_artifacts.rs`, `src/ship_command.rs`, possibly a new `src/evidence.rs`.
  - Tests to add or modify: ship and completion use same stale, failed, zero-test, superseded receipt cases.
  - Approach: Extract data structs and result enums usable by both gates.
  - Test scenarios: stale receipt fails both task completion and ship gate with matching reason.

- Unit 3: Evidence class labels.
  - Goal: Make narrative proof visibly different from receipts.
  - Requirements advanced: R4.
  - Dependencies: task/review parser updates.
  - Files to create or modify: `src/task_parser.rs`, `src/completion_artifacts.rs`, `src/parallel_command.rs`.
  - Tests to add or modify: narrative-only proof does not satisfy receipt-required task; external blocker remains allowed when labeled.
  - Approach: Add parser conventions and update prompts/status output.
  - Test scenarios: a task with executable verification and no receipt remains partial.

- Unit 4: Receipt freshness fixture suite.
  - Goal: Lock in the current freshness behavior and cover missing mismatch cases.
  - Requirements advanced: R2, R5.
  - Dependencies: Units 1-3.
  - Files to create or modify: tests in `completion_artifacts` and `ship_command`.
  - Tests to add or modify: keep existing current-commit, failed, superseded, and zero-test coverage; add dirty hash mismatch, plan hash mismatch, expected argv mismatch, artifact hash mismatch, and unsafe artifact path fixtures.
  - Approach: Use temp repos and synthetic receipts, then assert completion and ship gates report the same blocker class.
  - Test scenarios: each mismatch produces a precise blocker string in both completion and release contexts.

## Concrete Steps

From the repository root:

    rg -n "declared_artifact|verification_receipt|artifact_hash|Receipt|ShipGate|zero-test|superseded" src/completion_artifacts.rs src/ship_command.rs src/parallel_command.rs src/task_parser.rs

Expected observation: duplicate evidence paths and artifact handling.

    cargo test completion_artifacts
    cargo test ship

Expected observation before work: new containment and shared-behavior tests fail.

After implementation:

    cargo test completion_artifacts
    cargo test ship
    cargo test verification_lint
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: evidence behavior is consistent across completion and release gates.

## Validation and Acceptance

Acceptance requires tests proving unsafe artifact paths are rejected, stale/failed/zero-test receipts block both completion and release, and narrative proof has an explicit non-receipt class. The implementation should make duplicate receipt logic smaller, not larger.

## Idempotence and Recovery

Evidence inspection should be read-only. Rerunning it must not mutate receipts or ledgers. If old plans contain unsafe artifact paths, status should report clear blockers rather than deleting or rewriting them automatically.

## Artifacts and Notes

- Evidence to fill in: containment test names.
- Evidence to fill in: shared inspector type or module name.
- Evidence to fill in: before/after blocker strings for stale receipt.

## Interfaces and Dependencies

- Modules: `completion_artifacts`, `ship_command`, `verification_lint`, `task_parser`, `parallel_command`.
- Files: verification receipts, root ledgers, `SHIP.md`, `QA.md`, `HEALTH.md`.
- Commands: `auto parallel`, `auto loop`, `auto ship`.
