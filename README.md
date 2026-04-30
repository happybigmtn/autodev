# autodev

`autodev` is a lightweight repo-root planning and execution toolchain. It keeps the useful parts
of the old Malina workflow and drops the Fabro-centered workspace, orchestration layer, and other
legacy weight.

The local CLI command is `auto`.

## What It Owns

`auto` owns twenty-one commands:

- `auto corpus` reviews the repo and authors a fresh planning corpus under `genesis/`.
- `auto gen` generates specs and a new implementation plan from `genesis/`.
- `auto spec` turns a prompt into a conformant spec plus matching implementation-plan items.
- `auto design` perfects frontend/design doctrine with runtime/UI contract and QA proof.
- `auto super` runs the all-in-one CEO production-race workflow: corpus, design gate, functional reviews, gen, gates, then parallel.
- `auto reverse` reverse-engineers specs from code reality using `genesis/` as supporting context.
- `auto bug` runs a chunked multi-pass bug-finding, invalidation, verification, and implementation pipeline.
- `auto loop` runs the implementation loop on the repo's primary branch.
- `auto parallel` runs the experimental multi-lane implementation executor.
- `auto qa` runs a runtime QA and ship-readiness pass on the current branch.
- `auto qa-only` runs a report-only runtime QA pass on the current branch.
- `auto health` runs a repo-wide quality and verification health report.
- `auto book` rewrites the audit book into a deeper navigable codebase guide.
- `auto doctor` runs a no-model first-run preflight for local layout, binary metadata, and help surfaces.
- `auto review` reviews completed work on the current branch.
- `auto steward` runs a stewardship pass for a mid-flight repo.
- `auto audit` runs a file-by-file audit against an operator-authored doctrine.
- `auto ship` prepares the current branch to ship, pushes it, and opens or refreshes a PR when appropriate.
- `auto nemesis` runs a disposable Nemesis audit and appends its outputs into root specs and plan.
- `auto quota` manages quota-aware account multiplexing for Claude and Codex.
- `auto symphony` syncs implementation-plan items into Linear and runs the local Symphony runtime.

It does not own the old parallel `malina run` workflow.

## Defaults

All commands resolve the git repo root automatically from the current working directory. You do not
need to pass directories in the normal case.

- Planning root defaults to `<repo>/genesis`
- Generated output defaults to `<repo>/gen-<timestamp>`
- Internal state and logs live under `<repo>/.auto/`
- Bug pipeline output defaults to `<repo>/bug`
- Nemesis audit output defaults to `<repo>/nemesis`
- `auto bug` runs `gpt-5.5` `low` finder and skeptic passes, then `gpt-5.5`
  `high` fixer, reviewer, and finalizer passes by default
- `auto loop` runs on the repo's primary branch by default with `gpt-5.5` and `high`
- `auto parallel` runs on the repo's primary branch by default with five workers; outside tmux it
  launches a detached `<repo>-parallel` tmux session automatically
- `auto design` runs with `gpt-5.5` `high`, writes `.auto/design/<run-id>/`, and is report-only
  unless `--apply` is supplied
- `auto super` runs corpus and generation with `gpt-5.5` `xhigh`, runs additional
  design-perfection, functional-review, production-readiness, and execution-gate reviews, then
  launches `auto parallel` with `gpt-5.5` `high` workers unless `--no-execute` is supplied
- `auto parallel status` summarizes the active tmux session, host process, lane task IDs, lane git
  state, worker PIDs, and latest lane log lines
- `auto qa` runs on the currently checked-out branch by default with `gpt-5.5`, `high`, and the
  `standard` tier
- `auto qa-only` runs on the currently checked-out branch by default with `gpt-5.5`, `high`, and
  the `standard` tier
- `auto health` runs on the currently checked-out branch by default with `gpt-5.5` and `high`
- `auto doctor` is read-only and no-model; missing `codex`, `claude`, `pi`, and `gh` are reported
  as capability warnings rather than baseline first-run failures
- `auto review` runs on the currently checked-out branch by default with `gpt-5.5` and `high`
- `auto ship` runs on the currently checked-out branch by default with `gpt-5.5` and `high`,
  targeting the repo's resolved base branch
- `auto nemesis` runs `gpt-5.5` `high` audit, synthesis, fixer, and finalizer
  passes by default unless `--report-only` is used
- All mutating branch commands (`auto loop`, `auto qa`, `auto review`, `auto ship`, `auto bug`,
  and `auto nemesis`) now rebase onto `origin/<branch>` when that remote branch exists before
  starting work and again before pushing, so remote fast-forwards do not kill long runs at the end

## How To Think About The Commands

The canonical lifecycle is the **Perfect Development Playbook** below. It is
the doctrine path for agents building projects with `autodev`, including
Autonomy-world agents generating GDP through their own software projects.

### Perfect Development Playbook

Use explicit stages instead of a hidden macro when quality matters. Each stage
accepts a model override. A MiniMax agent can set `MODEL=minimax`; a Kimi agent
can set `MODEL=kimi`; a Codex agent can leave the defaults alone.

Recommended shell setup:

```bash
MODEL="${MODEL:-gpt-5.5}"
PLAN_EFFORT="${PLAN_EFFORT:-xhigh}"
WORK_EFFORT="${WORK_EFFORT:-high}"
AUDIT_FIRST_PASS_EFFORT="${AUDIT_FIRST_PASS_EFFORT:-low}"
THREADS="${THREADS:-8}"
```

Provider notes:

- `gpt-*`, `o*`, and other Codex model names route through `codex`.
- `kimi`, `k2.6`, `k2p6`, and provider-qualified Kimi names route through
  `kimi-cli`; set `FABRO_KIMI_CLI_BIN` or `FABRO_KIMI_CLI_MODEL` when needed.
- `minimax` and `minimax/*` route through `pi`; set `FABRO_PI_BIN` when the
  binary is not on `PATH`.
- Claude remains an explicit harness for `auto loop`, `auto parallel`, and
  `auto review` via `--claude`; generation-style commands route Claude-like
  names through their Claude authoring path.

Every model-backed stage receives the Autodev Builder Ethos prompt preamble:
boil the lake when the operator asks for it, search before building, protect
user sovereignty, prefer runtime truth over presentation, and demand evidence
for claims. This is inspired by gstack `ETHOS.md`; the local copy is embedded
in code so MiniMax, Kimi, Codex, and other model-routed agents receive the same
doctrine.

#### 0. Preflight The Checkout

Run this before model-backed work:

```bash
auto doctor
git status --short
```

The checkout should have a clear branch, readable agent instructions, a sane
binary surface, and no unexplained dirty files. Dirty state is allowed only when
the next step explicitly owns it.

#### 1. Create Or Refresh The Spec

For a single prompt or product idea, start with `auto spec`:

```bash
auto spec "$PROMPT" \
  --model "$MODEL" \
  --reasoning-effort "$PLAN_EFFORT"
```

`auto spec` writes one conformant `specs/*.md` file and matching
`IMPLEMENTATION_PLAN.md` items. The generated plan must name source of truth,
runtime owners, UI consumers, generated artifacts, fixture boundaries, retired
surfaces, contract generation, cross-surface tests, and independent closeout
proof.

For a repo-wide replan or greenfield project with meaningful existing code,
create a corpus first:

