# Specification: Quality Pipelines And Release Lifecycle

## Objective
Align `auto review`, `auto qa`, `auto qa-only`, `auto health`, `auto audit`, `auto bug`, `auto nemesis`, `auto steward`, `auto super`, and `auto ship` around truthful artifacts, scoped edits, receipt-backed validation, and an explicit release-readiness decision.

## Evidence Status

### Verified Facts

- `auto review`, `auto steward`, `auto audit`, `auto ship`, `auto nemesis`, `auto qa`, `auto qa-only`, and `auto health` are top-level command variants in `src/main.rs:73-93`.
- `auto review` harvests completed `IMPLEMENTATION_PLAN.md` items into `REVIEW.md` before review work in `src/review_command.rs:78-120` and `src/review_command.rs:447-591`.
- `auto qa` asks for real validation evidence, maintains `QA.md`, can apply fixes, and stages only QA-relevant files plus named durable docs in `src/qa_command.rs:12-63`.
- `auto qa-only` is prompt-level report-only, writes `QA.md`, and tells the worker not to change source, tests, build config, or docs other than `QA.md` in `src/qa_only_command.rs:9-40`.
- `auto health` writes `.auto/health` and `.auto/logs` prompt artifacts, then invokes Codex in `src/health_command.rs:63-84`.
- `auto ship` is a prompted release workflow that asks for `SHIP.md`, exact validation, blockers, rollback notes, monitoring, PR URL, and post-push evidence in `src/ship_command.rs:16-67`.
- `auto audit` requires the doctrine prompt path defaulting to `audit/DOCTRINE.md` in `src/main.rs:939`, runs audit orchestration in `src/audit_command.rs:141-423`, and has broad commit staging through `commit_all` in `src/audit_command.rs:927-935`.
- `auto bug` runs finder, skeptic, review, fixer, and final-review phases with generated JSON and markdown artifacts in `src/bug_command.rs:231-555` and validates finder and skeptic outputs in `src/bug_command.rs:2316-2523`.
- `auto nemesis` writes audit and plan documents under `nemesis/`, then syncs a generated spec to root `specs/` and appends tasks to root `IMPLEMENTATION_PLAN.md` in `src/nemesis.rs:309-680` and `src/nemesis.rs:1983-2042`.
- `auto nemesis` requires implementation results to include validation commands in `src/nemesis.rs:1596-1598`.
- `NemesisArgs.audit_passes` is declared in `src/main.rs:1230-1233`, while `rg -n "audit_passes" src/nemesis.rs src/main.rs` only found argument plumbing and tests, not a run-loop driver in `src/nemesis.rs`.
- `auto steward` has a first pass and finalizer pass through Codex in `src/steward_command.rs:140-210`, and its prompt distinguishes steward work from `auto corpus` planning in `src/steward_command.rs:397-412`.
- `auto super` is implemented in `src/super_command.rs:55-340`, uses `genesis` as default planning root in `src/super_command.rs:64`, runs corpus/generation/reverse-style phases, verifies a parallel-ready plan, and can launch `auto parallel` with worker model and reasoning settings in `src/super_command.rs:216-251`.
- Plan 012 says release readiness should decide whether the current lifecycle is ready and how `steward` changes product direction in `genesis/plans/012-release-readiness-and-command-lifecycle-gate.md:1` and `genesis/PLANS.md:40`.

### Recommendations

- Define one release gate that records fmt, clippy, tests, smoke tests, installed-binary proof, QA/health freshness, review status, blockers, rollback, monitoring, and PR or no-PR state.
- Make receipt-backed verification available to review, QA, bug, nemesis, and ship instead of leaving proof mostly prompt-based outside parallel completion.
- Resolve stale or unused command promises before release, including audit escalation behavior, nemesis audit pass count, qa-only mechanical dirty-state enforcement, and snapshot-only generation semantics.
- Keep `auto steward`, `auto corpus`, `auto gen`, `auto reverse`, and `auto super` roles explicit so operators know which command owns planning, synthesis, hardening, and execution.

### Hypotheses / Unresolved Questions

- It is unresolved whether `auto ship` should remain prompt-only or become a mechanical release gate that can fail before invoking a model.
- It is unresolved whether `auto nemesis` should keep syncing root specs and plans automatically or require explicit promotion.
- It is unresolved whether `auto super` is a product direction or an experimental wrapper around corpus, generation, review, and parallel execution.

## Acceptance Criteria

- Release readiness cannot pass while required validation is red unless a blocker is recorded with owner, scope, and next action.
- `SHIP.md` records branch, base branch, exact validation commands, validation outcomes, blockers, rollback notes, monitoring notes, PR URL or no-PR reason, and installed-binary proof.
- `auto qa-only` either mechanically prevents non-`QA.md` modifications or fails with a dirty-state report when the report-only contract is violated.
- `auto audit` automated commits use scoped staging or document a narrowly accepted broad-staging exception.
- `auto bug` and `auto nemesis` both require validation commands for fixed findings before reporting a finding as fixed.
- `auto nemesis --audit-passes <n>` either drives an observable multi-pass loop or is documented as inactive until implemented.
- `auto steward`, `auto corpus`, `auto gen`, `auto reverse`, and `auto super` each have documented boundaries for root planning edits, generated snapshot edits, and implementation execution.
- Release lifecycle documentation identifies which command is primary for normal operator use and which commands are research, audit, or recovery tools.

## Verification

- `rg -n "SHIP.md|QA.md|HEALTH.md|REVIEW.md|audit_passes|commit_all|git add -A|run_super|run_steward" src README.md`
- `cargo test review_command::tests::harvest_completed_plan_items_flows_through_completed_queue`
- `cargo test bug_command::tests::normalization_keeps_substantive_finder_validation_strict`
- `cargo test nemesis::tests:: -- --list`
- Add and run qa-only dirty-state enforcement tests.
- Add and run ship-gate tests that fail on missing installed-binary proof and stale QA/health evidence.

## Open Questions

- Should release readiness be enforced by `auto ship`, CI, or a separate `auto release-check` command?
- Should `auto nemesis` be allowed to mutate root planning files during a security audit by default?
- Should `auto super` be documented as experimental until the release lifecycle gate is complete?
