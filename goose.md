# goose.md — Autodev Improvement Plan from Goose Capability Review

> **Source:** Capability comparison between [`/home/r/coding/goose`](../goose) (Rust + Electron AI agent framework) and this autodev repo, conducted 2026-05-06.
>
> **Goal:** Identify patterns from goose that improve autodev's **long-running harness efficiency, productivity, and completeness** — without compromising autodev's strengths (queue discipline, verification receipts, lane-based parallelism, document-centric durability).

---

## TL;DR

Three goose patterns map directly onto autodev's existing architecture and would deliver outsized leverage:

1. ~~**Transparent context compaction** inside long Codex/Claude invocations → 5–10× longer effective lane runs without context overflow.~~ **INVALIDATED 2026-05-07** — Codex already performs internal compaction, and autodev already varies model size per stage. See P1 below for the corrected scope.
2. **MCP-shim for tool-call introspection** → close the gap autodev's own audit flagged ("no real-time tool execution validation"), enable richer receipts and live progress.
3. **YAML recipes with sub-recipes** → let operators compose new commands without Rust changes, multiply autodev's surface area.

Six smaller improvements (cross-model evals matrix, repetition detector, streaming `auto status`, LLM adversary on receipts, hooks subsystem, per-token cost surfacing) follow.

A prioritization section is left at the bottom for the operator to fill in based on their actual workflow pain points.

## Implementation status

| Proposal | Status | Notes |
|---|---|---|
| P1 Context compaction | **Invalidated** | Codex already compacts; autodev varies model size per stage. Follow-on: surface compaction events into the lane event stream once P6 ships (so operators can see when Codex compacted vs. when context was simply trimmed). |
| P6 Lane event stream | **Shipped in branch `feat/goose-review-cost-and-events`** | Append-only `events.jsonl` per lane (mirrored from `append_lane_host_event`) + `auto parallel watch` subcommand that tails events with 500ms polling. New module `src/lane_events.rs` with `LaneEvent` enum (TaskStarted, TaskCompleted, LaneIdle, ReceiptDrift, HostMessage). 2 unit tests. Existing `stdout.log` behavior unchanged. |
| P10 Per-task cost surfacing | **Shipped in branch `feat/goose-review-cost-and-events`** | `UsageSummary` (Codex token counts; Claude tokens + `cost_usd`) auto-persisted as `<rendered_log>.usage.json` sidecar at end of each Codex/Claude stream. New `auto cost` subcommand walks `.auto/` recursively and prints per-harness aggregate + optional per-invocation detail. PI/Kimi sidecars deferred (would touch ~10 call sites; follow-up). 1 unit test. |
| P2A MCP trace | Deferred | Needs Codex/Claude harness MCP wiring; larger, schedule for follow-up PR. |
| P3 Recipes | Deferred | Largest surface change; one-way door, deserves its own focused PR. |
| P4 Eval matrix | Deferred | Wait for P10 cost data to make defaults data-driven — cost data now flowing once this PR lands. |
| P5 Repetition detector | Deferred | Depends on P2A trace data. |
| P7 LLM adversary on receipts | Deferred | Standalone, schedule next. |
| P8 Self-test recipe | Deferred | Depends on P3 recipes. |
| P9 Hooks subsystem | Deferred | Standalone; natural follow-up to P6 since hooks would subscribe to the same event stream. |

### Build / test posture (this PR)

- `cargo build` clean, no warnings.
- `cargo test --bin auto`: 536 passed, 34 failed. The 34 failures are pre-existing on `main` (verified by checking out main and re-running) and live in `generation::tests::*`, `task_parser::tests::*`, and `spec_command::tests::*` — they predate this branch and are unrelated to cost/events.
- `cargo clippy --all-targets -- -D warnings`: clean for the new modules; the 2 clippy errors (`audit_everything.rs:5334`, `task_parser.rs:443`) are pre-existing on `main`.

---

## Capability Matrix

