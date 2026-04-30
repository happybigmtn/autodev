# Receipt Freshness and Release Evidence Binding

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

Receipts are the proof layer for autonomous work. Operators gain release evidence that proves the current tree, plan, and artifacts, not merely that a command with the same text once passed. They can see it working when `auto ship` and completion evidence reject stale receipts after HEAD, dirty state, plan text, or declared artifacts change.

## Requirements Trace

- R1: Receipt writer records commit, branch, dirty state, and relevant plan identity.
- R2: Receipt writer records artifact paths and hashes when a task declares completion artifacts.
- R3: Completion evidence rejects receipts that do not match current tree or plan expectations.
- R4: `auto ship` requires locked install proof consistent with README and CI.
- R5: CI exercises the shell/Python receipt writer path.

## Scope Boundaries

This plan does not redesign every receipt field or invent a remote attestation service. It extends local receipts enough to prevent stale proof from satisfying completion and release gates.

## Progress

- [x] 2026-04-30: Found stale receipt risk in completion and release gates.
- [ ] 2026-04-30: Add metadata to receipt writer.
- [ ] 2026-04-30: Bind completion and ship checks to current tree evidence.
- [ ] 2026-04-30: Add CI smoke for wrapper scripts.

## Surprises & Discoveries

- Existing receipts already capture useful stdout/stderr tails and zero-test summaries.
- The missing proof is not command output; it is identity of the tree and artifacts being proven.

## Decision Log

- Mechanical: Receipts must be current-tree bound before release gates can be trusted.
- Taste: Start with commit, dirty state, plan hash, and artifact hashes before adding heavier provenance systems.
- User Challenge: A stricter gate may reject old but real receipts; production proof should require rerunning validation after meaningful drift.

## Outcomes & Retrospective

None yet. Record exact new receipt fields and compatibility behavior for old receipts.

## Context and Orientation

Relevant files:

- `scripts/run-task-verification.sh`: wrapper that runs commands and writes receipts.
- `scripts/verification_receipt.py`: receipt JSON writer.
- `src/completion_artifacts.rs`: task completion evidence and receipt matching.
- `src/ship_command.rs`: release gate and receipt requirements.
- `.github/workflows/ci.yml`: CI install and smoke path.
- `.auto/symphony/verification-receipts/`: current receipt storage.

Non-obvious terms:

- Receipt: JSON evidence that a command ran, with status and output summaries.
- Dirty state: whether `git status --short` reports local changes when the command ran.
- Plan hash: a hash of the task block or plan content the receipt claims to satisfy.

## Plan of Work

Extend the Python receipt writer to collect git commit, branch, dirty status, command working directory, and optional artifact hashes. Update the shell wrapper to pass task identity and artifact hints where available. Update Rust completion and release gates to reject receipts that are missing required current-tree fields for new receipts, while giving a clear migration error for old receipts. Require `cargo install --path . --locked --root ...` for release proof and validate observed `auto --version` output.

## Implementation Units

- Unit 1: Receipt metadata. Goal: write current-tree identity. Requirements advanced: R1. Dependencies: Plan 005 pass. Files: `scripts/verification_receipt.py`, `scripts/run-task-verification.sh`. Tests: script unit or integration smoke. Approach: query git directly and include dirty summary. Scenarios: clean tree, dirty tree, outside git repo.
- Unit 2: Artifact and plan binding. Goal: bind proof to declared task. Requirements advanced: R2, R3. Dependencies: Unit 1. Files: `src/completion_artifacts.rs`, `src/task_parser.rs` if needed. Tests: receipt rejected after plan text or artifact hash changes. Approach: compute stable hashes in Rust and compare receipt metadata. Scenarios: stale artifact; missing artifact; changed plan block.
- Unit 3: Release gate strictness. Goal: prevent stale release proof. Requirements advanced: R4. Dependencies: Unit 1. Files: `src/ship_command.rs`, README/spec docs when promoted. Tests: old receipt rejected, non-locked install rejected, version mismatch rejected. Approach: require new fields and locked install command. Scenarios: HEAD mismatch; dirty mismatch; install without `--locked`.
- Unit 4: CI writer smoke. Goal: exercise the actual receipt scripts. Requirements advanced: R5. Dependencies: Unit 1. Files: `.github/workflows/ci.yml`. Tests: CI step runs wrapper around a harmless command. Approach: add low-cost script smoke. Scenarios: receipt JSON exists and parses.

## Concrete Steps

From the repository root:

    rg -n "verification_receipt|run-task-verification|receipt|cargo install|locked|zero_test" scripts src .github/workflows/ci.yml
    cargo test completion_artifacts::tests
    cargo test ship_command::tests
    scripts/run-task-verification.sh TEST-RECEIPT true
    cargo test

Expected observations after implementation: changing HEAD, dirty state, task block, or artifact content causes old receipts to fail matching.

## Validation and Acceptance

Acceptance:

- New receipts include commit, branch, dirty state, and command metadata.
- Completion evidence rejects stale tree or artifact receipts.
- `auto ship` requires locked install proof and observed version output for the current tree.
- CI runs at least one receipt-wrapper smoke test.
- Old receipts fail with actionable migration text rather than silently passing.

## Idempotence and Recovery

Receipts can be regenerated by rerunning the validation wrapper. If old receipts fail after this change, rerun the exact verification commands under the new wrapper. Do not hand-edit receipt JSON.

## Artifacts and Notes

Capture before/after receipt JSON field examples in the promoted handoff, redacting secrets and avoiding full logs.

## Interfaces and Dependencies

Interfaces: shell wrapper, Python receipt writer, Rust completion gate, Rust ship gate, CI workflow, git metadata. Dependencies: current task parser and declared completion artifacts.
