# Focus Brief

## Raw Focus

You are the new CEO inheriting this codebase. Over the next 14 days, race it to production with unlimited compute and resources. Do not capacity-trim the ambition: prioritize the deliverables that maximize production readiness, then assume max parallel execution can attack them. Perfect design/runtime integrity first, then run equally rigorous functional reviews across product, engineering, security, reliability, QA, data/contracts, operations, release, DX, and performance. Keep auto corpus and auto gen as the control primitives, but shape the corpus toward release blockers, operator trust, verification evidence, first-run DX, and maintainable execution contracts.

You are the new CEO of autodev. Continue the production-readiness auto super campaign from the cleared design/runtime ledger state. Design gate blockers have been reconciled; proceed through CEO functional review, generation, execution gate review, and auto parallel execution. Prioritize semantic consistency, scheduler safety, runtime/design sync, resumability, implementation quality, verification receipts, agent usability, and release readiness. Keep changes tightly scoped and commit/push successful improvements.

## Normalized Focus Themes

- Production-readiness race, not feature ideation.
- Design/runtime integrity before throughput.
- Scheduler safety, resumability, and completion evidence before broad `auto parallel` launch.
- Verification receipts and release gate proof as first-class product surfaces.
- Operator trust: terminal output, queue truth, review ledgers, and recovery paths must agree.
- First-run DX should create a fast, honest success moment for a new operator.
- Functional review must cover product, engineering, security, reliability, QA, data/contracts, operations, release, DX, and performance.

## Implied Surfaces

- Code surfaces: `src/main.rs`, `src/generation.rs`, `src/corpus.rs`, `src/super_command.rs`, `src/parallel_command.rs`, `src/loop_command.rs`, `src/task_parser.rs`, `src/completion_artifacts.rs`, `src/verification_lint.rs`, `src/quota_*`, `src/ship_command.rs`, `src/design_command.rs`, `src/qa_only_command.rs`, `src/health_command.rs`, `src/audit_everything.rs`, `src/nemesis.rs`, and `src/util.rs`.
- Product surfaces: `auto corpus`, `auto gen`, `auto super`, `auto parallel`, `auto quota`, `auto design`, `auto review`, `auto qa-only`, `auto health`, `auto audit --everything`, `auto nemesis`, `auto ship`, `auto doctor`, and `auto --help`.
- Planning and evidence surfaces: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `WORKLIST.md`, `genesis/`, root `specs/`, `.auto/state.json`, `.auto/parallel/`, `.auto/symphony/verification-receipts/`, `.auto/ship/`, `SHIP.md`, `QA.md`, and `HEALTH.md`.
- Operational surfaces: CI, installed binary smoke tests, model backend wrappers, quota credential profiles, state checkpointing, dirty worktree handling, run manifests, logs, and release receipts.

## Repo-Wide Review Still Required

- Security cannot be scoped only to scheduler code because credential profile names, `.auto/state.json`, generated artifact paths, prompt delivery, and report-only write boundaries all cross module boundaries.
- Runtime/design sync cannot be scoped only to `DESIGN.md`; CLI help, README, specs, queue ledgers, and model prompts all render product truth.
- Release readiness requires the full chain from queue row to worker lane to receipt to review handoff to ship gate.
- DX requires the full first-run path: clone, install, required external tools, doctor output, help text, and the first meaningful command.
- Performance and reliability require large-file orchestrators, CI, status commands, manifest/resume behavior, and stale worker recovery.

## Main Questions

1. Can a new operator trust the root queue, `REVIEW.md`, receipts, and status output to mean the same thing?
2. Can `auto corpus` and `auto gen` be run without losing the previous planning corpus or trusting corrupted saved state?
3. Can `auto parallel` fail closed when plan truth is stale, lanes are stale, or completion evidence is incomplete?
4. Can quota failover avoid duplicate side effects and keep account profiles inside the intended namespace?
5. Can release gates prove current tree readiness after sync and after any model-driven ship pass?
6. Can a new developer get to a meaningful success moment without reverse-engineering a long README?

## Priority Impact

The focus moves the plan order toward safety and evidence before feature breadth. It keeps `auto corpus`, `auto gen`, and `auto parallel` as the control primitives, but it does not launch parallel execution from the current root queue because the root queue is cleared and the generated corpus is subordinate until accepted and promoted. The ordering therefore becomes:

1. Restore and harden planning corpus/state safety.
2. Close quota, scheduler, and completion-ledger risks that can corrupt operator trust.
3. Bind receipts and release gates to current tree state.
4. Normalize report-only, audit, nemesis, and DX contracts.
5. Run a final release decision gate before promoting work into the active root queue.