```bash
auto corpus \
  --idea "$PROMPT" \
  --model "$MODEL" \
  --reasoning-effort "$PLAN_EFFORT" \
  --review-model "$MODEL" \
  --review-effort "$PLAN_EFFORT"
```

The corpus is disposable understanding under `genesis/`. It is not doctrine.
It exists so the spec and plan can be grounded in the live tree instead of a
thin manual prompt.

#### 2. Generate Durable Specs And Plan

When using a corpus, promote it into durable repo doctrine:

```bash
auto gen \
  --model "$MODEL" \
  --reasoning-effort "$PLAN_EFFORT" \
  --review-model "$MODEL" \
  --review-effort "$PLAN_EFFORT"
```

This is the standardization gate. If generated specs or plan rows omit the
required runtime/UI/generated/fixture/retire/review fields, `auto gen` fails
before workers can act on inconsistent doctrine.

Use `auto gen --snapshot-only` when you want to inspect the generated `gen-*`
output before syncing root specs and the root plan. Use `auto gen --sync-only
--output-dir <gen-dir>` to promote an accepted snapshot.

#### 3. Perfect Design And Runtime/UI Contracts

Before implementation workers touch frontend or user-facing surfaces, run the
design gate:

```bash
auto design "$PROMPT" \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT" \
  --apply
```

`auto design` reads the existing product doctrine, `DESIGN.md`, frontend code,
runtime/engine/API owners, generated bindings, and available QA surfaces. It
writes a design audit, design-system proposal, engine/UI contract, frontend QA
report, queue-ready design plan items, and a GO/NO-GO report. It rejects fake
mockups, manual frontend truth, fixture fallbacks, and pretty UI proposals that
are not wired to runtime-owned facts.

#### 4. Execute The Queue With Parallel Workers

Use `auto parallel` as the canonical executor, even when you only want one
worker:

```bash
auto parallel \
  --threads "$THREADS" \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"
```

Workers receive one dependency-ready task at a time. The host owns queue
truth, lane worktrees, checkpoint commits, landing, partial-completion
classification, and receipt-backed reconciliation.

Monitor progress from another shell:

```bash
auto parallel status
```

If a lane blocks, fix the underlying issue or update the task blocker. Do not
mark work complete from a successful compile alone; the task's acceptance
criteria, required tests, generated artifacts, and review/closeout proof must
all hold.

#### 5. Independently Review Completed Work

Run review after implementation commits land:

```bash
auto review \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"
```

`auto review` treats queue claims as suspect. It rechecks source-of-truth
ownership, generated contract sync, fixture boundaries, retired surfaces,
runtime-to-UI proof, and whether the cited validation would catch the original
failure returning.

#### 6. Runtime QA And Health

For user-facing or runtime-sensitive projects:

```bash
auto qa \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"

auto health \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"
```

Use `auto qa-only` when you need a report without fixes. Use `auto health`
when you want a broader repo health artifact after the targeted queue work is
reviewed.

#### 7. Whole-Repo Audit Before Treating The Project As Mature

For mature projects, write or update `audit/DOCTRINE.md`, then run the
professional audit:

```bash
auto audit --everything \
  --everything-threads 15 \
  --remediation-threads "$THREADS" \
  --first-pass-model "$MODEL" \
  --first-pass-effort "$AUDIT_FIRST_PASS_EFFORT" \
  --synthesis-model "$MODEL" \
  --synthesis-effort "$WORK_EFFORT" \
  --remediation-model "$MODEL" \
  --remediation-effort "$WORK_EFFORT" \
  --final-review-model "$MODEL" \
  --final-review-effort "$PLAN_EFFORT"
```

Use the legacy doctrine audit only when you specifically want per-file verdict
resolution against an existing `audit/MANIFEST.json`. The standalone closure
command is `--resolve-findings`; it remediates flagged findings, re-audits
changed files, verifies closure, and repeats until the verifier is clean or the
bounded pass limit is exhausted:

```bash
auto audit --resolve-findings \
  --model "$MODEL" \
  --reasoning-effort "$AUDIT_FIRST_PASS_EFFORT" \
  --escalation-model "$MODEL" \
  --escalation-effort "$WORK_EFFORT" \
  --resolve-passes 10
```

#### 8. Ship Only After Evidence Is Durable

When review, QA/health, and any required audit pass are green:

```bash
auto ship \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"
```

The ship step should not be used to discover whether the project is ready. It
is the final packaging and release-readiness check after the earlier gates have
already produced evidence.

### Canonical Vs Specialist Commands

The canonical end-to-end workflow is:

1. `auto doctor`
2. `auto spec` for prompt-to-spec work, or `auto corpus` then `auto gen` for
   repo-wide planning
3. `auto design` for design/runtime/UI contract proof
4. `auto parallel`
5. `auto review`
6. `auto qa` and `auto health`
7. `auto audit --everything` for mature whole-repo assurance
8. `auto ship`

Specialist commands:

- `auto reverse` is for legacy repos whose current behavior must be documented
  before planning; do not use it for normal greenfield work.
- `auto steward` is for mid-flight repos with existing planning drift; it is
  not the normal first step for a new project.
- `auto bug` and `auto nemesis` are focused hardening/audit tools. They are
  useful after the canonical path, not replacements for specs and queue truth.
- `auto book` is an audit comprehension artifact after `auto audit
  --everything`.
- `auto symphony` is an optional Linear/Symphony coordination surface.
- `auto quota` supports account routing and is not part of project doctrine.

Commands that should be demoted over time:

- `auto loop` overlaps with `auto parallel --threads 1` but has weaker host
  reconciliation. Prefer `auto parallel` for both serial and parallel work.
- `auto super` is the canonical macro only when you intentionally want a CEO
  production-race campaign. Prefer explicit stages when humans need to inspect
  each checkpoint before continuing.
- `auto qa-only` is a report-only variant of `auto qa`; keep it for audits,
  but do not treat it as a completion gate by itself.

### Older Lifecycle Summary

The commands originally formed this lifecycle:

1. `auto corpus` builds a disposable understanding of the repo and folds in light strategy and DX
   thinking.
2. `auto gen` turns that understanding into durable specs and an execution queue.
3. `auto loop` burns down the execution queue one truthful task at a time.
4. `auto qa` runs runtime checks and hardens the branch with direct evidence.
5. `auto health` captures the model-backed repo-wide verification state.
6. `auto review` reviews completed work and archives only what really clears.
7. `auto ship` prepares the branch to land, updates release artifacts, and creates or refreshes a
   PR when appropriate.

`auto super` is the high-agency composition for production-grade work: it runs
`auto corpus`, blocks on design perfection, runs functional CEO reviews, runs
`auto gen`, gates the generated root queue, and launches `auto parallel`.

The other commands are side lanes:

- `auto doctor` proves the first-run local checkout and binary surface without model credentials.
- `auto reverse` documents current behavior from code reality.
- `auto bug` runs a bug-finding and implementation pipeline.
- `auto steward` reconciles an active planning surface against the live repo and updates it in place.
- `auto audit` runs a doctrine-driven file-by-file audit and applies bounded fixes.
- `auto nemesis` runs a deeper audit, applies bounded hardening fixes, and appends unresolved
  findings back into specs and plan.
