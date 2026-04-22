# Specification: `auto steward` — mid-flight plan reconciliation

## Objective

Guarantee that `auto steward` stays an honest, two-pass reconciliation for repos that already have an active planning surface: it reads the live code and the existing `IMPLEMENTATION_PLAN.md` / `WORKLIST.md` / `LEARNINGS.md`, surfaces drift, hinge items, retirement candidates, and hazards in dedicated markdown deliverables, and applies approved updates in-place without re-creating the `corpus → gen` wipe cycle. Scope must not silently broaden into greenfield planning.

## Evidence Status

### Verified facts (code)

- `src/main.rs:74-80` doc-comment declares `steward` as "two-pass Codex (gpt-5.4) pipeline" that "replaces `auto corpus` and `auto gen` for repos that already have an active planning surface; greenfield repos should keep using those."
- `src/main.rs:87` (see `StewardArgs` block near line 800+) default model is `gpt-5.4`, effort `high`; a `--skip-finalizer` flag exists.
- `src/steward_command.rs:21-28` declares the deliverable set: `DRIFT.md`, `HINGES.md`, `RETIRE.md`, `HAZARDS.md`, `STEWARDSHIP-REPORT.md`, `PROMOTIONS.md`.
- Tests in `src/steward_command.rs`: 7 tests covering prompt content, planning-surface detection, and report-only mode (per corpus ASSESSMENT coverage table).
- README does not list `steward` in the inventory at `README.md:11-25` or the detailed command guide.

### Recommendations (corpus)

- `corpus/SPEC.md` §"Near-term direction" item 7 calls out that the README should tell operators when to choose `steward` vs `corpus + gen`.
- `corpus/plans/012-command-lifecycle-reconciliation-research.md` is a research-only plan on whether `steward` should supersede `corpus + gen` entirely for mid-flight repos.
- `corpus/DESIGN.md` §"Journey 2 — Mid-flight reconciliation" notes that the six deliverables are undocumented in the README and that operators discover them only by running the command.

### Hypotheses / unresolved questions

- Whether the finalizer pass rewrites `IMPLEMENTATION_PLAN.md` / `WORKLIST.md` / `LEARNINGS.md` atomically (one pass) or incrementally per-file is not source-verified in this spec pass; `corpus/SPEC.md` says "applies approved edits in-place" without naming the write path.
- Whether `steward` itself calls `auto_checkpoint_if_needed` before the finalizer pass is asserted by the broader code pattern in `corpus/SPEC.md` §"Checkpoint safety" but not verified against `steward_command.rs` in this pass.

## Acceptance Criteria

- `auto steward` invoked inside a repo without `IMPLEMENTATION_PLAN.md`, `PLANS.md`, or `plans/*.md` refuses to run or flags the repo as "no active planning surface" — operator is instructed to use `auto corpus + auto gen` instead.
- `auto steward` writes all six deliverables when the pipeline completes: `DRIFT.md`, `HINGES.md`, `RETIRE.md`, `HAZARDS.md`, `STEWARDSHIP-REPORT.md`, `PROMOTIONS.md`.
- `auto steward --skip-finalizer` writes the six deliverables but does not modify `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, or `LEARNINGS.md`.
- Without `--skip-finalizer`, the command applies approved edits directly to `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, and/or `LEARNINGS.md` as called for by the first-pass deliverables.
- The first-pass Codex invocation uses `gpt-5.4` with `high` effort by default; CLI flags may override.
- The second-pass Codex invocation also uses `gpt-5.4` with `high` effort by default.
- Before the finalizer writes, the command creates a git checkpoint when the worktree has tracked dirty state outside `.auto/`, `bug/`, `nemesis/`, and `gen-*`.
- All deliverable writes go through `util::atomic_write` (no partial-write artifacts visible on failure).
- The command exits non-zero if `codex` is not on `PATH`, naming the missing dependency.
- Prompt log is written to `.auto/logs/steward-<timestamp>-prompt.md`.
- README's `auto steward` section must explain when to prefer `steward` over `corpus + gen`; absence of that section blocks a successful truth-pass per `corpus/plans/002-readme-command-inventory-sync.md`.

## Verification

- `cargo test -p autodev steward_command` passes (7 existing tests).
- Add a fixture test exercising: (a) a repo with no planning surface — expect refusal; (b) a repo with `IMPLEMENTATION_PLAN.md` — expect all six deliverables; (c) `--skip-finalizer` — expect deliverables but no in-place edits.
- Smoke-test in a mid-flight repo: run `auto steward`, diff `IMPLEMENTATION_PLAN.md` before/after, confirm finalizer edits match the `PROMOTIONS.md` narrative.
- Grep `src/steward_command.rs` for each of the six deliverable filenames to confirm none are dropped silently.

## Open Questions

- Should `steward` ever retire `corpus + gen` for mid-flight repos, or do they coexist long-term? Pending `corpus/plans/012-*` research.
- Does the finalizer write file-by-file or in one transaction? Matters for rollback behavior when Codex errors mid-pass.
- What is the contract between `PROMOTIONS.md` and `IMPLEMENTATION_PLAN.md` — is `PROMOTIONS.md` the audit trail of every finalizer edit, or an advisory list that the finalizer may ignore?
- Should `HAZARDS.md` items automatically append to `WORKLIST.md`, or does the operator do that manually after reading?
- Is `steward` meant to be idempotent when the repo state is unchanged between runs, or should it always re-run? Not declared.
