# Genesis Report

## Refresh Summary

This corpus refresh reviewed the actual codebase first, then reconciled that code against README, root planning files, dated specs, CI, receipts, git history, previous genesis snapshots, and focused sub-reviews. The previous `genesis/` files were deleted in the working tree before this pass; this refresh restores `genesis/` with a current corpus instead of copying an archived snapshot forward.

The repository is a real Rust CLI product. It is not a speculative plan repository. The `auto` binary owns autonomous planning, model execution, scheduler orchestration, receipts, audit, quality reports, and release gating. The strongest next direction is to make those control primitives safe enough for production parallel execution.

## Major Findings

- Highest security risk: quota account names can escape the intended profile tree, and credential swaps are not held for the full child-process lifetime.
- Highest control-plane risk: `auto corpus` can archive/delete the old corpus and leave `genesis/` empty after failure, while later generation can accept the empty root.
- Highest scheduler risk: dependency truth is lossy for bare task IDs and missing dependency IDs; `auto loop` can select dependency-blocked work.
- Highest evidence risk: verification receipts are not bound to current commit, dirty state, plan hash, or artifact hash, and release gates can accept stale proof.
- Highest reconciliation risk: Symphony/review/root-plan paths can write before validation or mishandle partial rows.
- Highest release-ledger drift: `v0.2.0` exists, but active `TASK-016`, `COMPLETED.md`, archive prose, and tag annotation content do not fully agree.
- Highest documentation risk: older specs and previous snapshots claim obsolete command counts and missing command behavior, while README is closer to code reality.
- Highest DX risk: `auto doctor` is useful but narrow, and dry-run/report-only semantics vary across commands.

## Recommended Direction

Keep `auto corpus` and `auto gen` as the control primitives, but do not scale execution until the control plane is safer. The first production tranche should harden quota credentials, corpus atomicity, dependency truth, and receipt freshness. The second tranche should reconcile Symphony/review/super/loop contracts. The third tranche should normalize audit/nemesis/report-only/DX behavior and then run a release decision gate before queue promotion.

## Top Next Priorities

1. Implement path-safe quota account names and credential leases that cover the model child process lifetime.
2. Make corpus generation atomic and reject empty planning roots everywhere.
3. Parse dependency truth consistently and block missing dependency IDs.
4. Bind receipts to current tree state and release artifacts.
5. Fix Symphony/review reconciliation ordering and partial-row safety.
6. Reconcile stale root plan rows, especially `TASK-016`, `AD-014`, the empty `COMPLETED.md`, tag annotation drift, and the active `WORKLIST.md` items.

## Focus Impact

The operator focus moved the corpus from broad product ideation to a production-readiness race. It elevated release blockers, operator trust, verification evidence, first-run DX, scheduler safety, and execution contracts above new features. It did not hide non-focused risks: quota credential safety and corpus rollback safety outrank several named command-surface polish items because they can corrupt auth state or erase the planning root.

## Not Doing

- Not replacing the Rust CLI with a new architecture.
- Not making a web UI or dashboard before terminal/operator contracts are trustworthy.
- Not running `auto parallel` until root queue and safety blockers are reconciled.
- Not treating archived genesis snapshots as current truth.
- Not changing active root planning primacy from `IMPLEMENTATION_PLAN.md` and related root files without operator promotion.
- Not capacity-trimming ambition; instead, splitting it into parallelizable, evidence-backed slices.

## Decision Audit Trail

- Mechanical: `DESIGN.md` is included because the CLI has meaningful user-facing terminal/operator surfaces.
- Mechanical: `genesis/` remains subordinate because no root `PLANS.md` or root `plans/` directory exists, and repo control truth lives in root planning files.
- Mechanical: plans use current command count 21 because `src/main.rs` and README agree; older specs are stale.
- Mechanical: quota and corpus safety are first because code review found concrete high-severity risks.
- Mechanical: generated plan validation commands must be runnable; independent review split multi-filter `cargo test` examples and removed nonexistent `auto gen --dry-run` / `auto ship --dry-run` examples.
- Taste: the corpus is organized into 12 plans with two checkpoint gates, not a larger backlog, to keep parallel slices clear while preserving production ambition.
- Taste: release proof and DX are later than credential/corpus/scheduler safety, even though they are operator-visible, because they depend on trustworthy evidence plumbing.
- User Challenge: the focus asks to implement an approved queue with `auto parallel`, but this pass only authors the corpus because no newly approved queue exists and current safety findings argue against launching lanes immediately.
- User Challenge: `auto gen` remains the desired control primitive, but mutating generation by default should be reconsidered for production use.
