# Verification Receipt Schema

Receipts are execution evidence, not notes. `scripts/run-task-verification.sh`
owns receipt creation; agents and lane workers must not hand-edit receipt JSON.

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
