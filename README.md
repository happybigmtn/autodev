# autodev

`autodev` is a lightweight repo-root planning and execution toolchain. It keeps the useful parts
of the old Malina workflow and drops the Fabro-centered workspace, orchestration layer, and other
legacy weight.

The local CLI command is `auto`.

## What It Owns

`auto` owns eleven commands:

- `auto corpus`
- `auto gen`
- `auto reverse`
- `auto bug`
- `auto nemesis`
- `auto loop`
- `auto qa`
- `auto qa-only`
- `auto health`
- `auto review`
- `auto ship`

It does not own the old parallel `malina run` workflow.

## Defaults

All commands resolve the git repo root automatically from the current working directory. You do not
need to pass directories in the normal case.

- Planning root defaults to `<repo>/genesis`
- Generated output defaults to `<repo>/gen-<timestamp>`
- Internal state and logs live under `<repo>/.auto/`
- Bug pipeline output defaults to `<repo>/bug`
- Nemesis audit output defaults to `<repo>/nemesis`
- `auto bug` runs MiniMax finder, Kimi skeptic/reviewer, and a final `gpt-5.4` `xhigh`
  implementation pass by default
- `auto loop` runs on the repo's primary branch by default with `gpt-5.4` and `xhigh`
- `auto qa` runs on the currently checked-out branch by default with `gpt-5.4`, `xhigh`, and the
  `standard` tier
- `auto qa-only` runs on the currently checked-out branch by default with `gpt-5.4`, `xhigh`, and
  the `standard` tier
- `auto health` runs on the currently checked-out branch by default with `gpt-5.4` and `high`
- `auto review` runs on the currently checked-out branch by default with `gpt-5.4` and `xhigh`
- `auto ship` runs on the currently checked-out branch by default with `gpt-5.4` and `xhigh`,
  targeting the repo's resolved base branch
- `auto nemesis` runs a PI audit pair by default, then a `gpt-5.4` `xhigh` implementation pass
  unless `--report-only` is used

## How To Think About The Commands

The commands form one opinionated lifecycle:

1. `auto corpus` builds a disposable understanding of the repo and folds in light strategy and DX
   thinking.
2. `auto gen` turns that understanding into durable specs and an execution queue.
3. `auto loop` burns down the execution queue one truthful task at a time.
4. `auto qa` runs runtime checks and hardens the branch with direct evidence.
5. `auto health` captures the repo-wide verification state.
6. `auto review` reviews completed work and archives only what really clears.
7. `auto ship` prepares the branch to land, updates release artifacts, and creates or refreshes a
   PR when appropriate.

The other four commands are side lanes:

- `auto reverse` documents current behavior from code reality.
- `auto bug` runs a bug-finding and implementation pipeline.
- `auto nemesis` runs a deeper audit, applies bounded hardening fixes, and appends unresolved
  findings back into specs and plan.
- `auto qa-only` runs runtime QA without fixing anything.

## Detailed Command Guide

### `auto corpus`

Purpose:

- Build a fresh disposable planning corpus under `genesis/`
- Re-understand the repo from code reality before planning
- Fold light product strategy and focus-setting into the same pass

What it reads:

- The live repository
- Existing `genesis/` only as optional historical context

What it writes:

- `genesis/IDEA.md` when `--idea` is used
- `genesis/ASSESSMENT.md`
- `genesis/SPEC.md`
- `genesis/PLANS.md`
- `genesis/GENESIS-REPORT.md`
- `genesis/DESIGN.md` when the repo has meaningful UI surfaces
- `genesis/plans/*.md`
- prompt and model logs under `.auto/logs/`

What it actually does:

- Archives the previous `genesis/` snapshot under `.auto/fresh-input/`
- Rebuilds `genesis/` from scratch
- Reviews the repo as the primary truth source
- When `--idea "..."` is supplied, first runs a non-interactive office-hours-style shaping pass
  and writes the normalized seed brief to `genesis/IDEA.md`
- Produces a corpus that includes:
  - what the repo currently is
  - what users or operators appear to need
  - what success looks like
  - what assumptions are verified vs unverified
  - what candidate directions exist
  - what is explicitly out of scope right now
- For developer-facing repos, also assesses first-run DX, onboarding honesty, error clarity, and
  whether the fastest path leads to a real success moment
