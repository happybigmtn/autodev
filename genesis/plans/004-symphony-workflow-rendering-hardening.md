# Symphony Workflow Rendering Hardening

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be updated as work proceeds. No root `PLANS.md` exists in this repository today; if one is added later, maintain this plan in accordance with root `PLANS.md`.

## Purpose / Big Picture

This plan hardens generated Symphony workflow text so operator-provided branch, model, reasoning, path, and remote values cannot be interpreted as unintended shell or YAML. Users gain safer unattended execution because rendered workflows become data-safe before they are handed to another runtime.

The user can see it working by running golden tests with hostile values containing spaces, quotes, semicolons, command substitution, and newlines, then inspecting rendered YAML and shell snippets.

## Requirements Trace

- R1: Validate branch names before embedding them in shell commands.
- R2: Validate model and reasoning values before embedding them in command text.
- R3: Quote every shell scalar in rendered hooks.
- R4: YAML-quote every scalar that can contain operator or remote input.
- R5: Add golden tests for hostile scalar inputs.

## Scope Boundaries

This plan does not change Linear API behavior, Symphony issue parsing, workspace layout, or the default model. It does not redesign the workflow format. It only hardens validation and rendering.

## Progress

- [x] 2026-04-23: Unquoted `base_branch`, `model`, and `reasoning_effort` interpolation identified in `src/symphony_command.rs`.
- [ ] 2026-04-23: Add failing hostile-scalar render tests.
- [ ] 2026-04-23: Implement validators and quoting.
- [ ] 2026-04-23: Re-run targeted Symphony tests.

## Surprises & Discoveries

`shell_quote` already exists in `src/symphony_command.rs`, but nearby branch/model/reasoning interpolations do not consistently use it. This is a local hardening task, not a new dependency problem.

## Decision Log

- Mechanical: Generated shell text must quote dynamic values.
- Mechanical: Generated YAML must treat dynamic values as scalars, not syntax.
- Taste: Keep this focused on Symphony rather than building a global templating engine first.

## Outcomes & Retrospective

None yet. After implementation, record which scalars are validated, which helper owns YAML quoting, and whether any CLI inputs became stricter.

## Context and Orientation

Relevant code:

- `src/symphony_command.rs` renders workflow YAML and shell hooks.
- `src/main.rs` defines Symphony CLI arguments as strings.
- `shell_quote` exists in `src/symphony_command.rs`.
- `linear_tracker.rs` handles Linear sync but is not the main rendering target.

Terms:

- Shell scalar: a string inserted into a shell command.
- YAML scalar: a string inserted into YAML where punctuation can change structure if not quoted.
- Golden test: a test that compares a rendered text output against expected safe text or expected properties.

## Plan of Work

Add tests first around the workflow render function using hostile `base_branch`, `model`, and `reasoning_effort` values. Then introduce small validators for branch/model/reasoning values. Use a shell-quote helper for every shell interpolation and a YAML scalar helper for YAML-visible dynamic values. Prefer rejecting newlines and command separators in fields that are not meant to be free-form.

## Implementation Units

Unit 1 - Hostile scalar tests:

- Goal: Make unsafe rendering visible.
- Requirements advanced: R1, R2, R3, R4, R5.
- Dependencies: existing render tests.
- Files to create or modify: `src/symphony_command.rs`.
- Tests to add or modify: add tests near existing Symphony render tests.
- Approach: construct workflow specs with hostile branch/model/reasoning values and assert either rejection or safe quoting.
- Specific test scenarios: branch `main; echo bad`, branch with spaces, model with `$()`, reasoning with newline, remote URL with quote.

Unit 2 - Typed validators:

- Goal: Reject values that should never be executable syntax.
- Requirements advanced: R1, R2.
- Dependencies: Unit 1.
- Files to create or modify: `src/symphony_command.rs`, maybe argument parsing in `src/main.rs` if validation belongs at CLI boundary.
- Tests to add or modify: validator unit tests.
- Approach: accept normal branch names and known reasoning labels; reject newlines and shell metacharacter payloads where inappropriate.
- Specific test scenarios: `main`, `feature/name`, and `release-1.2` pass; newline and semicolon payloads fail with a clear error.

Unit 3 - Quoted rendering helpers:

- Goal: Ensure rendered YAML and shell preserve data boundaries.
- Requirements advanced: R3, R4.
- Dependencies: Unit 2.
- Files to create or modify: `src/symphony_command.rs`.
- Tests to add or modify: golden tests for rendered snippets.
- Approach: use existing `shell_quote` or improve it; add a YAML scalar helper.
- Specific test scenarios: rendered `git fetch`, `git checkout`, `git pull`, and Codex command lines contain quoted dynamic values; YAML fields remain parseable.

## Concrete Steps

From the repository root:

    cargo test symphony_command::tests::workflow_render -- --nocapture
    rg -n "format!\\(\"git fetch|format!\\(\"git checkout|model_reasoning_effort|shell_quote" src/symphony_command.rs

After edits:

    cargo test symphony_command::tests::workflow_render_is_repo_specific
    cargo test symphony_command::tests::shell_quote_escapes_single_quotes
    cargo test symphony_command::tests::hostile

Expected observation: hostile scalar tests fail before validation/quoting and pass after.

## Validation and Acceptance

Acceptance requires:

- hostile branch/model/reasoning inputs are rejected or rendered as inert data;
- no dynamic branch/model/reasoning value is inserted raw into shell command text;
- YAML-visible dynamic values are quoted or otherwise rendered safely;
- existing Symphony workflow tests still pass;
- error messages tell the operator which input was invalid.

## Idempotence and Recovery

Tests are local and should not call Linear or Symphony. If validation becomes too strict and blocks a legitimate branch/model shape, add that shape as an allowed test case and adjust the validator narrowly. If helper changes are risky, keep them private to `src/symphony_command.rs` first.

## Artifacts and Notes

Record sample hostile inputs and whether each is rejected or safely quoted. Include a short before/after render excerpt in the implementation notes, but do not include secrets or live Linear data.

## Interfaces and Dependencies

Interfaces touched:

- Symphony CLI argument validation in `src/main.rs` or render-time validation in `src/symphony_command.rs`;
- workflow rendering helpers in `src/symphony_command.rs`;
- generated Symphony workflow YAML consumed by the external Symphony runtime.
