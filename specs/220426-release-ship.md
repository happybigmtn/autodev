# Specification: `auto ship` — branch-level release prep and PR refresh

## Objective

Keep `auto ship` the single-entry release command: it takes the branch from "implementation done, QA and health passed" to "ready to merge." The command resolves the repo's base branch, runs a Codex-driven preparation pass that reconciles docs, version, and changelog, writes a `SHIP.md` report with rollback and monitoring paths, rebases onto origin, pushes, and creates or refreshes a PR via `gh` when the branch is not the base branch.

## Evidence Status

### Verified facts (code)

- `src/main.rs:89` declares `Ship`.
- `src/main.rs:1031-1090` `ShipArgs` (approximate range): default model `gpt-5.4`, default reasoning effort `high`, optional `--base-branch`, optional `--branch`, Codex binary `codex`.
- Default target is the repo's resolved base branch (`--base-branch` > `origin/HEAD` > `main` / `master` / `trunk`), per `corpus/ASSESSMENT.md` §"ship_command.rs" and `README.md:52-53`.
- Ship uses `util::sync_branch_with_remote` before work and `util::push_branch_with_remote_sync` before push (`corpus/SPEC.md` item 5).
- `auto ship` is the only mutating quality command documented as producing the `SHIP.md` report with rollback and monitoring sections (`corpus/SPEC.md` §"Artifact shapes"; `ship_command.rs` prompt includes the text "rollback path" per corpus test-gaps table).
- `auto ship` PR creation/refresh depends on `gh` on `PATH`; without `gh`, push succeeds but no PR is created (`corpus/SPEC.md` §"Runtime dependencies").
- `ship_command.rs` has exactly one test today (corpus ASSESSMENT §"Test gaps").

### Verified facts (docs)

- `README.md:52-53`: "auto ship runs on the currently checked-out branch by default with `gpt-5.4` and `high`, targeting the repo's resolved base branch."
- `README.md:56-58`: mutating commands including `ship` rebase onto `origin/<branch>` both before work and before push.

### Recommendations (corpus)

- `corpus/plans/011-integration-smoke-tests.md` calls for end-to-end smoke tests for `ship` (currently untested outside the one prompt-text test).
- `corpus/DESIGN.md` §"Decisions to recommend" flags a `--json` output variant of `SHIP.md` for CI integrations.

### Hypotheses / unresolved questions

- Whether `auto ship` re-runs QA/health as a gate before shipping is not source-verified; the README describes lifecycle order but does not assert an internal self-check inside `ship`.
- Whether `ship` can open a PR against a non-base branch (for stacked PRs) is not source-verified.

## Acceptance Criteria

- `auto ship` resolves the base branch in the order `--base-branch` flag → `origin/HEAD` symbolic-ref → first present of `main` / `master` / `trunk`; if none resolve, the command exits non-zero with a clear error.
- `auto ship` uses `gpt-5.4` with `high` reasoning effort by default; both are overridable by CLI flags.
- `auto ship` calls `util::sync_branch_with_remote` before the Codex ship-prep pass and `util::push_branch_with_remote_sync` before the push.
- `auto ship` writes a single `SHIP.md` file containing at minimum: rollback path, monitoring path, and a rollout-posture section; the file is overwritten on repeat runs.
- `auto ship` appends new items to `WORKLIST.md` and `LEARNINGS.md` only when the ship-prep pass identifies them.
- When running on a branch equal to the resolved base branch, `auto ship` does not open a PR; it may still push and produce `SHIP.md`.
- When running on a non-base branch and `gh` is present, `auto ship` creates a PR if none exists for the branch, or refreshes the existing PR (title, body, labels per the ship prompt contract).
- Missing `gh` binary causes `auto ship` to log an advisory (PR creation skipped) without failing the command.
- Missing `codex` binary causes `auto ship` to exit non-zero.
- `SHIP.md` is written through `util::atomic_write`.
- Prompt log for each ship run lands under `.auto/logs/ship-<timestamp>-prompt.md`.

## Verification

- Fixture test: branch equals base; assert `auto ship` does not call `gh pr create`.
- Fixture test: branch differs from base, `gh` stub installed; assert PR creation command is shaped correctly.
- Snapshot-test `SHIP.md` structure against the known sections.
- Covered-behavior smoke: `auto ship --dry-run` (if supported) / `--base-branch` override on a hermetic repo.
- `cargo test -p autodev ship_command` passes the existing prompt-text test; add fixtures per `corpus/plans/011-integration-smoke-tests.md`.

## Open Questions

- Should `auto ship` re-run `auto qa` / `auto health` internally as a precondition, or trust the operator to have run them? Product decision — explicit either way.
- Should `ship` refuse to push / PR if `QA.md` / `HEALTH.md` are older than the last code change on the branch? Staleness gate.
- When Codex produces no ship-relevant updates, should `SHIP.md` still be overwritten with a minimal "nothing to prep" report, or should the prior `SHIP.md` be preserved? Not source-verified.
- Is there a need for a `--dry-run` that prints the PR body and `SHIP.md` contents but does not push or call `gh`? Not declared.
