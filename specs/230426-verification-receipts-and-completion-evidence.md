# Specification: Verification Receipts And Completion Evidence

## Objective
Make task completion proof machine-checkable by requiring review handoff, receipt-backed executable verification, and declared artifact existence before automation marks work complete.

## Evidence Status

### Verified Facts

- `TaskCompletionEvidence::is_fully_evidenced` requires review handoff, verification receipt presence, and no missing completion artifacts in `src/completion_artifacts.rs:46-50`.
- `inspect_task_completion_evidence` reads `REVIEW.md`, `.auto/symphony/verification-receipts/<task-id>.json`, the task verification plan, and declared artifacts in `src/completion_artifacts.rs:115-149`.
- `ensure_host_review_handoff` writes or updates host review handoff entries in `src/completion_artifacts.rs:152-173`.
- `declared_completion_artifacts` reads task markdown from `Completion artifacts:` until the next task metadata section in `src/completion_artifacts.rs:242-272`.
- `verification_plan` separates executable commands from narrative verification guidance in `src/completion_artifacts.rs:274-310`.
- `inspect_verification_receipt` rejects missing wrappers, missing receipts, invalid JSON, missing expected commands, failed expected commands, and incomplete expected command coverage in `src/completion_artifacts.rs:349-457`.
- Receipt command matching accepts exact command text or argv-equivalent matching in `src/completion_artifacts.rs:461-475`.
- Executable verification detection recognizes commands such as `cargo`, `bash`, `sh`, `python`, `node`, `npm`, `rg`, `curl`, `ssh`, `docker`, `kubectl`, `git`, `make`, `just`, `uv`, `go`, `pytest`, and `scripts/` in `src/completion_artifacts.rs:539-563`.
- `scripts/run-task-verification.sh` records a receipt for a task ID and exact command in `scripts/run-task-verification.sh:4-30`.
- `scripts/verification_receipt.py` writes receipts under `.auto/symphony/verification-receipts` with command, argv, exit code, timestamp, and passed or failed status in `scripts/verification_receipt.py:27-88`.
- Parallel worker prompts tell workers to use `scripts/run-task-verification.sh` for host-parsed executable verification commands, to treat `0 tests` output as not run, and to avoid hand-editing receipt files in `src/parallel_command.rs:5314`.
- The planning corpus identifies malformed generated verification commands and false-positive proof paths as the Plan 006 surface in `genesis/plans/006-verification-command-and-receipt-hardening.md:57` and `genesis/ASSESSMENT.md:90`.

### Recommendations

- Classify executable verification commands by risk so network, deployment, destructive, and environment-sensitive commands require explicit task metadata before they can satisfy completion evidence.
- Teach receipt inspection to detect zero-test proof for common test runners instead of relying only on prompt instructions.
- Make receipt write failure fatal for proof-producing commands or explicitly mark the task as unverifiable.
- Reuse completion evidence checks across loop, review, QA, bug, nemesis, and ship where those commands currently rely mostly on prompt compliance.

### Hypotheses / Unresolved Questions

- It is unresolved whether receipt files should be signed, hashed, or only treated as local execution evidence.
- It is unresolved whether narrative-only verification should ever be sufficient for implementation tasks that touch executable code.
- It is unresolved whether external/live verification blockers should be represented as `[!]` blocked tasks or `[~]` partial tasks.

## Acceptance Criteria

- A task with executable verification commands cannot be marked complete without `scripts/run-task-verification.sh` and a matching receipt.
- A missing receipt, corrupted receipt, failed receipt command, or incomplete receipt command set keeps the task incomplete.
- A receipt for a command that exits successfully but reports `0 tests` does not satisfy verification evidence for test-command tasks.
- A task with declared completion artifacts remains incomplete while any declared repo-relative artifact path is missing.
- Narrative verification guidance remains visible to workers but is not executed as shell input.
- Receipt files are created only by the verification wrapper or receipt writer and are never edited by worker prompts as notes.
- Parallel, Symphony, and review handoff flows agree on whether a task is complete, partial, or blocked.

## Verification

- `cargo test completion_artifacts::tests::inspect_task_completion_evidence_requires_review_and_receipts`
- `cargo test completion_artifacts::tests::inspect_task_completion_evidence_requires_wrapper_for_executable_verification`
- `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_failed_receipts`
- `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_corrupted_receipts`
- `cargo test completion_artifacts::tests::inspect_task_completion_evidence_accepts_quoted_command_receipts_with_argv`
- Add and run zero-test receipt regressions for Cargo and at least one non-Cargo runner.

## Open Questions

- Should task verification commands be stored as structured argv in `IMPLEMENTATION_PLAN.md` instead of parsed from markdown prose?
- Should receipt storage remain under `.auto/symphony/` even when the executor is not Symphony?
- Which command classes require an explicit environment blocker rather than a failed verification status?