- `auto quota` manages quota-aware account routing for Codex and Claude sessions.
- `auto parallel` runs dependency-ready queue tasks across multiple tmux-backed worker lanes.
- `auto qa-only` runs runtime QA without fixing anything.
- `auto symphony` syncs implementation-plan work into Linear and runs the local Symphony runtime.

## Detailed Command Guide

### `auto corpus`

Purpose:

- Build a fresh disposable planning corpus under `genesis/`
- Re-understand the repo from code reality before planning
- Fold light product strategy and focus-setting into the same pass

What it reads:

- The live repository
- Existing `genesis/` only as optional historical context
- Existing planning standards and control docs such as `PLANS.md`,
  `plans/*.md`, `AGENTS.md`, and `CLAUDE.md`

What it writes:

- `genesis/FOCUS.md` when `--focus` is used
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
- Runs Codex `gpt-5.5` with `xhigh` by default unless you override it
- Gives Codex-backed authoring and independent review passes the maximum Codex model context
  window; Kimi and MiniMax model aliases use their provider CLI limits
- Reviews the repo as the primary truth source
- Determines the active planning surface from the repo's own instructions and
  control docs instead of assuming root-level primacy from filenames alone
- Reconciles against any already-active planning surface instead of quietly
  creating a second master plan inside `genesis/`
- Uses the repo's real agent-instruction convention in generated docs and plans; for Codex-first
  repos that means `AGENTS.md`, regardless of which planning model ran `auto corpus`
- When `--focus "..."` is supplied, writes the normalized steering brief to `genesis/FOCUS.md`
  and uses it to bias attention and plan ordering without skipping the full repo sweep
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

Focus mode:

- `auto corpus --focus "..."` is for "do the full repo-wide planning pass, but spend extra attention here"
- It treats the quoted focus as a steering signal, not a hard scope boundary
- Critical issues outside the focus should still be surfaced if they outrank the requested area
- The resulting `genesis/FOCUS.md` captures the raw focus string, normalized themes, affected
  surfaces, repo-wide checks that still mattered, and whether the focus changed priority ordering

When to run it:

- At the start of a new planning cycle
- After major drift between code and docs
- When the repo’s direction feels fuzzy and you want a fresh planning baseline

Useful flags:

- `--planning-root <dir>` to change the corpus destination
- `--focus "..."` to bias the planning pass toward specific surfaces while preserving repo-wide
  coverage
- `--idea "..."` to seed the corpus from a desired product direction or greenfield-style concept
- `--reference-repo <dir>` to require inspection of sibling or external repos as first-class
  reference inputs during corpus authoring and the mandatory independent review pass
- `--model <name>` to pick a different authoring model
- `--reasoning-effort <level>` to change the authoring effort level
- `--review-model <name>` and `--review-effort <level>` to use the same provider for the
  independent review pass
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
- Runs Codex `gpt-5.5` with `xhigh` by default unless you override it
- Gives Codex-backed authoring and independent review passes the maximum Codex model context
  window; Kimi and MiniMax model aliases use their provider CLI limits
- Uses the planning corpus for intended future direction, but treats the live codebase as
  authoritative for current-state facts such as commands, counts, metric names, filenames, and
  behavior claims
- Requires each generated spec to include:
  - `## Objective`
  - `## Source Of Truth`
  - `## Evidence Status`
  - `## Runtime Contract`
  - `## UI Contract`
  - `## Generated Artifacts`
  - `## Fixture Policy`
  - `## Retired / Superseded Surfaces`
  - `## Acceptance Criteria`
  - `## Verification`
  - `## Review And Closeout`
  - `## Open Questions`
- Refuses to accept generated specs that contradict each other on shared contracts such as message
  shapes, signature policy, or speculative future-phase behavior
- Generates a new implementation plan with dependency-ordered tasks
- Pushes the generated plan toward explicit checkpoint and decision-gate tasks after risky clusters
- Requires each active plan task to include real execution fields such as:
  - spec reference
  - why now
  - codebase evidence
  - source of truth
  - runtime owner
  - UI consumers
  - generated artifacts
  - fixture boundary
  - retired surfaces
  - owned surfaces
  - scope boundary
  - acceptance criteria
  - verification commands or runtime checks
  - contract generation
  - cross-surface tests
  - independent review/closeout proof
  - dependencies
  - estimated scope
  - completion signal
- For developer-facing repos, treats onboarding, learn-by-doing examples, error clarity, and
  uncertainty-reducing docs/tooling as first-class planning concerns
- Scrubs generated and rewritten plan `Spec:` references against the actual spec files before the
  run is accepted
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
- `--snapshot-only` to write and verify a reviewable `gen-*` snapshot without syncing root specs or
  the root `IMPLEMENTATION_PLAN.md`; promote it later with `--sync-only --output-dir <gen-dir>`
- `--model`, `--reasoning-effort`, `--max-turns`, and `--parallelism` to tune the generation pass

Binary provenance:

- `auto --version` prints the package version plus embedded git commit, dirty/clean status, and
  build profile so operators can confirm which binary they are actually running

### `auto spec`

Purpose:

- Turn a natural-language request into one conformant spec and matching plan items without relying
  on a hand-written prompt template

What it writes:

- `specs/<ddmmyy-topic-slug>.md` by default, or `--spec-path <path>`
- `IMPLEMENTATION_PLAN.md` by default, or `--plan-path <path>`
- prompt and model logs under `.auto/spec/`

What it enforces:

- The same spec sections and plan-task fields required by `auto gen`
- Runtime/API source-of-truth ownership before UI consumer work
- Generated-artifact regeneration commands when contracts change
- Fixture/demo/sample-data quarantine for production surfaces
- Retired or superseded surfaces named explicitly instead of left as active doctrine
- Cross-surface runtime-to-UI/readback proof when UI consumers are affected
- Independent review/closeout proof that can catch the original drift returning

Example:

```bash
auto spec "sync the portfolio UI with runtime-owned account balances"
```

### `auto design`

Purpose:

- Perfect product-specific frontend/design doctrine before implementation
- Tie every UI proposal to runtime/API/generated source of truth
- Find existing breaks between frontend surfaces and engine/runtime contracts
- Produce queue-ready plan items for unresolved design/runtime gaps

What it reads:

- `AGENTS.md`, product doctrine, `DESIGN.md`, specs, plans, and review history
- Frontend routes, components, styles, tokens, tests, and build/dev scripts
- Runtime/engine/API code that owns facts rendered by the UI
- Generated clients, schemas, hooks, stores, and regeneration commands

What it writes:

- `.auto/design/<run-id>/DESIGN-AUDIT.md`
- `.auto/design/<run-id>/DESIGN-SYSTEM-PROPOSAL.md`
- `.auto/design/<run-id>/ENGINE-UI-CONTRACT.md`
- `.auto/design/<run-id>/FRONTEND-QA.md`
- `.auto/design/<run-id>/DESIGN-PLAN-ITEMS.md`
- `.auto/design/<run-id>/DESIGN-REPORT.md`
- `.auto/design/<run-id>/DESIGN-RESOLVE-STATUS.md` when `--resolve` is used
- `.auto/design/<run-id>/parallel/` when `--resolve` launches implementation lanes

What it enforces:

- No fake mockups as acceptance evidence
- No duplicated frontend constants, catalogs, balances, risk classes, eligibility rules, or status
  derivations when runtime/API/generated truth exists
