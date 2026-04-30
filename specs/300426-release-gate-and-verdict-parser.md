# Specification: Release Gate And Verdict Parser

## Objective

Make release readiness and model report verdicts fail closed on stale gates, mixed verdicts, missing verdicts, and bypass ambiguity.

## Source Of Truth

- Runtime owners: `src/ship_command.rs`, `src/design_command.rs`, `src/audit_everything.rs`, `src/book_command.rs`, `src/health_command.rs`, `src/qa_only_command.rs`.
- Release owners: `SHIP.md`, `QA.md`, `HEALTH.md`, `.auto/ship/*`, `.auto/symphony/verification-receipts/*.json`, branch/base refs, PR state when `gh` is available.
- UI consumers: `auto ship` terminal output, `SHIP.md`, `auto design` design gate verdict, audit final review, book quality review, QA/health status blocks.
- Generated artifacts: `SHIP.md`, `QA.md`, `HEALTH.md`, `.auto/ship/*`, design reports, audit final review reports, book quality review reports.
- Retired/superseded surfaces: release gate before sync, no post-model gate rerun, any-line verdict search, mixed `GO`/`NO-GO` or `PASS`/`FAIL` reports that pass, and invisible bypasses.

## Evidence Status

Verified facts grounded in code or primary repo files:

- `ship_command` evaluates release readiness through `evaluate_ship_gate`, requiring cargo fmt, clippy, cargo test, cargo install, `auto --version`, QA, health, ship report, and blockers, verified by `rg -n "evaluate_ship_gate|missing validation receipt|check_release_report_freshness|check_ship_report" src/ship_command.rs`.
- `ShipArgs` has `--bypass-release-gate`, verified by `rg -n "bypass_release_gate|Bypass the pre-model release gate" src/main.rs src/ship_command.rs`.
- `ship_command` imports both `push_branch_with_remote_sync` and `sync_branch_with_remote`, but the planning corpus reports gate-order concerns that still require code review before declaring fixed, verified by `rg -n "evaluate_ship_gate|sync_branch_with_remote|push_branch_with_remote_sync" src/ship_command.rs`.
- `audit_everything` uses a `first_verdict_line`/`final_review_is_go` style helper for final review, verified by `rg -n "final_review_is_go|first_verdict_line|Verdict" src/audit_everything.rs`.
- `book_command` checks book quality with `quality_review_is_pass`, verified by `rg -n "quality_review_is_pass|PASS" src/book_command.rs`.
- `health_command` and `qa_only_command` provide report-style surfaces, verified by `rg -n "HEALTH.md|QA.md|Final status|status" src/health_command.rs src/qa_only_command.rs`.

Recommendations for the intended system:

- Create one exact terminal verdict parser used by design, audit, book, release, and future model report gates.
- Require exactly one terminal verdict in the expected final section or final lines; mixed, duplicated, missing, or contradicted verdicts fail closed.
- Run release mechanical gates after branch/base sync and rerun them after each model-driven ship iteration before declaring success.
- Preserve bypass reasons in `SHIP.md` and terminal output until replaced by real evidence.

Hypotheses / unresolved questions:

- Whether `auto ship` currently gates before sync must be verified by a focused test or direct code trace before implementation; the corpus flags it as a risk.
- The exact verdict grammar should remain small, but accepted labels for each command need inventory.
- PR creation/refresh behavior depends on `gh` availability and branch equality; live GitHub state is not verified by this snapshot.

## Runtime Contract

- `ship_command` owns release mechanical readiness and must gate against the current synced tree.
- A shared verdict parser owns model report verdict acceptance for design, audit, book, and release contexts.
- `health_command` and `qa_only_command` own report generation but must expose freshness and final status in forms ship can inspect.
- If sync changes the tree, if receipts are stale, if a report has mixed verdicts, or if a bypass exists without visible reason, ship readiness must fail closed.

## UI Contract

- `auto ship` must print a final status block: release status, blockers, bypass state, validations, PR/deploy state, rollback, and monitoring.
- `SHIP.md` must keep bypass reasons visible; a bypass is not readiness.
- Design/audit/book report UIs must place the accepted verdict in one terminal location, not rely on any matching line in the body.
- UI prose must not duplicate release gate checks; it must render the shared parser/gate result.
- Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- `SHIP.md`, `QA.md`, `HEALTH.md`.
- `.auto/ship/*` prompts and logs.
- Design reports under `.auto/design/*`.
- Audit final review files under `.auto/audit-everything/*`.
- Book quality review files under the selected CODEBASE-BOOK output.

## Fixture Policy

- Mixed-verdict and stale-release fixtures belong in tests.
- Production release must read live receipts, live reports, live branch refs, and live git status.
- Test reports must use synthetic names and must not be copied into root `SHIP.md`, `QA.md`, or `HEALTH.md`.

## Retired / Superseded Surfaces

- Retire any-line verdict scans.
- Retire release readiness evaluated before syncing the branch/base truth.
- Retire success output that does not rerun the mechanical gate after model edits.
- Retire hidden bypass state that only exists in CLI args.

## Acceptance Criteria

- A report containing both positive and negative verdict labels fails in design, audit, book, and release contexts.
- A report with no terminal verdict fails with an actionable message naming the expected final verdict format.
- A report with two terminal verdicts fails closed.
- `auto ship` syncs/checkpoints current branch/base truth before evaluating the release gate.
- `auto ship` reruns the mechanical gate after each model iteration and blocks if new edits make receipts or QA/health stale.
- `SHIP.md` and stdout show final status and any bypass reason.

## Verification

- `cargo test ship_command::tests`
- `cargo test design_command::tests`
- `cargo test audit_everything::tests`
- `cargo test book_command::tests`
- `rg -n "evaluate_ship_gate|bypass_release_gate|final_review_is_go|quality_review_is_pass|Verdict" src/ship_command.rs src/design_command.rs src/audit_everything.rs src/book_command.rs`
- `rg -n "QA.md|HEALTH.md|SHIP.md" src/ship_command.rs src/qa_only_command.rs src/health_command.rs`

## Review And Closeout

- A reviewer runs mixed-verdict fixtures for every command wired to the shared parser and confirms they fail.
- Grep proof must show old local verdict helpers are removed or delegated to the shared helper.
- A reviewer traces `auto ship` order with a focused fixture: sync/checkpoint, gate, model iteration, post-iteration gate, final status.
- Closeout includes one red-gate example and one green-gate example with no bypass.

## Open Questions

- Should verdict parser live in a new module such as `src/verdict.rs`?
- Should accepted terminal verdicts be `Verdict: GO/NO-GO` everywhere, or command-specific labels?
- How should `auto ship` behave when `gh` is unavailable but all local release evidence is green?
