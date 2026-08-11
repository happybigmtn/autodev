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
dirty-state metadata. Before creating that footer, the host writes
`.auto/parallel/verified-source/<TASK>.json` only after host verification
passes. This local attestation binds the task, exact proof payload, command set,
the task-owned-inputs-v2 fingerprint, and a content-addressed source-state
fingerprint. `source_state_v2` binds the
root index identity and working tree plus every initialized submodule's gitlink
index object, checked-out HEAD, child index, tracked and untracked contents,
filesystem modes, and symlink targets recursively. The collector uses direct
Git index/path enumeration, so `diff.ignoreSubmodules`,
`submodule.<name>.ignore`, and `core.fileMode` cannot hide source drift.
Tracked, content-bound per-directory `.gitignore` files remain authoritative
for declared build outputs, but mutable `.git/info/exclude`,
`core.excludesFile`, and untracked `.gitignore` files cannot hide source.
Mutable host queue files remain excluded only at the root, allowing the scoped
closeout transition without allowing source drift. Plan input, Git inventories,
paths, metadata, and file contents share one streaming collection budget.
Verification receipts always use this exact source fingerprint. A narrower
workspace-baseline optimization is available for deterministic post-plan hooks
that refresh a JSON string containing the SHA-256 of a host queue file. The
repository tracks `.autodev-source-state.json`, for example:

```json
{
  "version": 1,
  "queue_sha256": [
    {
      "target_path": "evidence/document-inventory.json",
      "target_pointer": "/documents/3/sha256",
      "source_path": "PLAN.md"
    }
  ]
}
```

This policy does not exempt the field from receipt freshness. During the one
host hook invocation immediately preceding the definition-of-done gates,
Autodev verifies that the old value equals the digest of the `HEAD` queue file,
the new value equals the digest of the worktree queue file, and the hook reports
the target path. It then compares fingerprints that replace only the unique raw
64-character digest token while preserving every other byte in the JSON. Reuse
requires the exact pre-hook fingerprint to equal the fully-green baseline, the
exact post-hook fingerprint to equal the current tree, and the normalized
pre/post fingerprints to match. The proof is in-memory and landing-scoped.

The config, source, and target must be regular tracked files. Sources are
restricted to host queue files; targets are bounded to 16 MiB; rules must be
sorted and unique; missing or non-string pointers, duplicate raw digest values,
unsafe paths, unreported targets, hash mismatches, and any sibling/source drift
fail closed to an ordinary workspace probe.
The public host-attestation and footer-generation paths apply that budget
before parsing their plan, receipt, or verified-source-attestation inputs and
carry it through freshness HEAD/status/diff collection and source hashing; an
oversized subprocess is killed and waited before the path fails closed.
Receipt freshness and inspection likewise report any unavailable or bounded-out
current `HEAD`, dirty state, plan input, or source-state collection as an
explicit stale-evidence problem; absence never skips the corresponding check.
Collection fails closed before draining an oversized inventory on a submodule
cycle, an uninitialized or misdirected gitlink, nesting deeper than 8, more
than 200,000 state entries, or more than 1 GiB of inventory/content.
The footer embeds the fingerprint as `source_state_v2`; project-owned staging
receipt schemas do not need to accept the field. Attestation schema version 2
is historical-only; version 3 is required for new host verification because it
also binds `task_owned_inputs_v2` and, when the command is statically
cacheable, the shared verification-input closure. Version-1 attestations and
`source_state_v1` footers are likewise historical-only and require host
re-execution. Readers prefer reachable commit footers and keep JSON receipts as
a compatibility/staging fallback.

## Required Metadata

- `commit`: current `HEAD` for the checkout that ran the command.
- `dirty_state.fingerprint`: `autodev-dirty-state-v2`, a content-sensitive
  fingerprint of porcelain state, staged and unstaged binary diffs, and
  untracked file paths, modes, and contents when the command ran. Mutable host
  queue files and `.auto/` runtime state are excluded so closeout bookkeeping
  cannot invalidate verified source.
- `plan_hash`: SHA-256 of the active `IMPLEMENTATION_PLAN.md`.
- `commands[].command`: exact command text from the task row.
- `commands[].expected_argv`: shell-split argv for the expected command.
- `commands[].exit_code` and `commands[].status`: command result.
  Older repo-local wrappers that wrote `exit_status` are accepted as a
  compatibility alias for `exit_code`, but new receipts should emit
  `exit_code`.
- `declared_artifacts[].path` and `declared_artifacts[].sha256`: hash evidence
  for declared completion artifacts when the task row requires them. Older
  repo-local wrappers that wrote `completion_artifacts` are accepted as a
  compatibility alias.

## Evidence Classes

- Evidence Class: executable -- wrapper-backed local command proof with fresh
  metadata and matching expected argv.
- Evidence Class: external -- live, credentialed, deploy, or operator-system
  proof that cannot be replayed locally; must be named in `REVIEW.md`.
- Evidence Class: operator-waiver -- explicit release or ship-gate bypass with
  a single-line operator reason recorded in the durable report.
- Evidence Class: archive -- historical audit/report artifact that is cited as
  context, not as fresh executable proof.

