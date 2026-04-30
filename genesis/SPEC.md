# Autodev Product Specification

## Problem Statement

How might we make `auto` a production-grade autonomous development control plane that converts repository truth into plans, runs model-backed implementation safely, verifies the result with durable evidence, and gives operators enough confidence to ship without hand-reconstructing what happened?

The current code shows a real Rust CLI, not a planning-only prototype. It owns command routing, corpus generation, task parsing, parallel lane orchestration, quota-backed model execution, design/QA/health/review surfaces, receipt capture, audit passes, and release gating. The next system step is not to invent a new product. It is to make the existing control plane impossible to fool by stale plans, unsafe credentials, empty corpora, lossy dependencies, or old receipts.

## Primary Users

- Repository operator: runs `auto corpus`, `auto gen`, `auto parallel`, `auto review`, `auto audit`, and `auto ship` to move real work through a repo.
- Engineering lead: uses generated plans, receipts, review handoffs, and release gates to decide what can be trusted.
- Agent worker: receives generated prompts and task contracts from `auto parallel`, `auto loop`, `auto super`, Symphony, or quota-backed execution.
- Maintainer of autodev itself: must be able to test, install, recover, and release the CLI without guessing state.

## Current System Behaviors Verified From Code

- `src/main.rs` defines 21 command variants and central dispatch for the `auto` binary.
- `README.md` presents the current command surface as 21 commands; older specs and previous genesis snapshots are stale where they claim 16 or 17.
- `src/generation.rs` validates generated corpus and task contracts, provides `--snapshot-only`, preserves blocked task rows better than older code, and rejects broad or low-signal verification commands.
- `src/corpus.rs` and generation state currently allow a partial planning root with `plans/` but no numbered plans to be treated as usable input; this is a production risk after an interrupted corpus run.
- `src/task_parser.rs` is the shared task parser and recognizes statuses, dependencies, verification text, and completion artifacts, but dependency extraction is still lossy for bare references such as `Dependencies: TASK-011`.
- `src/parallel_command.rs` implements tmux-backed lanes, status, preflight, salvage, drift audit, and queue state, but lane reuse can make salvage records point at dead working directories.
- `src/quota_config.rs` and `src/quota_exec.rs` support account profiles, credential swapping, owner-only file writes, and symlink rejection, but account names are not fully path-bounded and provider locks are not held while the child process runs.
- `src/completion_artifacts.rs` and `scripts/verification_receipt.py` capture executable proof and reject zero-test receipts, but receipts are not bound to the current commit, dirty state, plan hash, or artifact hashes.
- `src/ship_command.rs` has a mechanical release gate and bypass trail, but it can accept stale receipts and does not require the locked install proof claimed by README and CI.
- `src/symphony_command.rs` validates hostile workflow inputs, but reconciliation can corrupt partial task rows by marking `[~]` lines through a code path built around `[ ]` and `[!]`.
- `src/doctor_command.rs` gives a useful no-model first-run preflight, but optional-versus-required tool language differs between `AGENTS.md`, README, and doctor output.
- `.github/workflows/ci.yml` runs format, clippy, tests, locked install, and selected help smoke tests, but it does not exercise the shell/Python receipt writer path or every important operator help surface.

## Near-Term Direction

Recommended direction: keep `auto corpus` and `auto gen` as the control primitives, but harden the control plane before scaling parallel implementation. The next 14-day production race should be ordered around these contracts:

- Corpus roots are atomic and never accepted when empty or partially generated.
- Quota-backed model execution cannot cross-contaminate credentials across parallel lanes.
- Scheduler eligibility uses the same dependency truth everywhere and treats missing dependencies as blockers.
- Receipts prove current tree state and current artifacts, not just command text.
- Report-only and dry-run commands have explicit, tested write boundaries.
- Release gates reconcile root planning truth, review evidence, CI proof, install proof, and tag state.

## Requirements

- R1: `genesis/` must be generated atomically and validated as non-empty before it becomes the active planning root.
- R2: quota account names must be path-safe, and credential leases must cover the child process lifetime.
- R3: dependency parsing must handle backticked and bare task IDs, and missing dependency IDs must block scheduling.
- R4: completion and release receipts must record commit, dirty state, plan identity, artifact identity where relevant, command status, output tails, and zero-test summaries.
- R5: `auto parallel`, `auto loop`, `auto super`, Symphony reconciliation, and root-plan updates must share one task contract.
- R6: `auto doctor`, report-only commands, dry-run commands, and help surfaces must give honest first-run and recovery signals.
- R7: release readiness must be a gate over code, docs, specs, review handoff, receipts, CI, installed binary proof, and rollback notes.

## Non-Goals

- Do not replace the Rust CLI architecture.
- Do not build a web UI before the terminal/operator experience is trustworthy.
- Do not make `genesis/` the root active queue unless repo instructions explicitly promote it.
- Do not run `auto parallel` on a queue with unresolved stale plan rows, unsafe credentials, or unbound receipts.
- Do not treat older dated specs as current requirements when code and root plans have moved.

## Open Questions

- Should quota execution serialize per provider for safety, or can it use isolated per-process credential homes for real parallelism?
- Should `auto gen` remain mutating by default, or should production use require explicit root-sync flags after a snapshot passes validation?
- Should `auto doctor` gain structured output for automation, or remain a human-readable preflight with tests around important labels?
- Should stale `TASK-016` be closed by root-plan reconciliation, or should the release gate own it as a final tag-evidence assertion including `COMPLETED.md` and tag-annotation cleanup?
