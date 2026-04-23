# Release Readiness And Command Lifecycle Gate

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This final gate decides whether the repo is ready for the next release-quality handoff and whether the command lifecycle should remain `corpus + gen + loop/parallel` with `steward` as reconciliation, or shift toward `steward` as the primary mid-flight planning path.

Users gain a clear release readiness report and an explicit product decision instead of an accidental lifecycle change.

## Requirements Trace

- R1: All prior phase gates have passed or have documented exceptions.
- R2: Current tests, clippy, smoke tests, and installed-binary proof pass.
- R3: Root planning docs accurately record open work.
- R4: Command lifecycle alternatives are compared with code leverage and operator impact.
- R5: Any lifecycle change is classified as a user challenge.

## Scope Boundaries

This plan does not implement a release, tag a version, or retire commands. It is a checkpoint and research gate. If it recommends lifecycle changes, those become new plans.

## Progress

- [x] 2026-04-23: Gate created as final corpus checkpoint.
- [ ] 2026-04-23: Run after Plans 010 and 011.
- [ ] 2026-04-23: Record release readiness and lifecycle recommendation.

## Surprises & Discoveries

None yet. This section should capture whether first-run and CI work reveals deeper lifecycle confusion.

## Decision Log

- User Challenge: Retiring or demoting `corpus + gen` is product direction and needs operator approval.
- Taste: Compare lifecycle alternatives after safety/DX work, not before.
- Mechanical: A release gate cannot pass while local tests fail without an explicit accepted exception.

## Outcomes & Retrospective

None yet. After gate execution, record go/no-go, open blockers, and any recommended follow-up plans.

## Context and Orientation

Relevant lifecycle commands:

- `auto corpus`: creates `genesis/`.
- `auto gen`: turns corpus into specs and root implementation plan updates.
- `auto reverse`: produces specs/plans without merging root plan.
- `auto steward`: reconciles an already active planning surface.
- `auto loop`: sequential execution.
- `auto parallel`: multi-lane execution.
- `auto symphony`: Linear/Symphony-backed execution.

Relevant root docs:

- `README.md`;
- `AGENTS.md`;
- `IMPLEMENTATION_PLAN.md`;
- `ARCHIVED.md`;
- `WORKLIST.md`;
- `specs/`.

## Plan of Work

Run final validation after prior phases. Then compare three future command lifecycle directions:

1. Keep current split: `corpus + gen` for greenfield or deep replanning; `steward` for mid-flight reconciliation.
2. Promote `steward` as the default planning command for existing repos while preserving `corpus + gen` for greenfield.
3. Collapse planning commands behind one front door with modes.

Evaluate each against code leverage, docs burden, operator clarity, and risk. Recommend one direction and mark any change requiring operator approval.

## Implementation Units

Unit 1 - Release readiness validation:

- Goal: Confirm local and CI-style checks pass.
- Requirements advanced: R1, R2, R3.
- Dependencies: Plans 005, 009, 010, 011.
- Files to create or modify: release readiness note in root planning docs.
- Tests to add or modify: none at gate time.
- Approach: run the same commands documented by Plan 011.
- Specific test scenarios: fmt, clippy, tests, smoke tests, and installed-binary proof pass.

Unit 2 - Lifecycle alternatives research:

- Goal: Compare future planning command directions.
- Requirements advanced: R4, R5.
- Dependencies: Unit 1.
- Files to create or modify: root planning note or dated spec.
- Tests to add or modify: Test expectation: none -- research gate only.
- Approach: inspect current code leverage and docs for `corpus`, `gen`, `reverse`, and `steward`.
- Specific test scenarios: each alternative lists reuse, migration cost, docs impact, and failure mode.

Unit 3 - Follow-up plan creation:

- Goal: Convert the accepted recommendation into concrete work.
- Requirements advanced: R5.
- Dependencies: Unit 2 and operator decision if needed.
- Files to create or modify: `IMPLEMENTATION_PLAN.md`, possibly new dated spec.
- Tests to add or modify: Test expectation: none -- plan creation only.
- Approach: do not implement lifecycle changes during this gate.
- Specific test scenarios: any new work has owner files, dependencies, and targeted validation.

## Concrete Steps

From the repository root:

    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test
    cargo install --path . --root target/autodev-install-smoke
    target/autodev-install-smoke/bin/auto --version
    rg -n "corpus|gen|reverse|steward" README.md src/main.rs src/generation.rs src/steward_command.rs

Expected observation: validation passes before release readiness is declared. Lifecycle research identifies a recommended direction but does not silently retire commands.

## Validation and Acceptance

Acceptance requires:

- all prior gates are closed or have explicit exceptions;
- local validation and installed-binary proof pass;
- root planning docs record remaining open work;
- lifecycle alternatives are compared;
- any operator-facing lifecycle change is marked `User Challenge` and left for approval.

## Idempotence and Recovery

This gate can be rerun before any release. If validation fails, do not proceed to tagging; update root planning docs with exact failures. If lifecycle direction is unresolved, keep current commands and open a research follow-up rather than making implicit changes.

## Artifacts and Notes

Record:

- validation command outputs;
- installed binary version output;
- selected lifecycle recommendation;
- rejected lifecycle alternatives and reasons.

## Interfaces and Dependencies

Interfaces evaluated:

- planning commands `corpus`, `gen`, `reverse`, `steward`;
- execution commands `loop`, `parallel`, `symphony`;
- root docs and README;
- CI and installed binary validation.
