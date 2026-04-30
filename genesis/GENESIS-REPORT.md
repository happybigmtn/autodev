# Genesis Report

## Refresh Summary

This refresh reviewed the live codebase, root ledgers, CI, README, specs, git history, the archived previous genesis snapshot, and six read-only sub-reviews. It regenerates the live `genesis/` corpus with current planning material. The archived snapshot was useful for sequencing, but it was treated as history, not truth.

The repository is a real Rust CLI product. Its production-readiness problem is not lack of ambition; it is trust alignment across state, scheduler, model execution, receipts, and release gates.

## Major Findings

- The active root queue is currently cleared; `REVIEW.md` is intentionally empty, and there is no executable unchecked work for `auto parallel`.
- The authoring pass reported a degraded pre-refresh `genesis/` tree, while `.auto/state.json` and operator focus still point at corpus/generation as control primitives; the current generated corpus is complete and must remain subordinate until promoted.
- Highest security risk: quota account names are raw path components, and saved planning roots can influence destructive generation/corpus operations.
- Highest execution risk: quota failover can retry after worker progress, duplicating side effects.
- Highest scheduler risk: checked rows plus empty `REVIEW.md` can conflict with completion evidence rules and trigger demotion or operator confusion.
- Highest release risk: `auto ship` gates before branch sync and does not rerun the mechanical gate after model work.
- Highest evidence risk: receipt and declared-artifact containment/freshness logic is spread across modules and still relies on markdown conventions in several flows.
- Highest design risk: the interface has many authoritative-looking markdown surfaces but no single manifest-backed answer to "is it safe to launch?"
- Highest DX risk: the first-run path exists but is too cognitively heavy for a new production operator.

## Recommended Direction

Keep `auto corpus`, `auto gen`, `auto super`, and `auto parallel` as the control primitives. Do not scale execution until the control plane fails closed on unsafe state. The recommended campaign is:

1. Corpus/state and quota safety.
2. Scheduler completion truth and lane resume contracts.
3. Receipt/release evidence binding.
4. Report-only, audit, nemesis, and DX normalization.
5. Final release decision gate and root queue promotion.

## Top Next Priorities

1. Validate quota account names, contain profile paths, and stop retry-after-progress failover.
2. Constrain saved planning roots, make corpus regeneration atomic, and reject empty numbered plan sets.
3. Reconcile checked-row completion conventions with `REVIEW.md`, receipts, archives, and accepted external evidence classes.
4. Make `auto parallel` fail closed on plan refresh failure and bind lane resumes to task-body hashes.
5. Move release gate sync earlier, rerun the gate after model work, and share exact verdict parsing.
6. Add model-free workflow fixtures for critical report-only, scheduler, and release flows.

## Focus Impact

The operator focus moved this corpus away from generic backlog generation and toward production launch readiness. It elevated runtime/design sync, scheduler safety, resumability, verification receipts, first-run DX, and release proof. It also forced a user challenge: the focus asks to continue through execution and parallel launch, but the repo currently has no open root queue and has safety gaps that should be closed before launching lanes.

Higher-priority issues that escaped the focus wording:

- Quota account path traversal.
- Saved planning-root containment.
- Checked-row/empty-review split brain.
- Release-gate order after remote sync.
- Prompt leakage through argv for Kimi/PI paths.

## Not Doing

- Not building a web UI or dashboard.
- Not treating `genesis/` as the active execution surface until promoted.
- Not copying the archived snapshot forward unchanged.
- Not launching `auto parallel` from an empty root queue.
- Not accepting model-written prose as equivalent to host-created receipts.
- Not replacing the current markdown-ledger architecture wholesale in this campaign.
- Not capacity-trimming ambition; the plan splits high-ambition production work into parallelizable gates.

## Decision Audit Trail

- Mechanical: `DESIGN.md` is included because the CLI has meaningful user-facing terminal and markdown surfaces.
- Mechanical: `genesis/` is subordinate because no root `PLANS.md` or root `plans/` directory exists and root ledgers are the active planning truth.
- Mechanical: current `genesis/` has a complete generated corpus, but pre-refresh degradation is authoring-pass evidence; independent review should rely on current shape checks, saved-state inspection, and runtime code paths.
- Mechanical: quota/account and saved-state containment rank first because they can corrupt credentials or delete/trust the wrong planning root.
- Mechanical: checked-row completion truth ranks before new queue generation because scheduler output is only useful when completion semantics are stable.
- Mechanical: release gate freshness ranks before release prose because code order can make proof stale.
- Taste: the corpus uses twelve numbered plans with three checkpoint-style gates rather than a larger backlog, keeping scope ambitious but reviewable.
- Taste: Kimi/PI prompt security is grouped under quota/backend safety instead of a separate plan because it shares backend execution ownership.
- Taste: DX work comes after state/evidence hardening even though it is highly visible, because a polished first-run path should teach true behavior.
- User Challenge: the operator asks to proceed through execution and parallel execution; this corpus recommends root queue promotion only after the generated gates are accepted and safety blockers are addressed.
- User Challenge: keeping `auto gen` as a control primitive remains right, but production use should prefer snapshot/review gates before mutating root ledgers.
