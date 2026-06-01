# Canonical Development Operating Manual

Status: canonical v0.1

Owner: `autodev`

Scope: every repo, every machine, every agent-assisted development run.

This manual defines the operating system for development work. The README
explains commands. This file defines when to run them, what they produce, what
blocks progress, and how gbrain stays current.

## Control Model

| Layer | Role | Owns | Does not own |
|---|---|---|---|
| gbrain | Durable memory and artifact index | project pages, decisions, run closeouts, artifact links, lessons, source indexes | scheduling, branch mutation, completion truth |
| Hermes | Operator cockpit | Telegram commands, cron, concise summaries, invoking `auto` in repos | canonical memory, task completion |
| autodev | Workflow kernel | planning, generation, design gates, execution, review, QA, logs, receipts | private account authority, long-term memory |
| Codex | Primary worker | repo analysis, planning, implementation, testing, review, closeout writing | durable memory by chat history |
| Claude | Critic only unless explicitly assigned | design/product-plan critique, UX/design plan quality gates | implementation, repo execution, market/account authority |
| GitHub | Transport | branch and PR transport | run memory, artifact memory, canonical task truth |

## Global Rules

1. The live checkout plus promoted repo docs are current-state truth.
2. gbrain is durable advisory memory and closeout memory.
3. `genesis/` and `gen-*` are generated planning artifacts. They are not active
   queue truth until promoted.
