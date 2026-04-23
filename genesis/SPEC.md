# SPEC - autodev as an operator-trust CLI

## Product Summary

`autodev` builds the `auto` command, a repo-root CLI for planning, generating, executing, reviewing, and shipping agent-driven engineering work. It is a local operator tool, not a hosted service. Its main job is to convert a real working tree into actionable planning artifacts, run model-backed workers against that tree, and preserve enough evidence that a human can trust what happened.

Near-term product direction: stabilize the current command lifecycle before expanding the surface. The product should become an evidence-first operator console for existing commands: planning truth, credential safety, verification receipts, backend policy, and first-run confidence.

## Users And Jobs

Primary operator job: "I need to move a real repo forward with agent help without losing track of source truth, credentials, worktree state, or verification evidence."

Contributor job: "I need to change `auto` safely, understand which command owns which behavior, and run tests that prove I did not break the lifecycle."

Reviewer job: "I need generated plans, receipts, and logs that let me distinguish implemented code from model prose."

## Current System Behaviors Grounded In Code

Planning:

- `auto corpus` writes a planning corpus under `genesis/`, can accept idea/focus/reference repo inputs, runs an author phase, optionally runs Codex review, sanitizes absolute repo-root paths, and verifies required corpus shape.
- `auto gen` and `auto reverse` load the corpus, emit `gen-*` snapshots, generate specs and an implementation plan, and synchronize selected output back to root docs.
- `src/corpus.rs` defines the corpus loader shape. Numbered plans are expected under `genesis/plans/`.

Execution:

- `auto loop` picks sequential plan work, syncs/checkpoints/pushes the primary branch, and runs a model worker.
- `auto parallel` parses plan tasks, assigns lanes, runs tmux-backed workers, lands lane commits, writes review handoffs, and can sync with Linear.
- `auto symphony` renders and runs Linear/Symphony workflows.

Quality and review:

- `auto review`, `auto qa`, `auto qa-only`, `auto health`, and `auto ship` are model-backed operator commands around review, QA, status, and release control.
- `auto audit`, `auto bug`, and `auto nemesis` generate findings and remediation work through specialized prompts and parsers.

Quota and backend execution:

- `auto quota` manages provider accounts, usage selection, and credential swapping for Codex and Claude.
- Codex and Claude wrappers exist, but several command modules also build direct spawn paths.

State and generated artifacts:

- `.auto/` stores runtime state, logs, receipts, and snapshots.
- `bug/` and `nemesis/` store generated outputs for their respective commands.
- `genesis/` is a planning corpus.
- `gen-*` directories are generated plan/spec outputs.

## Verified Technical Details

The following values are verified from current code or checked-in configuration, not invented requirements:

- Package version: `0.2.0` from `Cargo.toml`.
- Rust edition: `2021` from `Cargo.toml`.
- Binary name: `auto` from `Cargo.toml`.
- Dependency tags in `Cargo.toml`: `anyhow = "1"`, `chrono = "0.4"`, `clap = "4"`, `console = "0.15"`, `dirs = "6"`, `fd-lock = "4"`, `regex = "1"`, `reqwest = "0.12"`, `serde = "1"`, `serde_json = "1"`, `shlex = "1.3"`, `tokio = "1"`, `base64 = "0.22"`, `sha2 = "0.10"`, `toml = "0.8"`.
- Corpus default model and review model in current `src/main.rs`: `gpt-5.5` with `xhigh` reasoning.
- Bug, nemesis, audit, loop, parallel, QA, review, ship, steward, and Symphony defaults currently favor Codex `gpt-5.5` paths unless explicit Claude/Kimi/PI options are selected.
- Codex max context helper: `MAX_CODEX_MODEL_CONTEXT_WINDOW = 1_000_000` in `src/codex_exec.rs`.
- Quota selector floors: weekly floor `10` percent and session floor `25` percent in `src/quota_selector.rs`.
- Quota exhaustion cooldown: `1` hour in `src/quota_state.rs`.
- Claude token refresh endpoint and client id are hardcoded in `src/quota_usage.rs`; treat those as verified implementation details, not a recommendation to expose them in docs.
- Claude refresh buffer: `300` seconds in `src/quota_usage.rs`.
- Codex CLI refresh timeout: `20` seconds in `src/quota_usage.rs`.
- Parallel poll/cleanup timing constants are defined in `src/parallel_command.rs`; keep future docs tied to those constants rather than retyping values casually.
- CI currently runs format check, clippy with warnings denied, and tests from `.github/workflows/ci.yml`.

The exact resolved dependency versions are in `Cargo.lock`; this corpus does not restate the full lockfile as requirements.

## Recommended Requirements

