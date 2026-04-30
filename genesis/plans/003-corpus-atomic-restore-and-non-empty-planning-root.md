# Corpus Atomic Restore and Non-Empty Planning Root

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

`auto corpus` and `auto gen` are supposed to be control primitives. They cannot be trusted if a failed corpus run deletes the old planning root and leaves an empty `genesis/` that later commands accept. Operators gain rollback-safe corpus refreshes and clear failure behavior. They can see it working by forcing a corpus failure and observing that the previous corpus remains active.

## Requirements Trace

- R1: Corpus generation must write to a staging directory before replacing `genesis/`.
- R2: Existing corpus content must be preserved or restored when generation fails.
- R3: Empty planning roots must be rejected by corpus and generation loading paths.
- R4: `.auto/state.json` must not point to an invalid or empty planning root.
- R5: Tests must cover interrupted or failed model output, empty-root validation, and symlink-safe archive/copy behavior.

## Scope Boundaries

This plan does not change the content requirements for the generated corpus except to require non-empty mandatory files and plan files. It does not change the active root planning surface or promote `genesis/` over root control docs.

## Progress

- [x] 2026-04-30: Verified current working tree had deleted tracked `genesis/` files before this refresh.
- [ ] 2026-04-30: Add atomic staging and restore behavior.
- [ ] 2026-04-30: Reject empty `genesis/` roots in all corpus-loading paths.

## Surprises & Discoveries

- The current repository state showed tracked `genesis/` churn with deleted old plan files before this refresh; that makes atomic replacement and recovery behavior especially important.
- The previous snapshot exists under `.auto/fresh-input/`, but that archive must remain historical context rather than active truth.
- Archive/copy helpers are part of the recovery story, so they must reject or preserve symlinks without following them outside the intended tree.

## Decision Log

- Mechanical: Empty corpus acceptance is a production blocker because it invalidates `auto corpus` and `auto gen`.
- Taste: Prefer atomic directory replacement over trying to repair partial files one by one.
- User Challenge: If the operator wants `auto gen` as a control primitive, it must refuse partial planning inputs even when that blocks fast iteration.

## Outcomes & Retrospective

None yet. Record final staging directory naming and restore behavior after implementation.

## Context and Orientation

Relevant files:

- `src/corpus.rs`: corpus command behavior.
- `src/generation.rs`: planning corpus loading, verification, generation, snapshot-only behavior, and root sync.
- `src/state.rs`: persisted autodev state path.
- `genesis/`: source-controlled planning corpus.
- `.auto/fresh-input/`: archive area for previous corpus snapshots.

Non-obvious terms:

- Planning root: the directory that contains `ASSESSMENT.md`, `SPEC.md`, `PLANS.md`, `GENESIS-REPORT.md`, optional `DESIGN.md`, and numbered plans.
- Staging directory: a temporary directory used to verify generated files before replacing the active root.

## Plan of Work

Change corpus generation so it writes to a new staging directory, verifies all mandatory files and at least one numbered plan, then atomically replaces `genesis/`. If verification fails, leave the previous corpus intact and emit the staging path for debugging. Update planning corpus loaders so an existing but empty root is an error. Harden copy/archive helpers so a symlink in a planning root cannot pull data from outside the intended tree. Update state only after successful validation.

## Implementation Units

- Unit 1: Non-empty loader guard. Goal: reject empty planning roots. Requirements advanced: R3, R4. Dependencies: none. Files: `src/generation.rs`, `src/corpus.rs`. Tests: empty `genesis/` returns a clear error. Approach: require mandatory files and at least one plan before load succeeds. Scenarios: missing `PLANS.md`; empty `plans/`; root absent.
- Unit 2: Atomic corpus staging. Goal: preserve previous corpus on failure. Requirements advanced: R1, R2. Dependencies: Unit 1. Files: `src/corpus.rs`, possibly shared helpers in `src/generation.rs`. Tests: simulated failed model output leaves old file content untouched. Approach: write to `.auto/fresh-input` or a temp staging path, verify, then replace. Scenarios: failure before write; failure after partial write; success replaces root.
- Unit 3: Symlink-safe archive/copy. Goal: preserve recovery boundaries. Requirements advanced: R5. Dependencies: Unit 1. Files: `src/util.rs`, archive callers in `src/generation.rs`, `src/corpus.rs`, and `src/nemesis.rs`. Tests: symlink inside source tree is rejected or copied inertly without following target content. Approach: use symlink metadata and canonical boundary checks. Scenarios: symlink to external file; symlink to external directory.
- Unit 4: State update ordering. Goal: avoid `.auto/state.json` pointing at invalid roots. Requirements advanced: R4. Dependencies: Unit 2. Files: `src/state.rs`, corpus/generation callers. Tests: failed corpus leaves state unchanged. Approach: persist state after validation only. Scenarios: state path preserved after failure.

## Concrete Steps

From the repository root:

    rg -n "load_planning_corpus|verify_corpus_outputs|genesis|fresh-input|copy_tree|symlink|state" src/corpus.rs src/generation.rs src/state.rs src/util.rs src/nemesis.rs
    cargo test generation::tests
    cargo test corpus::tests
    auto gen --help

Expected observations after implementation: empty or partial corpus roots fail loudly, and a failed refresh does not delete the prior corpus.

## Validation and Acceptance

Acceptance:

- `auto corpus` failure leaves the previous `genesis/` content intact.
- `auto gen` refuses an existing empty `genesis/`.
- Mandatory files are required before state is updated.
- Archive/copy helpers do not follow symlinks outside the intended source tree.
- Error messages name the invalid planning root and the recovery artifact path.

## Idempotence and Recovery

Rerunning corpus generation is safe because staging paths are disposable until verified. If a staging directory remains after failure, inspect it for debugging, then remove it after preserving any useful logs. Do not restore old snapshots as truth without rerunning validation.

## Artifacts and Notes

Record the staging path and validation output in the active task handoff. Keep archived previous snapshots under `.auto/fresh-input/` as context only.

## Interfaces and Dependencies

Interfaces: corpus command, generation command, planning corpus verifier, state persistence, filesystem directory replacement. External dependencies: model execution only for live corpus runs; tests should simulate output without network.