| Capability | Goose | Autodev | Gap? |
|---|---|---|---|
| **Streaming event model** with cancellation | `BoxStream<AgentEvent>` + cancellation tokens throughout async chain (`crates/goose/src/agents/agent.rs`) | Polling loop every 5s in `parallel_command.rs`; SIGTERM-based cancel | Yes — limits live observability |
| **Context compaction** inside one agent run | Tool-pair summarization at 80% threshold; transparent to model (`crates/goose/src/context_mgmt/mod.rs`) | None — agents re-receive full markdown ledgers each invocation | Yes — major efficiency loss on long lanes |
| **Sub-agent / delegation** | `subagent_handler.rs`: isolated `SessionType::SubAgent`, recursion-blocked, message streaming back to parent | Lane-based parallelism only; no in-lane delegation | Partial — lanes solve different problem |
| **MCP integration** as primary extension | All tools (built-in + external) are MCP servers; unified protocol; tool calls flow through `ExtensionManager` | None — agents run in opaque Codex/Claude harness; tools invisible to autodev | Yes — explicitly flagged in autodev's own audit |
| **Recipe / workflow composition** | YAML recipes with Jinja2 templating, parameters (incl. `file` type for content injection), sub-recipes with `sequential_when_repeated`, structured output schemas | 21 commands hardcoded as Clap subcommands in `src/main.rs`; no operator-authored composition | Yes — limits extensibility |
| **Cross-model evals matrix** | Open Model Gym: `models × runners × scenarios`, replay 3+ times, keep worst run | None; defaults hardcoded (gpt-5.5, high effort) | Yes — no data-driven model selection |
| **Permission / security inspection pipeline** | 4 layers: SecurityInspector, EgressInspector, AdversaryInspector (LLM-based), PermissionInspector, RepetitionInspector | None at autodev layer; relies on operator review post-hoc | Partial — different threat model |
| **Self-test / first-person validation** | `goose-self-test.yaml`: 5-phase meta-test where goose validates itself end-to-end | `auto doctor`: no-model preflight only | Yes — no live integration coverage |
| **Cost tracking per token** | Provider reports usage; UI shows running cost | Quota tracks freshness, not per-task spend | Partial — quota covers different need |
| **Verification receipts in commit footers** | None | Strong: argv, exit code, artifact hashes, plan hash, dirty fingerprint, embedded base64url in commit | **Autodev wins** — keep |
| **Host-owned queue with worktree lanes** | None | Strong: lane workers must not edit queue; host reconciles | **Autodev wins** — keep |
| **Operator-authored doctrine as judge** | None | Strong: `audit/DOCTRINE.md` is 100% operator-controlled; auditor verdict depends on doctrine | **Autodev wins** — keep |
| **Quota-aware account multiplexing** | None | Strong: `~/.autodev/quota/<provider>/<name>.toml` with fresh-account selection | **Autodev wins** — keep |

---

## Proposal 1 — ~~Transparent context compaction inside lane runs~~ INVALIDATED

**Status:** Withdrawn 2026-05-07 after operator feedback.

**Why withdrawn:** Codex already performs internal context compaction. Autodev already varies model size per stage (`PLAN_EFFORT`, `WORK_EFFORT`, `AUDIT_FIRST_PASS_EFFORT` env knobs documented in README), so the larger context windows are accessible where they matter most. A second compaction layer in autodev would duplicate work the underlying CLI already does and risk fighting Codex's own state.

**Corrected scope (smaller, follow-up PR):** Once the lane event stream (P6) is in place, surface Codex/Claude compaction signals as `event_type: "compaction"` entries in `events.jsonl` so operators can *see* when the underlying CLI compacts and how often. This is observability over the existing mechanism, not a parallel implementation. Effort: ~80 lines once P6 ships.

---

## Proposal 2 — MCP shim for tool-call introspection

### Problem
The autodev capability map flagged this directly: *"Agents run in Codex/Claude harness (external tool); autodev cannot control tool use. Verification happens post-hoc (receipt inspection). No real-time tool execution validation within autodev."*

This means receipts can only describe outcomes, never the *path* (which files were read, which scripts were run, which web fetches happened). Drift triage in `RECEIPTS-DRIFT.md` is harder than it needs to be because the host has no causal trace.

### Goose pattern
`crates/goose-mcp/` ships built-in MCP servers (`developer`, `computercontroller`, `memory`, `peekaboo`) that the agent calls through a uniform protocol. Every tool call passes through `ExtensionManager::dispatch_tool_call()` where it's instrumented before execution. The host has full visibility.

