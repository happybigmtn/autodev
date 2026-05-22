# Assessment

## Problem Statement

How might we make `auto` a trustworthy non-production control plane for model-backed repository work, so an operator can understand the current state, dispatch a bounded implementation slice, and accept or reject completion based on evidence rather than generated prose?

The current code shows a real product, not just a planning repo. The binary `auto` is a Rust CLI that generates plans, launches agents, reconciles queues, inspects verification receipts, routes quota-backed model calls, and gates shipping. The highest-leverage next cycle is not more corpus volume. It is restoring deterministic validation and aligning runtime source-of-truth behavior with the accepted decisions the operator already has.

## Independent Review Addendum

This outside review inspected the staged corpus files, root control ledgers, README, AGENTS instructions, CI workflow, current decision docs, and targeted runtime owners in `src/main.rs`, `src/super_command.rs`, `src/generation.rs`, `src/doctor_command.rs`, `src/task_parser.rs`, `src/completion_artifacts.rs`, `src/parallel_command.rs`, `src/ship_command.rs`, `src/quota_config.rs`, `src/quota_state.rs`, and `src/quota_exec.rs`.

Verified in this review:

- No root `PLANS.md` or root `plans/` directory exists in the current checkout, so this staged corpus is not the active root planning surface.
- Root `IMPLEMENTATION_PLAN.md` has no unchecked task rows, while `WORKLIST.md` still contains required follow-up items.
- `cargo fmt --check` fails on formatting in `src/spec_command.rs`, `src/super_command.rs`, and `src/task_parser.rs`.
- `cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture` fails because live Clap output includes `audit-harvest`.
- `cargo test generation::tests::generated_plan_rejects_multiple_cargo_test_filters -- --nocapture` fails, confirming generated-plan verification strictness drift.
- `cargo test ship_command::tests::ship_gate_rejects_stale_completion_receipt -- --nocapture` fails, confirming receipt/ship gate drift.
- `cargo test parallel_command::tests::lane_kind_routes_operator_and_evidence_tasks -- --nocapture` fails, confirming lane-kind drift.
- `cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs -- --nocapture` passes, so the generator has snapshot-only support; the remaining product drift is that `auto super` does not select it.

Supervisor revision before generation: split the original broad validation priority into two worker-sized implementation slices. The first owns rustfmt and live command-surface truth; the second owns generated-plan/spec-task proof validator strictness. Receipt/ship/lane failures remain in the evidence-contract slice. This prevents `auto gen` from producing one oversized "fix all red tests" task.

Not independently rerun in this outside pass: full `cargo test` and clippy. The authoring pass's reported full-suite result is retained as prior evidence, while this review relies on the targeted failures above plus direct source inspection.

## Project Claim vs Code Reality

| Area | What the project says | What the code shows | Status |
| --- | --- | --- | --- |
| Product identity | `README.md` presents `auto` as a repo-root planning and execution workflow with twenty-one commands. | `src/main.rs` exposes a broader control plane, including `audit-harvest`, planning, design, execution, review, QA, health, audit, ship, quota, and Symphony commands. | Useful but drifted. |
| Active planning surface | Root ledgers and decisions are canonical; generated genesis snapshots are subordinate unless explicitly promoted. | Root `IMPLEMENTATION_PLAN.md` is fully checked, `WORKLIST.md` still has required validation-proof items, and current generated snapshots are historical/staging. | Root control remains active; corpus is advisory. |
| Snapshot-first production control | Accepted decisions say snapshot creation should be reviewable and root sync should be explicit. | `src/super_command.rs` invokes generation with `snapshot_only: false`, which can sync root outputs during `auto super`. | Runtime/docs mismatch. |
| Evidence model | Durable proof should travel in commit footers; `.auto/` JSON receipts are compatibility/staging data. | `src/completion_artifacts.rs`, `src/parallel_command.rs`, and `src/loop_command.rs` partly enforce evidence, but recent behavior changes made receipt freshness and fallback semantics fail tests. | Half-built and currently red. |
| First-run story | `README.md` points new operators to `auto --version` and `auto doctor`. | `auto doctor` is useful, but it checks planning readiness early and external tools are treated differently across `AGENTS.md`, README, and code. | Good base with clarity gaps. |
| CI readiness | `AGENTS.md` says validate with `cargo test` and clippy; CI also runs fmt, test, install, and help smoke. | Local `cargo fmt --check` and `cargo test` currently fail. | Broken now. |

