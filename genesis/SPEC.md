# System Specification

## Product Frame

`auto` is a Rust command-line control plane for model-backed repository work. It helps an operator inspect a checkout, generate or refresh a planning corpus, design the next implementation slice, dispatch agent lanes, reconcile completion evidence, review changes, run quality gates, and prepare release or ship decisions.

The repository is not yet production-clean. The near-term system goal is therefore operational trust: the CLI must make one bounded autonomous implementation loop understandable, verifiable, and recoverable before it expands planning artifacts or release ceremony.

## Primary Users

- Operator: runs `auto` from the repository root, decides what work is allowed, watches status, and accepts or rejects completion.
- Worker agent: implements one lane task and leaves evidence in commits/logs rather than mutating host-owned shared queue files directly.
- Reviewer/release operator: inspects whether the claimed work has durable proof and whether ship gates are satisfied.
- Contributor: changes the Rust CLI and needs fast, honest local validation.

## Core Behaviors Verified From Code

- The binary is named `auto` and is built from the `autodev` Rust package, verified from `Cargo.toml`.
- CLI command routing is defined in `src/main.rs` with commands for book/spec/corpus/gen/design/super/loop/parallel/review/qa/health/audit/audit-harvest/ship/quota/Symphony-related flows.
- `auto doctor` is a no-model preflight implemented in `src/doctor_command.rs`.
- `auto parallel` owns host-side queue reconciliation, lane dispatch, lane closeout, receipt propagation, and plan row updates in `src/parallel_command.rs`.
- `auto loop` runs a serial worker loop and now checks task completion evidence before accepting a worker-marked `[x]` row in `src/loop_command.rs`.
- Execution-row parsing and validation are shared through `src/task_parser.rs`.
- Verification command linting is centralized in `src/verification_lint.rs`, with generation/spec flows also validating generated plan proof.
- Completion evidence inspection lives in `src/completion_artifacts.rs` and combines review handoff, declared artifacts, verification receipts, commit footers, and audit-finding closure.
- Quota account and backend routing are implemented across `src/quota_config.rs`, `src/quota_exec.rs`, `src/quota_state.rs`, `src/quota_status.rs`, `src/quota_usage.rs`, and backend modules.
- CI is defined in `.github/workflows/ci.yml` and runs formatting, clippy, tests, install proof, and help smoke.

## Near-Term Direction

The recommended near-term direction is evidence-first stabilization:

1. Restore the smallest local validation baseline: rustfmt and live command-surface truth.
2. Re-tighten generated-plan and spec-task proof validators.
3. Make `auto super` match the accepted snapshot-first promotion policy.
4. Tighten receipt/loop/ship/parallel evidence semantics until completion proof is durable and unambiguous.
5. Make first-run/status and quota hardening follow as bounded P1 implementation slices.

This direction deliberately ranks code, tests, runtime contracts, and operator-visible state above additional audit documents or broad corpus expansion.

## Required Product Invariants

- Runtime truth beats generated prose. If a doc says a behavior exists and code disagrees, code is the current fact and the plan should fix either the code or the doc deliberately.
- Root control ledgers remain the active planning surface unless the operator explicitly promotes generated outputs.
- Generated snapshots are reviewable staging artifacts, not silent replacements for root queue truth.
- Completion is not a markdown checkbox by itself. A completed task needs evidence that the product can inspect and that survives the local staging directory.
- Broad verification commands are suspect. A plan should prefer narrow commands that would fail if the relevant regression returned.
- Missing optional model tools should be reported as capability limits, while missing baseline repo/build requirements should fail clearly.
- Security-sensitive state should fail closed on invalid persisted data and avoid truncation or unsafe symlink writes.

## Current Broken Behaviors

- `cargo fmt --check` fails in the current checkout.
- `cargo test` fails with 16 tests in the observed run. Those failures are not one task; they split into command-surface, generated-plan validation, receipt/ship, and lane-semantics clusters.
- `auto super` still invokes generation with root-syncing behavior despite snapshot-first decisions.
- The live command surface includes `audit-harvest`, while README/test expectations are stale.
- Receipt freshness, ship gating, loop evidence, operator lanes, and generated-plan validation are inconsistent across code and tests.

## State Model

- Root ledgers: `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `REVIEW.md`, `COMPLETED.md`, `ARCHIVED.md`, and related decision/spec docs.
- Runtime/generated state: `.auto/`, `bug/`, `nemesis/`, and `gen-*` are excluded from checkpoints per repo instructions and `src/util.rs`.
- Receipt staging: `.auto/symphony/verification-receipts/` is compatibility/staging data. Durable proof should travel in commit footers.
- Generated corpus staging: this corpus under `.auto/corpus-staging/` is a planning artifact for the next implementation cycle, subordinate to root control files.

## Non-Goals For The Next Cycle

- No web UI.
- No release packaging push while validation is red.
- No large documentation refresh unless it prevents a worker or operator from following stale active instructions.
- No provider-specific prompt transport redesign without primary evidence that the backend supports a safer mode.
- No broad rewrite of orchestration modules before the current failing contracts are triaged and repaired.