### Autodev change
Two-stage rollout:

**Stage A (instrumentation only):** Ship a thin MCP server `autodev-trace` that exposes a `record_tool_use` tool and lives in the same process as the lane worker. Configure the Codex/Claude harness to register it (Codex supports MCP via stdio; Claude harness varies). Agent calls are instructed (via prompt ethos preamble) to fire `record_tool_use(name=..., args=..., outcome=...)` after every meaningful action. Trace lands in `.auto/<run-id>/lane-<n>/trace.jsonl`. Even if some agents skip the call, partial coverage is strictly better than zero.

**Stage B (replacement):** Replace direct shell access with autodev-owned MCP servers (`autodev-fs`, `autodev-shell`, `autodev-git`) that perform the action *and* record it. Receipt freshness checks gain a new field: `tool_trace_hash`. RECEIPTS-DRIFT.md gains a "what the agent actually did" section.

### Why this matters for completeness
Without tool introspection, "did the agent really verify the migration script?" is unanswerable except by re-running. With it, you can prove negative outcomes ("agent never opened the file it claims to have audited") and short-circuit bad lanes before they land.

### Effort
Stage A: small (~300 lines + prompt change). Stage B: large (~1500 lines, spans multiple commands). Recommend Stage A first as a pure observability win.

---

## Proposal 3 — YAML recipes with sub-recipes for operator-authored commands

### Problem
All 21 autodev commands are hardcoded Rust subcommands. An operator who wants `auto migration-audit` (similar to `auto bug` but specialized) must fork the binary or chain shell scripts. There's no way for the team to share custom workflows without recompiling.

### Goose pattern
`crates/goose/src/recipe/mod.rs` defines a YAML schema with: `instructions`, `prompt` (Jinja2), `parameters` (typed: string/number/bool/date/file/select), `extensions`, `settings` (model/temp/max_turns), `sub_recipes` (composition with parameter forwarding + `sequential_when_repeated`), `response.json_schema` (structured output), `retry`, `activities`. Skills in goose are just discoverable recipes — `~/.agents/skills/<name>/SKILL.md`.

### Autodev change
- New `src/recipe.rs` defining the schema (lift goose's struct verbatim where it fits autodev's model).
- New `auto run --recipe path.yaml [--param key=value …]` command.
- Recipe directory convention: `recipes/` at repo root + `~/.config/autodev/recipes/` for user-global.
- Built-in recipes for the existing commands (`recipes/builtin/super.yaml`, `corpus.yaml`, `bug.yaml`, …) — keeps Rust subcommands as fast paths but lets operators *fork a recipe* to specialize without touching Rust.
- Sub-recipes: a recipe can list other recipes with parameter forwarding. Maps naturally to `auto super`'s 6-phase orchestration.
- Recipe parameters of type `file` inject contents (great for "audit this DOCTRINE against this code").

### Why this matters for productivity
This is the single biggest "10× more autodev" change. Instead of 21 commands, the surface becomes 21 commands + N team-authored recipes. Every team can encode their own incident-response, migration, or release-cut playbook.

### Effort
Large (~800 lines + tests + docs). Worth it. Decouples autodev from "what the autodev maintainers thought of."

---

## Proposal 4 — Cross-model evals matrix (borrow Open Model Gym)

### Problem
Defaults are guessed: `gpt-5.5` for plan/work, `high` effort, no per-command tuning. Operators have no way to know whether `auto bug` would actually succeed more often on Claude than Codex, or whether `medium` effort matches `high` for `auto loop` at half the cost.

### Goose pattern
`evals/open-model-gym/` runs a matrix of `models × runners × scenarios`, repeats each cell 3+ times, keeps the **worst** result (catches flaky passes). Validation rules: `file_exists`, `file_contains`, `file_matches`, `command_succeeds`, `tool_called`. HTML dashboard.