4. Root `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, promoted `specs/*.md`, and
   landed commits are active repo truth.
5. `.auto/` is local run state until a closeout uploads or links it in gbrain.
6. One product delta uses one working branch. Do not create branch forests as
   memory.
7. Every executable task names source of truth, runtime owner, UI consumers,
   generated artifacts, fixture boundary, verification, tests, and closeout
   proof.
8. A successful compile is not completion. Completion requires the task's
   declared proof plus independent closeout.
9. Every run ends with a gbrain closeout page or an explicit blocker page.
10. The next run starts by reading gbrain through `GBRAIN-CONTEXT.md`; no manual
    copy/paste context packs.

## Machine Bootstrap

Run this once per machine and again after credential or gbrain changes.

```bash
command -v auto
command -v gbrain
command -v codex
command -v claude
gbrain doctor --fast
auto doctor
```

Canonical shared-memory mode is a shared gbrain backend or remote MCP. Local
PGLite is a single-machine brain or disposable per-worktree code index, not the
cross-machine durable source of truth. Multi-machine development uses one shared
project/campaign page namespace and one source id per repo.

Register each repo once with the same source id everywhere:

```bash
REPO=/absolute/path/to/repo
REPO_SLUG=$(basename "$REPO")
cd "$REPO"
gbrain sources add "$REPO_SLUG" --path "$REPO" --federated || true
gbrain sources attach "$REPO_SLUG" || true
gbrain sources list
```

Install a persistent sync path on the machine that owns indexing:

```bash
gbrain autopilot --install --repo "$REPO"
```

If autopilot is unavailable, use a scheduler with the gbrain live-sync primitive:

```bash
gbrain sync --repo "$REPO" && gbrain embed --stale
```

Humans do not run per-run sync/upload chores in the steady state. Hermes/autodev
may run a one-shot sync after writing a closeout, but the canonical path is
persistent sync on the indexing owner plus a shared backend for durable memory.

## Repository Bootstrap

Run this at the start of work in any repo:

```bash
cd "$REPO"
auto doctor
git status --short --branch
gbrain search "$(basename "$PWD") current priority" --limit 8
```

If gbrain has no project page, create one before planning:

```bash
REPO_SLUG=$(basename "$PWD")
mkdir -p .auto
cat > .auto/project-page.md <<EOF
---
type: project
repo: $REPO_SLUG
source_id: $REPO_SLUG
local_path: $PWD
created: $(date -u +%F)
---

# $REPO_SLUG

## Current Priority

- Establish repo baseline.

## Canonical Workflow

- Use autodev canonical workflow.

## Active Artifacts

- none

## Latest Closeout

- none
EOF
gbrain put "projects/$REPO_SLUG" < .auto/project-page.md
gbrain tag "projects/$REPO_SLUG" project
gbrain tag "projects/$REPO_SLUG" "$REPO_SLUG"
```

## Workflow Selector

| Situation | Workflow |
|---|---|
| Small, well-scoped product change | Workflow A: Prompt To Spec |
| Repo direction is unclear or stale | Workflow B: Repo-Wide Corpus And Generation |
| UI, TUI, CLI, report, dashboard, or operator surface changes | Workflow C: Design Gate |
| Queue already exists and is dependency-ready | Workflow D: Execute Queue |
| Work landed and needs verification | Workflow E: Review, QA, Health |
| Run is ending | Workflow F: GBrain Closeout |
| Telegram/Hermes command is driving | Workflow G: Hermes One-Command Loop |

## Workflow A: Prompt To Spec

Use this for one bounded feature, bug, or refactor.

Inputs:

- repo path
- operator prompt
- model settings

Commands:

```bash
cd "$REPO"
export PROMPT="<operator intent>"
export MODEL="${MODEL:-gpt-5.5}"
export PLAN_EFFORT="${PLAN_EFFORT:-xhigh}"

auto doctor
auto spec "$PROMPT" \
  --model "$MODEL" \
  --reasoning-effort "$PLAN_EFFORT"
```

Required outputs:

- `specs/*.md`
- root `IMPLEMENTATION_PLAN.md`
- prompt/model logs under `.auto/spec/`

Gate:

- Continue only when the generated task rows contain source of truth, runtime
  owner, UI consumers, generated artifacts, fixture boundary, verification,
  required tests, and review/closeout fields.

Next command:

- If a UI/TUI/operator surface is touched: run Workflow C.
- Otherwise run Workflow D.

## Workflow B: Repo-Wide Corpus And Generation

Use this when the repo needs a broad planning pass or the operator intent spans
multiple surfaces.

Inputs:

- repo path
- operator intent
- optional focus

Commands:

```bash
cd "$REPO"
export PROMPT="<operator intent>"
export FOCUS="${FOCUS:-repo-wide production improvement}"
export MODEL="${MODEL:-gpt-5.5}"
export PLAN_EFFORT="${PLAN_EFFORT:-xhigh}"

auto doctor
auto corpus \
  --idea "$PROMPT" \
  --focus "$FOCUS" \
  --model "$MODEL" \
  --reasoning-effort "$PLAN_EFFORT" \
  --review-model "$MODEL" \
  --review-effort "$PLAN_EFFORT"

auto gen \
  --snapshot-only \
  --model "$MODEL" \
  --reasoning-effort "$PLAN_EFFORT" \
  --review-model "$MODEL" \
  --review-effort "$PLAN_EFFORT"
```

Required outputs:

- `genesis/GBRAIN-CONTEXT.md`
- `genesis/GENESIS-REPORT.md`
- `gen-*/GBRAIN-CONTEXT.md`
- `gen-*/IMPLEMENTATION_PLAN.md`
- `.auto/logs/*`

Gate:

- Inspect `gen-*/IMPLEMENTATION_PLAN.md`.
- Promote only one accepted snapshot.

Promotion:

```bash
auto gen --sync-only --output-dir <gen-dir>
```

Next command:

- If UI/TUI/operator surface work exists: run Workflow C.
- Otherwise run Workflow D.

## Workflow C: Design Gate

Use this before implementation when the work changes a frontend, TUI, CLI,
dashboard, report, or operator workflow.

Commands:

```bash
cd "$REPO"
export PROMPT="<operator intent>"
export MODEL="${MODEL:-gpt-5.5}"
export WORK_EFFORT="${WORK_EFFORT:-high}"

auto design "$PROMPT" \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT" \
  --apply
```

When `DESIGN-REPORT.md` ends with `Verdict: NO-GO`, repair through the design
resolver:

```bash
auto design "$PROMPT" \
  --resolve \
  --threads "${THREADS:-5}" \
  --worker-model "$MODEL" \
  --worker-reasoning-effort "$WORK_EFFORT"
```

Required outputs:

- `.auto/design/<run-id>/DESIGN-AUDIT.md`
- `.auto/design/<run-id>/DESIGN-SYSTEM-PROPOSAL.md`
- `.auto/design/<run-id>/ENGINE-UI-CONTRACT.md`
- `.auto/design/<run-id>/FRONTEND-QA.md`
- `.auto/design/<run-id>/DESIGN-PLAN-ITEMS.md`
- `.auto/design/<run-id>/DESIGN-REPORT.md`

TUI acceptance:

- terminal plates for `80x24`, `120x32`, and `160x48` when the app can render
  at those sizes
- deterministic headless render or buffer snapshot
- cell-level geometry/style assertions for important components
- keyboard/input-state coverage
- animation-frame proof for TachyonFX or equivalent post-render effects
- reusable layout/theme/component contracts, not one-off screenshot polish

Gate:

- Continue to implementation only on `Verdict: GO`, or after `--resolve` has
  promoted dependency-ready `DESIGN-*` tasks into root `IMPLEMENTATION_PLAN.md`.

## Workflow D: Execute Queue

Use this for implementation. `auto parallel` is the executor even with one
worker.

Commands:

```bash
cd "$REPO"
export MODEL="${MODEL:-gpt-5.5}"
export WORK_EFFORT="${WORK_EFFORT:-high}"
export THREADS="${THREADS:-8}"