- No fixture/demo/sample data as production fallback truth
- Every design improvement names the runtime owner, API/schema or generator impact, UI consumer,
  and proof that would catch drift returning
- `DESIGN-REPORT.md` must end with `Verdict: GO` or `Verdict: NO-GO`
- `--resolve` must promote actionable NO-GO findings into root `IMPLEMENTATION_PLAN.md`; the
  artifact-only `DESIGN-PLAN-ITEMS.md` is not enough for execution

Example:

```bash
auto design "make the console production-ready without drifting from engine state" --apply
auto design "make the console production-ready without drifting from engine state" --resolve --threads 8
```

### `auto super`

Purpose:

- Run the all-in-one "new CEO, 14 days to production" workflow
- Keep `auto corpus` and `auto gen` as the control primitives
- Perfect design first, then apply similarly rigorous functional reviews before allowing parallel
  implementation

What it reads:

- The live repository
- Existing planning docs and root queue files
- `genesis/` after the corpus stage creates it
- `.auto/super/<run-id>/design/` after the design perfection gate creates it
- generated `gen-*` outputs after the generation stage creates them
- any explicit `--reference-repo <dir>` inputs

What it writes:

- `genesis/` via the normal `auto corpus` control path
- root `specs/*.md` and root `IMPLEMENTATION_PLAN.md` via the normal `auto gen` control path
- `.auto/super/<run-id>/manifest.json`
- `.auto/super/<run-id>/design/`
- `.auto/super/<run-id>/CEO-14-DAY-PLAN.md`
- `.auto/super/<run-id>/FUNCTIONAL-REVIEWS.md`
- `.auto/super/<run-id>/PRODUCTION-READINESS.md`
- `.auto/super/<run-id>/RISK-REGISTER.md`
- `.auto/super/<run-id>/QUALITY-GATES.md`
- `.auto/super/<run-id>/SYSTEM-MAP.md`
- `.auto/super/<run-id>/SUPER-REPORT.md`
- `.auto/super/<run-id>/EXECUTION-GATE.md`
- `.auto/super/<run-id>/DETERMINISTIC-GATE.json`
- `.auto/super/<run-id>/parallel/` when execution launches

What it actually does:

- Builds a CEO 14-day production-race focus brief from the positional prompt and optional `--focus`
- Runs `auto corpus` with GPT-5.5 `xhigh` and max Codex context
- Runs a design perfection gate that reads `DESIGN.md`, frontend code, runtime owners, generated
  bindings, and QA surfaces. By default, when execution is enabled, it can repair NO-GO design
  feedback by inserting executable design/runtime tasks into `IMPLEMENTATION_PLAN.md`, launching
  `auto parallel`, and re-running the design gate up to `--design-resolve-passes`.
- If a design repair pass leaves executable work only in `DESIGN-PLAN-ITEMS.md`, the host promotes
  missing unchecked design/runtime tasks into root `IMPLEMENTATION_PLAN.md` before launching
  workers. The artifact remains the audit trail; the root queue remains executor truth.
- Runs a CEO functional review board across Product, Design/Frontend, Architecture, Runtime/Engine,
  Security/Trust, Reliability/Ops, QA/Test, Data/Contracts, Performance, DX/Agent Workflow, and
  Release perspectives
- Lets design and functional reviews amend `genesis/` before generation starts
- Runs `auto gen` with GPT-5.5 `xhigh` and max Codex context
- Runs an execution-gate review that may amend root specs and `IMPLEMENTATION_PLAN.md`
- Requires `EXECUTION-GATE.md` to say exactly `Verdict: GO`
- Runs a deterministic Rust gate that rejects empty queues, missing task fields, oversized task
  scope, vague ownership, placeholders, and broad or malformed verification commands
- Writes cross-repo and closeout artifacts: `CROSS-REPO-MANIFEST.json`,
  `BRANCH-RECONCILIATION.md`, and `FINAL-SANITY.md`
- Launches `auto parallel` only after both the model gate and deterministic gate pass

When to run it:

- When the goal is broad production readiness rather than a narrow planning refresh
- When you want one command to generate the corpus, produce the execution queue, validate it, and
  start tmux-backed implementation
- When the repo needs a max-compute release-blocker campaign rather than a loose backlog

Useful flags:

- `auto super "make this repo production grade"` supplies the main steering prompt
- `--no-execute` stops after corpus, generation, and gates without launching workers
- `--skip-design` skips the design perfection gate; use only when an equivalent design/runtime
  review is already current and file-backed
- `--design-resolve-passes <n>` controls how many design audit/parallel/reverify rounds can run
  before the CEO campaign gives up on design/runtime integrity
- `--skip-super-review` keeps only the normal corpus/gen review controls and deterministic gate
- `--threads <n>` controls parallel worker lanes
- `--max-iterations <n>` limits successful parallel lands
- `--worker-model` and `--worker-reasoning-effort` tune implementation workers separately from
  planning

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
- Runs Codex `gpt-5.5` with `xhigh` by default unless you override it
- Uses the same stronger spec format as `auto gen`
- Surfaces assumptions and spec/code conflicts instead of silently reconciling them
- Writes the results into the root `specs/` snapshot directory and replaces same-day same-topic
  snapshots instead of accumulating `-2`, `-3`, and similar duplicates

Spec naming rule:

- Root spec snapshots use `ddmmyy-topic-slug.md`

When to run it:

- When the code has moved and the specs are stale
- When onboarding to a repo and you want to know what it really does
- Before a review or audit that depends on truthful current-state documentation

Useful flags:

- Same as `auto gen`: `--planning-root`, `--output-dir`, `--model`, `--reasoning-effort`, `--max-turns`,
  `--parallelism`, `--plan-only`, `--snapshot-only`

### `auto bug`

Purpose:

- Run a multi-pass bug-finding pipeline and optionally implement the verified fixes

What it reads:

- The tracked repository files, chunked by scope, rough token size, and static risk hints
- Existing `bug/` artifacts when `--resume` is used

What it writes:

- `bug/BUG_REPORT.md`
- `bug/verified-findings.json`
- `bug/implementation-results.json`
- `bug/pre-index.md`
- per-chunk prompts, raw model outputs, JSON verdicts, and markdown summaries

What it actually does:

- Builds a cheap static pre-index of risky files before invoking a model
- Splits the repo into manageable chunks using file count, rough token size, and static risk hints
- Runs finder, skeptic, and verification review chunk pipelines concurrently up to `--read-parallelism`
- Runs a final repo-wide implementation pass over the surviving findings unless `--report-only` is
  set
- Pushes truthful implementation fixes back to the current branch
- Rebases onto `origin/<branch>` before implementation and before pushing fixes when that remote
  branch exists, so verified-fix runs tolerate a moving remote branch
- Pushes harder on believable reproduction, root-cause fixes, and regression coverage than the old
  pipeline did
- Archives the previous `bug/` folder under `.auto/fresh-input/` before a fresh run
- Reuses existing chunk artifacts in `bug/` when `--resume` is set
- Prunes `bug/` automatically after a successful full implementation run so disposable artifacts do
  not accumulate

Default model layout:

- finder: Codex `gpt-5.5` with `low`
- skeptic: Codex `gpt-5.5` with `low`
- reviewer: Codex `gpt-5.5` with `high`
- fixer: Codex `gpt-5.5` with `high`
- finalizer: Codex `gpt-5.5` with `high`
- all Codex bug phases request the maximum model context window

