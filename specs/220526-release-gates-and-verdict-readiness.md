# Specification: Release Gates And Verdict Readiness

## Objective

Make release and model-gate decisions fail closed by sharing exact verdict parsing, running mechanical ship gates at the right time, and recording release blockers before any model ship-prep work.

This is P1 and checkpoint-shaped. It does not outrank validation, generation proof, snapshot promotion, or receipt semantics, but it directly unblocks a future release decision once those slices are green.

## Source Of Truth

- Runtime owner modules/APIs: `src/ship_command.rs::evaluate_ship_gate`, `src/ship_command.rs::run_ship`, `src/verdict.rs::exact_terminal_verdict`, `src/verdict.rs::terminal_verdict_is`, `src/super_command.rs::run_super_execution_gate`, `src/super_command.rs::verify_parallel_ready_plan`.
- UI consumers: `auto ship` stdout, `SHIP.md`, `QA.md`, `HEALTH.md`, `.auto/ship/**`, `.auto/super/<run-id>/EXECUTION-GATE.md`, `.auto/super/<run-id>/DETERMINISTIC-GATE.json`, design/audit/book GO/NO-GO reports.
- Generated artifacts: `SHIP.md`, `QA.md`, `HEALTH.md`, `.auto/ship/codex.stderr.log`, `.auto/logs/ship-*-prompt.md`, `.auto/super/<run-id>/EXECUTION-GATE.md`, `.auto/super/<run-id>/DETERMINISTIC-GATE.json`.
- Retired/superseded surfaces: any-line `Verdict: GO` scans; stale release decision text that says deterministic `auto ship` preflight is follow-on when live code already implements `evaluate_ship_gate`; ship-gate checks run before checkpoint/remote sync when the intended proof should describe the current branch state.

## Evidence Status

Verified facts grounded in code, docs, or commands:

- `src/verdict.rs:3-33` implements `exact_terminal_verdict`, requiring exactly one allowed terminal verdict line and rejecting invalid `Verdict:` lines.
- `src/verdict.rs:35-40` implements `terminal_verdict_is`.
- `src/verdict.rs:46-66` tests mixed verdict rejection and exact single-line matching.
- `src/super_command.rs:723-745` reads `.auto/super/<run-id>/EXECUTION-GATE.md` and accepts the gate when any line equals `Verdict: GO`.
- `src/super_command.rs:1444-1488` runs the deterministic parallel-ready plan gate and validates unchecked tasks before parallel execution.
- `src/ship_command.rs:122-172` evaluates release blockers from receipts, installed-binary proof, QA/HEALTH freshness, `SHIP.md`, and unresolved release blockers.
- `src/ship_command.rs:581-600` runs `evaluate_ship_gate` before model execution and records blockers or bypasses.
- `src/ship_command.rs:602-608` checkpoints or remote-syncs after the ship gate, so the current gate can run before the branch is synchronized.
- `docs/decisions/release-readiness-gate.md:9-18` says deterministic `auto ship` preflight should be added before model ship-prep.
- `docs/decisions/release-readiness-gate.md:76-82` says deterministic pre-model `auto ship` enforcement is follow-on implementation work, while live `src/ship_command.rs` already contains `evaluate_ship_gate`.
- Subagent command evidence from this generation pass: `cargo test verdict` passed 8 tests.
- Subagent command evidence from this generation pass: `cargo test deterministic_gate` passed 2 tests.
- Subagent command evidence from this generation pass: `cargo test ship_gate` had 7 passing tests and 2 failures: `ship_gate_uses_shared_receipt_inspector` and `ship_gate_rejects_stale_completion_receipt`.

Recommendations for the intended system:

- Make `run_super_execution_gate` use `exact_terminal_verdict` instead of an any-line scan.
- Run ship checkpoint/remote-sync before the first ship-gate evaluation, or prove by test that pre-sync gate facts remain valid after sync.
- Keep `SHIP.md` blocker and bypass sections as the operator-facing release truth.
- Update `docs/decisions/release-readiness-gate.md` so it reflects current implementation status after the code is fixed.

Hypotheses / unresolved questions:

- It is unresolved whether `auto ship` should rerun the gate immediately after remote sync even when no local checkpoint was created.
- It is unresolved whether `auto super` execution gate should allow `Verdict: GO` only as the final terminal line or anywhere as long as it is the only verdict.
- It is unresolved whether CI should expose a non-mutating release-gate helper, or whether `auto ship` remains the only entry point.

