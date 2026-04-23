# Verification Command And Receipt Hardening

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This plan closes known false-proof gaps in generated review and completion evidence. Operators gain confidence that a task is marked done because a meaningful command or artifact proves it, not because a model wrote a plausible receipt.

The user can see it working when malformed shell snippets, zero-test Cargo filters, directory greps, and broad network/destructive commands are classified or rejected instead of accepted as routine executable verification.

## Requirements Trace

- R1: Reject malformed generated commands such as bare flags or invalid `cargo --lib` shapes.
- R2: Detect zero-test Cargo filters as weak evidence unless explicitly justified.
- R3: Treat directory grep and broad shell snippets as risky proof, not safe local verification.
- R4: Share receipt expectations between loop, parallel, and review where practical.
- R5: Preserve legitimate narrative evidence for non-executable checks without pretending it is command output.

## Scope Boundaries

This plan does not execute arbitrary verification commands. It changes parsing, classification, prompts, and tests around evidence. It does not redesign the entire task parser; Plan 007 owns broader parser convergence.

## Progress

- [x] 2026-04-23: `WORKLIST.md` identified two open verification-proof issues.
- [ ] 2026-04-23: Add fixtures for malformed and false-positive commands.
- [ ] 2026-04-23: Implement risk classes and stricter acceptance.
- [ ] 2026-04-23: Re-run completion artifact and review tests.

## Surprises & Discoveries

The code already has meaningful receipt validation in `src/completion_artifacts.rs`, but the known failures are at the synthesis/proof boundary: generated commands can look executable while being malformed or too weak.

## Decision Log

- Mechanical: A command that executes zero tests is not strong proof by default.
- Mechanical: A shell interpreter command needs a risk label.
- Taste: Keep classification in completion artifacts first, then decide how much loop/review should share.

## Outcomes & Retrospective

None yet. After implementation, record which command classes are accepted, rejected, or accepted with risk notes.

## Context and Orientation

Relevant files:

- `WORKLIST.md`: names current verification-proof follow-ups.
- `src/completion_artifacts.rs`: parses verification plans, receipts, and completion evidence.
- `src/review_command.rs`: harvests and reviews completed work.
- `src/parallel_command.rs`: uses completion evidence during landing.
- `src/loop_command.rs`: sequential execution path with weaker receipt enforcement.
- `scripts/run-task-verification.sh`: expected receipt wrapper if present.

Definitions:

- Verification receipt: recorded command execution proof for a task.
- False-positive proof: evidence text that looks like validation but does not meaningfully prove the task.
- Risk class: safe-local, shell-interpreter, network/external, destructive, or narrative-only.

## Plan of Work

Turn each `WORKLIST.md` example into a fixture. Add a verification command classifier to `completion_artifacts.rs`. Make receipt acceptance depend on class and context. Update prompts or review synthesis so generated commands prefer safe local commands with specific filters. Then thread the stricter result into parallel/review and document loop behavior.

## Implementation Units

Unit 1 - Fixture the known failures:

- Goal: Reproduce malformed and false-positive verification proof.
- Requirements advanced: R1, R2, R3.
- Dependencies: Plan 005.
- Files to create or modify: `src/completion_artifacts.rs`, maybe test fixtures under an existing test module.
- Tests to add or modify: tests for invalid `cargo --lib`, malformed grep quoting, zero-test filters, and directory grep.
- Approach: express each bad proof as input markdown/receipt text and assert rejection or risk classification.
- Specific test scenarios: `cargo test --lib missing_filter` with zero tests; `cargo --lib`; `grep -R pattern directory`; `sh -c` wrapper.

Unit 2 - Command risk classifier:

- Goal: Categorize executable proof before acceptance.
- Requirements advanced: R2, R3, R5.
- Dependencies: Unit 1.
- Files to create or modify: `src/completion_artifacts.rs`.
- Tests to add or modify: classifier tests.
- Approach: parse first argv token and relevant arguments; avoid ad hoc string matching when `shlex` can parse.
- Specific test scenarios: `cargo test module::test_name` is safe-local; `curl`, `ssh`, `kubectl`, `docker`, `bash`, and `sh` are not safe-local by default.

Unit 3 - Receipt acceptance policy:

- Goal: Make completion evidence enforce the new classes.
- Requirements advanced: R1, R2, R3, R5.
- Dependencies: Unit 2.
- Files to create or modify: `src/completion_artifacts.rs`, possibly `src/parallel_command.rs`.
- Tests to add or modify: completion evidence acceptance/rejection tests.
- Approach: reject malformed commands; require explicit notes for risky commands; reject zero-test claims unless accompanied by a reason that no executable test exists.
- Specific test scenarios: narrative-only docs work remains possible; executable work requires receipt; zero-test output does not mark implementation complete.

Unit 4 - Prompt and review alignment:

- Goal: Reduce generation of bad proof commands.
- Requirements advanced: R4.
- Dependencies: Units 1-3.
- Files to create or modify: `src/review_command.rs`, `src/loop_command.rs`, `src/parallel_command.rs`, prompt text in related modules.
- Tests to add or modify: prompt text tests where they exist.
- Approach: tell workers and reviewers to prefer exact test names and avoid broad shell proof.
- Specific test scenarios: prompt snapshots mention exact tests, no zero-test filters, and receipt wrapper expectations.

## Concrete Steps

From the repository root:

    sed -n '1,220p' WORKLIST.md
    cargo test completion_artifacts::tests::verification_plan -- --nocapture
    cargo test completion_artifacts::tests::inspect_task_completion_evidence -- --nocapture

After edits:

    cargo test completion_artifacts::tests::
    cargo test review_command::tests::
    cargo test parallel_command::tests::audit_parallel_completion_drift_demotes_done_without_review_handoff

Expected observation: new false-proof fixtures fail before policy changes and pass after.

## Validation and Acceptance

Acceptance requires:

- known `WORKLIST.md` false-proof examples are covered by tests;
- malformed shell snippets are rejected;
- zero-test Cargo filters do not count as strong proof without explicit reason;
- risky commands are classified and require a risk note;
- narrative-only evidence remains available for docs/research tasks;
- parallel/review behavior remains compatible with existing valid receipts.

## Idempotence and Recovery

Classifier changes should be small and test-driven. If a legitimate existing receipt fails, add a fixture and decide whether it is safe-local or needs a risk note. Do not broaden acceptance by default; make exceptions explicit.

## Artifacts and Notes

Record the final risk classes and one example command for each. Link the implementation note back to the original `WORKLIST.md` items so they can be closed.

## Interfaces and Dependencies

Primary interface: `src/completion_artifacts.rs`.

Dependent interfaces: review harvesting, parallel landing, loop prompt contracts, and optional receipt wrapper scripts.
