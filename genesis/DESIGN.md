# Design Review

## Surface Definition

This repository has meaningful user-facing surfaces, but they are not graphical. The design surface is the operator experience across terminal output, CLI help, markdown ledgers, generated reports, receipts, logs, and recovery instructions.

The core design question is: can an operator glance at `auto` output and know what happened, what changed, what was proven, and what should happen next?

## Information Architecture

- Runtime implementation: `src/`.
- Active execution truth: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `WORKLIST.md`.
- Planning input: `genesis/`, root `specs/`, and generated `gen-*` snapshots.
- Evidence: `.auto/symphony/verification-receipts/`, QA/health/design/audit reports, lane logs, run roots, and release notes.
- Operator commands: `auto doctor`, `auto corpus`, `auto gen`, `auto super`, `auto parallel`, `auto quota`, `auto design`, `auto qa-only`, `auto health`, `auto audit --everything`, and `auto ship`.

This architecture is powerful but visually easy to misread because multiple files can look authoritative. Generated corpora must keep saying they are subordinate until promoted. Status commands should say which surface is active and why.

## User Journeys

### New Operator

1. Clone repository.
2. Install with Cargo.
3. Run `auto --version`.
4. Run `auto doctor`.
5. Run a non-mutating command that prints current queue/corpus health.
6. Learn what tools, credentials, and ledgers are missing.

Current gap: README has the information, but the fast path is buried. `auto doctor` should become the first meaningful success moment.

### Production Scheduler Operator

1. Check root queue and current branch.
2. Confirm corpus/generation inputs are non-empty and current.
3. Run design and functional gates.
4. Launch `auto parallel`.
5. Monitor lanes, stale workers, completion evidence, and landing.
6. Ship only after current receipts, QA, health, rollback, and PR state pass.

Current gap: status is strong but not backed by one manifest, and checked rows plus empty `REVIEW.md` can conflict.

### Reviewer / Release Owner

1. Read a top-level report.
2. See a verdict.
3. See exact receipts and artifacts.
4. See unresolved blockers.
5. Decide GO/NO-GO.

Current gap: verdict parsing accepts any matching line. Reports need one terminal verdict and host-side rejection of mixed verdicts.

## State Coverage

The UI must distinguish:

- no corpus exists;
- corpus exists but has zero numbered plans;
- corpus exists but is subordinate to root ledgers;
- root queue has no open work;
- root queue has pending work but dependencies block dispatch;
- lanes are live;
- lanes are stale but recoverable;
- current plan refresh failed;
- completion evidence is missing;
- completion evidence is intentionally external or operator-reviewed;
- release gate passed before sync versus after current sync;
- report-only mode wrote only allowed artifacts;
- mutating mode changed runtime or planning files.

## Accessibility And Readability

Terminal output should use plain labels that survive copy/paste and logs. Color can help but cannot be the only state signal. Markdown reports should put verdict, blockers, evidence, and next step near the top. Error messages should name the exact file, row, command, or receipt that blocked progress.

## Responsive Behavior

The primary interface is terminal width rather than browser viewport. Status output should remain scannable at narrow terminal widths by using short labels, one-line state summaries, and report paths rather than wide tables where possible. Markdown should remain readable in GitHub, terminal pagers, and plain text editors.

## AI-Slop Risk

This repo is especially vulnerable to AI-slop because model-generated prose can look like evidence. Design must make host-created proof visibly different from narrative claims:

- host receipt;
- model report;
- operator waiver;
- external blocker;
- stale snapshot;
- archived history.

Each should have different labels and acceptance rules.

## Design Recommendations

1. Add a single `auto parallel` manifest-backed status view that says whether launch/resume/land is safe.
2. Give `auto gen` and `auto corpus` a clear empty-corpus warning and recovery path.
3. Make completion evidence classes visible in `REVIEW.md` and status output.
4. Standardize final status blocks across report-only and release commands.
5. Use one verdict parser and print the accepted verdict source line.
6. Add a compact first-run guide that ends with a meaningful non-mutating success.

## Not Doing

- No web dashboard in this campaign.
- No decorative output polish ahead of evidence clarity.
- No new report format unless it reduces ambiguity in current markdown/terminal surfaces.
