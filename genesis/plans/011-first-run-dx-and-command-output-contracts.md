# First-Run DX and Command Output Contracts

This ExecPlan is a living document. Keep the Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective sections current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added later, this plan must be maintained in accordance with root `PLANS.md`.

## Purpose / Big Picture

Autodev is developer-facing. A new operator should reach a truthful success moment quickly and understand what each command proved. Operators gain lower first-run uncertainty and more automation-friendly output. They can see it working when `auto doctor`, help smoke tests, dry-run summaries, and final status blocks use consistent labels and required/optional tool language.

## Requirements Trace

- R1: Reconcile required versus optional tool language across `AGENTS.md`, README, and `auto doctor`.
- R2: Expand first-run smoke to cover important command help surfaces.
- R3: Define consistent final status blocks for doctor, parallel status, qa-only, health, design, review, and ship.
- R4: Clarify dry-run behavior, including whether prompt logs or state directories are written.
- R5: Add structured output only where it reduces operator uncertainty or enables automation.

## Scope Boundaries

This plan does not create a web UI and does not change core command behavior except output contracts and smoke coverage. It does not install external tools for the operator.

## Progress

- [x] 2026-04-30: Verified `DESIGN.md` is justified for terminal/operator UX.
- [ ] 2026-04-30: Reconcile tool availability language.
- [ ] 2026-04-30: Add help/output smoke tests and final status conventions.

## Surprises & Discoveries

- `auto doctor` already gives a useful no-model path and reportedly ends with `doctor ok`.
- Dry-run behavior is inconsistent enough that the output contract matters as much as the implementation.

## Decision Log

- Mechanical: Developer-facing repos need a DX pass.
- Taste: Prefer compact final status blocks over verbose prose.
- User Challenge: If a tool is optional for some workflows but required for production execution, docs should say both rather than flattening the distinction.

## Outcomes & Retrospective

None yet. Record final output labels and any structured-output decision.

## Context and Orientation

Relevant files:

- `src/doctor_command.rs`: first-run checks.
- `src/main.rs`: command help and args.
- `src/parallel_command.rs`, `src/qa_only_command.rs`, `src/health_command.rs`, `src/design_command.rs`, `src/review_command.rs`, `src/ship_command.rs`: status/report outputs.
- `README.md`, `AGENTS.md`, `.github/workflows/ci.yml`: onboarding and smoke surfaces.

Non-obvious terms:

- Success moment: the first command output that tells a new operator the tool is installed and the checkout is recognizable.
- Structured output: machine-readable JSON or similar output for automation.

## Plan of Work

Inventory all first-run and high-value operator commands. Update doctor and docs so required tools are described by workflow: no-model preflight, local planning, model-backed execution, GitHub/Symphony release. Add help smoke tests for important commands missing from CI, including design and doctor. Standardize final status blocks with status, artifacts, receipts, blockers, and next command. Decide whether `auto doctor --json` or `auto parallel status --json` is worth implementing now.

## Implementation Units

- Unit 1: Tool-language reconciliation. Goal: honest onboarding. Requirements advanced: R1. Dependencies: Plan 009. Files: `AGENTS.md`, README, `src/doctor_command.rs` when promoted. Tests: doctor optional/missing tool tests. Approach: distinguish required-for-all from required-for-workflow. Scenarios: missing `gh`; missing `codex`; no-model pass.
- Unit 2: Help smoke coverage. Goal: catch command surface drift. Requirements advanced: R2. Dependencies: Unit 1 optional. Files: `.github/workflows/ci.yml`, command tests. Tests: installed binary help for key commands. Approach: add low-cost help probes. Scenarios: `auto design --help`, `auto doctor --help`, `auto review --help`, `auto ship --help`.
- Unit 3: Output contract. Goal: consistent final status. Requirements advanced: R3, R4, R5. Dependencies: Unit 2 optional. Files: command modules listed above. Tests: snapshot or substring tests for final labels. Approach: define shared labels before broad formatting refactor. Scenarios: dry-run wrote no files; dry-run wrote prompt log; report-only wrote report.

## Concrete Steps

From the repository root:

    rg -n "Required tools|doctor ok|dry-run|report-only|--help|status:" README.md AGENTS.md .github/workflows/ci.yml src
    cargo test doctor_command::tests
    cargo test qa_only_command::tests
    cargo test health_command::tests
    cargo test design_command::tests
    cargo test review_command::tests
    cargo test ship_command::tests
    auto doctor
    auto design --help
    auto review --dry-run

Expected observations after implementation: first-run output and docs agree on what is required and what was checked.

## Validation and Acceptance

Acceptance:

- README, AGENTS, and doctor no longer contradict each other about required tools.
- CI or tests smoke all high-value help surfaces.
- Dry-run/report-only outputs say whether files were written.
- Final status labels are consistent across major operator commands.
- Any structured output added has tests and a documented use case.

## Idempotence and Recovery

Help and doctor commands should be safe to run repeatedly. If output contract changes break tests, update tests only after confirming the new labels improve operator clarity.

## Artifacts and Notes

Record the final first-run command sequence and the help surfaces covered by CI.

## Interfaces and Dependencies

Interfaces: CLI help, doctor checks, command stdout summaries, README/AGENTS onboarding, CI smoke. Dependencies: Plans 009 and 010 for write-boundary truth.