Exact package facts verified from `Cargo.toml`: package name `autodev`, binary name `auto`, version `0.2.0`, Rust edition `2021`. Dependency versions are governed by `Cargo.toml` and `Cargo.lock`; this assessment does not promote any guessed dependency update.

## What Works

- The repository has a substantial Rust CLI with focused modules for generation, corpus creation, design review, execution loops, parallel orchestration, completion evidence, quota routing, backend execution, QA, audit, health, and shipping.
- `auto doctor` provides a no-model preflight and already separates some capability warnings from hard failures.
- `auto parallel status` has useful operator state: host PID, tmux/session state, lane state, safety verdict, and health summary.
- The quota account path hardening work is real: account names are slug-validated, profile directories are containment-checked, credential capture rejects symlinks, and owner-only writes are used in several paths.
- CI is meaningful: `.github/workflows/ci.yml` runs formatting, clippy with denied warnings, tests, installed-binary proof, and help smoke.
- Test density is high for a non-production tool. Explorer review counted roughly 589 annotated tests, concentrated around orchestration, generation, evidence, and quota behavior.
- The repo already has decision records for promotion policy, receipt policy, first-run preflight, backend invocation, and quota prompt transport. These are useful when they match code.

## What Is Broken

- `cargo fmt --check` fails on formatting in `src/spec_command.rs`, `src/super_command.rs`, and `src/task_parser.rs`.
- The authoring pass reported a red full `cargo test` run: 569 passed and 16 failed. This outside review did not rerun the full suite, but independently reproduced representative failures in command-surface validation, generated-plan strictness, ship receipt freshness, and lane-kind routing.
- `src/main.rs` exposes `audit-harvest`, but the top-level command-surface test and README command count are stale.
- Current accepted docs say `auto super` should be snapshot-first, while runtime still calls generation in a root-syncing mode.
- `WORKLIST.md` still records required hardening for generated verification commands and ambiguous receipts while root `IMPLEMENTATION_PLAN.md` is fully checked.
- `.auto/symphony/verification-receipts/` was absent in the current checkout despite the plan ledger being fully checked; durable proof may exist in commit footers, but the current tree does not present a simple local receipt trail.

## What Is Half-Built

- Receipt inspection now tries commit footers and JSON staging receipts, but failing ship/completion tests show the precedence and freshness semantics are not settled.
- `auto loop` demotes false `[x]` rows when evidence is missing, but durable footer behavior is not aligned with parallel closeout.
- Operator/evidence lane semantics are split: docs and tests expect operator queue behavior, while live code disables operator routing and dispatches operator-labeled rows as autonomous code lanes.
- The generated-corpus prompt in `src/generation.rs` has been steered toward compact priority plans and implementation focus, but the validator changes are part of the current red test cluster.
- Quota profile isolation has strong pieces, including new isolated Codex home support in the dirty worktree, but atomic persistence, load-time validation, and Claude credential sync hardening still need one focused pass.
- First-run UX has useful components (`doctor`, `parallel status`, `quota status`, health output), but they are scattered and sometimes disagree about whether missing tools or missing planning files are blockers.

## Tech Debt Inventory

| Debt | Evidence | Impact | Priority |
| --- | --- | --- | --- |
| Validation strictness drift | Failing generation/spec/task-parser/verification lint tests. | Workers may accept weak proof, broad verification, or malformed plan rows. | P0 |
| Runtime promotion mismatch | `src/super_command.rs` invokes generation with root-syncing flags despite snapshot-first decisions. | `auto super` can alter root ledgers when operator expects reviewable snapshots. | P0 |
| Receipt semantics drift | Ship and completion-artifact tests fail; `.auto` receipt staging absent. | False green or false red ship decisions. | P1 |
| Lane-kind ambiguity | Operator lane docs/tests disagree with live code. | Operators cannot predict whether tasks become code lanes or manual queues. | P1 |
| Command-surface drift | README/test omit `audit-harvest`; help smoke incomplete. | First-run docs and automated guard disagree with product surface. | P1 |
| Stale historical specs | Older specs claim no `doctor`, fewer commands, missing install proof, and older quota behavior. | New workers may follow outdated context. | P2 unless active instructions cite them. |
| Non-atomic quota persistence | `QuotaConfig::save` and `QuotaState::save` write directly with owner-only mode but no temp+rename. | Crash or interruption can corrupt quota state. | P1 |
| Prompt argv exposure for Kimi/PI | Accepted limitation in backend decision and tests. | Prompt content can appear in process listings. | P2 research unless provider supports safer transport. |

