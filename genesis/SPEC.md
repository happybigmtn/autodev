# SPEC — autodev

## What this is

`autodev` is a single Rust binary named `auto` that wraps a fleet of coding-agent CLIs (`claude`, `codex`, `pi`, `kimi-cli`) in an opinionated repo-root planning and execution lifecycle. The operator runs `auto <command>` from inside a Git working tree; `auto` resolves the repo root, invokes one or more agent CLIs, consumes their streamed output, and writes durable artifacts back into the repo (`specs/`, `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `QA.md`, `HEALTH.md`, `SHIP.md`) or into disposable staging (`genesis/`, `gen-<timestamp>/`, `bug/`, `nemesis/`, `.auto/`).

The tool assumes one operator per repo per run, a reachable `origin`, and the expectation that commits will be pushed to the branch the operator is on. Most mutating commands rebase onto `origin/<branch>` before work and before push.

## Command surface (current truth)

`main.rs:52-96` enumerates sixteen top-level commands. Treating this as the real surface:

| Command | Module | Role | State |
|---|---|---|---|
| `corpus` | `corpus.rs`, driven by a Claude invocation in the caller | Build a disposable planning corpus under `genesis/` from repo reality | Stable |
| `gen` | `generation.rs` | Turn the corpus into durable specs and a plan | Stable |
| `reverse` | `generation.rs` (shared pipeline) | Reverse-engineer specs from code without touching the plan | Stable |
| `bug` | `bug_command.rs` | Chunked finder → skeptic → reviewer → fixer pipeline | Stable |
| `nemesis` | `nemesis.rs` | Draft audit → synthesis → implementation, feedback into root plan | Stable |
| `loop` | `loop_command.rs` | Single-worker implementation of the next `- [ ]` task | Stable |
| `parallel` | `parallel_command.rs` | Multi-lane tmux-backed implementation executor | Stable but large |
| `qa` | `qa_command.rs` | Runtime QA pass, may fix bounded issues | Stable |
| `qa-only` | `qa_only_command.rs` | Report-only QA (no fixes) | Stable |
| `health` | `health_command.rs` | Repo-wide quality/verification snapshot, no fixes | Stable |
| `review` | `review_command.rs` | Review completed work, archive or worklist-append | Stable |
| `steward` | `steward_command.rs` | Two-pass reconciliation for mid-flight repos; alternative to `corpus + gen` | **New (2026-04-21), undocumented in README** |
| `audit` | `audit_command.rs` | File-by-file audit against operator-authored `audit/DOCTRINE.md` | **New (2026-04-21), undocumented in README** |
| `ship` | `ship_command.rs` | Release-prep + docs/version sync + PR creation/refresh | Stable |
| `quota` | `quota_*.rs` | Multi-account routing for Claude/Codex | Stable, security gap |
| `symphony` | `symphony_command.rs` | Linear.app sync + orchestration runtime | **Undocumented in README command list** |

"State" column judges whether code and docs agree and whether the command has meaningful test coverage. Stability does not mean bug-free.

## Behavioral contract for the lifecycle (what operators rely on)

The following behaviors are exercised in the source and confirmed in code review. They form the implicit contract `autodev` is asking operators to trust.

1. **Binary provenance.** `auto --version` prints package version plus embedded git SHA, dirty/clean flag, and build profile (driven by `build.rs`).
2. **Repo-root auto-resolution.** Every command calls `util::git_repo_root` and operates relative to the resolved root.
3. **Checkpoint safety on mutating runs.** Before launching a long agent invocation, commands call `util::auto_checkpoint_if_needed` to commit dirty tracked/untracked state with a machine-readable message, honoring `CHECKPOINT_EXCLUDE_RULES` (`.auto/`, `bug/`, `nemesis/`, `gen-*`).
4. **Atomic file writes.** Spec, plan, report, and state artifacts go through `util::atomic_write`, which writes to `<name>.tmp-<pid>-<nanos>` and renames, cleaning up the temp on failure.
5. **Rebase-before-work and rebase-before-push** for `loop`, `qa`, `review`, `ship`, `bug`, `nemesis`, `parallel`, and `audit`. `util::sync_branch_with_remote` performs `git pull --rebase --autostash origin <branch>` when the remote branch exists.
6. **Archive-then-wipe for disposable output.** `nemesis.rs::prepare_output_dir` archives the previous `nemesis/` into `.auto/fresh-input/nemesis-previous-<timestamp>/` before wiping. `auto corpus` does the same for `genesis/`.
7. **Futility detection on agent streams.** `codex_stream.rs` tracks consecutive empty tool results (`CLAUDE_FUTILITY_THRESHOLD = 8`, `CLAUDE_FUTILITY_THRESHOLD_REVIEW = 16`); reaching the threshold kills the agent and returns exit code 137 (`FUTILITY_EXIT_MARKER`).
8. **Quota routing on exhaustion.** `quota_exec.rs` sequentially tries configured accounts, detects exhaustion via `quota_patterns.rs` (rate limit, quota exceeded, 429, overloaded), and rotates. Weekly floor is `10%`, session floor is `25%` (`quota_selector.rs:8-10`).
9. **Task queue protocol.** `IMPLEMENTATION_PLAN.md` uses `- [ ]` for pending, `- [!]` for blocked (skipped by `loop`), `- [x]` for completed. Parsing is identical across `loop_command`, `parallel_command`, `review_command`, `generation`.
10. **Verification receipts for parallel host reconciliation.** `parallel_command.rs` requires `.auto/symphony/verification-receipts/<task_id>.json` (shape validated by `completion_artifacts.rs`) to mark executable-`Verification:` tasks complete; prose handoff alone leaves the task `[~]`.

## Artifact shapes (user-visible file formats)

These are the persistent surfaces operators see and sometimes hand-edit:

- **`specs/<ddmmyy>-<topic-slug>[-<counter>].md`** — durable specs. Required sections per `auto gen` contract: `## Objective`, `## Acceptance Criteria`, `## Verification`, `## Evidence Status`, `## Open Questions`.
- **`IMPLEMENTATION_PLAN.md`** — plan queue with `- [ ]` / `- [!]` / `- [x]` task markers and per-task metadata (spec reference, evidence, owned surfaces, scope, acceptance, verification, dependencies, estimated scope, completion signal).
- **`REVIEW.md`** — completion handoffs for `auto review` to pick up.
- **`ARCHIVED.md`** — cleared review items.
- **`WORKLIST.md`** — issues that survived review, or that `auto audit` surfaces for manual follow-up.
- **`LEARNINGS.md`** — durable institutional knowledge surfaced during review/qa/ship.
- **`QA.md`** — branch-level runtime QA evidence and verdict.
- **`HEALTH.md`** — repo-wide health snapshot with 0-10 score and per-lane sub-scores.
- **`SHIP.md`** — release-prep report with rollback path, monitoring path, rollout posture.
- **`COMPLETED.md`** — completed work that has not yet flowed through `auto review`.
- **`genesis/{ASSESSMENT,SPEC,PLANS,GENESIS-REPORT,DESIGN,FOCUS,IDEA}.md` and `genesis/plans/*.md`** — disposable planning corpus.
- **`bug/BUG_REPORT.md`, `bug/verified-findings.json`, `bug/implementation-results.json`** — bug-pipeline outputs.
- **`nemesis/nemesis-audit.md`, `nemesis/IMPLEMENTATION_PLAN.md`, `nemesis/implementation-results.{json,md}`** — nemesis outputs.
- **`audit/MANIFEST.json`, `audit/files/<hash-prefix>/{verdict.json,patch.diff,response.log,prompt.md}`** — per-file audit manifest and artifacts.
- **`.auto/state.json`, `.auto/logs/`, `.auto/loop/`, `.auto/parallel/`, `.auto/qa/`, `.auto/health/`, `.auto/ship/`, `.auto/review/`, `.auto/fresh-input/`, `.auto/queue-runs/`, `.auto/symphony/`** — runtime state and logs.

## Models (current defaults, verified in source)

The README names specific defaults. Code review reveals some drift:

| Command / phase | README claim | Source reality |
|---|---|---|
| `corpus`, `gen`, `reverse` | `claude-opus-4-7` with `xhigh` | Matches (`claude_exec.rs`, model resolution tables) |
| `loop` | `gpt-5.4` with `xhigh` | Matches (`loop_command.rs` defaults) |
| `qa`, `qa-only`, `health`, `review`, `ship` | `gpt-5.4` with `high` (standard tier for QA) | Matches |
| `nemesis` draft | PI with `minimax/MiniMax-M2.7-highspeed` and `high` | Matches, but `--kimi` and `--minimax` flags exist |
| `nemesis` synthesis | PI with `kimi-coding/k2p6` and `high` | Matches |
| `nemesis` implementer | Codex `gpt-5.4` with `high` | Matches |
| `bug` finder | "MiniMax `minimax/MiniMax-M2.7-highspeed`" (README:341) | **Drift** — commit `639d953` made Kimi primary; code defaults now favor `kimi-coding/k2p6` |
| `bug` skeptic / reviewer | Kimi `kimi-coding/k2p6` with `high` | Matches |
| `bug` implementer | `gpt-5.4` with `high` | Matches |
| `audit` auditor | (not in README) | Kimi via `kimi-cli --yolo`; no PI fallback path, bails without `--use-kimi-cli` |
| `parallel` | `gpt-5.4` with `xhigh`, five workers | Matches |

These should be treated as **recommended defaults** subject to future change. Any plan or spec written against exact model strings must be rewrittable when defaults move again.

## Runtime dependencies (verified against code and README)

- `git` — always required; `util.rs::git_repo_root` calls `git rev-parse --show-toplevel` at startup.
- `claude` — required for `corpus`, `gen`, `reverse`; also used inside `parallel` / `review` / `loop` branches.
- `codex` — required for `nemesis`, `loop`, `qa`, `qa-only`, `health`, `review`, `ship`, `bug` (unless using PI for a phase).
- `pi` — required for `bug` PI phases and default `nemesis` audit passes.
- `kimi-cli` — required for the `audit` command (see `audit_command.rs:1023`) and for bug/nemesis Kimi phases.
- `gh` — optional; used by `ship` to create or refresh PRs.
- `tmux` — required for `parallel` (invoked by `parallel_command.rs` to host lane windows).

Absence of any required tool produces an `anyhow::Error` at the point of invocation; there is no single preflight command. `AGENTS.md` lists the essentials.

## Scope this repo intends to own

The design-goal paragraph in the README defines the scope in its own words: "if a feature does not directly improve `corpus`, `gen`, `reverse`, `bug`, `nemesis`, `quota`, `loop`, `parallel`, `qa`, `qa-only`, `health`, `review`, or `ship`, it probably does not belong here." The code has since added `steward`, `audit`, and `symphony`, which either belong in an updated scope statement or deserve an explicit justification.

Pragmatic read of current scope:

- **In scope.** Planning (`corpus`, `gen`, `reverse`, `steward`), implementation (`loop`, `parallel`), quality (`qa`, `qa-only`, `health`, `review`, `audit`), release (`ship`), audit (`bug`, `nemesis`), account management (`quota`), orchestration backbone (`symphony`).
- **Out of scope.** Language servers, IDE plugins, per-file formatting, non-agent code generation, cross-repo refactoring engines, anything a monorepo tool would handle.

## Near-term direction (what we want the next corpus pass to reflect)

1. **Docs match code.** README inventory, per-command guide, and `--help` text agree on the sixteen commands and their current defaults.
2. **Security hygiene on quota.** Credential files are chmod-restricted; error messages do not print refresh-token bodies.
3. **Dead code removed.** `codex_exec.rs` loses its ~400 lines of tmux scaffolding or the scaffolding ships as a used feature.
4. **CI enforces validate.** `cargo test`, `cargo clippy -D warnings`, and `cargo fmt --check` run on push.
5. **Audit command gains tests.** Verdict-application, manifest reconciliation, and escalation are covered by tests before any further feature expansion of `audit`.
6. **Shared utilities consolidated.** Branch resolution, reference-repo discovery, and prompt logging live in one place and are re-used.
7. **Command-lifecycle narrative clear.** An operator reading the README can tell when to use `steward` vs. `corpus + gen`, when to use `audit` vs. `nemesis`, and when `symphony` is the right front end.

Each of these is a `genesis/plans/NNN-*.md` target. Order and dependency are in `genesis/PLANS.md`.
