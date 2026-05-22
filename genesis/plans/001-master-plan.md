# Master Plan: Restore The Trustworthy Auto Control Loop

## Priority Decision

P0. The next implementation cycle should restore the core control loop before adding more corpus, audit, or release artifacts. Score: highest user/operator value, high design clarity, high engineering leverage, direct evidence, and good parallel executability. This outranks broad documentation cleanup because the current checkout fails validation and has runtime/source-of-truth drift that generated prose cannot fix.

## User / Operator Outcome

An operator gets a short, executable sequence for making `auto` trustworthy again: validation goes green, generated snapshots stop mutating root truth unexpectedly, completion evidence becomes durable, first-run/status output tells the truth, and quota state handling closes the remaining focused security gaps.

## Evidence

- `AGENTS.md` requires `cargo test` and clippy validation.
- `.github/workflows/ci.yml` runs `cargo fmt --check`, clippy, tests, install proof, and help smoke.
- The observed `cargo fmt --check` run failed on `src/spec_command.rs`, `src/super_command.rs`, and `src/task_parser.rs`.
- The observed `cargo test` run failed 16 tests across generation/spec validation, task parsing, parallel lane status, receipt propagation, review, ship, and command-surface expectations.
- `docs/decisions/super-snapshot-promotion-default.md` and `docs/decisions/production-control-promotion.md` describe snapshot-first behavior while `src/super_command.rs` still calls generation with root-syncing flags.
- `WORKLIST.md` still lists required validation-proof and receipt hardening items while `IMPLEMENTATION_PLAN.md` is fully checked.

## Scope Boundary

This master plan does not implement code directly. It sets the dependency order for focused implementation slices. It intentionally excludes broad historical spec cleanup, new audit/report formats, release packaging, and UI expansion until validation and evidence semantics are stable.

## Implementation Slice

Goal: run the next cycle as seven bounded implementation slices plus one checkpoint:

1. Restore rustfmt and live command-surface truth.
2. Re-tighten generated-plan and spec-task proof validators.
3. Make `auto super` snapshot-first by default.
4. Repair receipt, loop, ship, and lane-evidence semantics.
5. Align first-run/status truth after the underlying facts stabilize.
6. Harden quota persistence and credential sync.
7. Run a release-readiness checkpoint before any promotion or release work.

Dependencies: slice 2 should follow slice 1 because validator work is easier once formatting and command-surface truth are not adding noise. Slice 3 should follow slices 1 and 2 or run only after its tests are isolated. Slice 4 can proceed after the relevant failing receipt/lane tests are isolated. Slice 5 should follow the core truth fixes so status output renders stable facts. Slice 6 can run independently if a worker owns quota files only. Slice 7 depends on slices 1 through 6 or an explicit operator decision to defer some of them.

Files to create or modify: none for this master plan. Downstream plans name their write scopes.

Tests to add or modify: none for this master plan.

Decision artifact: this file and `PLANS.md` are the sequencing artifact for the next implementation cycle.

Approach: keep this as a small dependency map, not a parallel implementation task. Use it to select the first executable plan, then retire or revise it after the release-readiness checkpoint records actual evidence.

Test expectation: none -- this file is a priority and sequencing decision, not a code behavior change.

## Verification

From the repository root, verify that the generated plan set exists and that later plans use the required compact headings:

    rg -n "^## (Priority Decision|User / Operator Outcome|Evidence|Scope Boundary|Implementation Slice|Verification|Deferred)$" .auto/corpus-staging/genesis-20260522-042812/plans
    test "$(find .auto/corpus-staging/genesis-20260522-042812/plans -maxdepth 1 -type f | wc -l)" -ge 8

Expected observation: each numbered plan includes all seven required section headings, and at least eight plan files exist.

## Deferred

- Root queue edits that convert this plan set into `IMPLEMENTATION_PLAN.md` rows.
- Release packaging and ship decisions.
- Broad stale-spec cleanup that does not affect the next executable slice.