Profiles:

- `--profile balanced` keeps the default layout: low finder/skeptic, high reviewer/fixer/finalizer.
- `--profile fast` keeps finder/skeptic at `low` and lowers review effort to `medium` unless explicitly overridden.
- `--profile max-quality` raises default Codex efforts to `xhigh` unless explicitly overridden.

Safety behavior:

- Checkpoints and pushes pre-existing dirty changes before a full implementation run
- Pushes model-created bug-fix commits
- May create a trailing checkpoint commit if implementation work leaves additional unstaged changes
  behind
- Skips the final implementation pass entirely when `--report-only` is set
- Isolates PI/OpenCode state under `.auto/opencode-data/`
- Bounds old `.auto/logs/` entries automatically and prunes PI snapshot/session-diff caches after
  each PI phase

When to run it:

- When you want a broad bug hunt with adversarial invalidation
- When you want a report with optional automatic fixes
- When the repo is too large to sensibly review as a single monolithic pass

Useful flags:

- `--chunk-size <n>` to change chunk size
- `--max-chunks <n>` to cap the run
- `--read-parallelism <n>` to tune concurrent read-only chunk pipelines
- `--profile <fast|balanced|max-quality>` to choose an execution preset
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

- Archives the previous `nemesis/` folder under `.auto/fresh-input/`, unless `--resume` is used
- Runs a draft audit pass to maximize evidence-backed recall
- Runs a synthesis pass to tighten or discard weak claims
- Runs a final `gpt-5.5` `high` implementation pass against the synthesized Nemesis plan by
  default
- Treats the Nemesis plan as the execution contract for bounded hardening work
- Writes implementation results under `nemesis/`
- Appends only still-open unchecked Nemesis tasks back into the root plan after implementation
- Commits and pushes truthful Nemesis hardening increments plus trailing Nemesis outputs
- Rebases onto `origin/<branch>` before implementation and before each push when that remote
  branch exists, so long Nemesis runs do not die on a non-fast-forward at the end
- `--resume` reuses valid draft, final, implementation, and finalizer artifacts and continues from the
  first missing or invalid phase

Important rule:

- `auto nemesis` now edits repo code by default
- Use `--report-only` if you want the old audit-docs-only behavior

Backend selection:

- Draft auditor default: Codex `gpt-5.5` with `high`
- Final reviewer default: Codex `gpt-5.5` with `high`
- Final implementer default: Codex `gpt-5.5` with `high`
- Finalizer default: Codex `gpt-5.5` with `high`
- all Codex Nemesis phases request the maximum model context window
- `--profile fast|balanced|max-quality` applies the same effort presets as `auto bug`
- `--kimi` switches the draft pass to the current Kimi coding model
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

### `auto quota`

Purpose:

- Manage quota-aware account routing for Codex and Claude

What it reads:

- local quota-router config and state under the OS config directory
- captured Codex and Claude profile credentials

What it writes:

- quota-router config updates
- quota-router cooldown and selection state
- active provider credentials when you explicitly select or open an account

What it actually does:

- stores multiple named account profiles per provider
- captures credentials from the currently logged-in Codex or Claude session
- shows live session and weekly usage where the upstream provider API allows it
- lets you manually choose the primary account for `codex` or `claude` with
  `auto quota select <provider>`
- honors that primary account by default, but falls through to the next candidate when the
  selected account drops below 25% session or 5h remaining
- never routes to an account with known weekly quota below 10%
- rotates on quota-exhaustion signals during quota-routed Codex and Claude executions
- exposes an `open` mode that launches the provider CLI with the currently selected account active

Useful subcommands:

- `auto quota status`
- `auto quota select <codex|claude>`
- `auto quota open <codex|claude> [args...]`
- `auto quota reset [account-name]`
- `auto quota accounts add <name> <codex|claude>`
- `auto quota accounts list`
- `auto quota accounts capture <name>`
- `auto quota accounts remove <name>`

### `auto loop`

Purpose:

- Run the implementation loop against the repo’s live execution queue with one bounded worker

What it reads:

- the essential build / validation / staging rules from `AGENTS.md`
- `specs/*`
- `IMPLEMENTATION_PLAN.md`
- sibling git repos under the same parent directory, plus any extra repos passed via
  `--reference-repo`

What it writes:

- updates to code and tests
- `IMPLEMENTATION_PLAN.md`
- `REVIEW.md` completion handoffs
- `AGENTS.md` only when operational run/build knowledge improves
- logs under `.auto/loop/` and `.auto/logs/`

What it actually does:

- Selects the branch it is allowed to operate on
- Rebases onto `origin/<branch>` before work starts when that remote branch exists, so a behind
  local branch does not fail only at push time
- Reads the next pending `- [ ]` task from the top of the plan
- Treats `- [!]` tasks in `IMPLEMENTATION_PLAN.md` as blocked and skips them during task selection
- Auto-discovers sibling git repos under the same parent directory and treats them as valid
  implementation surfaces when the task contract points there
- Merges any `--reference-repo <dir>` entries on top of that default sibling repo set
- When the repo has multiple dated specs for the same surface, treats the newest spec referenced by
  the current unchecked task as authoritative and older duplicates as historical context
- Builds a short task brief from the task contract before editing
- Defaults to a RED/GREEN/REFACTOR implementation rhythm for behavior-changing work
- Implements the smallest truthful slice that fully closes the task
- Uses reproduce-first, root-cause debugging when failures appear
- Uses browser or runtime verification when the task actually needs it
- Runs a bounded simplification pass on touched code before commit when it improves clarity without
  widening scope
- Runs the verification steps required by the task
- Prefers task-scoped and affected-surface validation over workspace-wide or package-wide sweeps
- Does not default to broad workspace validation; it only runs broad suites when the current task
  explicitly requires them or when the repo offers no narrower truthful proof
- Preserves finished tasks in `IMPLEMENTATION_PLAN.md` and marks them `- [x]`
- Appends a completion record to `REVIEW.md`
- Commits and pushes truthful increments to the allowed branch
- Treats a commit in the queue repo or any declared reference repo as real loop progress
- Fails loudly if a declared reference repo was changed but left uncommitted at the end of an
  iteration, instead of pretending nothing happened
- Rebases onto `origin/<branch>` again before each push so direct-to-primary-branch loops tolerate
  remote fast-forwards instead of dying with a raw non-fast-forward error
- Runs serially; use `auto symphony` when you want parallel orchestration across a Linear-backed queue

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
- `--cargo-build-jobs <n>` to cap each worker’s nested Cargo build fanout
- `--reference-repo <dir>` to add an external repo beyond the auto-discovered sibling repo set
- `--prompt-file <path>` to override the loop prompt
- `--branch <name>` to lock the loop to a specific branch
- `--run-root <dir>` to change where loop logs are stored
- `--model` and `--reasoning-effort` to tune the worker

Queue markers:

- `- [ ]` means pending and runnable when dependencies are satisfied
- `- [!]` means blocked and skipped by `auto loop` until you explicitly unblock or rewrite it
- `- [x]` means completed

### `auto parallel`

Purpose:

- Run dependency-ready implementation-plan tasks across multiple isolated worker lanes