## Task-Owned Inputs Fingerprint (`task-owned-inputs-v2`)

When `auto parallel` stamps a task's closeout-commit footer it also embeds a
versioned per-task input fingerprint under the JSON key `task_owned_inputs_v2`.
This is the finer of two drift gates. The whole-repo drift-sweep fingerprint
(HEAD + `git status`) is the first, cheap gate: if nothing in the tree changed
at all, the sweep is skipped. When that global signal *does* change, the
per-task fingerprint decides, row by row, whether a completed `[x]` task must
re-verify.

The fingerprint hashes only what a task's verification can legitimately depend
on: (a) the task's normalized contract (its plan-row markdown with the status
checkbox neutralized, so a `[x]`/`[~]` flip is never a change), (b) its `Owns:`
paths, (c) its direct dependency task IDs, and (d) the union of each direct
dependency's `Owns:` paths plus their non-receipt completion-artifact paths. The
task's own declared completion-artifact paths are content-addressed too so a
declared-artifact drift is never silently trusted. All paths are content-
addressed via git enumeration (tracked + untracked, respecting `.gitignore`).
V2 uses working-tree lstat data, hashes regular-file bytes and symlink target
text separately, and reads the actual executable mode rather than the index
mode. Therefore file names, contents, executable/symlink modes, deletions, untracked files,
refs, and submodule gitlink commits all fold in, while unrelated repo paths are
absent from the hash.

Legacy `task_owned_inputs_v1` fields remain readable but cannot match a V2
recomputation. Their next drift sweep therefore re-runs verification and stamps
V2 proof instead of silently upgrading historical evidence.

## Derived-path footer

Host closeout commits may carry
`Auto-Verification-Receipt-Derived-Paths: ["path", ...]` when a trusted
repository hook deterministically refreshes tracked artifacts during queue
reconciliation. The value is a compact JSON array of sorted, unique,
repo-relative canonical paths. Absolute paths, parent traversal, duplicates,
and unsorted values are rejected.

This field records a host claim; it is not independent proof that arbitrary
source was generated. Only the host closeout path may stamp it, and readers
accept the exact stamped set in addition to the static host queue paths. Lane
workers cannot authorize paths through their task commit. Historical backfill
must not invent this footer: rerun the verification and host closeout path when
the original host provenance is unavailable.

Semantics during a drift sweep:

- Stamped fingerprint still matches the recomputed one -> the task's own inputs
  are unchanged; trust the receipt and skip re-verification even though the
  global tree moved.
- Stamped fingerprint changed -> the task's own inputs moved; re-verify even if
  the footer otherwise looks fresh. This also closes the legacy gap where footer
  and ancestor-commit receipts skip whole-tree dirty/plan freshness and could
  otherwise miss source drift outside a receipt's declared artifacts.
- No stamped fingerprint (legacy receipt) -> fall back to the pre-existing
  evidence-freshness behavior. Legacy receipts are never retroactively
  invalidated; the field applies going forward.
- Any git/hash failure computing the fingerprint is treated as "changed" (force
  re-verify), never as "unchanged".

When a completed row lacks a current durable footer, a drift sweep may still
have rerun its commands successfully and written a version-3 host
verified-source attestation. On later global `HEAD` movement, AutoDev may reuse
that local result only to avoid executing the same commands again, and only
when the attestation's exact receipt proof payload, command set,
`task_owned_inputs_v2`, directly invoked repository files (including ignored
files and nested static shell commands), host verification runner, and Cargo
workspace verification inputs all still match. Direct inputs are read with the
same 200,000-entry/1-GiB fail-closed bounds. Dynamic or compound shell payloads
and direct symlinks are not cacheable. This does not make staging JSON durable
authority: the row remains eligible for `RECEIPTS-DRIFT.md` until a normal
closeout path runs the complete definition-of-done gates and commits a footer.
Changes to owned/dependency source, declared artifacts, verification commands,
the normalized task contract, directly invoked scripts, the host runner, or
Cargo build/test inputs invalidate the local cache and rerun proof. If the
input closure cannot be computed, AutoDev reruns proof rather than reusing it.

### `[sweep-excluded]`

A task may carry a discoverable `[sweep-excluded]` marker anywhere in its plan
row (conventionally in the `Verification:` block). Once the task is `[x]` with a
valid receipt, a periodic drift sweep never re-runs its (often expensive)
verification on mere staleness, legacy-ness, or a hash error — only a definitive
owned-inputs fingerprint mismatch (its own inputs changed) or a forced full
re-verify re-runs it.

### Forced full re-verify

Setting `AUTO_PARALLEL_FORCE_FULL_REVERIFY` to a non-empty, non-`0` value
bypasses both fingerprints and re-verifies every completed row (a deliberate
full audit).

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
guards, superseded failures, declared artifact hashes, and the embedded
`source_state_v2` without requiring the embedded pre-closeout `commit` to equal
the current `HEAD`. A legacy footer with only `source_state_v1` (or no
source-state field) remains historical audit evidence, but it cannot authorize
a new `Done` transition; host re-execution must create a fresh version-3
verified-source attestation and footer.

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
