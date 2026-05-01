# Verification Receipt Schema

Receipts are execution evidence, not notes. `scripts/run-task-verification.sh`
owns transient receipt creation; agents and lane workers must not hand-edit
receipt JSON. Durable receipt truth is carried in commit-message footers created
by the host when it lands or closes out a task. Receipt validity informs host
reconciliation, but it is not queue truth by itself.

## Storage Model

The wrapper writes `.auto/symphony/verification-receipts/<TASK>.json` as a
staging artifact. `auto parallel` reads that staging artifact and embeds a
compact receipt in the task closeout commit using these footers:

```text
Auto-Verification-Receipt-Version: 1
Auto-Verification-Receipt-Task: <TASK-ID>
Auto-Verification-Receipt-JSON: <base64url-json>
```

The footer JSON omits bulky `stdout_tail` and `stderr_tail` fields while keeping
the command, argv, exit status, runner summary, artifact hashes, plan hash, and
dirty-state metadata. Readers prefer reachable commit footers and keep JSON
receipts as a compatibility/staging fallback.

## Required Metadata

- `commit`: current `HEAD` for the checkout that ran the command.
- `dirty_state.fingerprint`: fingerprint of tracked and untracked dirty state
  when the command ran.
- `plan_hash`: SHA-256 of the active `IMPLEMENTATION_PLAN.md`.
- `commands[].command`: exact command text from the task row.
- `commands[].expected_argv`: shell-split argv for the expected command.
- `commands[].exit_code` and `commands[].status`: command result.
- `declared_artifacts[].path` and `declared_artifacts[].sha256`: hash evidence
  for declared completion artifacts when the task row requires them.

## Evidence Classes

- Evidence Class: executable -- wrapper-backed local command proof with fresh
  metadata and matching expected argv.
- Evidence Class: external -- live, credentialed, deploy, or operator-system
  proof that cannot be replayed locally; must be named in `REVIEW.md`.
- Evidence Class: operator-waiver -- explicit release or ship-gate bypass with
  a single-line operator reason recorded in the durable report.
- Evidence Class: archive -- historical audit/report artifact that is cited as
  context, not as fresh executable proof.

## Directory Hash Limit

Directory completion artifacts are hashed recursively by stable relative path
and file hash. Keep declared directory artifacts bounded: if the directory is
large, volatile, credential-bearing, or log-like, declare a smaller manifest or
summary file instead.

## Freshness

The shared receipt inspector rejects stale commit metadata, dirty-state drift,
plan-hash drift, missing expected argv, failed commands, unsuperseded failed
attempts, zero-test receipts, and completion artifact hash drift.

For JSON staging receipts, `commit`, `dirty_state.fingerprint`, and `plan_hash`
are compared with the current checkout because the file can drift independently
from the work. For commit-footer receipts, the containing commit is the durable
source. Footer freshness therefore validates command argv/status, zero-test
guards, superseded failures, and declared artifact hashes without requiring the
embedded pre-closeout `commit` to equal the current `HEAD`.

## Parallel Drift Triage

`auto parallel` does not rewrite completed plan rows solely because receipt
freshness drifts. A completed `[x]` row represents landed queue truth; receipts
are the replayable proof trail that may need repair after rebases, regenerated
artifacts, or plan edits. When the host sees mismatch during a sync pass, it
writes `RECEIPTS-DRIFT.md` with the affected task IDs and exact missing or stale
evidence reasons, logs a warning, and leaves `IMPLEMENTATION_PLAN.md`
unchanged.

The same triage file also lists partial `[~]` rows that appear fully evidenced
as manual closeout candidates. The host does not silently promote those rows
during drift audit; promotion still belongs to an explicit landing or closeout
path that can preserve the review handoff and commit framing.