What it actually does:

- Defaults to five workers with `gpt-5.5` and `high`
- Requires a clean repo before launch
- When run outside tmux, starts a detached `<repo>-parallel` tmux session running the same command
  and prints the `tmux attach` command
- Inside tmux, creates `parallel-lane-1` through `parallel-lane-N` windows that tail each lane's
  live stdout/stderr logs
- Uses the quota router for Codex workers, including the 10% weekly quota floor
- Runs a host preflight once per parallel run and injects the report into lane prompts. The
  preflight calls out common shared blockers such as missing `agent-browser`, inactive Docker
  Compose services, and unavailable explicit local regtest RPC.
- Defaults `--cargo-target auto`, which uses lane-local Cargo target directories for multi-lane
  Rust repos. This avoids cross-lane artifact contamination and Cargo-lock pileups during final
  proof. Use `--cargo-target shared` only when shared build-cache speed is worth the risk.
- Lane prompts reject `0 tests` as passing evidence and reject direct target-dir test binaries as
  final proof unless the lane just built that exact artifact from its current sources.
- Host reconciliation requires receipt-backed proof for executable `Verification:` commands. If a
  repo has executable verification but no `scripts/run-task-verification.sh`, the host leaves the
  task `[~]` instead of marking it complete from a prose handoff alone.
- Host live logs use typed result labels such as `landed-clean`, `landed-partial`,
  `landed-after-nonzero`, `landed-with-host-repair-after-nonzero`,
  `landed-partial-after-nonzero`, and `retry-needed` instead of collapsing all recovery paths into
  generic landed/non-zero lines.
- `auto parallel status` detects lane repos left in cherry-pick or rebase recovery. If a stale
  `.git/rebase-merge` directory remains after an interrupted rebase/autostash, status reports the
  exact degraded state and recovery prompts tell the worker to run `git rebase --abort`, inspect any
  autostash, remove leftover `rebase-merge` files only when metadata is incomplete, and then rebase
  or cherry-pick the task work onto the target branch.
- Lanes can mark external infrastructure failures with `AUTO_ENV_BLOCKER: <reason>`; the host logs
  those separately from code failures and retries with explicit recovery context while retries
  remain.

Useful flags:

- `--threads <n>` to set worker lanes
- `--max-iterations <n>` to stop after a fixed number of successful lands
- `--cargo-build-jobs <n>` to cap each worker's nested Cargo build fanout
- `--cargo-target auto|lane|shared|none` to control worker `CARGO_TARGET_DIR` layout
- `--branch <name>` to lock the executor to a specific branch
- `--run-root <dir>` to change where parallel logs are stored
- `status` to print current host/tmux/lane health without starting new work

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
- Rebases onto `origin/<branch>` before QA starts and again before each push when that remote
  branch exists, so long QA passes tolerate remote fast-forwards

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
- sibling git repos under the same parent directory, plus any extra repos passed via
  `--reference-repo`

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
- Auto-discovers sibling git repos under the same parent directory and treats them as valid review
  and fix surfaces when the reviewed item points there
- Merges any `--reference-repo <dir>` entries on top of that default sibling repo set
- Reviews each item as a claim that must be verified
- Handles mixed `REVIEW.md` queues that contain both `## TASK` sections and top-level
  ``- `TASK`: ...`` backfill bullets, including multi-ID bullets split across
  continuation lines
- Skips an unchanged stale batch for the rest of the current run, then continues with later
  queued items instead of looping forever on one blocked prefix
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
- Treats a commit in the queue repo or any additional listed repo as real review progress
- Fails loudly if an additional listed repo was changed but left uncommitted at the end of an
  iteration, instead of pretending nothing happened
- Rebases onto `origin/<branch>` before review starts and again before each push when that remote
  branch exists, so long review passes tolerate remote fast-forwards

Important queue rule:

- `auto review` does not reopen work in `IMPLEMENTATION_PLAN.md`
- Review findings become worklist items instead

When to run it:

- After a batch of `auto loop` or manual implementation work
- Before calling a group of completed items truly done
- When you want a hardening pass that focuses on regressions and review risk, not new features

Useful flags:

- `--max-iterations <n>` to allow multiple review/fix cycles
- `--reference-repo <dir>` to add an external repo beyond the auto-discovered sibling repo set
- `--prompt-file <path>` to override the review prompt
- `--branch <name>` to require a specific branch
- `--run-root <dir>` to change review log location
- `--model` and `--reasoning-effort` to tune the review worker

### `auto steward`

Purpose:

- Replace `auto corpus` + `auto gen` for repos that already have an active planning surface
- Prefer this over a fresh corpus/gen cycle when the repo already has durable queue/spec artifacts
  and needs reconciliation against live code
- Run a two-pass Codex stewardship flow: first write drift / hinge / retire / hazard artifacts,
  then optionally review and apply bounded plan/spec promotions

What it reads:

- The live repository
- Existing planning-surface files detected from `IMPLEMENTATION_PLAN.md`, `REVIEW.md`,
  `SECURITY_PLAN.md`, `WORKLIST.md`, `LEARNINGS.md`, `ARCHIVED.md`, `AGENTS.md`, `CLAUDE.md`,
  and `PLANS.md`
- Any extra sibling or explicit `--reference-repo <dir>` inputs you pass

What it produces:

- `steward/DRIFT.md`
- `steward/HINGES.md`
- `steward/RETIRE.md`
- `steward/HAZARDS.md`
- `steward/STEWARDSHIP-REPORT.md`
- `steward/PROMOTIONS.md`
- prompt logs under `.auto/logs/steward-*-prompt.md`

Defaults:

- Output directory defaults to `<repo>/steward`
- The first steward pass defaults to Codex `gpt-5.5` with `high`
- The finalizer pass defaults to Codex `gpt-5.5` with `high`
- It uses the current checked-out branch unless you pass `--branch`
- It runs through `codex` unless you override `--codex-bin`
- `--skip-finalizer` leaves the six stewardship deliverables in place without the review/apply
  pass

### `auto audit`

Purpose:

- Audit tracked files one by one against an operator-authored doctrine
- Prefer this over `auto bug` or `auto nemesis` when the review standard is a specific
  repo-authored doctrine rather than open-ended defect discovery
- Keep clean files untouched, patch bounded drift or slop, and escalate larger retire/refactor
  calls with a durable artifact trail

What it reads:

- `audit/DOCTRINE.md` by default, or `--doctrine-prompt <path>`
- The bundled audit rubric shipped in the binary
- Tracked files that match the default include globs or the `--paths` / `--exclude` filters you
  supply

What it produces:

- `audit/MANIFEST.json`
- `audit/files/<hash-prefix>/prompt.md`
- `audit/files/<hash-prefix>/response.log`
- `audit/files/<hash-prefix>/verdict.json`
- `audit/files/<hash-prefix>/patch.diff` for patchable verdicts
- `audit/files/<hash-prefix>/worklist-entry.md` or `audit/files/<hash-prefix>/retire-reason.md`
  for escalated verdicts
- `audit/FINDING-VERIFY.{md,json}` when run with `--verify-findings`; this is the closure gate
  that independently checks every `DRIFT-LARGE`, `DRIFT-SMALL`, `REFACTOR`, `RETIRE`,
  apply-failed, or escalated manifest entry before declaring the audit findings closed