auto parallel \
  --threads "$THREADS" \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"
```

Monitor:

```bash
auto parallel status
```

Required outputs:

- local commits for completed tasks
- `.auto/parallel/*` logs and receipts
- updated root queue state

Gate:

- Completed tasks have commits, declared verification, declared artifacts, and
  closeout evidence.
- Partial tasks remain `[~]` with the exact blocker.
- Failed lanes leave logs and do not silently become completed work.

## Workflow E: Review, QA, Health

Run this after implementation lands into the working branch.

Commands:

```bash
cd "$REPO"
export MODEL="${MODEL:-gpt-5.5}"
export WORK_EFFORT="${WORK_EFFORT:-high}"

auto review \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"
```

For user-facing or runtime-sensitive repos:

```bash
auto qa \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"

auto health \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"
```

Required outputs:

- review artifact
- QA artifact when applicable
- health artifact when applicable
- clean or intentionally documented `git status --short`

Gate:

- Review has no unresolved blocker.
- QA has no unresolved user-facing blocker.
- Health findings are either fixed or recorded as follow-on tasks.

## Workflow F: GBrain Closeout

Run this at the end of every meaningful planning, execution, or review run.
This is an orchestrator responsibility, not an operator ritual. Until autodev
has a first class `auto closeout` command, the shell block below is the
implementation template Hermes/autodev should execute automatically.

Inputs:

- repo slug
- run id
- branch
- commit
- plan path
- design artifact path when applicable
- execution log/artifact paths
- tests run
- lessons
- next command

Orchestrator commands:

```bash
cd "$REPO"
REPO_SLUG=$(basename "$PWD")
RUN_ID="${RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
BRANCH=$(git branch --show-current)
COMMIT=$(git rev-parse --short HEAD)
CLOSEOUT_DIR=".auto/closeout/$RUN_ID"
CLOSEOUT_MD="$CLOSEOUT_DIR/closeout.md"
CLOSEOUT_SLUG="closeouts/$REPO_SLUG-$RUN_ID"
mkdir -p "$CLOSEOUT_DIR"

cat > "$CLOSEOUT_MD" <<EOF
---
type: closeout
repo: $REPO_SLUG
run_id: $RUN_ID
branch: $BRANCH
commit: $COMMIT
created: $(date -u +%F)
---

# $REPO_SLUG Closeout $RUN_ID

## Operator Intent

<paste or summarize intent>

## Workflow Run

- Preflight:
- Corpus:
- Generation:
- Design:
- Execution:
- Review:
- QA/health:

## Artifacts

- GBrain context:
- Generated plan:
- Design artifact:
- Execution logs:
- Review/QA/health:

## Tests Run

- <command> -- <result>

## Landed Delta

- Branch: $BRANCH
- Commit: $COMMIT
- Summary:

## Lessons Learned

- 

## Next Command

\`\`\`bash
<exact next command>
\`\`\`
EOF

gbrain put "$CLOSEOUT_SLUG" < "$CLOSEOUT_MD"
gbrain tag "$CLOSEOUT_SLUG" closeout
gbrain tag "$CLOSEOUT_SLUG" "$REPO_SLUG"
gbrain timeline-add "$CLOSEOUT_SLUG" "$(date -u +%F)" "$REPO_SLUG closeout $RUN_ID at $COMMIT"
# Optional repo-index refresh. The closeout already exists after `gbrain put`.
gbrain sync --source "$REPO_SLUG" || true
```

Upload durable artifact snapshots only when links are not enough, such as when
the artifact is outside the repo, likely to be overwritten, or needed for audit
without the checkout. Hermes/autodev owns this loop; operators should not copy
files by hand.

```bash
for artifact in \
  genesis/GBRAIN-CONTEXT.md \
  gen-*/GBRAIN-CONTEXT.md \
  gen-*/IMPLEMENTATION_PLAN.md \
  .auto/design/*/DESIGN-REPORT.md \
  .auto/logs/* \
  .auto/parallel/* \
  .auto/closeout/"$RUN_ID"/*
do
  [ -e "$artifact" ] || continue
  [ -f "$artifact" ] || continue
  gbrain files upload-raw "$artifact" --page "$CLOSEOUT_SLUG" || true
done
```

Gate:

- `gbrain get "$CLOSEOUT_SLUG"` returns the page.
- The page names the next command.
- A future `auto corpus` or `auto gen` can discover it through
  `GBRAIN-CONTEXT.md`.

## Workflow G: Hermes One-Command Loop

Hermes exposes one operator command:

```text
pilot-dev <repo-slug> "<operator intent>"
```

Canonical wrapper source:

```bash
scripts/pilot-dev
```

Installed orchestrator path:

```bash
/usr/local/bin/pilot-dev -> /home/dev/.local/bin/pilot-dev
```

Hermes resolves `<repo-slug>` to a repo path through gbrain project metadata,
then invokes the wrapper from the repo or `/srv/dev/repos` context. The wrapper
is the one-command contract; Hermes should not reimplement its shell in
Telegram handlers.

Required wrapper planning entrypoint:

```bash
auto pilot <repo-slug> "<operator intent>" \
  --planning-only \
  --base-dir "$PILOT_BASE_DIR" \
  --run-id "$RUN_ID" \
  --run-root "$RUN_ROOT" \
  --autodev-source "$PILOT_AUTODEV_SOURCE"
```

`pilot-dev` runs this typed planning path by default through
`PILOT_TYPED_PREFLIGHT=1` and `PILOT_TYPED_PLANNING=1`. The typed planning path
includes preflight, command-surface capture, doctor logs, gbrain context,
`auto corpus`, `auto gen --snapshot-only`, and optional `auto steward
--report-only` according to `PILOT_PLANNING_MODE` and
`PILOT_REQUIRE_PLANNING_SPINE`. If typed planning succeeds, the wrapper skips
legacy shell duplicate collection and planning execution. If it fails and
`PILOT_TYPED_PREFLIGHT_REQUIRED=1` or `PILOT_TYPED_PLANNING_REQUIRED=1`, the
wrapper exits with an orchestration failure.

Required typed preflight artifacts:

- `run.env`
- `pilot-preflight.json`
- `pilot-landing.json`
- `orchestrator-doctor.log`
- `doctor.log`
- `logs/auto-version.log`
- `logs/auto-help.log`
- `logs/auto-command-surface.json`
- `logs/auto-help-<command>.log`
- `autodev-command-selection.json`
- `autodev-command-selection.md`
- `pilot-planning.json` when `--planning-only` runs
- `pilot-execution.json` when `--execution-manifest-only` runs
- `gbrain/*.md`
- optional `plan-input.md`

No-Codex smoke proof:

```bash
PILOT_RUN_ID=wrapper-proof \
PILOT_PLANNING_MODE=none \
PILOT_REQUIRE_PLANNING_SPINE=0 \
PILOT_WRAPPER_PREFLIGHT_ONLY=1 \
PILOT_TYPED_PREFLIGHT_REQUIRED=1 \
PILOT_TYPED_PLANNING_REQUIRED=1 \
pilot-dev <repo-slug> "<operator intent>"
```

The smoke proof must create `pilot-preflight.json`, `pilot-landing.json`,
`pilot-planning.json`, `pilot-execution.json`, typed command-surface logs, and
gbrain context, then exit without `codex/codex-exec.jsonl`.

Default policy for `pilot-dev`: run typed planning first, then launch Codex only
after the planning artifact exists. Normal production pilots use:

- `auto doctor`
- `auto corpus --idea "$PROMPT" --focus "$FOCUS"`
- `auto gen --snapshot-only`
- `auto steward --report-only` when the repo already has active planning
  surfaces

For UI/TUI/operator surfaces, the worker must run or justify skipping
`auto design`. Claude may critique design/product plans only; Codex owns repo
analysis, planning, execution, review, and closeout.

Execution contract:

```bash
auto pilot <repo-slug> "<operator intent>" \
  --execution-manifest-only \
  --run-id "$RUN_ID" \
  --run-root "$RUN_ROOT"
```

`pilot-dev` runs this typed execution contract by default through
`PILOT_TYPED_EXECUTION_MANIFEST=1` before launching Codex. The command writes
`pilot-execution.json` with pending fields for selected task, executor,
verification, git, runtime restart policy, artifacts, and Telegram summary.
Codex must read `pilot-landing.json` before execution and treat it as the git
landing contract: normal repos require a successful `git push --dry-run origin
HEAD` probe, while no-origin repos must be explicitly marked local-only and must
not attempt remote git commands.
Codex must update the file before and after execution with the deterministic
helper, not by hand-editing JSON:

```bash
auto pilot <repo-slug> "<operator intent>" \
  --execution-update-only \
  --run-id "$RUN_ID" \
  --run-root "$RUN_ROOT" \
  --execution-status blocked \
  --no-task-reason "no safe execution slice selected" \
  --executor-kind none \
  --executor-reason "stopped before execution" \
  --verification-summary "planning and closeout gates only" \
  --no-commit-reason "no repository code changed" \
  --runtime-no-restart-reason "no runtime code changed" \
  --summary-branch-commit "main@no-commit" \
  --summary-plan "$RUN_ROOT/pilot-planning.json" \
  --summary-design "none" \
  --summary-execution "$RUN_ROOT/pilot-execution.json" \
  --summary-tests "not run; blocked before execution" \
  --summary-closeout "$RUN_ROOT/pilot-closeout.json" \
  --summary-next "auto pilot --closeout-only ..."
```

For successful execution, use `--execution-status executed`, provide
`--task-id`, `--executor-command`, at least one `--verification-command` or
`--verification-receipt`, `--git-commit` or `--no-commit-reason`, and runtime
restart evidence or a no-restart reason. A successful closeout cannot leave
status as `pending`, executor as `UNDECIDED`, verification empty,
commit/no-commit evidence absent, runtime restart/no-restart evidence absent,
or Telegram summary fields blank.

Task-state finalization:

```bash
auto pilot <repo-slug> "<operator intent>" \
  --task-finalize-only \
  --run-id "$RUN_ID" \
  --run-root "$RUN_ROOT" \
  --task-finalize-status done \
  --task-finalize-commit \
  --task-finalize-push
```

`pilot-dev` runs this automatically after Codex exits successfully and
`pilot-execution.json` says `executed` or `degraded` with a concrete
`selected_task.id`. The default policy is:

- `executed` finalizes the source-plan task as `done`
- `degraded` finalizes the source-plan task as `partial`
- commit the source-plan checklist change
- push the finalization commit only when the execution manifest already says
  the implementation commit was pushed and the repo has a normal origin

Use these environment controls only when a campaign has an explicit reason to
deviate:

- `PILOT_TYPED_TASK_FINALIZE=0` disables the wrapper-owned finalizer
- `PILOT_TASK_FINALIZE_STATUS=done|partial|auto` overrides status mapping
- `PILOT_TASK_FINALIZE_COMMIT=0` avoids the plan-state commit
- `PILOT_TASK_FINALIZE_PUSH=0|1|auto` controls the push policy

The worker should not hand-edit checklist state for a selected task after
execution. It must keep `selected_task.source_plan` accurate, and then let the
wrapper write `task-finalize.json` before closeout. If task finalization fails,
the run is an orchestration failure, not a successful closeout.

Promotion policy: promote the generated snapshot automatically only when
`auto gen --snapshot-only` exits 0, the newest `gen-*` directory contains
`IMPLEMENTATION_PLAN.md`, and the design gate is either not applicable or
reports `Verdict: GO`. Otherwise Hermes stops and returns the next command
without running implementation.

Promotion and execution:

```bash
GEN_DIR=$(ls -td gen-* | head -1)
test -n "$GEN_DIR"
test -f "$GEN_DIR/IMPLEMENTATION_PLAN.md"

auto gen --sync-only --output-dir "$GEN_DIR"
auto parallel \
  --threads "$THREADS" \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"
auto review \
  --model "$MODEL" \
  --reasoning-effort "$WORK_EFFORT"
```

Closeout gate:

```bash
auto pilot <repo-slug> "<operator intent>" \
  --closeout-only \
  --run-id "$RUN_ID" \
  --run-root "$RUN_ROOT"
```

`pilot-dev` runs this typed closeout gate by default through
`PILOT_TYPED_CLOSEOUT=1` after Codex exits. The gate writes
`pilot-closeout.json` and rejects success when required artifacts are missing
or incomplete:

- `pilot-preflight.json`
- `pilot-landing.json`
- `pilot-planning.json`
- `pilot-execution.json`
- `autodev-command-selection.json`
- `autodev-command-selection.md`
- `receipt.md`
- non-pending execution status, executor path, selected task/no-task reason,
  verification summary, commit/no-commit evidence, runtime restart/no-restart
  evidence, and Telegram summary fields in `pilot-execution.json`
- successful remote dry-run or explicit local-only no-origin policy in
  `pilot-landing.json`
- selected executed/degraded tasks are marked `[~]` or `[x]` in the resolved
  source plan; `pilot-dev` should have already done this through
  `auto pilot --task-finalize-only`
- selected/deferred/skipped decisions and reasons for every discovered command
- no `UNDECIDED` entries in the markdown companion
- required planning phases recorded as successful in `pilot-planning.json`
- steward promotion decisions when `steward-preflight/PROMOTIONS.md` exists

Hermes may send a successful Telegram summary only after the typed closeout
gate passes. If it fails, the wrapper reports the run as an orchestration
failure and links `pilot-closeout.json` plus `orchestration-failure.md`.

Project rollup:

```bash
auto pilot <repo-slug> "<operator intent>" \
  --project-rollup-only \
  --run-id "$RUN_ID" \
  --run-root "$RUN_ROOT"
```

`pilot-dev` runs this after a successful typed closeout by default through
`PILOT_PROJECT_ROLLUP=1`. The command writes:

- `project-rollup.json`
- `project-rollup.md`

When `PILOT_GBRAIN_PROJECT_ROLLUP=1`, the wrapper publishes
`project-rollup.md` to `projects/<repo-slug>` with `gbrain put`. If publication
fails, the wrapper writes `project-rollup-warning.md`; set
`PILOT_PROJECT_ROLLUP_REQUIRED=1` when stale gbrain project memory should fail
the run.

Hermes returns this Telegram summary shape:

```text
repo: <repo-slug>
branch/commit: <branch>@<commit>
plan: <GEN_DIR>/IMPLEMENTATION_PLAN.md
gbrain context: <GEN_DIR>/GBRAIN-CONTEXT.md
design: <.auto/design/run/DESIGN-REPORT.md or none>
execution: <.auto/parallel/run or none>
tests: <commands and result>
closeout: <gbrain slug>
next: <exact next command>
```

## Mature Release Workflow

Use this after the normal workflow has produced a stable queue, passing review,
and enough evidence to treat the project as release-bound.

```bash
cd "$REPO"
auto audit --everything \
  --everything-threads 15 \
  --remediation-threads "${THREADS:-8}" \
  --first-pass-model "${MODEL:-gpt-5.5}" \
  --first-pass-effort "${AUDIT_FIRST_PASS_EFFORT:-low}" \
  --synthesis-model "${MODEL:-gpt-5.5}" \
  --synthesis-effort "${WORK_EFFORT:-high}"

auto ship \
  --model "${MODEL:-gpt-5.5}" \
  --reasoning-effort "${WORK_EFFORT:-high}"
```

Gate:

- `auto audit --everything` has no unresolved production blocker.
- `auto ship` produces a release/PR artifact.
- Workflow F writes the release closeout to gbrain.

## Required GBrain Page Types

Every repo has:

- `projects/<repo-slug>`
- `closeouts/<repo-slug>-<run-id>` for every meaningful run
- `decisions/<repo-slug>-<date>-<slug>` for durable architecture/product calls
- `lessons/<repo-slug>-<date>-<slug>` for reusable lessons not tied to one run

Every closeout page contains:

- repo
- run id
- branch and commit
- operator intent
- commands run
- generated plan path
- design artifact path or `none`
- execution artifact/log paths
- tests and result
- lessons learned
- next command

## Consistency Rules Across Machines

1. Use the same gbrain backend for durable memory.
2. Use the same source id for the same repo on every machine.
3. Keep one indexing owner running `gbrain autopilot` or scheduled
   `gbrain sync && gbrain embed --stale`; do not depend on humans remembering
   per-run sync.
4. Run `gbrain doctor --fast` before trusting a machine as an orchestrator.
5. Treat local `.auto/` as disposable until Workflow F links or uploads it.
6. Treat chat transcripts as noncanonical until summarized into gbrain.
7. Treat GitHub branches as transport only. Branches do not replace closeout
   pages.
8. If gbrain write-back fails, create `.auto/closeout/<run-id>/closeout.md`,
   leave the run blocked, and make the next command fix gbrain sync.

## Definition Of Done

A development run is done when all are true:

- repo delta is committed or explicitly blocked
- focused validation ran or blocker is recorded
- review/QA gate ran when applicable
- artifacts are under `.auto/`, `gen-*`, or promoted repo docs
- gbrain closeout page exists
- artifacts are uploaded or linked from that page
- next command is explicit
- `git status --short` is clean or intentionally documented
