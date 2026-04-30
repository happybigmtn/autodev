# Release Gate And Verdict Parser Hardening

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This slice makes GO/NO-GO and release readiness mean exactly one thing. The operator gains release gates that run against the current synced tree, rerun after model-driven ship work, and reject ambiguous reports with mixed or duplicated verdicts.

## Requirements Trace

- R1: `auto ship` must sync or checkpoint before evaluating release readiness.
- R2: `auto ship` must rerun the mechanical gate after every model iteration before declaring success.
- R3: Ship output must include a final status block comparable to other quality commands.
- R4: Design, audit, book, and release verdict parsing must use one exact terminal verdict helper.
- R5: Mixed, missing, or duplicated verdicts must fail closed.
- R6: Bypass reasons must remain explicit and recorded.

## Scope Boundaries

This plan does not change what release evidence is required beyond ordering and parser correctness. It does not automate deployment. It does not remove release-gate bypass; it keeps bypass explicit.

## Progress

- [x] 2026-04-30: Verified `auto ship` evaluates gate before remote sync.
- [x] 2026-04-30: Verified `auto ship` does not rerun the gate after model iterations.
- [x] 2026-04-30: Verified design, audit, and book verdict checks accept any matching line.
- [ ] Add shared verdict parser.
- [ ] Move ship sync before gate and add post-iteration gate.
- [ ] Add final ship status block.

## Surprises & Discoveries

- The release gate itself is much stronger than its current ordering.
- GO/PASS parsing risk appears in multiple mature commands, so a shared helper is better than local fixes.

## Decision Log

- Mechanical: Release proof evaluated before sync can become stale immediately.
- Mechanical: A report with both GO and NO-GO must not pass.
- Taste: Use a terminal verdict line near the end of the report rather than scanning any line, because it matches human review expectations.
- User Challenge: Bypass remains possible, but it must be treated as an explicit operator risk acceptance.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Relevant files:

- `src/ship_command.rs`: release gate evaluation, bypass recording, model iteration loop.
- `src/design_command.rs`: `Verdict: GO` design gate check.
- `src/audit_everything.rs`: final review GO check.
- `src/book_command.rs`: book quality PASS check.
- `src/health_command.rs`, `src/qa_only_command.rs`: examples of final status blocks.
- `SHIP.md`, `QA.md`, `HEALTH.md`: release evidence files.

Non-obvious terms:

- Terminal verdict: the single final verdict line accepted by a gate, such as `Verdict: GO`.
- Bypass: an operator-supplied reason allowing release gate continuation despite blockers.

## Plan of Work

Introduce a shared verdict parser that accepts a known set of verdict labels, requires exactly one terminal verdict line, and rejects mixed or duplicated verdicts. Replace local `any line matches` checks in design, audit, and book. Use the same helper for any release report parsing where applicable.

Change `auto ship` order so checkpoint/sync happens before release gate evaluation. After each model iteration, rerun the mechanical gate before continuing or declaring completion. Add a final status block with branch, gate status, blockers, run root, ship notes path, and next step.

## Implementation Units

- Unit 1: Shared verdict parser.
  - Goal: Reject ambiguous report verdicts.
  - Requirements advanced: R4, R5.
  - Dependencies: none.
  - Files to create or modify: new helper in `src/util.rs` or a focused `src/verdict.rs`, plus callers.
  - Tests to add or modify: accepts exactly one terminal `Verdict: GO`; rejects mixed GO/NO-GO; rejects two GO lines; rejects missing verdict.
  - Approach: Parse trimmed lines, record allowed verdict lines, require last non-empty verdict-compatible line to be unique and terminal under the chosen contract.
  - Test scenarios: report with early `Verdict: GO` and later `Verdict: NO-GO` fails.

- Unit 2: Replace design/audit/book checks.
  - Goal: Use identical verdict semantics across gates.
  - Requirements advanced: R4, R5.
  - Dependencies: Unit 1.
  - Files to create or modify: `src/design_command.rs`, `src/audit_everything.rs`, `src/book_command.rs`.
  - Tests to add or modify: command-specific mixed verdict fixtures.
  - Approach: Replace local `lines().any(...)` checks with shared helper and precise errors.
  - Test scenarios: design report containing both verdicts blocks generation.

- Unit 3: Ship gate order.
  - Goal: Evaluate release readiness on current synced tree.
  - Requirements advanced: R1, R6.
  - Dependencies: Plan 006 evidence helper if available.
  - Files to create or modify: `src/ship_command.rs`.
  - Tests to add or modify: sync happens before gate; bypass records pre/post-sync state; gate blocker after sync blocks.
  - Approach: Move checkpoint/sync before `evaluate_ship_gate`, preserving bypass recording.
  - Test scenarios: remote-sync-induced change invalidates receipt and gate fails.

- Unit 4: Post-model release gate and final status.
  - Goal: Prevent model iterations from changing readiness without recheck.
  - Requirements advanced: R2, R3.
  - Dependencies: Unit 3.
  - Files to create or modify: `src/ship_command.rs`.
  - Tests to add or modify: model pass changes `SHIP.md` or tree state, final gate reruns; final status printed.
  - Approach: Evaluate gate after each iteration and at loop exit; print a deterministic status block.
  - Test scenarios: post-iteration missing QA blocks release before success message.

## Concrete Steps

From the repository root:

    rg -n "Verdict: GO|Verdict: PASS|is_go|is_pass|evaluate_ship_gate|sync_branch_with_remote" src

Expected observation: current local verdict checks and ship gate order.

    cargo test design
    cargo test audit_everything
    cargo test book
    cargo test ship

Expected observation before work: new mixed-verdict and ship-order tests fail.

After implementation:

    cargo test design
    cargo test audit_everything
    cargo test book
    cargo test ship
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: all gate parser and ship ordering tests pass.

## Validation and Acceptance

Acceptance requires mixed verdict reports to fail in design, audit, and book contexts; `auto ship` gate evaluation must happen after sync; and ship must rerun mechanical readiness after model work. The final output must state release status and blockers unambiguously.

## Idempotence and Recovery

Verdict parsing is read-only. Ship gate reruns should not mutate evidence except for explicit bypass/blocker notes already owned by ship. If sync fails, gate should not run against stale local state. If model iteration changes files, rerun gate after checkpoint/sync as designed.

## Artifacts and Notes

- Evidence to fill in: shared parser test names.
- Evidence to fill in: ship final status example.
- Evidence to fill in: before/after mixed-verdict failure output.

## Interfaces and Dependencies

- Commands: `auto ship`, `auto design`, `auto audit --everything`, `auto book`.
- Files: `SHIP.md`, `QA.md`, `HEALTH.md`, design/audit/book reports.
- Modules: `ship_command`, `design_command`, `audit_everything`, `book_command`, shared verdict helper.
