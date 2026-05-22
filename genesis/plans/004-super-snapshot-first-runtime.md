# Make Auto Super Snapshot-First By Default

## Priority Decision

P0. This is the third implementation slice after baseline validation and generated-plan proof contracts are clear. Score: high operator value, high design clarity, high engineering leverage, direct evidence, and medium parallel executability. It outranks documentation cleanup because it decides whether `auto super` silently changes root control files or produces reviewable staging output.

## User / Operator Outcome

An operator can run `auto super` and know that generated planning output is staged for review by default. Root control ledgers change only through an explicit promotion path or an explicit flag documented by the CLI.

## Evidence

- `docs/decisions/production-control-promotion.md` says generated snapshots are reviewable and root queue truth should not be silently replaced.
- `docs/decisions/super-snapshot-promotion-default.md` says `auto super` should default to snapshot-first production planning.
- `src/super_command.rs` currently invokes `generation::run_gen` with `snapshot_only: false` and `sync_only: false`.
- `src/generation.rs` syncs generated specs and plans to root when snapshot-only is false.

## Scope Boundary

Do not change the generated corpus format. Do not remove the explicit root promotion path. Do not edit root ledgers as part of this slice except through tests. Do not resolve unrelated generated-plan validator failures here; this slice should start after plan validation is green or clearly isolated.

## Implementation Slice

Goal: make `auto super` default to snapshot-only generation and preserve an explicit promotion path.

Dependencies: plans 002 and 003 should be complete or the relevant super/generation tests should be isolated.

Files likely to modify:

- `src/super_command.rs`
- `src/generation.rs` only if the API needs a clearer helper or mode enum.
- `src/main.rs` if CLI help needs a visible flag for explicit root sync.
- Tests embedded in `src/super_command.rs`, `src/generation.rs`, and `tests/planning_primacy.rs`.

Tests to add or modify:

- A super test that proves default `auto super` generation does not call the root-sync path.
- A test that proves explicit promotion or sync-only behavior still updates root control files when requested.
- A help or dry-run assertion that makes snapshot-first behavior visible to operators.

Approach:

1. Replace boolean ambiguity with a named generation mode if the existing arguments make tests hard to read.
2. Change the `auto super` generation phase to pass snapshot-only behavior by default.
3. Preserve explicit sync-only promotion in `auto gen --sync-only --output-dir <snapshot>`.
4. Update dry-run or status text so the operator sees "snapshot" vs "root sync" plainly.
5. Add regression tests that would fail if `auto super` starts mutating root ledgers by default again.

## Verification

From the repository root:

    cargo test super_command::tests
    cargo test generation::tests
    cargo test planning_primacy
    cargo test

Expected observation: default super behavior leaves root plan files unchanged in the regression fixture, explicit sync still works, and the full suite remains green.

## Deferred

- New snapshot browser UX.
- Release process changes.
- Rewriting older genesis snapshots.
- Changing root planning primacy policy without operator approval.
