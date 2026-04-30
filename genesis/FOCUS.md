# Focus Brief

## Raw Focus String

You are the new CEO inheriting this codebase. Over the next 14 days, race it to production with unlimited compute and resources. Do not capacity-trim the ambition: prioritize the deliverables that maximize production readiness, then assume max parallel execution can attack them. Perfect design/runtime integrity first, then run equally rigorous functional reviews across product, engineering, security, reliability, QA, data/contracts, operations, release, DX, and performance. Keep auto corpus and auto gen as the control primitives, but shape the corpus toward release blockers, operator trust, verification evidence, first-run DX, and maintainable execution contracts.

You are the new CEO of autodev. Review the autodev repository as the canonical autonomous development framework. Identify the highest-impact production-readiness improvements to the auto spec, auto super, auto parallel, auto review, auto design, auto audit, and end-to-end workflow surfaces. Prioritize suggestions that improve semantic consistency, scheduler safety, runtime/design sync, resumability, implementation quality, verification receipts, and agent usability. Generate any needed specs and plan items, then implement the approved queue with auto parallel. Keep changes tightly scoped and commit/push successful improvements.

## Normalized Focus Themes

- Production readiness before feature expansion.
- Runtime truth before planner presentation, especially for `auto corpus`, `auto gen`, `auto parallel`, `auto review`, `auto design`, `auto audit`, `auto super`, and `auto ship`.
- Operator trust through receipt freshness, clean queue state, rollback-safe corpus generation, dependency-safe scheduling, and clear release gates.
- Agent usability through honest first-run checks, consistent dry-run semantics, deterministic plan schemas, and recovery paths that preserve evidence.
- Parallel execution as the future delivery mechanism, but only after the queue and credential model are safe enough to run many lanes at once.

## Likely Surfaces

Code surfaces implied by the focus:

- `src/main.rs` command definitions and defaults.
- `src/corpus.rs` and `src/generation.rs` for `genesis/` creation, corpus validation, `auto gen`, and root-plan synchronization.
- `src/parallel_command.rs`, `src/task_parser.rs`, and `src/completion_artifacts.rs` for scheduler truth, dependency parsing, lane recovery, and completion evidence.
- `src/quota_config.rs`, `src/quota_exec.rs`, `src/quota_accounts.rs`, and model execution modules for credential safety and backend routing.
- `src/review_command.rs`, `src/design_command.rs`, `src/health_command.rs`, `src/qa_only_command.rs`, `src/super_command.rs`, `src/audit_everything.rs`, `src/audit_command.rs`, `src/nemesis.rs`, `src/symphony_command.rs`, and `src/ship_command.rs`.
- `scripts/run-task-verification.sh`, `scripts/verification_receipt.py`, `.github/workflows/ci.yml`, `README.md`, `AGENTS.md`, `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `REVIEW.md`, `ARCHIVED.md`, `specs/`, and `docs/decisions/`.

Product and operational surfaces implied by the focus:

- First-run command path: install, `auto --version`, `auto doctor`, help surfaces, and tool availability.
- Planning path: `auto corpus`, `genesis/`, `auto gen --snapshot-only`, root `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, and dated specs.
- Execution path: `auto parallel`, tmux lanes, salvage notes, receipts, review handoffs, and completion artifacts.
- Release path: `auto ship`, CI install proof, QA/health/design/review reports, tag evidence, and rollback notes.

## Repo-Wide Review Still Required

The focus is broad but it is still biased toward autonomous workflow surfaces. The review still covered and should continue to cover:

- Security boundaries outside scheduler code, especially quota account path handling, credential activation, prompt transport, and dangerous model execution flags.
- Documentation and spec staleness that can feed bad prompts into otherwise correct automation.
- Test and CI gaps around scripts, shell wrappers, installed binaries, and report-only commands.
- Performance and reliability issues created by very large command modules, long-running child processes, and missing timeout contracts.
- Design and DX as terminal/operator experiences, not only web or visual UI.

## Main Questions

- Can `auto corpus` and `auto gen` be trusted as control primitives if a failed corpus run can leave `genesis/` empty?
- Can `auto parallel` safely select work when dependencies are written in common human forms, or when missing dependency IDs are present?
- Can quota-backed model execution run parallel lanes without one lane changing another lane's active credentials?
- Can completion and release receipts prove the current tree, current plan, and current artifacts, rather than merely proving that a similar command passed sometime earlier?
- Can review/design/health/qa/super/audit outputs be treated as honest product surfaces with consistent report-only, dry-run, and final status contracts?
- Is the active root queue ready for `auto parallel`, or must stale `TASK-016`, `AD-014`, and `WORKLIST.md` truth be reconciled first?

## Priority Impact

The focus moved priority away from new command features and toward safety-critical control-plane work:

1. Quota credential safety outranks command polish because parallel production execution can corrupt global auth state.
2. Corpus atomicity and non-empty validation outrank generated-plan expansion because an empty `genesis/` breaks the stated control primitive.
3. Dependency truth, salvage durability, and completion evidence outrank speed because an autonomous scheduler that runs the wrong row is worse than a slow one.
4. Receipt freshness and release proof outrank docs cleanup, but docs cleanup remains necessary because stale specs feed future model prompts.
5. First-run DX remains high priority because this is a developer-facing CLI; the fastest successful path must produce a meaningful, honest success moment.