Defaults:

- Doctrine prompt defaults to `audit/DOCTRINE.md`
- Output directory defaults to `<repo>/audit`
- Resume mode defaults to `resume`
- The primary auditor defaults to Codex `gpt-5.5` with `low`
- Escalations default to Codex `gpt-5.5` with `high`
- Verdicts are `CLEAN`, `DRIFT-SMALL`, `DRIFT-LARGE`, `SLOP`, `RETIRE`, and `REFACTOR`

Closure verification:

- Prefer `auto audit --resolve-findings` for remediation closeout. It runs parallel
  remediation lanes, re-audits only changed flagged files, runs `--verify-findings`, and repeats
  up to `--resolve-passes` times, defaulting to 10. If the drift-only re-audit exits non-zero
  because findings remain, the resolver records that as retry-needed and continues the next pass
  instead of stopping the whole closeout early.
- Use `auto audit --resume-mode only-drifted` manually only when you have already made
  remediation edits outside the resolver and need to refresh changed entries against the same
  manifest.
- `auto audit --verify-findings` fails with `NO-GO` until every flagged finding is either
  re-audited out of the manifest's significant verdict set or removed from the current tree.
  `StillOpen` means the last audited significant verdict is still current for that file;
  `NeedsReaudit` means the file changed after the finding and must be re-audited before closure
  can be trusted.

#### `auto audit --everything`

Purpose:

- Run a professional whole-codebase audit with one fresh Codex loop per tracked file
- Start with context engineering: a dedicated audit checkout (or the current checkout with
  `--everything-in-place`), revised `AGENTS.md`, revised `ARCHITECTURE.md`, and injected
  `doctrine/` content when present
- Produce comprehensive markdown reports split by logical crate/module group, then revise those
  reports based on cross-file relationships before attempting bounded crate-by-crate remediation

What it does:

- Creates a resumable run under `.auto/audit-everything/<run-id>`
- Creates an audit worktree on `auto-audit/<repo>-<run-id>` from the primary branch
  unless `--everything-in-place` is set
- Writes human reports under `audit/everything/<run-id>` in the audit worktree
- Writes and injects `GSTACK-SKILL-POLICY.md` so every worker gets deterministic, phase-aware
  gstack lenses instead of deciding ad hoc which skills to consider
- Writes and injects `CODEBASE-IMPROVEMENT-POLICY.md` so the audit treats orphaned code,
  deprecated paths, accumulated technical debt, weak architecture, and AI-slop as first-class
  remediation targets
- Runs first-pass per-file analysis with Codex `gpt-5.5` `low`
- Runs synthesis with Codex `gpt-5.5` `high`
- Generates `REMEDIATION-PLAN.md` / `REMEDIATION-PLAN.json` from the synthesized reports, with
  dependency-ready tasks and lane ownership
- Runs remediation as isolated worktree lanes with Codex `gpt-5.5` `high`, then host-lands lane
  commits back onto the audit branch
- Runs final review with Codex `gpt-5.5` `xhigh`
- If final review writes `Verdict: NO-GO` with actionable required blockers, runs a bounded
  repair pass and reruns final review before merge judgment
- For non-report runs, successful completion requires both a `Verdict: GO` final review and a
  completed file-quality gate where every tracked file rerates at least 9/10. `--no-everything-merge`
  skips only the primary-branch merge; it does not bypass final-review or file-quality closeout.
- Writes `CODEBASE-BOOK/` as the final human-readable codebase explanation: a chaptered,
  first-principles tour organized by the repository's logical architecture. The expected standard
  is a Feynman-style technical walkthrough that teaches a smart junior developer the important
  crates/files, runtime flows, state boundaries, validation posture, and production risks before
  they open the raw source.
- Keeps `reports/` as durable evidence behind the book. The book is the readable map; the
  reports are the group-by-group audit trail used by remediation and final review. Bulky
  first-pass mirrors under `audit/everything/<run-id>/files/**` remain transient by default and
  should not be committed unless an operator explicitly asks to preserve them.
- Use `auto book` after a completed audit when the first generated book is too high-level. It
  rewrites only the narrative `CODEBASE-BOOK/` chapters from the last audit corpus using Codex's
  maximum context window, preserves appendix/file-catalog walkthroughs byte-for-byte, and runs a
  quality review against the "deep codebase substitute for a junior developer" standard.
- Attempts a fast-forward merge back to the resolved primary branch only after final review writes
  `Verdict: GO`, unless `--no-everything-merge` is set
- After a successful merge, refreshes `RUN-STATUS.md` with an explicit final-status note and
  lands that status refresh so the committed run artifact records merge completion. Exact branch
  heads can still move after the status commit, so `git rev-parse` remains the source of truth for
  current commit IDs.
- `--everything-in-place` runs against the current checkout, requires a clean checkout for a new
  run, writes reports directly under that checkout, and marks merge complete once the final
  `Verdict: GO` artifacts are committed because changes are already in place

Skill policy:

- First-pass prompts inject only the selected compact lenses for that file's surface and forbid
  direct tool invocation, keeping one-file loops clean. They also require orphan/deprecation,
  AI-slop, deletion-candidate, architecture-smell, and behavior-preservation findings.
- Synthesis and remediation-lane prompts inject the selected group lenses; direct browser, QA,
  benchmark, devex, release, or documentation checks are allowed only when the lane surface and
  report recommendations call for them. Group reports include a debt register whose classes are
  `safe_delete`, `deprecated_remove`, `consolidate`, `simplify`, `deepen_module`, and
  `leave_with_reason`.
- Remediation lanes may delete, retire, consolidate, simplify, or deepen modules when the group
  report supplies proof. Required deletion proof includes references/imports/exports, entrypoints,
  config/docs/generated bindings, API/CLI/operator/runtime impact, and narrow validation or
  behavior characterization.
- Final review injects review, CSO, health, QA-only, benchmark, devex, docs, ship,
  land-and-deploy, canary, careful, and checkpoint lenses before judging merge readiness
- Final review must include an evidence-class checklist that distinguishes local static/build/unit
  validation, generated contract/binding validation, browser QA, deployment/canary health, live
  production or mainnet/on-chain proof, external-owner proof, and documentation/status integrity.
  Local, fixture, regtest, or synthetic proof must not be counted as live production proof.
- Final review must also include a deletion/refactor proof checklist and reject deletions or
  architectural rewrites that cannot show no product substance was lost.

Useful controls:

- `--everything-phase init-context|first-pass|synthesize|plan-remediation|remediate|final-review|merge|status|all`
- `--everything-run-id <id>` to resume a specific run
- `--everything-in-place` to use the current checkout instead of creating the canonical audit
  worktree
- `--everything-threads <n>` for read-only parallel phases, capped at 15
- `--remediation-threads <n>` for isolated remediation lanes, defaulting to 5 and capped at 10
- `--final-review-retries <n>` to control the bounded NO-GO repair/rerun loop, defaulting to 1
- `--report-only` to stop before remediation
- `--branch trunk|main` when the primary branch cannot be inferred

#### `auto book`

Purpose:

- Rewrite the latest professional audit's `CODEBASE-BOOK/` into a detailed narrative walkthrough
  without rerunning file analysis, synthesis, remediation, or final review
