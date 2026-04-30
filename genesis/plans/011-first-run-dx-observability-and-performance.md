# First-Run DX Observability And Performance

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This slice makes `auto` easier to operate under pressure. The new contributor gets a fast, honest first success. The production operator gets manifest-backed status, clear errors, and basic performance proof for large queues and audit workloads.

## Requirements Trace

- R1: First-run path must install, identify prerequisites, and produce a meaningful non-mutating success.
- R2: `auto doctor` must reduce uncertainty about required tools, credentials, repo layout, and active planning truth.
- R3: Help and README must distinguish report-only, mutating, queue-promoting, and release commands.
- R4: `auto parallel status` and related status commands must identify safe/unsafe next action.
- R5: Large queue and audit performance must have benchmark or fixture proof before production claims.
- R6: CI must keep installed binary help smoke and add critical workflow fixtures as they become stable.

## Scope Boundaries

This plan does not add a web dashboard. It does not change core scheduler semantics except status/observability hooks created by earlier plans. It does not invent performance targets without evidence.

## Progress

- [x] 2026-04-30: Verified CI already installs `auto` and smokes major help surfaces.
- [x] 2026-04-30: Verified README is comprehensive but long for T0 onboarding.
- [x] 2026-04-30: Verified `auto doctor` exists and is the natural first success surface.
- [ ] Define first-run success path and update docs/help.
- [ ] Add active planning truth and corpus health to doctor/status.
- [ ] Add basic performance fixtures or benchmarks.

## Surprises & Discoveries

- The installed binary smoke in CI is an unusually valuable guard for this repo.
- The main DX problem is not missing documentation; it is too much undifferentiated documentation before the first useful command.

## Decision Log

- Mechanical: New operators need a non-mutating first success before running model-backed commands.
- Mechanical: Status output must separate safe launch, unsafe launch, recovery, and release readiness.
- Taste: Keep README detailed but add a short quickstart near the top instead of splitting docs immediately.
- User Challenge: Performance targets should be evidence-derived, not asserted from ambition.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Relevant files:

- `README.md`: primary operator documentation.
- `AGENTS.md`: repo instructions and required commands.
- `src/main.rs`: help text and command definitions.
- `src/doctor_command.rs` if present, or the module implementing `auto doctor`.
- `src/parallel_command.rs`: status output.
- `src/audit_everything.rs`: large resumable audit workload.
- `.github/workflows/ci.yml`: CI install and help smoke.

Non-obvious terms:

- T0: the first few minutes after cloning the repo.
- Meaningful success: a command that proves the installed binary can inspect the repo and tell the operator a true next step without mutating files.
- Performance proof: repeatable local or CI evidence for queue/audit scale behavior.

## Plan of Work

Define a short first-run script in README: build/install, `auto --version`, `auto doctor`, and one non-mutating status or verify command. Update `auto doctor` to report required tools, detected credentials without printing secrets, active planning surface, corpus completeness, root queue state, and release evidence freshness where cheap.

Standardize help text labels for report-only, mutating, queue-promoting, scheduler, and release commands. Add status output that names safe next action and the evidence source. Add model-free performance fixtures for parsing large `IMPLEMENTATION_PLAN.md`, rendering `auto parallel status`, and audit manifest/status operations. Keep targets as recommendations until measured.

## Implementation Units

- Unit 1: First-run quickstart.
  - Goal: Give new operators a fast, honest path.
  - Requirements advanced: R1, R3.
  - Dependencies: none.
  - Files to create or modify: `README.md`, possibly `AGENTS.md`.
  - Tests to add or modify: installed help smoke if help labels change.
  - Approach: Add a compact "First 5 minutes" section with expected observations.
  - Test scenarios: A fresh clone can run the quickstart without model credentials until doctor reports missing tools.

- Unit 2: Doctor uncertainty reduction.
  - Goal: Make `auto doctor` the first meaningful success.
  - Requirements advanced: R1, R2.
  - Dependencies: Plans 003-004 for corpus/queue truth helpers if available.
  - Files to create or modify: doctor command module, `src/main.rs`, tests.
  - Tests to add or modify: doctor reports missing `claude`, `codex`, `pi`, `gh`; reports active planning surface; reports empty corpus; redacts secrets.
  - Approach: Reuse existing helpers for repo layout and planning surface detection.
  - Test scenarios: temp repo without `genesis` shows a warning but exits in the intended status class.

- Unit 3: Help and command mode labels.
  - Goal: Reduce accidental mutating command use.
  - Requirements advanced: R3.
  - Dependencies: none.
  - Files to create or modify: `src/main.rs`, README.
  - Tests to add or modify: help text contains mode labels for key commands.
  - Approach: Add concise labels, not long tutorials, to command help.
  - Test scenarios: `auto nemesis --help` clearly distinguishes report-only or planning-sync behavior after Plan 010.

- Unit 4: Manifest-backed observability.
  - Goal: Make status output say what to do next.
  - Requirements advanced: R4.
  - Dependencies: Plan 004 manifest.
  - Files to create or modify: `src/parallel_command.rs`, status tests.
  - Tests to add or modify: safe launch, unsafe launch, stale lane, no run, release gate waiting.
  - Approach: Render from structured state rather than log inference where possible.
  - Test scenarios: `auto parallel status` prints "safe to launch: no" with named blockers.

- Unit 5: Performance proof.
  - Goal: Add evidence before setting production performance claims.
  - Requirements advanced: R5, R6.
  - Dependencies: stable parser/status helpers.
  - Files to create or modify: benchmarks or fixture tests, CI optional.
  - Tests to add or modify: large plan parse/status fixture; audit manifest/status fixture.
  - Approach: Start with deterministic fixture tests that assert runtime stays under a recommended threshold measured locally, then decide whether to add formal benches.
  - Test scenarios: parse a synthetic 1,000-row plan and render status without excessive memory or time.

## Concrete Steps

From the repository root:

    rg -n "Doctor|doctor|help|status|parallel status|IMPLEMENTATION_PLAN|README" src README.md .github/workflows/ci.yml

Expected observation: first-run and status surfaces.

    cargo test doctor
    cargo test parallel_status
    cargo test -- --list

Expected observation before work: missing first-run/status/performance coverage.

After implementation:

    cargo test doctor
    cargo test parallel_status
    cargo test performance
    cargo test
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: first-run and status tests pass; full tests still pass.

## Validation and Acceptance

Acceptance requires README or help to show a short first-run path, doctor to report active planning and corpus health, status to name safe/unsafe next actions, and at least one deterministic large-plan or audit-status performance fixture. Performance targets must be labeled as measured recommendations or open questions.

## Idempotence and Recovery

Doctor and status commands should be read-only. Performance fixtures should use temp files and avoid writing to the real repo. README/help changes can be rerun safely through CI smoke.

## Artifacts and Notes

- Evidence to fill in: first-run quickstart text location.
- Evidence to fill in: doctor output example with missing tools redacted.
- Evidence to fill in: large-plan fixture timing and hardware/context note.

## Interfaces and Dependencies

- Commands: `auto doctor`, `auto --help`, `auto parallel status`, installed binary smoke.
- Files: `README.md`, `AGENTS.md`, `.github/workflows/ci.yml`.
- Modules: doctor command, `main`, `parallel_command`, `audit_everything`, parser/status helpers.
