# Genesis Plan Index

## Planning Surface

No root `PLANS.md` file and no root `plans/` directory exist in this checkout. The active root planning truth is therefore the repository's existing control corpus: `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `ARCHIVED.md`, `REVIEW.md`, `specs/`, `docs/decisions/`, and receipts under `.auto/symphony/verification-receipts/`.

This generated `genesis/` corpus is a strategic planning artifact and is subordinate to that root planning truth until the operator promotes specific slices into the active queue. The numbered plans below use the full ExecPlan envelope requested for this corpus. If a root `PLANS.md` standard is added later, these plans should be reconciled to it.

## Sequencing Rationale

The chosen order front-loads work that can make autonomous execution unsafe or misleading:

1. Fix credential and corpus control-plane safety before adding more parallel throughput.
2. Fix dependency truth before trusting lane assignment.
3. Add checkpoint gates after the first safety tranche and after execution-contract alignment.
4. Bind receipts and release evidence before treating `auto ship` as production proof.
5. Reconcile report-only, audit, nemesis, docs, and DX after the scheduler can be trusted.
6. End with a release decision gate that decides whether root queue promotion and `auto parallel` launch are safe.

Obvious alternative rejected: starting with README/spec cleanup. That would improve prompts, but it would not prevent unsafe credential swaps, empty corpus roots, or stale receipts from producing false confidence.

Obvious alternative rejected: immediately running `auto parallel`. The root queue still contains stale or partially reconciled rows, and code review found safety issues in quota execution, dependency parsing, and salvage durability.

## Plan Set

- `001-master-plan.md`: master production-readiness frame and 14-day sequencing.
- `002-quota-profile-path-and-credential-lease-hardening.md`: path-bound account names and process-lifetime credential leases.
- `003-corpus-atomic-restore-and-non-empty-planning-root.md`: atomic corpus generation and non-empty `genesis/` validation.
- `004-dependency-truth-and-scheduler-eligibility.md`: dependency parser and scheduler readiness fixes.
- `005-security-and-state-checkpoint-gate.md`: checkpoint before broader execution.
- `006-receipt-freshness-and-release-evidence-binding.md`: commit/dirty/artifact-aware receipts and release gate proof.
- `007-symphony-and-review-reconciliation-safety.md`: AD-014, partial-row safety, branch validation, and review write ordering.
- `008-auto-loop-auto-review-and-super-schema-parity.md`: loop dependency safety and shared plan schema enforcement.
- `009-execution-contract-checkpoint-gate.md`: checkpoint before audit/DX/release polish.
- `010-audit-nemesis-and-report-only-lifecycle-truth.md`: audit status, nemesis flags, and report-only write boundaries.
- `011-first-run-dx-and-command-output-contracts.md`: doctor, help, dry-run, and terminal-output contracts.
- `012-release-decision-gate-and-queue-promotion.md`: final go/no-go gate for root queue promotion and `auto parallel`.

## Dependency Order

- Plans 002, 003, and 004 can be implemented in parallel only if write ownership is split across quota, corpus/generation, and task/scheduler modules.
- Plan 005 must run after 002-004.
- Plans 006, 007, and 008 can run after Plan 005, with care around shared parser and review/ship evidence helpers.
- Plan 009 must run after 006-008.
- Plans 010 and 011 can run after Plan 009 or in parallel if the report-only and DX write sets remain disjoint.
- Plan 012 is the final gate and should not be executed until all earlier blockers are closed or explicitly waived.
