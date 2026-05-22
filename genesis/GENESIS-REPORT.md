## Priority Focus

1. Restore the formatting and public command-surface baseline. This outranks documentation, audit, and artifact work because `cargo fmt --check` and a live command-surface test are red now, and they are small enough for one worker to prove.
2. Re-tighten generated-plan and spec-task proof validators. This stays separate from the baseline slice so broad cargo-test, malformed grep, ownership, and execution-row strictness can be repaired without swallowing receipt, ship, and super behavior.
3. Make `auto super` snapshot-first by default. This outranks report polish because it controls whether generated planning output silently mutates root queue truth.
4. Repair receipt, loop, ship, and lane evidence semantics. This outranks broader process work because completion proof is the product's trust boundary.
5. Reconcile first-run/status truth and harden quota persistence only after the central loop facts are stable. These remain P1 implementation work, not documentation campaigns.

The operator focus changed the recommended order by pushing docs-only cleanup and audit inventory below implementation slices. The only documentation work kept in the first wave is documentation that directly prevents runtime/source-of-truth confusion. A higher-priority issue outside the focus is quota credential safety; it remains in the plan set, but not above the red validation and orchestration contracts.

## Corpus Refresh

This refresh reviewed the current Rust CLI, root control ledgers, accepted decisions, specs, CI, tests, dirty worktree context, and the archived previous genesis snapshot. The previous snapshot was used as historical context only. The generated corpus is written under `.auto/corpus-staging/genesis-20260522-042812/` and is subordinate to root control files.

Four explorer agents contributed read-only coverage:

- CLI/product and first-run UX.
- State, quota, backend, and credential safety.
- Parallel/super/loop/completion evidence semantics.
- Tests, CI, docs, specs, and history drift.

## Major Findings

- The project is a real terminal-first operator control plane, not just a planning artifact generator.
- Current validation is red: `cargo fmt --check` fails, and the observed `cargo test` run failed 16 tests. The corpus now splits this into worker-sized slices instead of one broad "fix all tests" task.
- `auto super` runtime behavior conflicts with accepted snapshot-first promotion decisions.
- Receipt, lane, loop, and ship evidence semantics have diverged across code, tests, and docs.
- The live command surface includes `audit-harvest`, but README/test expectations are stale.
- Quota path traversal and credential capture have meaningful mitigations, but persistence/load validation and Claude credential sync still need hardening.
- Root `IMPLEMENTATION_PLAN.md` is fully checked, yet `WORKLIST.md` still has required hardening items and current tests are red.

## Recommended Direction

Make `auto` an evidence-first control plane for one bounded implementation slice at a time. The next cycle should prioritize green validation, safe promotion semantics, durable completion proof, and no-model operator clarity. Documentation should support these runtime fixes rather than becoming the work itself.

## Top Next Priorities

1. Fix formatting and command-surface truth first.
2. Restore generated-plan and spec-task proof validator strictness.
3. Change `auto super` so its default generation phase produces reviewable snapshots instead of silently syncing root ledgers.
4. Decide and implement one receipt/lane evidence contract across `completion_artifacts`, `parallel`, `loop`, and `ship`.
5. Make `auto doctor`/status and quota persistence hardening follow the central loop fixes as bounded P1 slices.

## Not Doing

- No broad documentation rewrite while validation is red.
- No new web UI or dashboard outside the terminal/status surface.
- No release push until fmt, tests, promotion behavior, and evidence gates are coherent.
- No silent root promotion from generated corpus staging.
- No provider-specific prompt transport redesign without verified backend support.
- No rewrite of orchestration modules before failing contracts are isolated.

## Decision Audit Trail

| Decision | Classification | Rationale |
| --- | --- | --- |
| Split validation into baseline and proof-contract slices. | Mechanical | CI and repo instructions require fmt/test, but one "fix all tests" lane would be too broad for productive parallel execution. |
| Treat this corpus as subordinate staging guidance. | Mechanical | No root `PLANS.md` or root `plans/` exists; root ledgers and repo instructions remain active control truth. |
| Prefer snapshot-first `auto super`. | Mechanical | Accepted decisions say generated snapshots should be reviewable and root sync explicit; runtime currently disagrees. |
| Keep terminal UX in `DESIGN.md`. | Mechanical | The repo has meaningful user-facing CLI, help, status, and markdown surfaces. |
| Put quota hardening after orchestration fixes. | Taste | Both matter, but quota has existing mitigations while the central product loop is red. |
| Defer broad historical spec cleanup. | Taste | Stale specs matter, but fixing them before runtime validation would not improve the next executable slice. |
| Do not silently change the operator's stated focus into a docs corpus. | User Challenge | The operator explicitly asked for implementation-priority planning; broad artifact expansion would change that direction. |

## Files Written

- `ASSESSMENT.md`
- `SPEC.md`
- `DESIGN.md`
- `FOCUS.md`
- `PLANS.md`
- `GENESIS-REPORT.md`
- `plans/001-master-plan.md`
- `plans/002-restore-green-validation-and-plan-proof.md`
- `plans/003-re-tighten-generated-plan-proof-validators.md`
- `plans/004-super-snapshot-first-runtime.md`
- `plans/005-receipt-and-lane-evidence-contract.md`
- `plans/006-operator-status-and-command-surface.md`
- `plans/007-quota-persistence-and-credential-hardening.md`
- `plans/008-release-readiness-checkpoint.md`