## Runtime Contract

`src/verdict.rs` owns model verdict parsing. Every gate that accepts or rejects a model-authored report must use the shared exact parser or document a narrower deterministic parser with tests.

`src/ship_command.rs` owns release readiness. If required receipts, installed-binary proof, fresh QA/HEALTH, rollback/monitoring/PR state, or blocker scans are absent or stale, ship must fail closed before model release-prep execution unless an explicit bypass reason is recorded.

## UI Contract

Release UI must make gate state visible without duplicating gate logic:

- `auto ship` stdout says passed, blocked, or bypassed.
- `SHIP.md` records branch, base branch, blockers, bypass reason, rollback path, monitoring path, PR/no-PR state, and final readiness.
- `EXECUTION-GATE.md` has exactly one accepted terminal verdict when it is model-authored.
- `DETERMINISTIC-GATE.json` records deterministic task readiness, not model confidence.

Presentation consumers must not treat report prose, stale QA/HEALTH files, or generated snapshots as release truth when runtime gate facts are missing.

Production UI must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; it must consume runtime helpers/generated contracts or render an explicit unavailable/error state when runtime truth is missing.

## Generated Artifacts

- `SHIP.md`
- `QA.md`
- `HEALTH.md`
- `.auto/ship/codex.stderr.log`
- `.auto/logs/ship-*-prompt.md`
- `.auto/super/<run-id>/EXECUTION-GATE.md`
- `.auto/super/<run-id>/DETERMINISTIC-GATE.json`

Refresh commands:

```bash
auto ship
auto ship --bypass-release-gate "<operator reason>"
auto qa
auto health
auto super --no-execute
```

## Fixture Policy

Release-gate tests may use synthetic receipts, synthetic QA/HEALTH/SHIP files, and temp repos. Production release readiness must read live receipts, live branch/base refs, live reports, and live blocker files. Fixture release reports cannot satisfy production release proof.

## Retired / Superseded Surfaces

- Any-line `Verdict: GO` scans for model-authored gate reports.
- Stale release decision prose that says deterministic ship preflight is unimplemented after live code owns it.
- Ship-gate order that evaluates stale branch state before checkpoint/remote sync without a rerun.

## Acceptance Criteria

- `run_super_execution_gate` rejects mixed `Verdict: GO` and `Verdict: NO-GO` reports.
- `run_super_execution_gate` rejects `Verdict: PASS-ish` or other invalid verdict variants.
- `auto ship` evaluates release gate facts after checkpoint/remote sync, or reruns the gate after sync.
- Stale validation receipts block `auto ship`.
- Missing installed-binary proof blocks `auto ship`.
- Stale QA/HEALTH reports block `auto ship`.
- Operator bypass records the reason in `SHIP.md` without claiming readiness.
- Decision docs describe the current implemented ship-gate status after the code lands.

## Verification

Run from `/home/r/coding/autodev`:

```bash
cargo test verdict
cargo test super_command::tests::deterministic_gate
cargo test ship_command::tests::ship_gate_uses_shared_receipt_inspector
cargo test ship_command::tests::ship_gate_rejects_stale_completion_receipt
cargo test ship_command::tests::ship_gate_fails_without_installed_binary_proof
cargo test ship_command::tests::ship_gate_bypass_records_operator_reason
```

Grep proof:

```bash
rg -n "exact_terminal_verdict|terminal_verdict_is|Verdict: GO|evaluate_ship_gate|bypass-release-gate|release gate" src/verdict.rs src/super_command.rs src/ship_command.rs docs/decisions/release-readiness-gate.md
```

## Review And Closeout

A reviewer should inspect every gate that accepts model-authored verdict text and confirm it uses the shared exact parser or has a stricter local parser. They should also check `run_ship` ordering so the gate evaluates the branch state that will actually be shipped.

Closeout must include at least one failing-before/passing-after test for stale receipts and one grep/assertion proof that `run_super_execution_gate` no longer uses an any-line `Verdict: GO` scan.

## Open Questions

- Should `auto ship` expose a no-model `--check-only` mode for CI, or remain a single operator command?
- Should `EXECUTION-GATE.md` require the verdict as the final non-empty line?
- Should release gate docs be updated in the same code slice or in the checkpoint closeout slice?