- Emits stage-by-stage observability to stdout, including binary provenance, repo root, prompt log
  path, Claude phase start/finish markers, Claude PID, cwd, and elapsed timings so long runs are
  inspectable instead of silent

Idea mode:

- `auto corpus --idea "..."` is for "here is the thing we want this repo to become"
- It treats the quoted idea as intentional future direction, then reconciles it against current
  codebase reality, reusable assets, constraints, and missing pieces
- The idea is pressure-tested in an office-hours style pass: demand reality, status quo, target
  user, narrowest wedge, future-fit, assumptions, risks, and non-goals
- Missing evidence is marked as hypothesis instead of being faked
- The rest of the corpus then expands that seed into the normal `genesis/` outputs

When to run it:

- At the start of a new planning cycle
- After major drift between code and docs
- When the repo’s direction feels fuzzy and you want a fresh planning baseline

Useful flags:

- `--planning-root <dir>` to change the corpus destination
- `--idea "..."` to seed the corpus from a desired product direction or greenfield-style concept
- `--reference-repo <dir>` to require inspection of sibling or external repos as first-class
  reference inputs during corpus authoring
- `--model <name>` to pick a different Claude model
- `--max-turns <n>` to raise or lower the planning budget
- `--parallelism <n>` to encourage more or less parallel planning work
- `--dry-run` to preview without invoking the model

### `auto gen`

Purpose:

- Turn the disposable planning corpus into durable specs and an actionable execution queue

What it reads:

- `genesis/`

What it writes:

- `gen-<timestamp>/specs/*.md`
- `gen-<timestamp>/IMPLEMENTATION_PLAN.md`
- root `specs/*.md` snapshot files
- root `IMPLEMENTATION_PLAN.md`
- prompt and model logs under `.auto/logs/`

What it actually does:

- Generates fresh specs from the planning corpus
- Requires each generated spec to include:
  - `## Objective`
  - `## Acceptance Criteria`
  - `## Verification`
  - `## Evidence Status`
  - `## Open Questions`
- Refuses to accept generated specs that contradict each other on shared contracts such as message
  shapes, signature policy, or speculative future-phase behavior
- Generates a new implementation plan with dependency-ordered tasks
- Pushes the generated plan toward explicit checkpoint and decision-gate tasks after risky clusters
- Requires each active plan task to include real execution fields such as:
  - spec reference
  - why now
  - codebase evidence
  - owned surfaces
  - scope boundary
  - acceptance criteria
  - verification commands or runtime checks
  - dependencies
  - estimated scope
  - completion signal
- For developer-facing repos, treats onboarding, learn-by-doing examples, error clarity, and
  uncertainty-reducing docs/tooling as first-class planning concerns
- Merges the fresh generated plan into the repo-root `IMPLEMENTATION_PLAN.md`
- Emits first-class observability for every stage, including command header, prompt log paths,
  Claude phase start/finish markers, PID, cwd, and elapsed time per stage

Root plan merge rule:

- The newly generated plan becomes the new baseline
- Old still-open tasks that are missing from the new generated plan are appended back in
- Completed items are not preserved in the live root queue

When to run it:

- After `auto corpus`
- After refreshing planning and wanting a new working queue
- After substantial product or architectural changes that require replanning

Useful flags:

- `--planning-root <dir>` to point at a non-default corpus
- `--output-dir <dir>` to control the disposable generation output
- `--plan-only` to reuse an existing `gen-*` output and only regenerate the plan
- `--model`, `--max-turns`, and `--parallelism` to tune the generation pass

Binary provenance:

- `auto --version` prints the package version plus embedded git commit, dirty/clean status, and
  build profile so operators can confirm which binary they are actually running

### `auto reverse`

Purpose:

- Reverse-engineer durable specs from the current codebase

What it reads:

- The live repository as truth
- `genesis/` only as supporting context

What it writes:

- `gen-<timestamp>/specs/*.md`
- root `specs/*.md` snapshot files
- prompt and model logs under `.auto/logs/`

What it does not write:

- It does not rewrite root `IMPLEMENTATION_PLAN.md`

What it actually does:

- Produces specs grounded in current behavior
- Uses the same stronger spec format as `auto gen`
- Surfaces assumptions and spec/code conflicts instead of silently reconciling them
- Appends the results into the append-only snapshot-based root `specs/` directory

Spec naming rule:

- Root spec snapshots use `ddmmyy-topic-slug.md`

When to run it:

- When the code has moved and the specs are stale
- When onboarding to a repo and you want to know what it really does
- Before a review or audit that depends on truthful current-state documentation

Useful flags:

- Same as `auto gen`: `--planning-root`, `--output-dir`, `--model`, `--max-turns`,
  `--parallelism`, `--plan-only`

### `auto bug`

Purpose:

- Run a multi-pass bug-finding pipeline and optionally implement the verified fixes

What it reads:

- The tracked repository files, chunked by top-level scope
- Existing `bug/` artifacts when `--resume` is used

What it writes:

- `bug/BUG_REPORT.md`
- `bug/verified-findings.json`
- `bug/implementation-results.json`
- per-chunk prompts, raw model outputs, JSON verdicts, and markdown summaries

What it actually does:

- Splits the repo into manageable chunks
- Runs a finder pass to maximize plausible bug recall
- Runs a skeptic pass to eliminate weak or speculative findings
- Runs a verification review pass to decide what is concrete enough to fix
- Runs a final repo-wide implementation pass over the surviving findings unless `--report-only` is
  set
- Pushes truthful implementation fixes back to the current branch
- Pushes harder on believable reproduction, root-cause fixes, and regression coverage than the old
  pipeline did

Default model layout:

- finder: MiniMax `minimax/MiniMax-M2.7-highspeed` with `high`
- skeptic: Kimi with `high`
- reviewer: Kimi with `high`
- implementer: `gpt-5.4` with `xhigh`

Safety behavior:

- Checkpoints and pushes pre-existing dirty changes before a full implementation run
- Pushes model-created bug-fix commits
- May create a trailing checkpoint commit if implementation work leaves additional unstaged changes
  behind
- Skips the final implementation pass entirely when `--report-only` is set

When to run it:

- When you want a broad bug hunt with adversarial invalidation
- When you want a report with optional automatic fixes
- When the repo is too large to sensibly review as a single monolithic pass

Useful flags:

- `--chunk-size <n>` to change chunk size
- `--max-chunks <n>` to cap the run
- `--resume` to continue in-place instead of starting over
- `--report-only` to stop after the verification/reporting phases
- `--allow-dirty` to intentionally layer implementation on top of an already-dirty tree
- `--dry-run` to preview chunking without invoking models
- `--finder-model`, `--skeptic-model`, `--reviewer-model` to override the audit passes
- `--codex-bin` and `--pi-bin` to point at non-default executables

### `auto nemesis`

Purpose:

- Run a deeper audit, implement bounded hardening fixes, and feed unresolved findings back into
  repo specs and plan

What it reads:

- The live repository

What it writes:

- `nemesis/nemesis-audit.md`
- `nemesis/IMPLEMENTATION_PLAN.md`
- `nemesis/implementation-results.json`
- `nemesis/implementation-results.md`
- `nemesis/draft-nemesis-audit.md`
- `nemesis/draft-IMPLEMENTATION_PLAN.md`
- appended audit spec snapshots in root `specs/`
- appended unchecked Nemesis tasks in root `IMPLEMENTATION_PLAN.md`

What it actually does:

- Archives the previous `nemesis/` folder under `.auto/fresh-input/`
- Runs a draft audit pass to maximize evidence-backed recall
- Runs a synthesis pass to tighten or discard weak claims
- Runs a final `gpt-5.4` `xhigh` implementation pass against the synthesized Nemesis plan by
  default
- Treats the Nemesis plan as the execution contract for bounded hardening work
- Writes implementation results under `nemesis/`
- Appends only still-open unchecked Nemesis tasks back into the root plan after implementation
- Commits and pushes truthful Nemesis hardening increments plus trailing Nemesis outputs

Important rule:

- `auto nemesis` now edits repo code by default
- Use `--report-only` if you want the old audit-docs-only behavior

Backend selection:

- Draft auditor default: PI with `minimax/MiniMax-M2.7-highspeed` and `high`
- Final reviewer default: PI with `kimi-coding/k2p5` and `high`
- Final implementer default: Codex `gpt-5.4` with `xhigh`
- `--kimi` switches the draft pass to Kimi
- `--minimax` switches the draft pass to MiniMax
- `--model kimi` and `--model minimax` do the same through the generic model flag
- `--reviewer-model` and `--reviewer-effort` override the final synthesis pass
- `--fixer-model` and `--fixer-effort` override the final implementation pass

When to run it:

- When you want a stronger logic-and-invariant audit than `auto bug`
- Before or during a risky hardening cycle where you want Nemesis findings turned into real fixes
- When you want unresolved audit findings converted into durable root plan items only after
  implementation has taken its best shot

Useful flags:

- `--output-dir <dir>` to change the disposable audit destination
- `--prompt-file <path>` to override the prompt template
- `--report-only` to stop after audit and synthesis without running the implementation pass
- `--branch <name>` to require a specific checked-out branch for implementation
- `--dry-run` to preview without invoking models
- `--codex-bin` and `--pi-bin` to point at non-default executables

### `auto loop`

Purpose:

- Run the single-worker implementation loop against the repo’s live execution queue

What it reads:

- `AGENTS.md`
- `specs/*`
- `IMPLEMENTATION_PLAN.md`

What it writes:

- updates to code and tests
- `IMPLEMENTATION_PLAN.md`
- `COMPLETED.md`
- `WORKLIST.md` when useful out-of-scope follow-ups are found
- `AGENTS.md` only when operational run/build knowledge improves
- logs under `.auto/loop/` and `.auto/logs/`

What it actually does:

- Selects the branch it is allowed to operate on
- Reads the next unchecked task from the top of the plan
- Builds a short task brief from the task contract before editing
- Defaults to a RED/GREEN/REFACTOR implementation rhythm for behavior-changing work
- Implements the smallest truthful slice that fully closes the task
- Uses reproduce-first, root-cause debugging when failures appear
- Uses browser or runtime verification when the task actually needs it
- Runs a bounded simplification pass on touched code before commit when it improves clarity without
  widening scope
- Runs the verification steps required by the task
- Removes finished tasks from `IMPLEMENTATION_PLAN.md`
- Appends a completion record to `COMPLETED.md`
- Commits and pushes truthful increments to the allowed branch

Default branch resolution:

- If the current branch is `main`, `master`, or `trunk`, use it
- Otherwise try `origin/HEAD`
- Otherwise fall back to any available `main`, `master`, or `trunk`
- Use `--branch <name>` to require a specific branch instead

When to run it:

- After `auto gen`
- Whenever you want the repo to execute the next planned task
- When you want one bounded implementation worker instead of a broad audit

Useful flags:

- `--max-iterations <n>` to stop after a fixed number of completed task iterations
- `--prompt-file <path>` to override the loop prompt
- `--branch <name>` to lock the loop to a specific branch
- `--run-root <dir>` to change where loop logs are stored
- `--model` and `--reasoning-effort` to tune the worker

### `auto qa`

Purpose:

- Run a runtime QA and ship-readiness hardening pass on the current branch

What it reads:

- `AGENTS.md`
- `specs/*`
- `IMPLEMENTATION_PLAN.md`
- `COMPLETED.md`
- `REVIEW.md`
- `WORKLIST.md`
- `LEARNINGS.md`
- `QA.md` when it already exists
- `HEALTH.md` when it already exists

What it writes:

- `QA.md`
- `WORKLIST.md`
- `LEARNINGS.md`
- bounded code or test fixes when evidence supports them
- logs under `.auto/qa/` and `.auto/logs/`

What it actually does:

- Builds a QA charter from recent work, open review items, prior health signals, and actual
  runnable surfaces
- Prefers real runtime evidence over static reasoning
- Uses browser or runtime tools when they exist and the repo exposes user-facing flows
- Scores the branch before and after fixes on a 0-10 scale
- Records a ship-readiness verdict: `Ready`, `Ready with follow-ups`, or `Not ready`
- Records tested surfaces, evidence, findings, fixes landed, performance notes, and remaining
  risks in `QA.md`
- Fixes bounded high-signal problems directly when the issue is clear and worth addressing in the
  pass
- Pushes truthful QA increments back to the same branch

QA tiers:

- `quick`: focus on critical and high-severity issues first
- `standard`: cover critical, high, and medium-severity issues
- `exhaustive`: continue through polish, edge cases, and lower-severity defects when evidence
  supports them

Important branch rule:

- By default it stays on the currently checked-out branch
- `--branch <name>` can be used as a guard so the command fails if you are not on the expected
  branch

When to run it:

- After `auto loop`
- Before merging or handing work off for review
- When you want runtime evidence, not just static confidence

Useful flags:

- `--max-iterations <n>` to allow multiple QA/fix cycles in one run
- `--prompt-file <path>` to override the QA prompt
- `--branch <name>` to enforce a specific branch
- `--run-root <dir>` to change QA log location
- `--tier <quick|standard|exhaustive>` to control depth
- `--model` and `--reasoning-effort` to tune the QA worker

### `auto qa-only`

Purpose:

- Run the same runtime QA and ship-readiness workflow as `auto qa`, but in report-only mode

What it reads:

- `AGENTS.md`
- `specs/*`
- `IMPLEMENTATION_PLAN.md`
- `COMPLETED.md`
- `REVIEW.md`
- `WORKLIST.md`
- `LEARNINGS.md`
- `QA.md` when it already exists
- `HEALTH.md` when it already exists

What it writes:

- `QA.md`
- logs under `.auto/qa-only/` and `.auto/logs/`

What it actually does:

- Builds a QA charter from recent work, open review items, prior health signals, and runnable
  surfaces
- Runs runtime checks with direct evidence
- Produces a branch health score, ship-readiness verdict, severity-grouped findings, and a
  performance note
- Does not change source code, tests, build config, or docs other than `QA.md`
- Does not stage, commit, or push

When to run it:

- When you want a QA report without any fixes
- Before deciding whether a branch is worth hardening
- When you want evidence for handoff, triage, or release review

Useful flags:

- `--prompt-file <path>` to override the report-only QA prompt
- `--branch <name>` to enforce a specific branch
- `--run-root <dir>` to change QA log location
- `--tier <quick|standard|exhaustive>` to control depth
- `--model` and `--reasoning-effort` to tune the QA worker

### `auto health`

Purpose:

- Produce a repo-wide quality and verification report without fixing code

What it reads:

- `AGENTS.md`
- `specs/*`
- `IMPLEMENTATION_PLAN.md`
- `COMPLETED.md`
- `REVIEW.md`
- `WORKLIST.md`
- `LEARNINGS.md`
- `HEALTH.md` when it already exists

What it writes:

- `HEALTH.md`
- logs under `.auto/health/` and `.auto/logs/`

What it actually does:

- Detects the real validation surface from the repo: manifests, CI config, scripts, docs, and
  repo instructions
- Runs the strongest honest checks available for the repo
- Records exact commands, pass/fail status, warnings, blind spots, and partial lanes
- Scores the repo 0-10 overall
- Adds sub-scores for build, correctness, static analysis, and test confidence when those lanes
  exist
- Does not change code, stage, commit, or push

When to run it:

- Before `auto ship`
- After major refactors or dependency changes
- When you want a truthful repo-health snapshot separate from runtime QA

Useful flags:

- `--prompt-file <path>` to override the health prompt
- `--branch <name>` to enforce a specific branch
- `--run-root <dir>` to change health log location
- `--model` and `--reasoning-effort` to tune the health worker

### `auto review`

Purpose:

- Review completed work, harden what needs hardening, and archive only what truly clears

What it reads:

- `AGENTS.md`
- `specs/*`
- `IMPLEMENTATION_PLAN.md`
- `COMPLETED.md`
- `REVIEW.md`
- `ARCHIVED.md`
- `WORKLIST.md`
- `LEARNINGS.md`

What it writes:

- `REVIEW.md`
- `ARCHIVED.md`
- `WORKLIST.md`
- `LEARNINGS.md`
- bounded code or test fixes when review findings are clear and worth fixing immediately
- logs under `.auto/review/` and `.auto/logs/`

What it actually does:

- Moves current `COMPLETED.md` items into `REVIEW.md` before review starts
- Leaves `COMPLETED.md` free for new implementation work while review is happening
- Reviews each item as a claim that must be verified
- Reconstructs changed files and blast radius before clearing an item
- Reviews correctness, readability, architecture, security, trust boundaries, and performance
- Applies a bounded simplification pass on reviewed code when it clearly improves readability
  without changing behavior
- Pays extra attention to structural issues that tests often miss:
  - SQL and query safety
  - trust-boundary violations
  - unintended conditional side effects
  - stale config or migration coupling
  - blast radius wider than the touched files suggest
- Writes unresolved issues to `WORKLIST.md`
- Moves only truly cleared review items into `ARCHIVED.md`

Important queue rule:

- `auto review` does not reopen work in `IMPLEMENTATION_PLAN.md`
- Review findings become worklist items instead

When to run it:

- After a batch of `auto loop` or manual implementation work
- Before calling a group of completed items truly done
- When you want a hardening pass that focuses on regressions and review risk, not new features

Useful flags:

- `--max-iterations <n>` to allow multiple review/fix cycles
- `--prompt-file <path>` to override the review prompt
- `--branch <name>` to require a specific branch
- `--run-root <dir>` to change review log location
- `--model` and `--reasoning-effort` to tune the review worker

### `auto ship`

Purpose:

- Prepare the current branch to ship, update release artifacts, and create or refresh a PR when
  appropriate

What it reads:

- `AGENTS.md`
- `specs/*`
- `IMPLEMENTATION_PLAN.md`
- `COMPLETED.md`
- `REVIEW.md`
- `ARCHIVED.md`
- `WORKLIST.md`
- `LEARNINGS.md`
- `QA.md`
- `HEALTH.md`
- `README.md`
- `CHANGELOG.md` when it exists
- `VERSION` when it exists

What it writes:

- `SHIP.md`
- `WORKLIST.md`
- `LEARNINGS.md`
- `README.md`, `CHANGELOG.md`, `VERSION`, or other release-facing docs when they need truthful sync
- logs under `.auto/ship/` and `.auto/logs/`

What it actually does:

- Resolves the working branch and base branch
- Reviews the branch diff against the base branch
- Runs the real validations required by the repo
- Updates docs, versioning, and changelog surfaces only when warranted by what is actually shipping
- Refreshes QA or health evidence when it is missing or obviously stale
- Maintains `SHIP.md` as the durable release report for the branch
- Records rollback path, monitoring path, and rollout posture in `SHIP.md` when those surfaces
  exist
- Treats accessibility and performance checks as part of release confidence for user-facing repos
- Appends unresolved blockers and follow-ups to `WORKLIST.md`
- Commits and pushes truthful ship-prep increments
- If the current branch is not the base branch and `gh` is available, creates or refreshes a PR

Base branch resolution:

- `--base-branch <name>` wins when supplied
- Otherwise try `origin/HEAD`
- Otherwise fall back to `main`, `master`, or `trunk`
- If none can be resolved, `auto ship` fails and asks you to pass `--base-branch`

When to run it:

- After `auto qa`, `auto health`, and `auto review`
- When a branch is close to mergeable and you want release bookkeeping handled honestly
- When docs, changelog, or version surfaces need to match what is really going out

Useful flags:

- `--max-iterations <n>` to allow multiple ship/fix cycles
- `--prompt-file <path>` to override the ship prompt
- `--branch <name>` to require a specific checked-out branch
- `--base-branch <name>` to explicitly control diff and PR target
- `--run-root <dir>` to change ship log location
- `--model` and `--reasoning-effort` to tune the ship worker

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
- `IDEA.md`
- `QA.md`
- `HEALTH.md`
- `SHIP.md`
- `bug/`
- `nemesis/`

Only some are required at startup. The command will create missing files when appropriate for its
workflow.

## Runtime Requirements

- Git repository with a valid `origin`
- `claude` on `PATH` for `auto corpus`, `auto gen`, and `auto reverse`
- `codex` on `PATH` for `auto nemesis`, `auto loop`, `auto qa`, `auto qa-only`, `auto health`,
  `auto review`, and `auto ship`
- `codex` on `PATH` for any `auto bug` phase using a non-PI model
- `pi` on `PATH` for `auto bug` MiniMax/Kimi passes and both default `auto nemesis` audit passes
- `gh` on `PATH` if you want `auto ship` to create or refresh pull requests

Recommended environment:

- Claude Code with Compound Engineering installed if you want optional helpers such as `/ce:review`,
  `/ce:work`, and `/ce:compound`

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

Seed planning from a product idea:

```bash
auto corpus --idea "build X for Y user with Z constraint"
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

Run Nemesis in report-only mode:

```bash
auto nemesis --report-only
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

Run runtime QA and hardening:

```bash
auto qa
```

Run report-only QA:

```bash
auto qa-only --tier exhaustive
```

Capture a repo-health snapshot:

```bash
auto health
```

Review completed work:

```bash
auto review
```

Prepare the branch to land:

```bash
auto ship
```

## Design Goal

This repo should stay small. If a feature does not directly improve `corpus`, `gen`, `reverse`,
`bug`, `nemesis`, `loop`, `qa`, `qa-only`, `health`, `review`, or `ship`, it probably does not
belong here.
