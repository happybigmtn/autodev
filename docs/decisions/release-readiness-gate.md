# Decision: release readiness gate policy

Status: accepted
Date: 2026-04-23
Task: `AD-017`

## Context

- `auto ship` is the release command operators already run. Its prompt maintains `SHIP.md`, records validation, rollback, monitoring, blockers, PR state, and post-push evidence, but the current Rust command does not mechanically reject missing release evidence before invoking Codex.
- `scripts/run-task-verification.sh` and `scripts/verification_receipt.py` already provide receipt-backed proof for exact executable verification commands, and `src/completion_artifacts.rs` treats missing, failed, incomplete, or zero-test receipts as incomplete task evidence.
- `auto qa` and `auto qa-only` maintain `QA.md`; `auto health` maintains `HEALTH.md`. Those reports are useful release inputs only when they are fresh for the branch being shipped and backed by concrete commands or runtime observations.
- `.github/workflows/ci.yml` currently runs formatting, clippy, and tests. It does not install `auto` or prove the installed binary that an operator would run.

## Decision

`auto ship` remains the command that enforces release readiness. The next implementation should add a deterministic preflight inside `auto ship` before the model ship-prep pass. That preflight may still let Codex prepare `SHIP.md` after the gate, but the command must fail early when required local release evidence is missing, stale, or red.

No separate `auto release-check` command is introduced for the first implementation. CI can later call the same `auto ship` preflight or a shared internal helper, but operator release readiness should have one primary entry point.

## Required Receipts

Release readiness requires passing verification receipts for every executable command listed in the branch's release checklist. At minimum, the checklist must include:

- Formatting: `cargo fmt --check` when this is a Rust repo.
- Static analysis: `cargo clippy --all-targets --all-features -- -D warnings` when clippy is available.
- Tests: `cargo test` or the repo's stronger declared test command.
- Smoke or runtime checks named by `AGENTS.md`, `README.md`, `QA.md`, `HEALTH.md`, or the current release diff.
- Installed-binary proof: `cargo install --path . --root ~/.local`, followed by a PATH-resolved `auto --version` smoke that proves the installed binary can start and reports build provenance.

Executable proof must be recorded through `scripts/run-task-verification.sh` when the wrapper exists. A command that exits zero but records a zero-test receipt is not release evidence.

## QA.md and HEALTH.md Freshness

`QA.md` and `HEALTH.md` are stale when any of the following are true:

- The file is missing.
- The report does not name the current branch or the base branch used for the release diff.
- The report predates the newest source, test, workflow, build, or release-facing documentation change in the branch diff.
- The report does not record concrete commands, flows, screenshots, logs, or runtime observations for the surfaces affected by the diff.
- The report says coverage was partial and the untested area overlaps the release diff.

When either report is stale, `auto ship` must refresh the relevant evidence or fail with a blocker before claiming readiness. A fresh `QA.md` or `HEALTH.md` can reference receipt paths instead of duplicating full command output.

## Release Blockers

`auto ship` must block release when any of these are present:

- A required verification receipt is missing, failed, incomplete, corrupted, or reports a zero-test run.
- Installed-binary proof is missing or `auto --version` does not identify the expected build provenance.
- `QA.md` or `HEALTH.md` is missing or stale by the freshness rules above.
- `REVIEW.md`, `QA.md`, `HEALTH.md`, `WORKLIST.md`, or `SHIP.md` records an unresolved critical or required release blocker.
- The working tree contains uncommitted implementation changes that are not part of the release-prep increment.
- The branch cannot be compared to its base branch, cannot sync safely when sync is requested, or has unresolved merge/rebase conflicts.
- Rollback or monitoring notes are empty for a release that changes runtime behavior, data, infrastructure, credentials, CI, or deployment posture.
- PR state is unknown: `SHIP.md` must record either a PR URL or an explicit no-PR reason, such as shipping directly on the base branch.

Blockers must be written plainly to `SHIP.md` with owner, scope, next action, and whether the branch is safe to merge. Blocking evidence belongs in `SHIP.md`; follow-up work that should survive the release pass belongs in `WORKLIST.md`.

## SHIP.md Attachment

`SHIP.md` is the release gate report. The installed-binary section must include:

- The exact install command and receipt path.
- The PATH-resolved binary location used for the smoke.
- The exact `auto --version` command, receipt path, and observed version/provenance line.
- Whether the installed binary was clean, dirty, or built from a different commit than the release branch.

`SHIP.md` must also include branch, base branch, validation commands and outcomes, `QA.md` and `HEALTH.md` freshness status, review state, blockers, rollback path, monitoring path, PR URL or no-PR reason, and final verdict.

## Implementation Prerequisites

The follow-on ship-gate implementation should reuse the existing receipt reader instead of parsing ad hoc logs. It should keep the current prompt-based ship workflow for release-note generation, but only after deterministic local evidence passes or a blocker is recorded.

CI can add installed-binary proof later by running `cargo install --path . --root ~/.local`, putting `~/.local/bin` on `PATH`, and running `auto --version`; this decision does not change `.github/workflows/ci.yml`.

## AD-018 Checkpoint

The quality and security checkpoint found the release lifecycle ready for follow-on CI installed-binary proof and mechanical ship-gate implementation.

- Audit staging proof: `cargo test audit_command::tests::commit_audit_outputs_uses_scoped_pathspecs` passed through `scripts/run-task-verification.sh AD-018`, proving audit commits use scoped pathspecs instead of broad staging for the covered path.
- QA-only dirty-state proof: `cargo test qa_only_command::tests::qa_only_rejects_non_report_file_changes` passed through `scripts/run-task-verification.sh AD-018`, proving report-only QA rejects non-`QA.md` file changes for the covered path.
- Release gate status: this accepted decision remains the current release readiness policy. Installed-binary proof and pre-model `auto ship` enforcement are intentionally follow-on implementation work, not part of this checkpoint.
- Blockers: none found in the local AD-018 proof set. The parallel host still owns the canonical `REVIEW.md` queue handoff for this checkpoint.
