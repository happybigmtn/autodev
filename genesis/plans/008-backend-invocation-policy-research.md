# Backend Invocation Policy Research

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This research gate designs a shared backend invocation policy before live command runners are refactored. Operators gain a future path where dangerous-mode flags, quota routing, logs, context windows, and futility handling are visible and consistent across Codex, Claude, PI, and Kimi paths.

The user can see it working when the research artifact maps every current backend spawn path, identifies which are safe to consolidate, and calls out any default-behavior changes as user challenges.

## Requirements Trace

- R1: Inventory all direct model CLI spawn paths.
- R2: Identify which paths bypass shared wrappers.
- R3: Define a backend policy vocabulary for dangerous mode, sandbox, approval, quota, logging, context window, and redaction.
- R4: Recommend an incremental implementation sequence with tests.
- R5: Classify default changes as `User Challenge`.

## Scope Boundaries

This is research and design only. It does not change backend behavior, defaults, prompts, or command implementations. It does not remove dangerous flags. It does not introduce a trait until the design proves the shape.

## Progress

- [x] 2026-04-23: Direct and shared backend paths identified at review level.
- [ ] 2026-04-23: Produce backend policy map.
- [ ] 2026-04-23: Decide whether to implement a shared runner in a later plan.

## Surprises & Discoveries

The code has both shared wrappers and direct spawn paths. This means help text or defaults alone cannot prove real backend behavior; the runtime path must be verified per command.

## Decision Log

- Taste: Research first because a premature abstraction could destabilize model-heavy commands.
- Mechanical: Any policy must expose dangerous-mode behavior.
- User Challenge: Changing default sandbox/approval behavior needs operator approval.

## Outcomes & Retrospective

None yet. The expected outcome is a design note, not code.

## Context and Orientation

Relevant files:

- `src/codex_exec.rs`: shared Codex execution wrapper and max-context helper.
- `src/claude_exec.rs`: shared Claude execution wrapper.
- `src/pi_backend.rs` and `src/kimi_backend.rs`: PI/Kimi command construction.
- `src/generation.rs`: author and review model execution.
- `src/bug_command.rs`, `src/nemesis.rs`, `src/audit_command.rs`, `src/symphony_command.rs`: direct or specialized model launches.
- `src/quota_exec.rs`: quota-aware provider command opening.
- `src/codex_stream.rs`: stream rendering and futility handling.

Terms:

- Backend: the external model CLI or provider path used to run a worker.
- Dangerous mode: explicit provider flags that bypass sandbox or approval prompts.
- Policy layer: a small interface that records and enforces execution choices before spawning a backend.

## Plan of Work

Create a research document or root planning note that inventories every model spawn path. For each path, record command, wrapper usage, quota behavior, log path, redaction, dangerous flags, model/reasoning source, context-window behavior, and tests. Then recommend an implementation sequence, likely starting with a grep-style guard that prevents new direct `codex` or `claude` spawns outside approved modules.

## Implementation Units

Unit 1 - Spawn path inventory:

- Goal: Find every place the repo launches a model CLI.
- Requirements advanced: R1, R2.
- Dependencies: Plan 005.
- Files to create or modify: research artifact under root planning docs or `genesis` if not promoted.
- Tests to add or modify: Test expectation: none -- research inventory only.
- Approach: use `rg` for `Command::new`, provider binary names, dangerous flags, and quota open.
- Specific test scenarios: inventory includes generation, bug, nemesis, audit, symphony, loop, parallel, review, QA, health, ship, and steward paths.

Unit 2 - Policy vocabulary:

- Goal: Define common fields for backend execution decisions.
- Requirements advanced: R3, R5.
- Dependencies: Unit 1.
- Files to create or modify: research artifact.
- Tests to add or modify: Test expectation: none -- design only.
- Approach: document a small policy shape and mark every default-changing field as operator-sensitive.
- Specific test scenarios: policy can describe current Codex, Claude, Kimi, and PI paths without losing behavior.

Unit 3 - Implementation sequence proposal:

- Goal: Recommend a safe follow-up plan.
- Requirements advanced: R4.
- Dependencies: Units 1 and 2.
- Files to create or modify: research artifact and `IMPLEMENTATION_PLAN.md` if promoted.
- Tests to add or modify: Test expectation: none -- sequence proposal only.
- Approach: propose smallest guard first, then shared runner extraction, then call-site migration.
- Specific test scenarios: proposed first implementation has clear targeted tests and does not change defaults.

## Concrete Steps

From the repository root:

    rg -n "Command::new\\(|codex exec|claude -p|dangerously|--yolo|quota open|run_codex|run_claude|run_pi|run_kimi" src
    rg -n "model_context_window|model_reasoning_effort|reasoning_effort|stderr.*log|redact" src

Expected observation: multiple command modules build or invoke model commands directly or through specialized helpers.

## Validation and Acceptance

Acceptance requires a research artifact that:

- inventories all backend spawn paths;
- distinguishes shared wrapper paths from direct paths;
- documents current dangerous-mode flags;
- names tests needed before implementation;
- recommends a sequence that preserves current behavior unless the operator approves a user challenge;
- explicitly says which default changes are not being made.

## Idempotence and Recovery

This research can be rerun after code changes. Keep the inventory grep commands in the artifact so future operators can refresh it. If the inventory is incomplete, do not implement the shared policy yet.

## Artifacts and Notes

Candidate artifact names:

- root promoted: a new section in `IMPLEMENTATION_PLAN.md` plus a dated spec;
- generated-only: update this plan and `genesis/ASSESSMENT.md`.

Record exact grep patterns used and any spawn path that was intentionally excluded.

## Interfaces and Dependencies

Research interfaces:

- provider wrappers;
- direct command spawn paths;
- quota router;
- stream/log redaction;
- CLI defaults in `src/main.rs`;
- external CLIs `codex`, `claude`, `pi`, and `kimi-cli` when enabled.
