# Specification: Super Snapshot-First Runtime

## Objective

Make `auto super` stage generated planning output for review by default, with root ledger sync happening only through an explicit promotion path.

This is P0 because silent root plan mutation breaks operator sovereignty and source-of-truth clarity. It outranks historical spec cleanup because the mismatch is a live runtime call in the macro command.

## Source Of Truth

- Runtime owner modules/APIs: `src/super_command.rs::run_super`, `src/generation.rs::run_gen`, `src/generation.rs::finalize_verified_generation_outputs`, `src/state.rs` for latest output state.
- UI consumers: `auto super` stdout, `.auto/super/<run-id>/manifest.json`, `.auto/super/<run-id>/DETERMINISTIC-GATE.json`, `auto gen --snapshot-only`, `auto gen --sync-only --output-dir <gen-dir>`, README lifecycle prose.
- Generated artifacts: `gen-*/corpus/**`, `gen-*/specs/*.md`, `gen-*/IMPLEMENTATION_PLAN.md`, `.auto/super/<run-id>/**`, `.auto/state.json`.
- Retired/superseded surfaces: default `auto super` behavior that syncs generated specs and root `IMPLEMENTATION_PLAN.md` before the generated snapshot is reviewed and promoted.

## Evidence Status

Verified facts grounded in code or commands:

- `docs/decisions/super-snapshot-promotion-default.md:1-19` says `auto super` should default to snapshot-first production planning and keep `--sync-only` or explicit promotion as the durable root-sync mechanism.
- `docs/decisions/production-control-promotion.md:3-18` says root queue truth remains the production-control artifact and generated snapshots must not silently replace root queue truth.
- `src/generation.rs:478-487` rejects combining `--snapshot-only` and `--sync-only`.
- `src/generation.rs:529-533` prints snapshot and sync-only mode in generation output.
- `src/generation.rs:553-580` implements explicit `sync_only` verification and root sync for an existing output dir.
- `src/generation.rs:679-691` finalizes verified generation output with the `snapshot_only` flag.
- `src/generation.rs:762-778` saves generator state and returns no root sync summary when `snapshot_only` is true; otherwise it calls `sync_verified_generation_outputs`.
- `src/generation.rs:792-806` syncs generated specs to root, rewrites generated plan spec refs, syncs the generated plan to root for `GenerationMode::Gen`, scrubs root outputs, and saves state.
- `src/generation.rs:5411-5455` has a regression test for snapshot-only generation preserving root specs, root plan, genesis, and source files.
- Command evidence from this generation pass: `cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs -- --nocapture` exited 0.
- `src/super_command.rs:202-221` calls `generation::run_gen` with `snapshot_only: false` and `sync_only: false` during the `gen` stage.

Recommendations for the intended system:

- Change `auto super` to pass snapshot-only generation by default.
- Preserve explicit promotion through `auto gen --sync-only --output-dir <gen-dir>` or a clearly named super flag if the operator requests root sync.
- Make `auto super --dry-run` and normal stage output say whether the run is staging a snapshot or syncing root.
- Add a super-level regression test that fails if default `auto super` selects root-syncing generation again.

Hypotheses / unresolved questions:

- It is unresolved whether `auto super` should expose its own explicit promotion flag or require operators to run `auto gen --sync-only --output-dir <gen-dir>` after review.
- It is unresolved whether `.auto/super/<run-id>/manifest.json` should record the generation mode as `snapshot` vs `root-sync`.

## Runtime Contract

`src/generation.rs` owns the canonical generation mode semantics. `src/super_command.rs` may orchestrate generation, but it must not redefine what snapshot-only or sync-only means.

Default `auto super` must fail closed by leaving root specs and root `IMPLEMENTATION_PLAN.md` unchanged until the operator explicitly promotes the generated snapshot. If output state is missing or generation verification fails, super must not sync partial output to root.

## UI Contract

The operator-facing UI must clearly distinguish reviewable snapshot output from root queue truth. `auto super` stdout, manifest artifacts, README instructions, and any deterministic gate output must not imply generated rows are active until promotion.

Presentation consumers must render generation mode from runtime flags or manifest fields, not from duplicated prose.

Production UI must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; it must consume runtime helpers/generated contracts or render an explicit unavailable/error state when runtime truth is missing.

## Generated Artifacts

- `gen-*/corpus/**`
- `gen-*/specs/*.md`
- `gen-*/IMPLEMENTATION_PLAN.md`
- `.auto/state.json`
- `.auto/super/<run-id>/manifest.json`
- `.auto/super/<run-id>/CROSS-REPO-MANIFEST.json`
- `.auto/super/<run-id>/DETERMINISTIC-GATE.json`
- `.auto/super/<run-id>/EXECUTION-GATE.md`
- `.auto/super/<run-id>/SUPER-REPORT.md` and sibling super report files

Refresh and promotion commands:

```bash
auto super --no-execute
auto gen --sync-only --output-dir <gen-dir>
```

## Fixture Policy

Snapshot tests must use temp repos with synthetic root specs, root plans, genesis files, and source files. Production super runs must not import fixture snapshots or old `gen-*` directories as active root truth without explicit operator promotion.

## Retired / Superseded Surfaces

- Default `auto super` root sync before deterministic review.
- Any README or decision prose that describes generated snapshots as active doctrine before explicit promotion.
- Tests that only prove `auto gen --snapshot-only` while leaving `auto super` untested.

## Acceptance Criteria

- Default `auto super` generation leaves root `specs/*.md` unchanged.
- Default `auto super` generation leaves root `IMPLEMENTATION_PLAN.md` unchanged.
- Default `auto super` records or prints the generated snapshot path for operator review.
- Explicit promotion still syncs reviewed generated specs and plan into root.
- A regression test fails if `src/super_command.rs` passes `snapshot_only: false` in the default generation stage.
- README or help text tells operators how to promote an accepted snapshot.

## Verification

Run from `/home/r/coding/autodev`:

```bash
cargo test generation::tests::snapshot_only_generation_does_not_sync_root_outputs
cargo test super_command::tests
cargo test planning_primacy
```

Grep proof:

```bash
rg -n "snapshot_only|sync_only|run_gen|sync_verified_generation_outputs|snapshot-only|sync-only" src/super_command.rs src/generation.rs README.md docs/decisions/super-snapshot-promotion-default.md docs/decisions/production-control-promotion.md
```

## Review And Closeout

A reviewer should inspect `src/super_command.rs` and confirm the default generation stage selects snapshot-only behavior, then verify the generated output path is still discoverable from stdout/state/manifest.

Closeout must include the generation snapshot-only test plus a super-level test or grep assertion that would fail if the default super call returned to `snapshot_only: false`.

## Open Questions

- Should `auto super` have an explicit `--sync-root` flag, or should promotion remain exclusively `auto gen --sync-only --output-dir <gen-dir>`?
- Should `DETERMINISTIC-GATE.json` include a field that states whether it inspected a snapshot or root queue truth?
