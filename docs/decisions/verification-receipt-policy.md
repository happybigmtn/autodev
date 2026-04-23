# Decision: verification receipt output summaries and zero-test policy

Status: accepted
Date: 2026-04-23
Task: `AD-012`

## Context

- `scripts/run-task-verification.sh` runs the requested verification command and then asks `scripts/verification_receipt.py` to record task ID, command text, argv, exit code, timestamp, and pass/fail status.
- `src/completion_artifacts.rs` requires matching receipts for executable `Verification:` commands, rejects missing, corrupted, incomplete, or failed receipts, and matches commands by exact command text or argv equivalence.
- Receipts do not currently store stdout, stderr, or structured runner details, so the evidence checker cannot reject a command that exits zero while reporting `0 tests`.
- Parallel worker prompts already tell workers not to count zero-test output as proof, but prompt guidance is weaker than receipt-backed completion evidence.

## Decision

Future receipt entries should add an output summary block for each command. The summary stores bounded, redacted stdout and stderr tails plus enough metadata for completion evidence to detect zero-test runs without keeping full logs.

Proposed command-entry fields:

```json
{
  "command": "cargo test task_parser::tests::parses_all_plan_statuses_and_fields",
  "argv": ["cargo", "test", "task_parser::tests::parses_all_plan_statuses_and_fields"],
  "exit_code": 0,
  "status": "passed",
  "recorded_at": "2026-04-23T00:00:00+00:00",
  "output_summary": {
    "stdout_tail": "...",
    "stderr_tail": "...",
    "stdout_bytes": 1234,
    "stderr_bytes": 567,
    "stdout_truncated": false,
    "stderr_truncated": false,
    "redaction_version": 1
  },
  "runner_summary": {
    "kind": "cargo-test",
    "tests_discovered": 1,
    "tests_run": 1,
    "zero_test_detected": false
  }
}
```

The exact JSON shape can be adjusted during implementation, but the durable contract is:

- Store stdout and stderr separately, not only a combined transcript.
- Store tails, byte counts, and truncation booleans so reviewers can tell whether a summary is incomplete.
- Store structured runner facts only when the wrapper can classify the command confidently.
- Keep the existing command, argv, exit code, status, and timestamp fields stable.

## Byte Limits

- Capture at most 16 KiB of stdout tail and 16 KiB of stderr tail per command after redaction.
- Count original stream bytes before truncation in `stdout_bytes` and `stderr_bytes`.
- Prefer UTF-8 lossless text when possible; replace invalid byte sequences rather than failing the verification command solely because a runner emitted non-UTF-8 output.
- Do not store full output by default. A command that needs full logs should write a declared completion artifact, not expand the receipt.

## Redaction

Receipt output summaries are local execution evidence, not a log archive. The wrapper must redact before writing receipts.

Initial redaction posture:

- Redact common secret assignments and headers, including `TOKEN=...`, `PASSWORD=...`, `SECRET=...`, `Authorization: Bearer ...`, GitHub tokens, OpenAI keys, Anthropic keys, and Codex or Claude auth material when recognizable.
- Redact values, not entire lines, so test-runner context such as `0 tests` remains inspectable.
- Apply the same redaction to stdout and stderr before truncation.
- Mark the redaction policy with `redaction_version` so future receipt readers know which rules produced the summary.

If redaction finds a high-confidence credential pattern that cannot be safely rewritten, the receipt writer should omit that stream tail and record a redacted placeholder instead of leaking it.

## Zero-Test Detection

Zero-test detection should ship in narrow runner-specific steps. A generic regex over every command is too likely to reject valid non-test tools.

First covered runners:

- Cargo test commands, including `cargo test`, `cargo nextest run` when it appears later, and filtered Rust tests that exit zero while reporting `0 tests`, `0 passed`, or equivalent no-tests-run output.
- Pytest commands, including `pytest` and `python -m pytest`, when output reports `collected 0 items`, `no tests ran`, or equivalent zero-test forms.

Completion evidence should reject a matching successful receipt when `runner_summary.zero_test_detected` is true. The failure reason should name the command and say the receipt reported a zero-test run.

Non-goals for the first implementation:

- Do not classify `rg`, `curl`, `git`, `docker`, deployment commands, or narrative proof commands as zero-test runners.
- Do not infer zero-test status from missing output alone.
- Do not treat a runner's skipped-test count as zero-test proof unless the runner also reports that no tests were discovered or run.

## Receipt Write Failures

Receipt write failures should become fatal for wrapper-backed proof commands.

The wrapper may still return the underlying command status when the command itself fails. When the command succeeds but the receipt cannot be written, the wrapper should exit nonzero and print a clear `verification-receipt` error to stderr. A task cannot claim executable verification passed if the required receipt was not recorded.

## Compatibility

This decision does not change current wrapper behavior, receipt JSON shape, or completion evidence logic. It only fixes the policy that `AD-013` should implement.

Existing receipts without `output_summary` remain readable for old tasks. Once zero-test enforcement is implemented, commands that require zero-test inspection need fresh receipts produced by the updated wrapper.