## Security Risks

- Quota account path traversal is mostly mitigated by account slug validation and profile containment checks.
- Credential capture rejects symlink inputs and writes owner-only files in the tested capture/swap paths.
- Remaining security concern: quota config/state saves are owner-only but not atomic, so interrupted writes can corrupt the operator's routing state.
- Remaining security concern: Claude credential sync uses raw copy behavior in one path before the same level of symlink refusal and owner-only enforcement is applied.
- Remaining security concern: Kimi and PI prompt transport still passes full prompts through command arguments. This is currently an accepted limitation, not a verified provider-safe design.
- Checkpoint exclusions in `src/util.rs` cover generated/runtime state including `.auto/`, `bug/`, `nemesis/`, and `gen-*`, consistent with repo instructions.

## Test Gaps

- The current top-level command-surface test is stale rather than protective; it fails because `audit-harvest` exists.
- CI help smoke does not include `auto doctor --help` or `auto audit-harvest --help`.
- Validation tests reveal real contract ambiguity around broad cargo filters, bin-only crates, malformed grep commands, bold-field parsing, and ownership field parsing.
- Ship gate tests fail around stale completion receipts and shared receipt inspection.
- Parallel tests fail around operator/evidence lanes and receipt propagation dirty-state handling.
- Quota tests cover many security paths, but additional tests should cover load-time validation, atomic save behavior, and Claude credential sync mode/symlink handling.

## Documentation Staleness

- `README.md` command count and command list are stale relative to `src/main.rs`.
- `AGENTS.md` says `claude`, `codex`, `pi`, and `gh` are required tools, while `auto doctor` and README treat missing model tools as capability warnings rather than baseline failure. This is a real policy choice that should be stated consistently.
- `specs/220426-cli-command-surface.md` claims a smaller command surface and no `doctor`; current code and README contradict it.
- `specs/230426-first-run-ci-and-installed-binary-proof.md` says CI lacks install/version proof and no `Doctor` variant exists; current CI and code contradict it.
- `docs/decisions/loop-receipt-gating.md` says loop receipt enforcement is prompt-only and not Rust-side demotion; current `src/loop_command.rs` demotes missing-evidence completions.
- Genesis plans retain old unchecked rows for work now checked in root `IMPLEMENTATION_PLAN.md`. This is acceptable only when genesis is treated as subordinate historical context.

## Implementation Status For Prior Claims And Plans

| Prior claim or plan | Current evidence | Assessment |
| --- | --- | --- |
| Root implementation plan is complete. | `IMPLEMENTATION_PLAN.md` rows are all `[x]`. | Claim is ledger-complete, but validation is red and `WORKLIST.md` still has required hardening. |
| Genesis previous quota/security P0s are completed. | Quota traversal and credential capture tests exist; dirty worktree extends Codex home isolation. | Mostly implemented, but atomic save/load validation remains. |
| Receipt JSON is staging data; durable proof belongs in commit footers. | Repo instructions and receipt docs say this; code has footer and JSON inspection. | Direction is right, current semantics fail tests. |
| Snapshot-first promotion is accepted. | Decision docs and planning primacy tests support generated snapshots as subordinate. | Runtime `auto super` still needs to match. |
| First-run preflight exists. | `src/doctor_command.rs` exists and is exposed by `src/main.rs`. | Useful, but policy and messaging need a focused pass. |
| Generated corpus should be implementation-first. | Dirty `src/generation.rs` and prompt ethos changes move in that direction. | Correct direction, but plan validation failures show it is incomplete. |

## Code Review Coverage

Local review and explorer review covered these source and test areas:

- Entry and CLI surface: `src/main.rs`, `Cargo.toml`, `README.md`, `AGENTS.md`.
- Generation and planning: `src/generation.rs`, `src/corpus.rs`, `src/spec_command.rs`, `src/design_command.rs`, root control ledgers, `genesis/`, prior snapshot under `.auto/fresh-input/`.
- Execution and orchestration: `src/super_command.rs`, `src/parallel_command.rs`, `src/loop_command.rs`, `src/task_parser.rs`, `src/completion_artifacts.rs`, `src/verification_lint.rs`.
- Operator surfaces: `src/doctor_command.rs`, `src/health_command.rs`, `src/qa_only_command.rs`, `src/audit_command.rs`, `src/audit_everything.rs`, `src/ship_command.rs`.
- Quota and backend security: `src/quota_accounts.rs`, `src/quota_config.rs`, `src/quota_exec.rs`, `src/quota_patterns.rs`, `src/quota_selector.rs`, `src/quota_state.rs`, `src/quota_status.rs`, `src/quota_usage.rs`, `src/backend_policy.rs`, `src/codex_exec.rs`, `src/claude_exec.rs`, `src/kimi_backend.rs`, `src/pi_backend.rs`.
- State and utilities: `src/state.rs`, `src/util.rs`.
- CI and tests: `.github/workflows/ci.yml`, `tests/parallel_status.rs`, `tests/performance_status.rs`, `tests/lifecycle_flows.rs`, `tests/planning_primacy.rs`, plus embedded module tests reported by `cargo test`.
- Docs and decisions: `docs/decisions/*.md`, `docs/verification-receipt-schema.md`, root `DESIGN.md`, `WORKLIST.md`, `REVIEW.md`, `COMPLETED.md`, `ARCHIVED.md`, and specs under `specs/`.

## Target Users

- Primary operator: a technical maintainer running `auto` in a repository to plan, dispatch, review, and ship model-backed work.
- Lane worker: a model-backed implementation agent that consumes one task row and must preserve evidence in commits/logs instead of editing shared host-owned queue files directly.
- Reviewer or release operator: a human or model phase deciding whether a claimed completion has enough proof.
- Developer contributor: someone changing the `auto` CLI, tests, and orchestration internals.

## Success Criteria

- `cargo fmt --check`, `cargo test`, and clippy pass locally and in CI.
- `auto super` creates reviewable snapshots unless the operator explicitly requests root promotion.
- Completed tasks require durable evidence that survives checkout state and can be inspected by `auto ship`.
- `auto doctor` and status surfaces tell the operator what is ready, blocked, and safe to run next.
- The README and command-surface tests match live Clap behavior.
- Quota account state and credential handling fail closed on unsafe persisted data and avoid corrupting state on interrupted writes.

## Repo Constraints

