# Repair Receipt And Lane Evidence Contract

## Priority Decision

P1. This is the fourth implementation slice. Score: high operator value, medium/high design clarity, high engineering leverage, direct evidence, and medium parallel executability. It outranks a new dashboard because status output is only useful when receipt and completion facts are coherent.

## User / Operator Outcome

An operator can trust `[x]`, `[~]`, ship gates, and receipt warnings. Completed work has durable proof; partial work remains partial; stale or failed verification blocks ship; operator/evidence lane behavior is predictable.

## Evidence

- `src/completion_artifacts.rs` inspects review handoff, artifacts, receipts, commit footers, and audit findings.
- `src/parallel_command.rs` propagates lane receipts and writes closeout commit footers.
- `src/loop_command.rs` demotes worker-marked `[x]` rows when evidence is incomplete, but loop durable footer behavior is not aligned with parallel.
- Failing tests include stale completion receipt ship gates, shared receipt inspector behavior, lane-kind routing, operator/evidence status verdicts, and canonical receipt dirty-state handling.
- Receipt docs say `.auto/` JSON is staging/compatibility data and durable task proof should travel in commit footers.

## Scope Boundary

Do not invent a second evidence model. Do not hand-edit receipt JSON as a durable source of truth. Do not broaden this into a full release-readiness redesign. Decide operator lane semantics explicitly in code/tests/docs instead of letting comments, tests, and runtime disagree.

## Implementation Slice

Goal: define and implement one evidence contract across `completion_artifacts`, `parallel`, `loop`, and `ship`.

Dependencies: plans 002 and 003 should restore or isolate validation/proof-contract failures first. Plan 004 can run independently if write scopes do not overlap.

Files likely to modify:

- `src/completion_artifacts.rs`
- `src/parallel_command.rs`
- `src/loop_command.rs`
- `src/ship_command.rs`
- `src/task_parser.rs` if lane-kind parsing is involved.
- `docs/verification-receipt-schema.md` or the relevant decision doc only if a policy choice changes.

Tests to add or modify:

- Receipt freshness tests for current commit, dirty state, and plan hash.
- Ship gate tests that reject stale, failed, zero-test, or superseded-failed receipts.
- Loop tests proving whether loop completion is durable or explicitly local/non-authoritative.
- Parallel tests for operator/evidence lane routing and receipt propagation.

Approach:

1. Decide whether `Lane kind: operator` queues manual operator actions or dispatches autonomous code lanes. Update code, tests, and active decision text together.
2. Decide whether `auto loop` writes durable receipt footers or only demotes false completion. Do not let it imply durable proof unless it creates durable proof.
3. Make commit footer receipts the durable preferred source and JSON staging receipts a compatibility fallback with clear freshness checks.
4. Remove ambiguous host mutation of receipt JSON unless the mutated fields are explicitly restaged as compatibility data and tested.
5. Re-run targeted ship, completion, parallel, and loop tests.

## Verification

From the repository root:

    cargo test completion_artifacts::tests
    cargo test ship_command::tests::ship_gate_rejects_stale_completion_receipt
    cargo test ship_command::tests::ship_gate_uses_shared_receipt_inspector
    cargo test parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks
    cargo test parallel_command::tests::repair_parallel_canonical_checkpoints_verification_receipts
    cargo test loop_command::tests::loop_marks_task_partial_when_completion_evidence_missing
    cargo test

Expected observation: stale or failed proof blocks completion/ship, valid durable proof passes, lane semantics are explicit, and no receipt propagation test leaves `.auto/` as unexpected dirty state.

## Deferred

- UI changes beyond necessary status/help wording.
- Long-term receipt schema migration.
- Provider quota prompt transport changes.
- Full release checklist rewrite.
