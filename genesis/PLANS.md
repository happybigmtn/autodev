# Genesis Plan Index

## Planning Surface

No root `PLANS.md` file and no root `plans/` directory exist in this checkout. The active planning truth is the repository's root control corpus: `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `ARCHIVED.md`, `COMPLETED.md`, `REVIEW.md`, root `specs/`, decision docs, and receipts under `.auto/symphony/verification-receipts/`.

This generated `genesis/` corpus is a strategic planning artifact and is subordinate to that root planning truth until the operator promotes specific slices into the active queue. The numbered plans below follow the full ExecPlan envelope requested for this corpus. If a root `PLANS.md` standard is added later, these plans should be reconciled to it.

## Sequencing Rationale

The chosen order front-loads work that can make autonomous execution unsafe or misleading:

1. Fix state, corpus, and credential safety before adding more parallel throughput.
2. Fix scheduler and completion truth before trusting lane assignment.
3. Add checkpoint gates after the first safety tranche and after execution-contract alignment.
4. Bind receipts and release evidence before treating `auto ship` as production proof.
5. Normalize report-only, audit, nemesis, docs, and DX after scheduler truth is reliable.
6. End with a release decision gate that decides whether root queue promotion and `auto parallel` launch are safe.

Obvious alternative rejected: start with README/spec cleanup. That would improve narrative confidence but would not prevent unsafe credential paths, empty corpus roots, stale receipts, or split-brain completion state.

Obvious alternative rejected: immediately run `auto parallel`. The root queue is cleared, and the next campaign needs explicit promotion after safety gates.

## Plan Set

- `001-master-plan.md`: master production-readiness frame and 14-day sequencing.
- `002-quota-backend-and-credential-safety.md`: quota profile containment, retry-after-progress safety, backend prompt secrecy, and account-state writes.
- `003-corpus-state-and-planning-root-safety.md`: atomic corpus restore, non-empty planning roots, and saved-state containment.
- `004-scheduler-completion-truth-and-lane-resume.md`: root ledger evidence classes, fail-closed plan refresh, and lane assignment hashes.
- `005-security-state-and-scheduler-checkpoint.md`: checkpoint gate before broader execution.
- `006-receipt-artifact-and-release-evidence-binding.md`: root-contained receipts/artifacts and current-tree release proof.
- `007-release-gate-and-verdict-parser-hardening.md`: ship gate order, post-model recheck, exact verdict parsing, and final status blocks.
- `008-super-loop-review-and-schema-parity.md`: shared plan-row schema and execution contract parity across super, loop, review, and parallel.
- `009-execution-contract-checkpoint.md`: checkpoint gate before audit/DX/release promotion.
- `010-audit-nemesis-and-report-only-lifecycle-truth.md`: audit/nemesis/report-only semantics and model-free workflow fixtures.
- `011-first-run-dx-observability-and-performance.md`: doctor, help, status, manifest-backed observability, and performance proof.
- `012-release-decision-gate-and-queue-promotion.md`: final go/no-go gate for root queue promotion and `auto parallel`.

## Dependency Order

- Plans 002, 003, and 004 can run in parallel only if ownership is split across quota/backend, corpus/state, and scheduler/completion modules.
- Plan 005 must run after 002-004 and decides whether execution-contract work may proceed.
- Plans 006, 007, and 008 can run after 005, with careful coordination around shared evidence helpers and plan-row schema.
- Plan 009 must run after 006-008 and decides whether lifecycle/DX/release work can be trusted.
- Plans 010 and 011 can run after 009 or in parallel if report-only and DX write sets remain disjoint.
- Plan 012 is the final gate and should not be executed until all earlier blockers are closed or explicitly waived.
