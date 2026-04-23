# autodev

`autodev` is a lightweight repo-root planning and execution toolchain. It keeps the useful parts
of the old Malina workflow and drops the Fabro-centered workspace, orchestration layer, and other
legacy weight.

The local CLI command is `auto`.

## What It Owns

`auto` owns eighteen commands:

- `auto corpus` reviews the repo and authors a fresh planning corpus under `genesis/`.
- `auto gen` generates specs and a new implementation plan from `genesis/`.
- `auto super` runs the all-in-one production-grade workflow: corpus, gen, gates, then parallel.
- `auto reverse` reverse-engineers specs from code reality using `genesis/` as supporting context.
- `auto bug` runs a chunked multi-pass bug-finding, invalidation, verification, and implementation pipeline.
- `auto loop` runs the implementation loop on the repo's primary branch.
- `auto parallel` runs the experimental multi-lane implementation executor.
- `auto qa` runs a runtime QA and ship-readiness pass on the current branch.
- `auto qa-only` runs a report-only runtime QA pass on the current branch.
- `auto health` runs a repo-wide quality and verification health report.
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
- `auto bug` runs `gpt-5.5` `high` finder, skeptic, fixer, reviewer, and finalizer
  passes by default
- `auto loop` runs on the repo's primary branch by default with `gpt-5.5` and `high`
- `auto parallel` runs on the repo's primary branch by default with five workers; outside tmux it
  launches a detached `<repo>-parallel` tmux session automatically
- `auto super` runs corpus and generation with `gpt-5.5` `xhigh`, runs additional
  production-readiness and execution-gate reviews, then launches `auto parallel` with
  `gpt-5.5` `high` workers unless `--no-execute` is supplied
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

The commands form one opinionated lifecycle:

1. `auto corpus` builds a disposable understanding of the repo and folds in light strategy and DX
   thinking.
2. `auto gen` turns that understanding into durable specs and an execution queue.
3. `auto loop` burns down the execution queue one truthful task at a time.
4. `auto qa` runs runtime checks and hardens the branch with direct evidence.
5. `auto health` captures the model-backed repo-wide verification state.
6. `auto review` reviews completed work and archives only what really clears.
7. `auto ship` prepares the branch to land, updates release artifacts, and creates or refreshes a
   PR when appropriate.

`auto super` is the high-agency composition of steps 1-3 for production-grade work: it runs
`auto corpus`, adds production-readiness review artifacts, runs `auto gen`, gates the generated
root queue, and launches `auto parallel`.

The other ten commands are side lanes:

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
- Gives the authoring and independent review passes the maximum Codex model context window
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
  reference inputs during corpus authoring and the mandatory Codex review pass
- `--model <name>` to pick a different Claude model
- `--reasoning-effort <level>` to change the Claude effort level
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
- Gives the authoring and independent review passes the maximum Codex model context window
- Uses the planning corpus for intended future direction, but treats the live codebase as
  authoritative for current-state facts such as commands, counts, metric names, filenames, and
  behavior claims
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

### `auto super`

Purpose:

- Run the all-in-one "make this repo production-grade" workflow
- Keep `auto corpus` and `auto gen` as the control primitives
- Add production-readiness review gates before allowing parallel implementation

What it reads:

- The live repository
- Existing planning docs and root queue files
- `genesis/` after the corpus stage creates it
- generated `gen-*` outputs after the generation stage creates them
- any explicit `--reference-repo <dir>` inputs

What it writes:

- `genesis/` via the normal `auto corpus` control path
- root `specs/*.md` and root `IMPLEMENTATION_PLAN.md` via the normal `auto gen` control path
- `.auto/super/<run-id>/manifest.json`
- `.auto/super/<run-id>/PRODUCTION-READINESS.md`
- `.auto/super/<run-id>/RISK-REGISTER.md`
- `.auto/super/<run-id>/QUALITY-GATES.md`
- `.auto/super/<run-id>/SYSTEM-MAP.md`
- `.auto/super/<run-id>/SUPER-REPORT.md`
- `.auto/super/<run-id>/EXECUTION-GATE.md`
- `.auto/super/<run-id>/DETERMINISTIC-GATE.json`
- `.auto/super/<run-id>/parallel/` when execution launches

