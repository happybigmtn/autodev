# autodev

`autodev` is a lightweight repo-root planning and execution toolchain. It keeps the useful parts of the old Malina workflow and drops the Fabro-centered workspace, orchestration layer, and other legacy weight.

The local CLI command is `auto`.

## What It Owns

`auto` only owns seven commands:

- `auto corpus`
- `auto gen`
- `auto reverse`
- `auto bug`
- `auto nemesis`
- `auto loop`
- `auto review`

It does not own the old parallel `malina run` workflow.

## Defaults

All commands resolve the git repo root automatically from the current working directory. You do not need to pass directories in the normal case.

- Planning root defaults to `<repo>/genesis`
- Generated output defaults to `<repo>/gen-<timestamp>`
- Internal state and logs live under `<repo>/.auto/`
- Bug pipeline output defaults to `<repo>/bug`
- Nemesis audit output defaults to `<repo>/nemesis`
- `auto bug` runs MiniMax finder, Kimi skeptic/reviewer, and a final `gpt-5.4` `xhigh` implementation pass by default
- `auto loop` runs on the repo's primary branch by default with `gpt-5.4` and `xhigh`
- `auto nemesis` runs a PI-only two-stage audit by default: MiniMax draft pass, then Kimi synthesis pass
- `auto review` runs on the currently checked-out branch by default with `gpt-5.4` and `xhigh`

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
- Requires each generated spec to include a `## Acceptance Criteria` section with concrete bullets

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
- Requires each generated spec to include a `## Acceptance Criteria` section with concrete bullets

Root plan merge rule:

- The new generated plan becomes the baseline
- Existing still-open root tasks that are not present in the new generated plan are appended back in
- Completed items are not preserved in the live root plan

This keeps the root implementation plan non-destructive for unfinished work while still letting each generation pass replace stale planning structure.

### `auto nemesis`

`auto nemesis` runs a disposable deep audit inspired by the upstream Nemesis auditor.

Behavior:

- Uses a Nemesis-style iterative audit:
  - initial draft pass to maximize evidence-backed recall
  - final synthesis pass to discard weak claims and tighten the surviving work
  - each pass still performs the internal Feynman/state back-and-forth method from the prompt
- Writes disposable outputs into `nemesis/`
- Produces:
  - `nemesis/nemesis-audit.md`
  - `nemesis/IMPLEMENTATION_PLAN.md`
  - `nemesis/draft-nemesis-audit.md`
  - `nemesis/draft-IMPLEMENTATION_PLAN.md`
- Appends the generated audit spec into root `specs/`
- Appends new unchecked Nemesis tasks into root `IMPLEMENTATION_PLAN.md`
- Treats `nemesis/` as disposable and archives the previous folder under `.auto/fresh-input/` before refresh

Backend selection:

- Draft auditor default: PI with `minimax/MiniMax-M2.7-highspeed` and `high`
- Final reviewer default: PI with `kimi-coding/k2p5` and `high`
- `auto nemesis --kimi`: uses Kimi for the draft auditor pass
- `auto nemesis --minimax`: uses MiniMax for the draft auditor pass
- `auto nemesis --model kimi`: same as `--kimi`
- `auto nemesis --model minimax`: same as `--minimax`
- `--reviewer-model <model>` and `--reviewer-effort <level>` override the final synthesis pass

Code-writing rule:

- `auto nemesis` is audit-docs only. The model passes are not used for repo code edits.

Unlike `auto gen`, Nemesis does not replace the root implementation plan structure. It only appends new unchecked audit tasks that are not already present.

### `auto bug`

`auto bug` runs a single chunked multi-pass bug pipeline over the repo.

Behavior:

- Splits tracked repo files into manageable chunks by top-level scope
- Runs three per-chunk audit passes plus one repo-wide implementation pass by default:
  - MiniMax finder
  - Kimi skeptic
  - Kimi verification review
  - final `gpt-5.4` `xhigh` implementation over all verified findings
- Streams parsed model output live for both Codex and PI backends
- Writes durable artifacts under `bug/`:
  - per-chunk prompts, raw model responses, JSON verdicts, and markdown summaries
  - `bug/BUG_REPORT.md`
  - `bug/verified-findings.json`
  - `bug/implementation-results.json`
- Archives the previous `bug/` folder under `.auto/fresh-input/` before refresh

Safety:

- Full implementation mode checkpoints and pushes pre-existing dirty changes on the current branch before the bug pipeline starts
- Use `--report-only` to stop after verification and summary generation
- Use `--allow-dirty` if you intentionally want the final implementation pass to layer on top of an already-dirty tree without the startup checkpoint

Default pass layout:

- finder: `minimax/MiniMax-M2.7-highspeed` with `high`
- skeptic: `kimi` with `high`
- reviewer: `kimi` with `high`
- implementer: `gpt-5.4` with `xhigh`

Model routing:

- MiniMax aliases resolve to `minimax/MiniMax-M2.7-highspeed` and Kimi aliases resolve to `kimi-coding/k2p5`
- Any other model name uses Codex
- The implementation pass is pinned to `gpt-5.4` with `xhigh`

Useful flags:

- `--chunk-size <n>` to change the per-chunk file budget
- `--max-chunks <n>` to cap the run
- `--report-only` to skip the final implementation pass
- `--dry-run` to preview the chunk plan

### `auto loop`

`auto loop` is the single-worker implementation loop.

Behavior:

- Runs Codex on the checked-out primary branch alias: `main`, `master`, or `trunk`
- Reads `AGENTS.md`, `specs/*`, and `IMPLEMENTATION_PLAN.md`
- Takes the next unchecked task from the top of the plan
- Implements it fully
- Runs the required validations
- Removes completed items from `IMPLEMENTATION_PLAN.md`
- Appends a completion record to `COMPLETED.md`
- Commits and pushes truthful increments to the repo's primary branch
- Creates a git tag after a green increment
- Automatically creates and pushes checkpoint commits when the worker leaves repo changes behind

Default branch:

- If the current branch is `main`, `master`, or `trunk`, `auto loop` runs there
- Otherwise it tries `origin/HEAD`
- Then it falls back to any available branch named `main`, `master`, or `trunk`
- Use `--branch <name>` to override explicitly

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
- Commits and pushes truthful review increments back to the current branch
- Automatically creates and pushes checkpoint commits when the worker leaves repo changes behind

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
- `codex` on `PATH` for any `auto bug` phase using a non-PI model
- `pi` on `PATH` for `auto bug` MiniMax/Kimi passes and both default `auto nemesis` audit passes

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

Run the multi-pass bug pipeline:

```bash
auto bug
```

Preview chunking or run report-only:

```bash
auto bug --dry-run
auto bug --report-only
```

Use PI instead:

```bash
auto nemesis --kimi
auto nemesis --minimax
auto nemesis --model kimi
auto nemesis --model minimax
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
