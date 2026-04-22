# Decision: keep verification-receipt enforcement prompt-only in `auto loop` for now

Status: accepted
Date: 2026-04-22
Task: `TASK-012`

## Context

- The execution-pipeline spec says both runners should require receipt-backed proof for executable `Verification:` steps, and it explicitly claims `auto loop` never leaves a task at `- [x]` unless the task's completion evidence is present (`specs/220426-execution-loop-and-parallel.md:5`, `specs/220426-execution-loop-and-parallel.md:46`, `specs/220426-execution-loop-and-parallel.md:64`).
- The built-in `auto loop` worker prompt already tells the worker to preserve the task row, mark `- [x]` only when review handoff, verification evidence, and completion artifacts are present, and use `- [~]` when code landed but evidence is incomplete (`src/loop_command.rs:51-58`).
- The `auto loop` runtime does not verify any of that itself. After a successful worker exit it only checks whether commits or dirty tracked changes exist, then pushes and optionally checkpoints trailing changes (`src/loop_command.rs:286-323`).
- `inspect_task_completion_evidence` is the existing repo-side gate for review handoff, receipt presence, and declared completion artifacts (`src/completion_artifacts.rs:115-149`). For executable `Verification:` commands it requires `scripts/run-task-verification.sh`, which now records JSON receipts via `scripts/verification_receipt.py`; without that wrapper it reports the evidence as missing (`src/completion_artifacts.rs:125-133`, `src/completion_artifacts.rs:347-365`).
- The real existing call sites are in `parallel_command.rs`, not `review_command.rs`:
  - host drift audit demotes completed tasks back to `- [~]` when evidence is missing (`src/parallel_command.rs:3461-3485`)
  - landed-task reconciliation decides `Done` vs `Partial` after the host re-checks evidence and synthesizes a review handoff (`src/parallel_command.rs:5511-5532`)
  - partial follow-up notes also inspect the same evidence to guide repair passes (`src/parallel_command.rs:4189-4228`)
- The task row's claim that `review_command.rs` also calls `inspect_task_completion_evidence` is stale. The live code imports only `review_contains_task` there (`src/review_command.rs:11`).
- The task row's optional follow-up test command is also still hypothetical. `src/loop_command.rs` has branch-selection, reference-repo, and queue-parsing tests, but no `downgrades_marker_when_receipt_missing` case yet, so `cargo test loop_command::tests::downgrades_marker_when_receipt_missing` currently filters to zero tests rather than validating shipped behavior.
- The current README scopes hard receipt-backed proof to `auto parallel` host reconciliation, not to `auto loop` (`README.md:596-598`). The same loop section still describes prompt-driven worker behavior and says finished tasks are removed from the plan, which is already a different model from the built-in prompt's preserve-row-and-mark-`[x]` flow (`README.md:534-535`).

## Decision

Recommend option `(b)`: keep `auto loop` prompt-only for verification-receipt enforcement for now. Do not add Rust-side receipt demotion inside `run_loop` as part of this task.

## Why

- `auto loop` is currently a thin single-worker runner. Unlike `auto parallel`, it does not act as a host reconciler that owns queue truth after the worker returns.
- The repo now contains `scripts/run-task-verification.sh`, so wrapper-backed receipt capture is available. That removes one prerequisite for Rust-side enforcement, but it does not solve the larger loop contract gap around who owns review handoffs, artifact checks, and final queue truth after the worker exits.
- The surrounding loop completion contract is still inconsistent. The prompt says preserve the row and choose between `- [x]` and `- [~]`; the README says remove finished rows; the queue parser in `loop_command.rs` still only reads `- [ ]` and `- [!]` when it selects work (`src/loop_command.rs:361-372`). Receipt enforcement should not be the first place that broader contract drift gets patched over.
- `auto parallel` needs hard evidence gating because the host is landing work from isolated lanes and cannot trust prose handoff alone. That rationale is materially stronger there than in `auto loop`, where the same worker is already responsible for the commit it wants to ship.

## Rejected Alternatives

- Option `(a)`, enforce in Rust now: correct in principle, but still not truthful until `auto loop` also owns post-worker reconciliation and a consistent completion contract for review handoffs and artifact checks.
- Option `(c)`, add only a warning: it would add noise without preventing false `- [x]` states, and it still depends on a post-run source of truth that `auto loop` does not consistently maintain today.

## Consequences

- The spec text that says `auto loop` itself prevents `- [x]` without evidence is forward-looking, not currently implemented behavior. Treat that as spec/code drift rather than as a hidden runtime guarantee.
- If the project later wants hard loop-side enforcement, queue it as a broader contract task: first standardize loop completion semantics and decide whether `auto loop` itself owns review handoffs and completion-artifact validation, then reuse `inspect_task_completion_evidence` from `run_loop`.