What it actually does:

- Builds a production-grade focus brief from the positional prompt and optional `--focus`
- Runs `auto corpus` with GPT-5.5 `xhigh` and max Codex context
- Runs a super corpus review board across CEO/Product, Principal Engineer, Security,
  Reliability/Ops, QA/Test Architect, DX/Operator, and Release Manager perspectives
- Lets that review amend `genesis/` before generation starts
- Runs `auto gen` with GPT-5.5 `xhigh` and max Codex context
- Runs an execution-gate review that may amend root specs and `IMPLEMENTATION_PLAN.md`
- Requires `EXECUTION-GATE.md` to say exactly `Verdict: GO`
- Runs a deterministic Rust gate that rejects empty queues, missing task fields, oversized task
  scope, vague ownership, placeholders, and broad or malformed verification commands
- Launches `auto parallel` only after both the model gate and deterministic gate pass

When to run it:

- When the goal is broad production readiness rather than a narrow planning refresh
- When you want one command to generate the corpus, produce the execution queue, validate it, and
  start tmux-backed implementation
- When the repo needs a release-blocker campaign rather than a loose backlog

Useful flags:

- `auto super "make this repo production grade"` supplies the main steering prompt
- `--no-execute` stops after corpus, generation, and gates without launching workers
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

- finder: Codex `gpt-5.5` with `high`
- skeptic: Codex `gpt-5.5` with `high`
- reviewer: Codex `gpt-5.5` with `high`
- fixer: Codex `gpt-5.5` with `high`
- finalizer: Codex `gpt-5.5` with `high`
- all Codex bug phases request the maximum model context window

Profiles:

- `--profile balanced` keeps the default `gpt-5.5 high` layout.
- `--profile fast` lowers read-only discovery/review effort to `medium` unless explicitly overridden.
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

Defaults:

- Doctrine prompt defaults to `audit/DOCTRINE.md`
- Output directory defaults to `<repo>/audit`
- Resume mode defaults to `resume`
- The primary auditor defaults to Codex `gpt-5.5` with `high`
- Escalations default to Codex `gpt-5.5` with `high`
- Verdicts are `CLEAN`, `DRIFT-SMALL`, `DRIFT-LARGE`, `SLOP`, `RETIRE`, and `REFACTOR`

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
- `codex` on `PATH` for `auto corpus`, `auto gen`, `auto reverse`, `auto bug`,
  `auto nemesis`, `auto audit`, `auto loop`, `auto parallel`, `auto qa`, `auto qa-only`,
  `auto health`, `auto review`, and `auto ship`
- `codex` and `claude` logged in locally if you want `auto quota` to capture and rotate accounts
- `kimi-cli` or `pi` on `PATH` only when you explicitly select Kimi/PI legacy models
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

First success path:

```bash
export PATH="$HOME/.local/bin:$PATH"
auto --version
auto doctor
```

`auto doctor` checks the repo layout, `Cargo.toml` binary declaration, embedded version metadata,
and parseable help for `auto --help`, `auto corpus --help`, `auto gen --help`,
`auto parallel --help`, `auto quota --help`, and `auto symphony --help`. It does not call Codex,
Claude, PI, GitHub, Linear, Symphony, Docker, browser automation, tmux, network endpoints, or model
providers. Missing `codex`, `claude`, `pi`, and `gh` are capability warnings for later workflows,
not first-run failures.

CI proves the installed binary through the same surface from a temporary install root:

```bash
install_root="$RUNNER_TEMP/autodev-install-proof"
cargo install --path . --root "$install_root"
export PATH="$install_root/bin:$PATH"
auto --version
auto --help
auto corpus --help
auto gen --help
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
