# Specification: Receipt And Lane Evidence Contract

## Objective

Make completion evidence coherent across `completion_artifacts`, `parallel`, `loop`, and `ship` so `[x]`, `[~]`, lane queues, and ship blockers mean the same thing to operators.

This is P1 because status and release decisions are only useful when completion proof is durable. It follows the P0 validation and generation slices, but outranks dashboard or release-note work because it owns the truth of completed work.

## Source Of Truth

- Runtime owner modules/APIs: `src/completion_artifacts.rs::inspect_task_completion_evidence`, `src/completion_artifacts.rs::shared_receipt_freshness_problem`, `src/parallel_command.rs::reconcile_parallel_landed_task`, `src/parallel_command.rs::commit_task_closeout`, `src/ship_command.rs::evaluate_ship_gate`, `src/loop_command.rs` completion demotion logic, `src/task_parser.rs::LaneKind`.
- Receipt writer/runtime owner: `scripts/run-task-verification.sh` and `scripts/verification_receipt.py`.
- UI consumers: `IMPLEMENTATION_PLAN.md` checkboxes, `REVIEW.md`, `RECEIPTS-DRIFT.md`, `auto parallel status`, lane logs, `SHIP.md`, ship gate stdout.
- Generated artifacts: `.auto/symphony/verification-receipts/<TASK>.json`, `Auto-Verification-Receipt-*` commit footers, `.auto/task-receipts/<TASK>/**`, `REVIEW.md`, `RECEIPTS-DRIFT.md`, `SHIP.md`.
- Retired/superseded surfaces: hand-edited receipt JSON as durable proof; model narrative treated as host-observed receipt; ambiguous `Lane kind: operator` behavior where docs/tests and runtime disagree.

## Evidence Status

Verified facts grounded in code, docs, or commands:

- `docs/verification-receipt-schema.md:3-7` says receipts are execution evidence, not notes, and durable receipt truth is carried in commit-message footers created by the host.
- `docs/verification-receipt-schema.md:11-24` defines JSON staging under `.auto/symphony/verification-receipts/<TASK>.json` and commit footers `Auto-Verification-Receipt-Version`, `Auto-Verification-Receipt-Task`, and `Auto-Verification-Receipt-JSON`.
- `docs/verification-receipt-schema.md:56-67` says the shared inspector rejects stale metadata, dirty-state drift, plan-hash drift, missing expected argv, failed commands, unsuperseded failures, zero-test receipts, and artifact hash drift.
- `scripts/run-task-verification.sh:1-47` runs a command, tees stdout/stderr, calls `scripts/verification_receipt.py record`, and exits nonzero when a passing command cannot record a receipt.
- `scripts/verification_receipt.py:33-44` writes receipts under `.auto/symphony/verification-receipts`, including lane-aware receipt roots.
- `scripts/verification_receipt.py:81-107` records current commit, dirty-state fingerprint, and plan hash.
- `src/completion_artifacts.rs:56-62` defines fully evidenced work as review handoff present, verification receipt present, no missing completion artifacts, and no unresolved audit findings.
- `src/completion_artifacts.rs:133-171` inspects review handoff, receipt requirement, wrapper presence, receipt status, declared artifacts, and unresolved audit findings.
- `src/completion_artifacts.rs:453-469` converts JSON receipts into compact commit footer text.
- `src/completion_artifacts.rs:793-868` prefers commit footer receipts first, then falls back to on-disk JSON when needed.
- `src/completion_artifacts.rs:937-1025` rejects failed, zero-test, and unsuperseded failed receipt entries.
- `src/completion_artifacts.rs:1204-1295` implements freshness checks for JSON vs commit-footer receipt sources.
- `src/parallel_command.rs:6774-6854` propagates lane JSON receipts into canonical `.auto/symphony/verification-receipts` and rewrites commit and dirty-state metadata.
- `src/parallel_command.rs:7037-7074` reconciles a landed lane into `Done` only if evidence is fully evidenced; otherwise it marks partial.
- `src/parallel_command.rs:7082-7097` writes verification receipt footers into task closeout commits.
- `src/task_parser.rs:31-55` defines `LaneKind::Code`, `LaneKind::Operator`, and `LaneKind::Evidence`.
- `src/parallel_command.rs:4838-4847` currently returns `false` for every `is_operator_task` call and comments that historical operator-queue behavior is retired, so operator-tagged tasks dispatch like code lanes.
- `src/parallel_command.rs:9444-9479` still has a test expecting `Lane kind: operator` to appear in an operator queue.
- `src/ship_command.rs:122-172` evaluates ship gate receipts, QA/HEALTH freshness, ship report, and unresolved release blockers.
- `src/ship_command.rs:219-286` loads receipt commands from commit footers first and JSON fallback receipts second.
- Command evidence from this generation pass: `cargo test completion_artifacts::tests -- --nocapture` exited 0 with 34 passing tests.
- Command evidence from this generation pass: `cargo test loop_command::tests::loop_marks_task_partial_when_completion_evidence_missing -- --nocapture` exited 0.
- Command evidence from this generation pass: `cargo test ship_command::tests::ship_gate_rejects_stale_completion_receipt -- --nocapture` exited 101 because `report.is_blocked()` was false.
- Command evidence from this generation pass: `cargo test parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks -- --nocapture` exited 101 because the expected status string did not contain `code lanes ready: CODE-001`.
- Command evidence from this generation pass: `.auto/symphony/verification-receipts` is currently missing in the checkout.

Recommendations for the intended system:

- Treat commit footers as durable preferred proof and JSON files as staging or compatibility fallback.
- Decide `Lane kind: operator` explicitly. If autonomous dispatch is intended, update tests/docs/status text. If manual queue is intended, change runtime to honor `LaneKind::Operator`.
- Keep `auto loop` behavior honest: either document it as local demotion without durable footer creation, or make it create durable proof through the same receipt footer path as `auto parallel`.
- Make `ship` use the shared inspector semantics in a way that rejects stale/failing receipts for release-required commands.

Hypotheses / unresolved questions:

- It is unresolved whether operator-tagged tasks should be human/manual queue items or autonomous code lanes with full tool access.
- It is unresolved whether JSON receipt propagation should rewrite freshness metadata at all, or whether only footerized receipts should become durable after landing.
- It is unresolved whether `auto loop` should become receipt-footer durable or remain a weaker legacy executor.

## Runtime Contract

`src/completion_artifacts.rs` owns canonical completion evidence semantics. `src/parallel_command.rs`, `src/loop_command.rs`, and `src/ship_command.rs` must call or match that contract rather than reimplementing separate receipt truth.

When executable verification is required and receipt data is absent, stale, failed, zero-test, missing expected argv, or has drifted artifact hashes, runtime must fail closed: mark task partial, block ship, or report drift instead of accepting `[x]`.

`LaneKind` must be a real routing contract. Presentation may label operator/evidence/code lanes, but runtime dispatch and status must agree with that label.

## UI Contract

Terminal and markdown UI must render evidence classes from runtime facts:

- `IMPLEMENTATION_PLAN.md` checkboxes show queue state, not proof by themselves.
- `REVIEW.md` shows host handoff and non-local evidence context.
- `RECEIPTS-DRIFT.md` shows completed rows whose proof needs repair without silently demoting completed rows.
- `auto parallel status` shows code, evidence, and operator queues according to runtime routing.
- `SHIP.md` and ship gate stdout show missing, stale, red, or bypassed release proof.

Presentation consumers must not duplicate receipt freshness rules, settlement math, or lane eligibility rules outside the shared runtime helpers.

Production UI must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; it must consume runtime helpers/generated contracts or render an explicit unavailable/error state when runtime truth is missing.

## Generated Artifacts

- `.auto/symphony/verification-receipts/<TASK>.json`
- `.auto/task-receipts/<TASK>/**`
- Commit footers:
  - `Auto-Verification-Receipt-Version: 1`
  - `Auto-Verification-Receipt-Task: <TASK-ID>`
  - `Auto-Verification-Receipt-JSON: <base64url-json>`
- `REVIEW.md`
- `RECEIPTS-DRIFT.md`
- `SHIP.md`
- `.auto/parallel/live.log`

Refresh commands:

```bash
scripts/run-task-verification.sh <TASK-ID> -- <exact verification command>
auto parallel status
auto ship
```

## Fixture Policy

Receipt fixtures belong in temp repos and unit tests. Production code must not import fixture receipts, copied JSON excerpts, sample `REVIEW.md` text, or generated snapshots as proof. Staging JSON under `.auto/` must not be hand-edited to manufacture durable task completion.

## Retired / Superseded Surfaces

- `.auto/symphony/verification-receipts/*.json` as durable proof without commit footer framing.
- Any `Lane kind: operator` semantics that are only described in comments while tests/status/runtime disagree.
- Ship gate duplicate freshness logic if it diverges from shared completion receipt inspection.
- Model-written completion prose that calls itself host-observed verification.

## Acceptance Criteria

- A task with missing executable receipt is marked partial or blocked, not done.
- A task with stale receipt metadata cannot satisfy `auto ship`.
- A task with failed or zero-test receipt entries cannot satisfy completion evidence.
- A task with superseded failed attempts passes only when a later matching passed receipt explicitly supersedes the failed command.
- `Lane kind: code`, `Lane kind: evidence`, and `Lane kind: operator` are parsed, routed, and displayed consistently.
- `auto parallel status` and `run_parallel_loop` agree about which ready tasks are executable code lanes.
- JSON staging receipts are either footerized or treated as compatibility fallback with documented freshness rules.
- `auto loop` either creates durable proof or clearly remains a demoting legacy/local executor.

## Verification

Run from `/home/r/coding/autodev`:

```bash
cargo test completion_artifacts::tests
cargo test ship_command::tests::ship_gate_rejects_stale_completion_receipt
cargo test ship_command::tests::ship_gate_uses_shared_receipt_inspector
cargo test parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks
cargo test parallel_command::tests::repair_parallel_canonical_checkpoints_verification_receipts
cargo test loop_command::tests::loop_marks_task_partial_when_completion_evidence_missing
```

Grep proof:

```bash
rg -n "Auto-Verification-Receipt|Lane kind|operator queue|is_operator_task|shared_receipt_freshness_problem|verification_receipt_present|RECEIPTS-DRIFT" src/completion_artifacts.rs src/parallel_command.rs src/loop_command.rs src/ship_command.rs docs/verification-receipt-schema.md scripts/run-task-verification.sh scripts/verification_receipt.py
```

## Review And Closeout

A reviewer should prove each original failure mode with targeted tests: stale ship receipt blocks, lane-kind status matches dispatch, and completion evidence rejects missing/failed/zero-test proof.

Closeout must include grep proof that `Lane kind: operator` no longer has conflicting semantics between `is_operator_task`, tests, docs, and status strings. It must also include a receipt proof path showing whether durable truth came from a commit footer or a JSON compatibility fallback.

## Open Questions

- Should operator lanes be manual queues or autonomous lanes?
- Should `auto loop` write receipt footers, or should docs continue to steer operators toward `auto parallel --threads 1` for durable completion?
- Should JSON receipt propagation rewrite commit and dirty-state metadata, or should that be replaced by immediate footerization?