- Use the last audit's `reports/`, `FINAL-REVIEW.md`, `REMEDIATION-PLAN.md`, `RUN-STATUS.md`,
  existing appendix/catalog files, and referenced first-pass artifacts as the source corpus
- Teach the codebase Feynman-style: first principles, concrete runtime/data/control-flow
  walkthroughs, key crates/files and functions, validation evidence, production risks, and plain
  explanations suitable for a junior developer who has not read the source yet

What it does:

- Defaults to the run recorded in `.auto/audit-everything/latest-run`, falling back to the newest
  directory under `audit/everything/`
- Invokes Codex through the max-context execution path (`model_context_window=1000000`)
- Preserves existing appendix/catalog markdown files byte-for-byte while rewriting narrative book
  chapters
- Writes logs under `.auto/book/<run-id>/<timestamp>/`
- Runs a second max-context quality review and writes `CODEBASE-BOOK/BOOK-QUALITY-REVIEW.md`
  with `Verdict: PASS` or `Verdict: NO-GO`; the command exits nonzero on NO-GO

Useful controls:

- `--audit-run-id <id>` to target a specific audit run
- `--audit-root <path>` when the audit artifacts live somewhere other than `audit/everything`
- `--output-dir <path>` to write the book somewhere else
- `--model <model>` / `--reasoning-effort <effort>` / `--codex-bin <path>`
- `--dry-run` to print the book prompt without invoking Codex
- `--skip-quality-review` for a faster rewrite when you want to inspect the book yourself

### `auto symphony`

Purpose:

- Bridge `IMPLEMENTATION_PLAN.md` into a Linear project and launch the local Symphony runtime
- Prefer this over `auto loop` or `auto parallel` when Linear is the coordination surface for the
  implementation queue
- Keep the repo's plan, rendered workflow, and Linear issue state aligned instead of drifting
  apart

What it reads:

- `IMPLEMENTATION_PLAN.md`
- Existing `.auto/symphony/WORKFLOW.md` when resolving saved project configuration
- Git remote metadata plus repo defaults needed to render the workflow
- Linear project state via the configured API credentials

What it produces:

- `.auto/symphony/WORKFLOW.md` by default from `auto symphony workflow`
- `.auto/symphony/sync-planner-prompt.md`,
  `.auto/symphony/sync-planner-response.jsonl`, and
  `.auto/symphony/sync-planner-result.json` when `auto symphony sync` uses the AI planner
- `.auto/symphony/logs/log/symphony.log` when `auto symphony run` launches the local runtime
- Linear issue creates, updates, reopenings, and terminal-state sync from `auto symphony sync`

Subcommands:

- `sync` reads unchecked implementation-plan items and syncs them into a Linear project
- `workflow` renders the repo-specific `.auto/symphony/WORKFLOW.md`
- `run` refreshes the workflow and launches the local Symphony dashboard in the foreground

Defaults:

- `sync` defaults `--todo-state` to `Todo`, planner model to `gpt-5.5`, planner effort to
  `high`, and planner binary to `codex`
- `workflow` defaults to `.auto/symphony/WORKFLOW.md`, `max_concurrent_agents = 1`,
  `poll_interval_ms = 5000`, model `gpt-5.5`, effort `high`, `In Progress`, and `Done`
- `run` reuses the workflow defaults, can `--sync-first` before launch, and writes logs under
  `.auto/symphony/logs/` unless you override `--logs-root`

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
- Runs a mechanical release gate before the Codex ship-prep pass and fails early when installed
  binary proof, validation receipts, fresh `QA.md`/`HEALTH.md`, release blockers, rollback notes,
  monitoring notes, or PR/no-PR state are missing or red
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
- Rebases onto `origin/<branch>` before ship work starts and again before each push when that
  remote branch exists, so release-prep runs tolerate remote fast-forwards
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
- `--bypass-release-gate <reason>` to let the Codex ship-prep pass run despite missing local
  release evidence; the reason and current gate blockers are recorded in `SHIP.md`
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
- `codex` on `PATH` for Codex-backed runs of `auto corpus`, `auto gen`, `auto spec`,
  `auto reverse`, `auto bug`, `auto nemesis`, `auto audit`, `auto loop`, `auto parallel`,
  `auto qa`, `auto qa-only`, `auto health`, `auto review`, and `auto ship`
- `codex` and `claude` logged in locally if you want `auto quota` to capture and rotate accounts
- `kimi-cli` on `PATH` when `--model kimi`, `--model k2.6`, or another Kimi alias is selected
- `pi` on `PATH` when `--model minimax` or another MiniMax alias is selected
- `gh` on `PATH` if you want `auto ship` to create or refresh pull requests

Recommended environment:

- Claude Code with Compound Engineering installed if you want optional helpers such as `/ce:review`,
  `/ce:work`, and `/ce:compound`

## Install

Build and install locally:

```bash
cargo install --path . --locked --root ~/.local
```

That installs the CLI as:

```bash
~/.local/bin/auto
```

First success path:

```bash
export PATH="$HOME/.local/bin:$PATH"
auto --version
auto doctor
```

`auto doctor` checks the current git checkout, embedded version metadata, and parseable help for
`auto --help`, `auto corpus --help`, `auto gen --help`, `auto design --help`,
`auto super --help`, `auto parallel --help`, `auto quota --help`, and
`auto symphony --help`. In the `autodev` source checkout it also checks the strict `Cargo.toml`
package and `auto` binary declaration. In other project checkouts it requires repo-local agent
instructions such as `AGENTS.md`, `CLAUDE.md`, or `.github/copilot-instructions.md` so model-backed
work starts from explicit local guidance. It does not call Codex, Claude, PI, GitHub, Linear,
Symphony, Docker, browser automation, tmux, network endpoints, or model providers. Missing
`codex`, `claude`, `pi`, and `gh` are capability warnings for later workflows, not first-run
failures.

CI proves the installed binary through the same surface from a temporary install root:

```bash
install_root="$RUNNER_TEMP/autodev-install-proof"
cargo install --path . --locked --root "$install_root"
export PATH="$install_root/bin:$PATH"
auto --version
auto --help
auto corpus --help
auto gen --help
auto design --help
auto super --help
auto parallel --help
auto quota --help
auto symphony --help
```

## Typical Flow

Refresh planning:

```bash
auto corpus
auto gen
```

Run the all-in-one production-grade campaign and launch parallel workers after gates pass:

```bash
auto super "make this repo production grade"
```

Run the same campaign but stop before execution:

```bash
auto super "make this repo production grade" --no-execute
```

Seed planning from a product idea:

```bash
auto corpus --idea "build X for Y user with Z constraint"
auto gen
```

Refresh planning with a full sweep but extra attention on specific surfaces:

```bash
auto corpus --focus "wire reconnects, TLS failures, session-token handling, and player/runtime parity"
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
auto bug --profile max-quality --read-parallelism 8
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

Inspect or switch quota-routed accounts:

```bash
auto quota status
auto quota select codex
auto quota select claude
```

Prepare the branch to land:

```bash
auto ship
```

## Design Goal

This repo should stay small. If a feature does not directly improve `corpus`, `gen`, `reverse`,
`bug`, `loop`, `parallel`, `qa`, `qa-only`, `health`, `review`, `steward`, `audit`, `ship`,
`nemesis`, `quota`, or `symphony`, it probably does not belong here.
