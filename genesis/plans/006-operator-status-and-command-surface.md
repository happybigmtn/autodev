# Align Operator Status And Command Surface

## Priority Decision

P1. This is the fifth implementation slice. Score: high operator value, high design clarity, medium engineering leverage, direct evidence, and high parallel executability. It outranks historical spec cleanup because it touches the live first-run path and command surface operators actually see.

## User / Operator Outcome

An operator can run the binary, inspect help and doctor/status output, and understand what is ready, what is blocked, and what command is safe next. README, tests, and help smoke agree with the live Clap command surface.

## Evidence

- `src/main.rs` exposes `audit-harvest`.
- README describes twenty-one commands and omits or undercounts the live command surface.
- The top-level command-surface test fails because expected commands do not match the live enum.
- `src/doctor_command.rs` probes only selected help surfaces.
- `auto doctor`, `auto parallel status`, `auto quota status`, and audit status each expose useful facts, but there is no single no-model top-level state summary.
- `AGENTS.md` says model tools are required while code and README treat them as capability warnings.

## Scope Boundary

Do not build a web UI. Do not make status model-backed. Do not rewrite README broadly. Do not hide validation failures behind friendly output. This slice should render code-owned facts and reconcile live command truth.

## Implementation Slice

Goal: make first-run and command-surface truth coherent.

Dependencies: plans 002 through 005 should be complete or their blocking facts should be represented in the status output.

Files likely to modify:

- `src/main.rs`
- `src/doctor_command.rs`
- `src/parallel_command.rs` if reusing status summaries.
- `src/quota_status.rs` if exposing quota readiness in a shared status summary.
- `README.md`
- `.github/workflows/ci.yml`
- `tests/parallel_status.rs`, `tests/lifecycle_flows.rs`, or command-surface tests as needed.

Tests to add or modify:

- Command-surface test that derives or verifies the current public commands, including `audit-harvest` if it remains public.
- Doctor/help smoke tests for `auto doctor --help` and `auto audit-harvest --help`.
- A focused test for baseline-ready vs execution-ready doctor output if `doctor` behavior changes.
- Optional test for a no-model `auto status` command if that command is added.

Approach:

1. Decide whether `audit-harvest` is public. If public, update README, command test, and CI help smoke. If private, hide it deliberately and test that it is hidden.
2. Split `auto doctor` messaging between baseline readiness and execution/model readiness, or add a no-model `auto status` command that aggregates existing readiness facts.
3. Reuse existing status helpers rather than duplicating business logic in output formatting.
4. Make missing model tools a clear capability warning unless the operator requested a model-backed phase.
5. Keep README changes small: command list, first-run truth, and validation commands only.

## Verification

From the repository root:

    cargo test tests::top_level_command_surface_matches_live_enum
    cargo test doctor_command::tests
    cargo test parallel_status
    cargo test lifecycle_flows
    cargo test
    cargo run -- --help
    cargo run -- doctor --help
    cargo run -- audit-harvest --help

Expected observation: live help, tests, README command truth, and CI smoke coverage agree. Doctor/status output distinguishes baseline readiness from optional execution/model readiness.

## Deferred

- Rich dashboard UI.
- Full README rewrite.
- Historical stale spec rewrite unless it is cited by active instructions.
- Release marketing or changelog work.
