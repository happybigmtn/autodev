# Snapshot-Only Generation

Date: 2026-04-23

Status: Accepted

Task: `AD-005`

Spec: `specs/230426-planning-corpus-and-generation.md`

## Context

`auto gen` and `auto reverse` share `GenerationArgs` today. The live argument
surface has `--plan-only` and `--sync-only`, but it does not have a
snapshot-only, no-sync, or spec-only flag.

Normal generation currently writes a `gen-*` output directory, verifies
generated specs and the generated `IMPLEMENTATION_PLAN.md`, optionally runs the
independent review pass, and then calls `sync_verified_generation_outputs`. That sync
copies verified generated specs into root `specs/`, rewrites generated plan
`Spec:` references to the root spec filenames, and for `auto gen` updates root
`IMPLEMENTATION_PLAN.md`.

`--sync-only` is already the explicit root-sync path for an existing generated
output directory: it verifies generated specs and the generated plan, then calls
the same sync helper. `--plan-only` is not a sync control; it reuses existing
generated specs and refreshes the generated plan before the normal sync path.

The current planning spec recommends snapshot-only generation but leaves the
product shape unresolved. This decision records the product contract only; it
does not add CLI flags, change root sync behavior, or rewrite generated or root
planning files.

## Decision

Snapshot-only generation should be a public mode on the existing generation
commands:

```text
auto gen --snapshot-only [--output-dir <gen-dir>]
auto reverse --snapshot-only [--output-dir <gen-dir>]
```

The public flag should be positive and task-oriented. It should not be named
`--no-sync` because that describes an implementation side effect instead of the
artifact contract, and it should not be a hidden internal command contract
because operators need a safe, memorable way to create reviewable snapshots
without mutating root planning surfaces.

Rejected shapes:

- `--no-sync` or `--no-sync-root`: rejected as a primary user-facing name
  because it frames the mode around a negated implementation detail. The help
  text may still say that snapshot-only performs no root sync.
- `--spec-only`: rejected because the mode must still be able to write a
  generated `IMPLEMENTATION_PLAN.md` inside the snapshot. Spec-only generation
  is a different future capability.
- A separate subcommand such as `auto snapshot`: rejected for now because the
  existing `auto gen` and `auto reverse` prompts, output layout, verification,
  and review pass already define the generation surface.
- A hidden internal-only path: rejected because the repo needs an operator-safe
  command for dry planning, reviews, and queue proposal runs.

## Root Sync Contract

Root sync is explicit through the existing `--sync-only` mode:

```text
auto gen --sync-only --output-dir <gen-dir>
auto reverse --sync-only --output-dir <gen-dir>
```

`--sync-only` should continue to require an existing output directory, verify
the generated `specs/` and `IMPLEMENTATION_PLAN.md`, and then call
`sync_verified_generation_outputs`.

For `auto gen`, explicit sync may update root `specs/` and root
`IMPLEMENTATION_PLAN.md`. For `auto reverse`, explicit sync may update root
`specs/`, but it must keep the current reverse contract that root
`IMPLEMENTATION_PLAN.md` is unchanged.

Snapshot-only generation must not call `sync_verified_generation_outputs`, must
not modify root specs, and must not modify root `IMPLEMENTATION_PLAN.md`.
`--plan-only` remains orthogonal: without snapshot-only it follows the existing
normal sync behavior, and with snapshot-only it should refresh only the plan
inside the selected generated output directory.

## Spec Path Mapping

Snapshot outputs keep generated plan references local to the output directory.
Inside `<gen-dir>/IMPLEMENTATION_PLAN.md`, task `Spec:` values should refer to
generated files as:

```text
Spec: `specs/<generated-spec-file>.md`
```

Those paths are resolved relative to `<gen-dir>`, not relative to the repo root,
while the snapshot is unsynced.

Explicit root sync maps generated spec filenames to root spec filenames using
the existing live algorithm:

1. Take each generated spec file stem.
2. Normalize the topic with `spec_topic_slug`.
3. Copy it to root `specs/<ddmmyy>-<topic-slug>.md` using the sync date.
4. Rewrite generated plan `Spec:` lines by matching the normalized topic slug to
   the root filename produced in that sync.

The contract intentionally depends on topic-slug mapping, not on preserving the
generated snapshot date. Operators should treat the synced root filename printed
by the command as the promoted durable path.

## Non-Goals

- Do not change normal `auto gen` behavior in this decision.
- Do not add a spec-only mode.
- Do not promote generated `gen-*` directories into an active planning control
  plane.
- Do not rewrite root `specs/` or root `IMPLEMENTATION_PLAN.md` during
  snapshot-only generation.
- Do not infer completion status from generated plans without the existing
  verification and review evidence gates.

## Implementation Prerequisites

The implementation task should update `GenerationArgs`, `run_generation`, help
text, and tests together. It should add regression coverage proving that
snapshot-only generation skips `sync_verified_generation_outputs`, leaves root
`specs/` unchanged, leaves root `IMPLEMENTATION_PLAN.md` unchanged, and still
verifies generated snapshot specs and generated plan references.

Any future help text should mention `--sync-only` as the explicit promotion path
for a reviewed snapshot.
