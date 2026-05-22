# Release Readiness Checkpoint

## Priority Decision

P1. This is a checkpoint gate, not an implementation slice. Score: high operator value after previous slices, high design clarity, medium engineering leverage, evidence-dependent, and not parallel-executable until dependencies land. It outranks release packaging only if the prior slices are green or explicitly deferred; before then, release work is premature.

## User / Operator Outcome

An operator gets a concrete go/no-go decision on whether the repo is ready for root promotion, autonomous execution, or release preparation. The checkpoint prevents a polished report from substituting for passing validation and durable proof.

## Evidence

- `AGENTS.md` defines validation commands.
- `.github/workflows/ci.yml` defines CI expectations.
- Root `IMPLEMENTATION_PLAN.md` is fully checked, but `WORKLIST.md` still lists required validation-proof and receipt hardening.
- Current corpus plans 002 through 006 each name concrete verification commands that should produce narrow proof.
- Release and ship gates depend on receipt freshness and completion evidence, which are currently part of the failing test cluster.

## Scope Boundary

Do not ship or promote generated corpus output during this checkpoint unless validation and evidence gates pass. Do not add new implementation work inside the checkpoint. Do not create broad release notes or changelogs unless the operator explicitly proceeds to release preparation.

## Implementation Slice

Goal: decide whether to proceed to promotion/release, continue stabilization, or split a remaining blocker.

Dependencies:

- Plan 002 must restore formatting and command-surface truth.
- Plan 003 must restore generated-plan/spec-task proof contracts or explicitly classify remaining validator failures.
- Plan 004 must prove `auto super` promotion behavior.
- Plan 005 must settle receipt/lane/ship evidence semantics.
- Plan 006 should reconcile operator status truth.
- Plan 007 may be complete or explicitly deferred if the operator accepts the residual quota risk.

Files to create or modify: none by default.

Tests to add or modify: none by default.

Decision artifact: update the active root control surface only after the operator chooses the next action. Candidate actions are promote generated planning output, run `auto parallel`, prepare release, or add a new focused blocker plan.

Approach: inspect the evidence produced by plans 002 through 006, record a go/no-go decision, and avoid writing new implementation scope inside the checkpoint. If evidence is missing, create one focused blocker plan instead of expanding release process artifacts.

Test expectation: none -- this checkpoint only inspects evidence and records a go/no-go decision.

## Verification

From the repository root:

    cargo fmt --check
    cargo test
    cargo clippy --all-targets --all-features -- -D warnings
    cargo install --path . --root ~/.local
    auto --version
    auto doctor

Expected observation: validation passes, installed binary runs, doctor output is truthful about baseline and execution readiness, and no unresolved P0 remains in the active root control surface.

## Deferred

- Release notes.
- Version bump.
- Tagging.
- Publishing or distribution work.
- New product feature planning beyond the next accepted implementation slice.