- The repository is a non-production but real operator tool; it should prefer small verified slices over release-scale ceremony.
- Generated/runtime state under `.auto/`, `bug/`, `nemesis/`, and `gen-*` is excluded from checkpoints.
- Host-owned parallel queue files include `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, legacy queue files, and `RECEIPTS-DRIFT.md`; lane workers should preserve evidence in commits/logs instead of editing those shared files directly.
- Model tools may not all be present on first run; code already treats some as capability warnings.
- The current worktree was dirty before this corpus pass in source files related to generation, prompt ethos, parallel receipts, quota, and backend execution. This assessment treats those changes as current working state and does not revert them.

## Assumption Ledger

| Assumption | Status | Proof or next check |
| --- | --- | --- |
| `auto` is developer/operator-facing rather than end-user consumer software. | Verified. | CLI entry point, README lifecycle, root control ledgers. |
| Root planning ledgers remain active control truth. | Verified. | Repo instructions, root ledgers, planning primacy decisions/tests; no root instruction makes this staging corpus primary. |
| The current checkout is not validation-clean. | Verified. | `cargo fmt --check` failed; `cargo test` failed with 16 tests. |
| Snapshot-first promotion is accepted product direction. | Verified as decision text; not implemented in runtime. | `docs/decisions/production-control-promotion.md`, `docs/decisions/super-snapshot-promotion-default.md`, `src/super_command.rs`. |
| Receipt footers should outrank `.auto` JSON for durable proof. | Verified as policy; implementation currently unstable. | Repo instructions, receipt docs, failing ship/completion tests. |
| Kimi/PI prompt argv transport can be replaced now. | Open question. | Current decision accepts argv as limitation pending provider-supported alternative. |
| Missing model tools should not fail first-run baseline. | Mostly verified in code/docs, contradicted by `AGENTS.md`. | `src/doctor_command.rs`, README, `AGENTS.md`. |

## Focus Response

The operator focus emphasized implementation, product clarity, and engineering leverage over more artifact generation. The code supports that emphasis: the most urgent problems are failing validation, runtime/source-of-truth drift, evidence semantics, and operator status clarity. Non-focused risks that still matter are quota credential safety and prompt exposure. They do not outrank green validation and promotion semantics because they are narrower and mostly behind existing mitigations, but they should remain in the first implementation wave after the core loop is trustworthy.

## Opportunity Framing

Recommended direction: make `auto` a small, honest, evidence-first control plane for one complete autonomous slice at a time. The product should bias toward explicit state, strict proof, and safe promotion rather than generating more plans or reports.

Alternative considered: build more corpus/reporting infrastructure first. Rejected because the checkout is red and more generated artifacts would not prove the product loop.

Alternative considered: prioritize quota security before orchestration. Rejected as the top priority because quota has several working mitigations and tests, while the central validation/receipt/super path is currently failing.

Alternative considered: add a new dashboard or UI first. Rejected as the immediate top priority because status clarity is useful only after validation and source-of-truth semantics stop contradicting themselves. A no-model status command remains a strong follow-up once the underlying facts are stable.

## Not Doing

- Do not create a large new documentation suite before fixing red validation.
- Do not rewrite the CLI architecture or replace Clap.
- Do not introduce a web UI; the current user-facing product is terminal and markdown ledgers.
- Do not treat previous genesis snapshots as truth over current code.
- Do not silently promote generated snapshots to root control truth.
- Do not broaden scope to release packaging until the local validation and evidence gates are green.
- Do not resolve Kimi/PI prompt transport by inventing unverified provider behavior.

## Priority Focus Map

| Focus area | User/operator value | Design clarity | Engineering leverage | Evidence | Parallel executability | Rank |
| --- | --- | --- | --- | --- | --- | --- |
| Restore fmt and public command-surface baseline. | High: operators need advertised validation and help truth. | High: pass/fail and command availability become clear. | Medium: small surface with immediate CI value. | `cargo fmt --check` and command-surface test failures. | High: one worker can own formatting plus command-list tests/docs. | 1 |
| Re-tighten generated-plan and spec-task proof validators. | High: weak plans create unproductive parallel runs. | High: workers get concrete proof contracts. | High: repairs shared parser/validator behavior. | Generation/spec validation test failures. | High: one worker can own validator modules/tests. | 2 |
| Make `auto super` snapshot-first by default. | High: prevents unexpected root ledger mutation. | High: snapshots vs promotion becomes understandable. | High: fixes source-of-truth drift. | Decision docs contradict `src/super_command.rs`. | Medium: one worker can modify super/generation tests. | 3 |
| Repair receipt, loop, ship, and lane evidence semantics. | High: completion proof is core product truth. | Medium: reduces false green/false red states. | High: centralizes proof semantics. | Failing ship/parallel/completion tests and receipt docs. | Medium: focused but touches several modules. | 4 |
| Align operator status and harden quota persistence. | Medium/high: operators need next safe command and safe account state. | Medium/high: readiness and failure modes become legible. | Medium: reuses doctor/status/quota helpers. | README/test drift and quota load/save gaps. | Medium/high: split into separate P1 worker slices. | 5 |

## DX Assessment

First-run friction is lower than earlier specs suggest because CI has installed-binary proof and `auto doctor` exists. The current problem is honesty, not absence. `auto doctor` is positioned as the first success path, but it checks planning health in a way that can fail before the operator has created the planning surface. README and `AGENTS.md` also disagree about whether model tools are required at T0 or only for model-backed phases.

Copy-paste onboarding is mostly honest for a repo that already has plans, but less honest for a fresh repo. The fastest meaningful success moment should be: build or install `auto`, run `auto doctor`, see baseline repository readiness, then receive a precise next command for planning or execution readiness. Today that path can blur baseline readiness with execution readiness.

Error clarity is improving in validators and receipt inspectors, but the current failing tests show strictness has become inconsistent. The next DX win is not a tutorial. It is a no-model state summary that says which facts are verified, which checks are red, and which command is safe next.
