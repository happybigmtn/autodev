# autodev

`autodev` is a lightweight repo-root planning and execution toolchain. It keeps the useful parts of the old Malina workflow and drops the Fabro-centered workspace, orchestration layer, and other legacy weight.

The local CLI command is `auto`.

## What It Owns

`auto` only owns five commands:

- `auto corpus`
- `auto gen`
- `auto reverse`
- `auto nemesis`
- `auto loop`
- `auto review`

It does not own the old parallel `malina run` workflow.

## Defaults

All commands resolve the git repo root automatically from the current working directory. You do not need to pass directories in the normal case.

- Planning root defaults to `<repo>/genesis`
- Generated output defaults to `<repo>/gen-<timestamp>`
- Internal state and logs live under `<repo>/.auto/`
- Nemesis audit output defaults to `<repo>/nemesis`
- `auto loop` runs on `main` by default with `gpt-5.4` and `xhigh`
- `auto nemesis` runs on `gpt-5.4` with `high` by default
- `auto review` runs on `main` by default with `gpt-5.4` and `xhigh`

## Command Contract

### `auto corpus`

`auto corpus` builds a fresh planning corpus under `genesis/`.

Behavior:

- Reads the live repo as the primary source of truth
- Treats any existing `genesis/` as optional historical context
- Archives the previous `genesis/` snapshot under `.auto/fresh-input/`
- Destructively refreshes `genesis/`

Expected outputs:

- `genesis/ASSESSMENT.md`
- `genesis/SPEC.md`
- `genesis/PLANS.md`
- `genesis/GENESIS-REPORT.md`
- `genesis/DESIGN.md` when the repo has meaningful UI surfaces
- `genesis/plans/*.md`

### `auto reverse`

`auto reverse` reverse-engineers durable product specs from code reality.

Behavior:

- Uses the live codebase as truth
- Uses `genesis/` only as supporting context
- Writes a fresh `gen-<timestamp>/specs/`
- Appends new snapshot specs into root `specs/`
- Does not modify root `IMPLEMENTATION_PLAN.md`

Root spec filenames use this format:

- `ddmmyy-topic-slug.md`

The root `specs/` directory is snapshot-based and append-only. Existing snapshots are not reconciled in place.

### `auto gen`

`auto gen` turns the disposable planning corpus into a fresh actionable plan.

Behavior:

- Reads `genesis/`
- Writes a fresh `gen-<timestamp>/specs/`
- Writes `gen-<timestamp>/IMPLEMENTATION_PLAN.md`
- Appends new generated spec snapshots into root `specs/`
- Merges the latest generated plan into root `IMPLEMENTATION_PLAN.md`

Root plan merge rule:

- The new generated plan becomes the baseline
- Existing still-open root tasks that are not present in the new generated plan are appended back in
- Completed items are not preserved in the live root plan

This keeps the root implementation plan non-destructive for unfinished work while still letting each generation pass replace stale planning structure.

### `auto nemesis`

`auto nemesis` runs a disposable deep audit inspired by the upstream Nemesis auditor.

Behavior:

- Uses a Nemesis-style iterative audit:
  - Feynman-style logic pass
  - state inconsistency pass
  - targeted back-and-forth re-passes until convergence
- Writes disposable outputs into `nemesis/`
- Produces:
  - `nemesis/nemesis-audit.md`
  - `nemesis/IMPLEMENTATION_PLAN.md`
- Appends the generated audit spec into root `specs/`
- Appends new unchecked Nemesis tasks into root `IMPLEMENTATION_PLAN.md`
- Treats `nemesis/` as disposable and archives the previous folder under `.auto/fresh-input/` before refresh

Backend selection:

- Default: Codex with `gpt-5.4` and reasoning effort `high`
- `auto nemesis --kimi`: OpenCode with `kimi-for-coding/k2p5`
- `auto nemesis --minimax`: OpenCode with `minimax/MiniMax-M2.5`

Unlike `auto gen`, Nemesis does not replace the root implementation plan structure. It only appends new unchecked audit tasks that are not already present.

### `auto loop`

`auto loop` is the single-worker implementation loop.

Behavior:

- Runs Codex on `main`
- Reads `AGENTS.md`, `specs/*`, and `IMPLEMENTATION_PLAN.md`
- Takes the next unchecked task from the top of the plan
- Implements it fully
- Runs the required validations
- Removes completed items from `IMPLEMENTATION_PLAN.md`
- Appends a completion record to `COMPLETED.md`
- Commits and pushes truthful increments to `origin/main`
- Creates a git tag after a green increment

Default model:

- `gpt-5.4`
- reasoning effort `xhigh`

### `auto review`

`auto review` is the completed-work hardening and archival loop.

Behavior:

- Moves current `COMPLETED.md` items into `REVIEW.md` before review starts
- Leaves `COMPLETED.md` free for new implementation completions while review is running
- Uses `/ce:review` as the primary review workflow when available
- Falls back to `/review` if `/ce:review` is unavailable
- Uses `/ce:work` for follow-up implementation work
- Uses `/ce:compound` to record durable learnings in `LEARNINGS.md`
- Writes unresolved findings to `WORKLIST.md`
- Moves only truly cleared review items from `REVIEW.md` to `ARCHIVED.md`
- Commits and pushes truthful review increments to `origin/main`

Default model:

- `gpt-5.4`
- reasoning effort `xhigh`

`auto review` does not reopen work in `IMPLEMENTATION_PLAN.md`. Review findings become worklist items instead.

## Repo Files

`auto` expects or manages these repo-root files:

- `AGENTS.md`
- `specs/`
- `IMPLEMENTATION_PLAN.md`
- `COMPLETED.md`
- `REVIEW.md`
- `ARCHIVED.md`
- `WORKLIST.md`
- `LEARNINGS.md`
- `nemesis/`

Only some are required at startup. The command will create missing files when appropriate for its workflow.

## Runtime Requirements

- Git repository with a valid `origin`
- `claude` on `PATH` for `auto corpus`, `auto gen`, and `auto reverse`
- `codex` on `PATH` for `auto nemesis`, `auto loop`, and `auto review`
- `opencode` on `PATH` for `auto nemesis --kimi` and `auto nemesis --minimax`

Recommended environment:

- Claude Code with Compound Engineering installed for `/ce:review`, `/ce:work`, and `/ce:compound`

## Install

Build and install locally:

```bash
cargo install --path . --root ~/.local
```

That installs the CLI as:

```bash
~/.local/bin/auto
```

## Typical Flow

Refresh planning:

```bash
auto corpus
auto gen
```

Refresh durable specs from current code:

```bash
auto reverse
```

Run a disposable Nemesis audit:

```bash
auto nemesis
```

Use OpenCode instead:

```bash
auto nemesis --kimi
auto nemesis --minimax
```

Execute implementation work:

```bash
auto loop
```

Review completed work:

```bash
auto review
```

## Design Goal

This repo should stay small. If a feature does not directly improve `corpus`, `gen`, `reverse`, `nemesis`, `loop`, or `review`, it probably does not belong here.