### Autodev change
- New `evals/` directory with scenario YAMLs (one per autodev command + a few combo scenarios).
- New `auto eval` command that iterates `(scenario × model × effort)`, runs each cell N times, captures verification receipts as the validation oracle (this maps better to autodev than goose's file-content checks).
- Output: `evals/results/<timestamp>/MATRIX.md` + per-cell logs.
- Use this to publish data-driven defaults: "as of <date>, `auto bug` is +18% pass rate on Claude vs Codex; `auto loop` is indistinguishable, default to cheaper Codex."

### Why this matters for efficiency
Stop paying for `high` effort where `medium` works. Stop using a model that fails 30% of the time on a command where another model nails it.

### Effort
Medium (~500 lines + scenario library). Pays off perpetually; should run nightly in CI once stable.

---

## Proposal 5 — Repetition / loop detector

### Problem
`FUTILITY_EXIT_MARKER` exits when the agent emits a sentinel. But agents that loop on the same broken test, the same import error, or the same failed grep don't always emit the marker — they just keep retrying. Operators discover this only by watching tmux.

### Goose pattern
`RepetitionInspector` tracks tool-call signatures and blocks identical-call sequences past a threshold, returning `DECLINED_RESPONSE` so the model has to pivot.

### Autodev change
- Hook into the (proposed) MCP trace from Proposal 2.
- Detect repetition by hashing `(tool_name, normalized_args)` across the trace; trip when the same hash appears 3× within a 10-call window.
- On trip: inject a synthetic message into the next agent turn ("you have called X 3 times with the same args; the result is not changing. Try a different approach or report blocked."), and tag the lane state as `[!] blocked-by-loop` for host reconciliation.

### Why this matters
Pre-empts wasted budget on lanes that have already failed but haven't realized it. Also makes RECEIPTS-DRIFT reasoning cleaner: a blocked lane is explicit, not silently stuck.

### Effort
Small (~200 lines, depends on Proposal 2 Stage A).

---

## Proposal 6 — Streaming `auto status` (replace 5s polling)

### Problem
`parallel_command.rs` polls lane state every 5 seconds. Updates are stale, and the host can't react to lane events (e.g., trip a hook the moment a receipt lands).

### Goose pattern
`BoxStream<AgentEvent>` — provider, tool, message, history-replaced events flow as a stream with backpressure.

### Autodev change
- Lanes write to a per-lane append-only `.auto/<run-id>/lane-<n>/events.jsonl`.
- `auto parallel status` and a new `auto parallel watch` use `tokio::fs::File` with inotify (or fall back to 250ms tail-poll on filesystems without it) to stream events.
- TUI in `auto parallel watch` shows real-time per-lane activity heatmap.

### Why this matters
The user experience of "is this thing alive?" goes from 5-second-blind to real-time. Also enables Proposal 9 (hooks).

### Effort
Small to medium (~400 lines). Most of the work is the TUI; the stream itself is trivial.

---

## Proposal 7 — LLM adversary on verification receipts

### Problem
Receipt freshness checks today are structural: did the file exist? Did the hash match? Did the plan hash drift? They cannot detect *semantic* drift — e.g., a receipt that claims "test passes" pointing to a test that passes for the wrong reason.

### Goose pattern
`AdversaryInspector` is an optional LLM-based review pass: a separate model reads what's about to happen and asks "is this suspicious?" Returns DECLINED if so.

### Autodev change
- New optional pass in `inspect_task_completion_evidence()`: when enabled, send `(task description, receipt JSON, diff)` to an adversary model with the prompt "is this evidence consistent with completing the described task? Identify gaps."
- Verdicts: `PASS` / `SUSPICIOUS` / `INSUFFICIENT`. `SUSPICIOUS` writes a `RECEIPTS-DRIFT.md` entry with the adversary's reasoning. `INSUFFICIENT` blocks landing.
- Cheap model (Haiku or `gpt-5.5-mini`) is the right tier; this is a sanity check, not a deep audit.

### Why this matters for completeness
Closes the loop between "receipt structurally valid" and "receipt actually proves work was done." Catches the most common autodev failure mode: agents that mark `[x]` after writing the right shape of receipt but not the right substance.

### Effort
Small (~250 lines + prompt design).

---

## Proposal 8 — Self-test recipe (`auto self-test`)

### Problem
`auto doctor` is a no-model preflight. It cannot catch regressions in the autodev workflow itself — e.g., a parser change that breaks `IMPLEMENTATION_PLAN.md` reading, or a queue reconciliation bug that surfaces only under load.

### Goose pattern
`goose-self-test.yaml`: 5-phase meta-test where goose uses its own tools to validate its own capabilities. The ability to complete the test *is itself* the test.

### Autodev change
- A `recipes/self-test.yaml` recipe (depends on Proposal 3) that runs `auto corpus → gen → loop (1 task) → review → ship` against a tiny throwaway repo set up in a temp dir.
- Phases mirror goose's: Basic (corpus + gen), Lanes (parallel with 2 workers + 4 tasks), Audit (run audit on the throwaway repo), Receipts (verify receipt freshness gates trip on planted drift), Report.
- New `auto self-test` shortcut command runs it.
- CI hook: nightly run produces `SELF-TEST-RESULTS.md`.

### Why this matters for completeness
Catches regressions across the whole stack. Today autodev is dogfooded but not adversarially tested.

### Effort
Medium (~600 lines incl. throwaway repo fixtures). Depends on Proposal 3.

---

## Proposal 9 — Hooks subsystem

### Problem
Operators have no way to react to autodev events. Want a Slack ping when a receipt drifts? A pre-commit linter run before `auto ship`? A webhook on lane completion? Today: shell wrappers around autodev, fragile.

### Goose pattern
Goose itself doesn't have formal hooks (uses internal channels), but Claude Code's `settings.json` hooks pattern is well-established.

### Autodev change
- New `~/.config/autodev/hooks.toml` (and per-repo `.auto/hooks.toml` overlay).
- Event types: `lane_started`, `lane_completed`, `receipt_drift`, `task_blocked`, `ship_attempted`, `audit_finding`.
- Each hook is a shell command receiving event JSON on stdin.
- Bonus: Pre-event hooks can return non-zero to abort the action (e.g., a `pre_ship` hook that blocks ship if a security scanner fails).

### Why this matters for productivity
Lets teams integrate autodev into existing infra (Slack, PagerDuty, internal dashboards) without forking.

### Effort
Small (~300 lines).

---

## Proposal 10 — Per-token cost surfacing in receipts and quota

### Problem
Quota tracks freshness (weekly/session) but not per-task or per-command spend. Operators can't answer "which lane was the most expensive last week?" or "is `auto super` worth $X per run?"

### Goose pattern
Provider's `ModelInfo` includes input/output cost per token + cache control support. UI computes running cost.

### Autodev change
- Extend `ModelInfo` (or equivalent) in autodev's backend modules with input/output cost per token.
- Codex/Claude responses already carry usage; capture and persist to receipt `usage` field.
- New `auto cost` subcommand: aggregates by command, by lane, by day, by model.
- Quota system gains "estimated cost remaining for this account this week" alongside freshness.

### Why this matters
Accountability. And data for Proposal 4 (which cells in the eval matrix are *cost*-effective, not just outcome-effective).

### Effort
Small (~250 lines + cost table maintenance).

---

## Cross-cutting: Things autodev should NOT change

These are autodev strengths goose lacks. Don't accidentally erode them while adopting goose patterns:

1. **Verification receipts in commit footers** — durable, replayable, git-grep-friendly. Goose has no equivalent. Keep.
2. **Host-owned queue with worktree lanes** — clean concurrency model. Lanes not editing queue files prevents whole classes of bugs. Keep.
3. **Operator-authored doctrine** (`audit/DOCTRINE.md`) — judges depend on doctrine, not hardcoded rules. Goose is more opinionated. Keep autodev's flexibility.
4. **Quota-aware account multiplexing** — unique to autodev. Goose only has cost tracking, not account selection. Keep and extend per Proposal 10.
5. **Markdown-as-state** — `IMPLEMENTATION_PLAN.md` as queue is grep-friendly, diff-friendly, and survives any tool failure. Goose uses SQLite. Don't migrate.

---

## Suggested rollout sequencing

A defensible order based on dependency and leverage:

| Phase | Proposals | Why this order |
|---|---|---|
| **Phase 1** (foundation) | 2A (MCP trace), 6 (streaming events), 9 (hooks) | All three add observability/extensibility infrastructure that later phases depend on. None requires breaking changes. |
| **Phase 2** (efficiency) | 1 (compaction), 5 (repetition detector), 10 (cost surfacing) | Direct token/cost wins. Compaction is the headline gain. Repetition detector depends on Phase 1's trace. |
| **Phase 3** (extensibility) | 3 (recipes), 8 (self-test) | Recipes are the biggest surface-area change; build on the now-stable observability stack. Self-test depends on recipes. |
| **Phase 4** (validation) | 4 (eval matrix), 7 (LLM adversary) | Need the eval matrix to make data-driven choices; need the adversary to harden the receipt gate. Both benefit from prior phases being settled. |
| **Phase 5** (deepening) | 2B (full MCP replacement) | Only after the trace-based version proves the value. High-cost, high-reward; do last. |

---

## Open questions for the operator

These are calls only the autodev maintainer can make. Add answers below; the rollout sequence above should bend to these answers, not the other way around.

> **Q1 — Headline pain:** Of the three TL;DR items (compaction, MCP introspection, recipes), which one would have prevented the most recent autodev-related frustration? Naming the specific incident is more useful than picking one in the abstract.
>
> *Operator answer (2026-05-07):* Redirected to "implement directly". Operator clarified that **codex already handles compaction** and autodev already varies model size per stage, invalidating Proposal 1 as originally framed. See the implementation status table at the top.
>
> ---
>
> **Q2 — Compaction trust:** Goose's compaction is transparent to the agent. Are you comfortable with autodev's lane workers losing access to summarized turns, or do you want compaction to be visible in `.auto/<run-id>/` AND surfaced in the next prompt as `<!-- compacted: see file -->`?
>
> *Operator answer (2026-05-07):* N/A — P1 invalidated. Replacement question for the follow-up PR: *what compaction signals from Codex are useful to surface in the lane event stream?*
>
> ---
>
> **Q3 — Recipe scope:** If recipes (Proposal 3) ship, should the existing 21 commands stay as Rust fast paths, or get migrated to recipes (with Rust providing only the runtime)? Migration is a one-way door; fast paths is more conservative.
>
> *Operator answer:* (deferred until the recipes PR is scheduled)
>
> ---
>
> **Q4 — Adversary model cost:** Proposal 7 calls a cheap model on every receipt. At ~50 receipts/week × ~2K input tokens, this is real money on Sonnet but trivial on Haiku. Acceptable to default to Haiku-tier models for the adversary pass?
>
> *Operator answer:* (deferred until the adversary PR is scheduled)
>
> ---
>
> **Q5 — Top 3 prioritization:** Override the suggested phase order. Which 3 proposals do you want shipped first, and why?
>
> *Operator answer (2026-05-07, claude judgment per "use your judgment" + "implement directly" redirect):*
>
> 1. **P10 Per-task cost surfacing** — smallest blast radius, immediately useful, generates the data needed to make later decisions (P4, P10) data-driven instead of guess-driven. Receipts already exist; we just stop throwing token usage on the floor.
> 2. **P6 Lane event stream** — replaces 5s polling with a real-time JSONL stream. Necessary substrate for P9 hooks and any future observability work. Conservative addition: existing `stdout.log` stays intact; we add `events.jsonl` alongside.
> 3. ~~**P1 Compaction**~~ → **deferred / withdrawn.** Replaced in this PR slot by goose.md updates and PR scaffolding so the PR is reviewable end-to-end. Future PR: P9 (hooks) is the natural next pick because it composes with P6.
>
> **What's NOT in this PR (and why):** P3 recipes (one-way door, deserves dedicated PR + design review), P2 MCP shim (larger scope, depends on Codex/Claude harness MCP-wiring details that need investigation), P5/P7/P8 (depend on P2 or P3 prerequisites).

---

## Appendix — Goose files worth reading directly

If you want to study the patterns first-hand before implementing:

- Compaction logic: `goose/crates/goose/src/context_mgmt/mod.rs`
- Recipe schema: `goose/crates/goose/src/recipe/mod.rs` and `goose/crates/goose/src/recipe/template_recipe.rs`
- Sub-agent isolation: `goose/crates/goose/src/agents/subagent_handler.rs`
- Permission inspection pipeline: `goose/crates/goose/src/permission/` and `goose/crates/goose/src/security/`
- Self-test: `goose/goose-self-test.yaml`
- Eval harness: `goose/evals/open-model-gym/`
- Streaming agent loop: `goose/crates/goose/src/agents/agent.rs:1028-1232`