R1. Planning truth must be singular. Root `IMPLEMENTATION_PLAN.md` plus `specs/` remain the active planning surface unless the repo adds a root `PLANS.md` or explicit instruction to promote another control plane. `genesis/` remains generated corpus input for planning.

R2. Credential swapping must be transactional. Every provider auth file or directory backed up during quota execution must be restored or removed on all success and failure paths.

R3. Credential profile storage must be owner-only and symlink-safe. Profile capture must reject or dereference symlinks intentionally, prune stale files, and create sensitive files with restricted permissions on Unix.

R4. Generated executable workflow text must validate and quote every shell/YAML scalar. Branch, model, reasoning, path, and remote values must not be interpolated raw.

R5. Verification evidence must be risk-classed. Safe local tests, shell interpreters, network/external tools, and destructive commands should not be treated as the same proof type.

R6. Task parsing should converge. Generation, loop, parallel, review, completion artifacts, and Symphony should share one model for status, dependencies, verification commands, spec refs, and completion artifacts.

R7. Backend policy should be explicit. Dangerous bypass flags may remain available, but they should be controlled through one policy layer and visible in command help or logs.

R8. First-run operator experience should include a local, no-model success path that proves the binary, repo layout, required tools for the selected command, and safe dry-run behavior.

## Non-Goals

- No web UI or TUI in this phase.
- No new provider backend in this phase.
- No rewrite into a workspace, daemon, or plugin architecture.
- No silent default change that makes existing operator workflows fail without migration text.
- No encryption-at-rest commitment yet. File permissions and symlink safety are immediate requirements; encryption is a separate user challenge because it introduces key management.
- No attempt to make `genesis/` the active root queue.

## System Interfaces

CLI interface: `src/main.rs`.

Planning corpus interface: `genesis/ASSESSMENT.md`, `genesis/SPEC.md`, `genesis/PLANS.md`, `genesis/GENESIS-REPORT.md`, `genesis/DESIGN.md`, and numbered plans under `genesis/plans/`.

Active repo planning interface: `IMPLEMENTATION_PLAN.md`, `ARCHIVED.md`, `WORKLIST.md`, and `specs/`.

Provider execution interface: `src/codex_exec.rs`, `src/claude_exec.rs`, `src/pi_backend.rs`, `src/kimi_backend.rs`, plus direct command-specific spawn paths that should be audited.

Credential interface: `src/quota_config.rs`, `src/quota_exec.rs`, `src/quota_state.rs`, `src/quota_usage.rs`, and provider home-directory auth files.

Workflow rendering interface: `src/symphony_command.rs` and `src/linear_tracker.rs`.

Verification interface: `src/completion_artifacts.rs`, `scripts/run-task-verification.sh` if present, `auto parallel` handoff logic, and loop/review prompt contracts.

## Data Flow

1. A human starts from a repo root and runs planning (`auto corpus`, then `auto gen`) or execution (`auto loop`, `auto parallel`, `auto symphony`).
2. The command reads root instructions, current docs, specs, and source.
3. The command may call provider CLIs through direct or shared wrappers.
4. It writes logs, prompts, receipts, generated artifacts, and sometimes commits.
5. Review and landing commands parse those artifacts to decide whether tasks are done, partial, blocked, or stale.

The safest future architecture keeps these contracts explicit and shared. The highest-risk current drift is that several commands do similar parsing and proof evaluation differently.

## Observability Requirements

Every mutating command should emit:

- command name, model/backend, reasoning effort, and whether dangerous mode is active;
- repo branch and dirty-state summary before mutation;
- output directory or log path before model invocation;
- verification receipt path when executable proof is claimed;
- credential provider/account label when quota routing is active, without secrets;
- recovery instructions when an archive, checkpoint, or backup is created.

Logs should pass through one redaction layer before persistence.

## Validation Strategy

Near-term validation should prioritize targeted unit and integration tests:

- quota restore and profile capture regression tests;
- Symphony hostile scalar golden tests;
- completion-artifact false-proof fixtures;
- shared task parser fixtures for `[ ]`, `[~]`, `[!]`, `[x]`, dependencies, spec refs, verification, and artifacts;
- fake-model CLI smoke tests for command wiring;
- installed-binary proof after source changes that affect user invocation.

Full `cargo test`, clippy, and CI remain release gates, but targeted tests should fail before and pass after each vertical slice.

## Open Questions

- Should `genesis/` be tracked long-term or treated as disposable generated state? Current code treats it as generated corpus, but it is currently tracked.
- Should dangerous backend bypass remain the default for all agent commands, or should commands require an explicit trust profile?
- Should `steward` eventually replace `corpus + gen` for mid-flight repos, or remain a separate reconciliation command?
- Should quota credentials be encrypted at rest, or is owner-only file permission sufficient for this local developer tool?
- Should the repo add a new `auto doctor` command or extend `auto health` for command-specific preflight?
