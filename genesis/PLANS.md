# Plan Index

## Planning Surface

No root `PLANS.md` or root `plans/` directory was present in the current checkout. The active control surface remains the repository root ledgers and instructions: `AGENTS.md`, `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `REVIEW.md`, `COMPLETED.md`, `ARCHIVED.md`, root `specs/`, decision docs, and receipt evidence. This generated corpus is subordinate staging guidance for the next implementation cycle.

The numbered plans below intentionally use the compact priority-plan shape requested for this corpus, not an older broad ExecPlan envelope.

## Sequence

1. `plans/001-master-plan.md` sets the implementation-first decision and dependency order for the next cycle.
2. `plans/002-restore-green-validation-and-plan-proof.md` restores the formatting and live command-surface baseline without taking on every failing validator.
3. `plans/003-re-tighten-generated-plan-proof-validators.md` restores strict generated-plan and spec-task proof contracts as a separate focused slice.
4. `plans/004-super-snapshot-first-runtime.md` aligns `auto super` runtime with accepted snapshot-first promotion decisions.
5. `plans/005-receipt-and-lane-evidence-contract.md` repairs completion receipt, loop, ship, and lane-evidence semantics.
6. `plans/006-operator-status-and-command-surface.md` makes first-run/status surfaces truthful once the underlying facts are stable.
7. `plans/007-quota-persistence-and-credential-hardening.md` closes the remaining quota persistence and credential-sync risks.
8. `plans/008-release-readiness-checkpoint.md` is a decision gate after the core loop is green enough to consider promotion or release work.

## Why This Order

The first slice is deliberately narrow: restore rustfmt and reconcile the public command surface that CI/help smoke can prove immediately. The broader validator failures are real, but bundling every red test into one lane would create the same overbroad artifact-shaped work this corpus is trying to avoid.

The second slice restores generated-plan proof strictness. It owns the validator/test contract rather than mixing receipt semantics, super promotion, and command-surface documentation into one large "make tests green" item.

The third slice is source-of-truth control. Once the baseline and validator contracts are clear, `auto super` must stop surprising operators by syncing root outputs when accepted decisions say generated snapshots should be reviewable first.

The fourth slice is evidence semantics. Completion proof is the product's trust boundary, and failing receipt/ship/lane tests show that the boundary is currently unstable.

The fifth slice is operator clarity. `auto doctor`, command-surface docs, and a possible no-model status aggregator become more valuable after the underlying facts are reliable.

Quota hardening follows because it is security-relevant and focused, but the code already has meaningful mitigations. It should not outrank the red central control loop.

The checkpoint exists because later release or promotion work depends on evidence that validation, promotion policy, receipt semantics, operator status, and quota hardening have converged or been explicitly deferred.

## Alternatives Rejected

- Documentation cleanup first: rejected because stale docs are not the highest risk while CI validation is red.
- Quota security first: rejected as the top sequence because quota has partial mitigations and focused tests, while orchestration validation is actively failing.
- New dashboard first: rejected because status UX must render stable facts, and those facts are currently disputed by failing tests.
- Release packaging first: rejected because a non-production repo with red fmt/tests should not spend priority on release ceremony.
