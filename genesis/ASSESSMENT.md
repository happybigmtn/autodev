# ASSESSMENT — autodev

## What the project says it is

The `README.md` opens with: "a lightweight repo-root planning and execution toolchain ... keeps the useful parts of the old Malina workflow and drops the Fabro-centered workspace, orchestration layer, and other legacy weight." `Cargo.toml` echoes this: "Lightweight repo-root planning and execution workflow."

The README enumerates "thirteen commands" and names them: `corpus`, `gen`, `reverse`, `bug`, `nemesis`, `quota`, `loop`, `parallel`, `qa`, `qa-only`, `health`, `review`, `ship`. Each command gets a dedicated section describing reads/writes/behavior, with explicit default models (`claude-opus-4-7`, `gpt-5.4`, `kimi-coding/k2p6`, `minimax/MiniMax-M2.7-highspeed`) and explicit default branches/tiers.

The design-goal paragraph at the bottom of the README says the repo should stay small and that anything not directly improving those same thirteen commands "probably does not belong here."

## What the code shows it is

Counted from `src/main.rs` lines 52-96, the `Command` enum has **sixteen** top-level variants, not thirteen:

1. `Corpus`
2. `Gen`
3. `Reverse`
4. `Bug`
5. `Loop`
6. `Parallel`
7. `Qa`
8. `QaOnly`
9. `Health`
10. `Review`
11. `Steward`  — introduced 2026-04-21, commit `7d60819`
12. `Audit`  — introduced 2026-04-21, commit `0b59aec`
13. `Ship`
14. `Nemesis`
15. `Quota`
16. `Symphony` — has its own subcommand tree (`Sync`, `Workflow`, `Run`)

The `Steward`, `Audit`, and `Symphony` variants are not present in the README's command list and none of them appear in the "Detailed Command Guide" (README lines 84-918). `Symphony` has doc-comment coverage in `main.rs` and a 3062-LOC implementation in `src/symphony_command.rs`, and `auto symphony run` is the command the README points at when it says "use `auto symphony` when you want parallel orchestration across a Linear-backed queue" (README line 536) — so the command exists in prose inside the loop section, but is absent from the top-level inventory.

Source-tree weight (`wc -l src/*.rs` reports 37,987 total SLOC across 30 modules) is dominated by a small number of very large files:

| Module | LOC | Role |
|---|---|---|
| `parallel_command.rs` | 7853 | `auto parallel` lane orchestration |
| `generation.rs` | ~4515 | `auto gen` / `auto reverse` pipelines |
| `bug_command.rs` | ~3533 | `auto bug` multi-pass bug pipeline |
| `symphony_command.rs` | 3062 | Linear sync + Symphony runtime |
| `nemesis.rs` | ~2921 | `auto nemesis` audit + hardening |
| `review_command.rs` | ~2127 | `auto review` queue review |
| `audit_command.rs` | 1154 | `auto audit` file-by-file auditor |
| `main.rs` | 1214 | CLI dispatch and argument parsing |
| `util.rs` | 1142 | Git, checkpoint, atomic-write helpers |
| `completion_artifacts.rs` | 918 | Verification receipts, completion evidence |
| `linear_tracker.rs` | 846 | Linear GraphQL client |
| `codex_exec.rs` | 685 | Codex invocation, tmux-lane wiring (dead) |

The repo is not "small" in the README's aspirational sense. It is a mid-sized Rust CLI with one dominant feature (`parallel`), a long tail of smaller commands, and recent aggressive expansion (`steward` and `audit` landed on the same day, 2026-04-21).

## What works

- **`auto` CLI dispatch and arg parsing.** `main.rs` compiles cleanly, wires every command to its module, and passes `CLI_LONG_VERSION` from `build.rs` (which captures git SHA, dirty flag, and build profile at compile time). The `auto --version` output carries real binary provenance.
- **Git operations layer in `util.rs`.** Extensive, with 30+ tests covering checkpointing, staging, remote sync, rebase handling, and `atomic_write` recovery. Checkpoint-exclusion logic was consolidated through the `CHECKPOINT_EXCLUDE_RULES` constant after the NEM-F3 Nemesis finding.
- **Nemesis audit findings NEM-F1 through NEM-F10.** `COMPLETED.md` at the repository root claims all ten are addressed; spot-checks against `util.rs` (`atomic_write_failure` helper, `CHECKPOINT_EXCLUDE_RULES`, `ensure_repo_layout_with`) and `nemesis.rs` (`resolve_auditor_model`, `next_nemesis_spec_destination`, pending-task short-circuit) confirm the claims match the code.
- **Quota routing, excluding credential storage.** `quota_selector.rs:8-10` enforces the documented `WEEKLY_FLOOR_PCT = 10` and `SESSION_FLOOR_PCT = 25`; `quota_exec.rs` uses `fd_lock::RwLock` to serialize credential swaps; rotation on exhaustion is driven by `quota_patterns.rs` pattern matching (rate limit, quota exceeded, 429, overloaded). The thresholds match the README.
- **Completion-evidence layer.** `completion_artifacts.rs` has 16 tests covering verification-receipt parsing, narrative-only rejection, and declared-artifact checking.
- **Linear sync core.** `linear_tracker.rs` has 5 tests covering config parsing, drift detection, and fingerprinting; the drift categories (`missing`, `stale`, `terminal`, `completed_active`) are implemented.
- **`util.rs::atomic_write` correctness after the NEM-F4 fix.** Temp-file cleanup now runs on both write and rename failures, with two tests covering both paths.
- **`kimi_backend.rs::preflight_kimi_cli`.** Proactively verifies that `kimi-cli` can return a trivial response before committing to an expensive run; parses the "LLM not set" silent-failure string.

## What is broken or half-built

- **`codex_exec.rs` is fronted by `#![allow(dead_code)]` (line 1).** Roughly 400 of its 685 lines implement a tmux-backed lane invocation path that is "staged for CLI integration but not yet wired." No caller invokes `run_codex_exec_in_tmux_with_env`, `spawn_codex_in_tmux`, `ensure_tmux_lanes`, `render_tmux_codex_script`, or `wait_for_tmux_completion`. This contradicts the README claim (line 43) that `auto parallel` "launches a detached `<repo>-parallel` tmux session automatically" — the tmux session management for `auto parallel` lives in `parallel_command.rs`, not `codex_exec.rs`, and the `codex_exec.rs` tmux helpers are genuinely unreferenced.
- **README command count is stale by three.** "Thirteen commands" is wrong; the enum has sixteen. `Steward`, `Audit`, and `Symphony` have full doc-comment descriptions in `main.rs` but no entry in the README inventory or detailed-command guide. Operators reading the README cannot discover `steward` or `audit` from it.
- **`auto audit` has no PI fallback.** `audit_command.rs:1023` bails with `"auto audit currently requires --use-kimi-cli"` if the operator disables Kimi CLI. The docstring on the `Audit` variant (`main.rs:81-87`) does not say Kimi is mandatory.
- **`FileVerdict::touched_paths` and `FileVerdict::escalate` are parsed but never consumed.** `audit_command.rs:129-138` defines the fields; `audit_command.rs:381` reads them into the struct; subsequent verdict-application logic only branches on `verdict.verdict`. The escalation pathway is half-wired.
- **No GitHub Actions / CI.** `.github/` does not exist. The `AGENTS.md` "Validate" block names `cargo test` and `cargo clippy --all-targets --all-features -- -D warnings` as the validation commands, but there is no automated enforcement of either.
- **`IMPLEMENTATION_PLAN.md` is an empty skeleton.** Three headers, no tasks. The plan surface the commands are designed around is effectively unused in this repo itself.
- **`salvage/autonomy-salvage-review.md` and `salvage/autonomy-p021-raw-snapshot-reference.patch` belong to a different project.** They're checked into `/salvage/` (ignored in `.gitignore` but present in the working tree at review time per `git status`), and reference work against a separate Autonomy checkout. This is operator scratch space, not `autodev` code — but it's tracked enough to appear as `??` in the status snapshot.

## Half-built that the README does not admit

- **Default model for the `auto bug` finder.** README line 39 says "MiniMax finder by default." `pi_backend.rs:21` and the bug-phase defaults favor `kimi-coding/k2p6` as the current primary; the recent commit `639d953` (2026-04-21) is titled "bug + nemesis: kimi-cli primary, fix-on-verify, Codex finalizer," which reads as an intentional switch that the README has not been updated to reflect.
- **`auto parallel` tmux behavior description is likely correct for `parallel_command.rs`** but the dead tmux code in `codex_exec.rs` suggests a planned broader tmux integration (probably per-subprocess) that never landed.
- **Steward's relationship to `corpus` + `gen`.** `main.rs:74-80` says steward "replaces `auto corpus` and `auto gen` for repos that already have an active planning surface; greenfield repos should keep using those." This creates a two-path lifecycle (greenfield vs. mid-flight) that is not documented in the README and not discoverable without reading the Rust source.

## Tech debt inventory

| Item | Location | Severity | Evidence |
|---|---|---|---|
| Dead tmux module | `codex_exec.rs:1-3, 122-397` | Medium | `#![allow(dead_code)]` at top; ~400 LOC unreferenced |
| Duplicated branch-resolution helpers | `loop_command.rs`, `review_command.rs`, `parallel_command.rs`, `bug_command.rs` | Medium | Each reimplements "default to current branch, then `origin/HEAD`, then `main`/`master`/`trunk`" |
| Duplicated reference-repo discovery | `generation.rs`, `loop_command.rs`, `parallel_command.rs`, `review_command.rs`, `bug_command.rs` | Medium | `resolve_reference_repos` / `discover_sibling_git_repos` copy-pasted |
| Duplicated `LlmBackend` enums | `bug_command.rs`, `nemesis.rs` | Low-Medium | Same Codex/Pi/Kimi variants and dispatch logic |
| Per-module prompt-logging paths | `loop_command.rs`, `parallel_command.rs`, `review_command.rs`, `bug_command.rs`, `nemesis.rs`, `audit_command.rs` | Low | Each writes `.auto/logs/<command>-<timestamp>-prompt.md` with its own helper |
| Oversized `parallel_command.rs` (7853 LOC) | `parallel_command.rs` | Medium | Mixes tmux session mgmt, lane state machine, git orchestration, queue parsing |
| Oversized `generation.rs` (~4515 LOC) | `generation.rs` | Medium | Mixes spec extraction, plan-merge, codex review loop, corpus hydration |
| `audit_command.rs` mixes verdict logic + manifest + worklist append | `audit_command.rs` | Medium | 1154 LOC; tests cover glob match + sha256 only |
| `unwrap_or_else` on path operations | `corpus.rs:66` | Low | Path-stem unwrap panics if path has no parent/stem |
| `timestamp_nanos_opt().unwrap_or_default()` | `util.rs:413` | Low | Default 0 is unsafe for collision avoidance in atomic-write temp names |
| `FileVerdict` escalation fields unused | `audit_command.rs:129-138, 381` | Medium | Half-wired feature |

## Security risks

| Risk | Location | Notes |
|---|---|---|
| **Plaintext credentials at rest** | `quota_config.rs:109, 224-227`; `quota_state.rs:51`; `quota_usage.rs:150` | Profile dirs contain raw `auth.json` / `.claude.json`; `fs::write` uses default umask; no `chmod 0o600` anywhere in quota subsystem. This is the single largest security gap. |
| **Error-message credential leakage** | `quota_usage.rs:125-126, 245`; `quota_status.rs:75` | Token-refresh error bodies and full error chains are printed to stderr; depending on OAuth response content, tokens or refresh tokens could appear in logs. |
| **Hardcoded `--dangerously-skip-permissions` for Claude** | `claude_exec.rs:161, 221` | No operator override; every Claude invocation runs with full permission skip. Documented as intentional by the `-dangerously-` prefix, but means compromised prompts can request anything. |
| **Hardcoded `--dangerously-bypass-approvals-and-sandbox` for Codex** | `codex_exec.rs:217-232` | Same pattern; Codex runs with sandbox bypass and approval bypass by default. |
| **No SSL pinning or cert validation hardening** | `linear_tracker.rs:473-502`; `quota_usage.rs` | Uses `reqwest` with OS-default cert store; acceptable for a developer tool but worth noting. |
| **Non-atomic state updates in quota** | `quota_state.rs:36-53` | Config/state load-modify-save without a held lock for the config file itself (locks exist for credential swaps, not state writes across concurrent `auto` invocations). |
| **`--report-only` output-dir wipe path in Nemesis** | Historical, addressed in `prepare_output_dir` via pre-wipe archival to `.auto/fresh-input/` (NEM-F1) | Safety is via archive-then-wipe; operator recovery still requires manual extraction if the model fails. |

## Test gaps

Coverage is bimodal. Some modules are well tested; several high-value modules have almost none.

| Module | Test count | Scope covered | Scope uncovered |
|---|---|---|---|
| `util.rs` | 30+ | Checkpoint excludes, atomic-write, remote sync, rebase | Shell-injection boundaries (no tests, but also uses arg-array subprocess) |
| `parallel_command.rs` | ~57 | Lane state machine, queue parsing, loop-plan parsing | Real multi-lane tmux, network-adjacent paths |
| `generation.rs` | ~38 | Markdown extraction, task-block parsing | Live Claude integration (not mocked, understandably) |
| `review_command.rs` | ~30 | Batch extraction, archival, stale detection | Reference-repo cloning, push failures |
| `nemesis.rs` | ~26 | JSON repair, task parsing, findings validation | End-to-end audit flow |
| `symphony_command.rs` | ~18 | GraphQL query construction, state parsing | Real Linear API |
| `completion_artifacts.rs` | 16 | Receipt validation, narrative-only rejection | — |
| `bug_command.rs` | ~16 | Finding dedup, chunk assignment | Per-chunk commit flow under failure |
| `loop_command.rs` | ~14 | Branch picking, repo progress, task queue | — |
| `linear_tracker.rs` | 5 | Config parsing, drift, fingerprint | Network failure paths, token refresh |
| `kimi_backend.rs` | 9 | Model resolution, exec args, error parsing, preflight | — |
| `pi_backend.rs` | 8 | Model aliasing, error parsing | — |
| `claude_exec.rs` | 4 | Model/effort resolution | Quota routing, futility detection, spawn failures |
| `codex_exec.rs` | 2 | Stdout progress detection | ~400 LOC of dead tmux code (uncovered and unused) |
| `corpus.rs` | 0 | — | All |
| `state.rs` | 0 | — | All |
| `health_command.rs` | 0 | — | All |
| `ship_command.rs` | 1 | Prompt text includes "rollback path" | Base-branch resolution, iteration loop |
| `qa_command.rs` | 0 | — | All |
| `qa_only_command.rs` | 0 | — | All |
| `steward_command.rs` | 7 | Prompt content, planning-surface detection, report-only | Deliverable verification, finalizer apply |
| `audit_command.rs` | 3 | Glob match, sha256 | Verdict application, manifest reconcile, escalation, resume |
| `main.rs` | 2 | Symphony `--sync-first` validation | All other commands' arg parsing |

The biggest concrete gaps are `audit_command.rs` (verdict-application, manifest-reconcile, escalation), `qa_command.rs` / `qa_only_command.rs` / `health_command.rs` / `ship_command.rs` (effectively no tests on any of the single-pass Codex-prompt commands), and `claude_exec.rs` / `codex_exec.rs` (quota routing and futility detection are branching hot spots with minimal direct coverage).

## Documentation staleness

| Document | Status | Evidence |
|---|---|---|
| `README.md` | **Stale by three commands.** | Says "thirteen commands," enum has sixteen. No section for `steward`, `audit`, or `symphony` in the detailed guide. README line 39 says MiniMax finder default; code now uses Kimi as primary (commit `639d953`). |
| `AGENTS.md` | Accurate. | Build, validate, essentials block match reality. |
| `specs/050426-nemesis-audit.md` | Historical; accurate for its scope. | Audits the eleven commands that existed when it was authored; does not cover `steward` or `audit`. |
| `COMPLETED.md` | Accurate for the NEM-F findings it claims. | Cross-checked against code. |
| `IMPLEMENTATION_PLAN.md` | Empty skeleton. | Three headers, no tasks. |
| `docs/audit-doctrine-template.md` | Accurate template. | Describes the `auto audit` doctrine file shape; example content only. |

## Implementation-status table for prior claims

| Prior claim (source) | Status | Evidence |
|---|---|---|
| NEM-F1: Output dir wipe archives before wiping (`COMPLETED.md`) | Verified | `nemesis.rs::prepare_output_dir`, `annotate_output_recovery` |
| NEM-F2: Explicit `--model` beats `--kimi`/`--minimax` (`COMPLETED.md`) | Verified | `nemesis.rs::resolve_auditor_model` |
| NEM-F3: Unified checkpoint excludes (`COMPLETED.md`) | Verified | `util.rs::CHECKPOINT_EXCLUDE_RULES` |
| NEM-F4: `atomic_write` cleanup on both paths (`COMPLETED.md`) | Verified | `util.rs::atomic_write_failure` + two tests |
| NEM-F5: Atomic staging around nemesis commit (`COMPLETED.md`) | Verified | `nemesis.rs::commit_nemesis_outputs_if_needed` snapshots + restores |
| NEM-F6: `ensure_repo_layout` collects all failures (`COMPLETED.md`) | Verified | `util.rs::ensure_repo_layout_with` |
| NEM-F7: No redundant PI prune (`COMPLETED.md`) | Verified | Removed from `nemesis.rs::run_pi`; bug phase-boundary only |
| NEM-F8: `verify_nemesis_outputs` pairs both files (`COMPLETED.md`) | Verified | Four-arm match |
| NEM-F9: Time-precise spec filename (`COMPLETED.md`) | Verified | `%d%m%y-%H%M%S` format |
| NEM-F10: Short-circuit zero-task plan (`COMPLETED.md`) | Verified | `pending_tasks.is_empty()` guard |
| README: "thirteen commands" (README:11) | **Refuted** | 16 enum variants |
| README: "MiniMax finder by default" (README:39) | **Refuted / Stale** | Commit `639d953` made Kimi primary; README not updated |
| README: `auto parallel` launches detached tmux session (README:43) | Accurate for `parallel_command.rs`; tmux helpers in `codex_exec.rs` are dead code | Verified split |
| Quota: 25%/5h/10% thresholds (README:442-470) | Verified | `quota_selector.rs:8-10` |
| Quota: encryption at rest | **Not in README, not in code** | No `chmod`/`set_permissions`; plaintext `auth.json` |
| `auto audit` PI fallback implied by doc-comment | **Absent in code** | `audit_command.rs:1023` bails without `--use-kimi-cli` |

## Code-review coverage list

Files read directly in this pass (either fully or in targeted ranges):

- `Cargo.toml`, `Cargo.lock` (dependency survey)
- `AGENTS.md`, `README.md`, `COMPLETED.md`, `IMPLEMENTATION_PLAN.md`, `.gitignore`
- `specs/050426-nemesis-audit.md`
- `docs/audit-doctrine-template.md`
- `salvage/autonomy-salvage-review.md`
- `build.rs`
- `src/main.rs`
- `src/util.rs`
- `src/state.rs`
- `src/corpus.rs`
- `src/steward_command.rs`
- `src/audit_command.rs`
- `src/completion_artifacts.rs`
- `src/claude_exec.rs`
- `src/codex_exec.rs`
- `src/pi_backend.rs`
- `src/kimi_backend.rs`
- `src/health_command.rs`
- `src/ship_command.rs`
- `src/qa_only_command.rs`
- `src/qa_command.rs`
- `src/linear_tracker.rs`
- `src/codex_stream.rs`
- `src/generation.rs` (surveyed, not line-by-line)
- `src/parallel_command.rs` (surveyed, not line-by-line; too large to fully read in one pass)
- `src/nemesis.rs` (surveyed)
- `src/bug_command.rs` (surveyed)
- `src/symphony_command.rs` (surveyed)
- `src/review_command.rs` (surveyed)
- `src/loop_command.rs` (surveyed)
- `src/quota_accounts.rs`, `src/quota_config.rs`, `src/quota_exec.rs`, `src/quota_patterns.rs`, `src/quota_selector.rs`, `src/quota_state.rs`, `src/quota_status.rs`, `src/quota_usage.rs`

Git history was surveyed via `git log --since='3 months ago'` to identify command-introduction dates (especially `steward` and `audit` on 2026-04-21), fix/harden patterns, and recent churn.

## Target users, success criteria, and repo constraints

**Target users.** Solo operators and small teams who run coding agents (Claude Code, Codex, Kimi, PI) against real repositories and want repeatable, evidence-backed planning and execution. The README's typical-flow section assumes a single human driving the CLI from a terminal, occasionally inside tmux.

**Success for those users looks like:**

- Running `auto corpus` on a new or drifting repo produces a planning corpus that is honest about what the code actually does and does not claim completeness it cannot defend.
- Running `auto gen` produces an `IMPLEMENTATION_PLAN.md` whose tasks can be handed to `auto loop` or `auto parallel` without further editing.
- `auto loop` completes tasks one at a time without corrupting the plan queue, leaves truthful commits, and never pretends to finish work it did not verify.
- `auto parallel` distributes lane work without losing commits, and the host reconciliation does not mark a lane complete on prose alone.
- `auto nemesis` and `auto bug` reliably turn up real findings and stop short of writing hallucinated fixes.
- `auto quota` keeps long-running jobs alive across account exhaustion without the operator noticing.
- `auto ship` produces a truthful branch-level release report and a PR that can be reviewed in one pass.

**Repo constraints.**

- **External tool dependence.** `claude`, `codex`, `pi`, `kimi-cli`, and `gh` must be on `PATH`. There is no abstraction layer to run offline.
- **Git monoculture.** The repo assumes Git with a reachable `origin`. There is no support for other VCSes or for repos without a remote.
- **No CI.** All enforcement (`cargo test`, `cargo clippy -D warnings`) is manual.
- **Rust 2021 edition, no workspace.** Single-crate layout; all commands live in the same binary.
- **No integration tests against real agent CLIs.** Every model-invoking path is tested through its parser or argument builder, not through real subprocess calls.

## Assumption ledger

| Assumption | Status | How to verify |
|---|---|---|
| The operator already knows which agent CLIs are installed | Working assumption; README lists requirements but no preflight | Add a `auto doctor` or extend `auto health` to check `claude`/`codex`/`pi`/`kimi-cli` presence |
| `IMPLEMENTATION_PLAN.md` format is stable enough for all commands to parse | Verified | Parsed identically by `loop_command`, `parallel_command`, `review_command`, `generation` |
| The 16-command inventory is the intended surface | **Unverified**; feels incrementally grown | Needs operator decision: should any command be retired or merged? |
| `steward` genuinely replaces `corpus + gen` for mid-flight repos | Claim in `main.rs:74-80`; no test or narrative proves non-overlap | Product-lens decision — see Plans 008 and 012 |
| `symphony` is a permanent part of the surface (not experimental) | **Unverified** | Only entry in README is a one-liner pointer at loop description (line 536) |
| Credentials on disk are acceptable at default umask | **Security assumption the code makes silently** | Needs explicit operator decision — see Plan 006 |
| `auto audit` is a short-term tool for the operator's own codebases | Inferred from 2026-04-21 introduction + `audit/DOCTRINE.md` template | Needs scope statement in README |
| `cargo test` and `cargo clippy -D warnings` currently pass | **Not verified in this pass** | Run both before landing anything in Plans 002/003 |

## Focus-response section

The operator supplied no `--focus` seed and no `--idea` seed. No `genesis/FOCUS.md` or `genesis/IDEA.md` was authored.

Without an operator-stated focus, this corpus weighs three signals equally: (1) the README is the product claim the operator has already published, so gaps between README and code are high-salience; (2) the 2026-04-21 burst added two large commands and a concurrency of "harden" commits, so the recent direction matters; (3) the security risk from plaintext credentials escapes both the README and the COMPLETED ledger, so it deserves explicit surfacing.

Higher-priority issues that would outrank a narrower focus if one had been given:

- **Plaintext credentials in the quota subsystem.** This is the single concrete security issue and affects every operator running `auto quota`.
- **README command-count drift.** Operators pick up the tool from the README; the README currently lies by omission about `steward`, `audit`, and `symphony`.
- **Absence of CI.** The `AGENTS.md` validate block is aspirational without automated enforcement, and a tool that enforces discipline on other repos should enforce its own.

## Opportunity framing

**Strongest direction: Honesty pass, then consolidation.** Close the gap between docs and code, remove the half-wired dead code, raise test coverage on the commands that are most exposed to user failure (`audit`, `qa`, `ship`), then consider structural refactors (shared utilities, backend trait).

**Rejected directions:**

- **Greenfield rewrite or workspace split.** The single-crate layout is still small enough to maintain, and the hotspots are in a handful of files. Splitting into sub-crates before the command surface is stable would lock in current fragmentation.
- **Add another command.** The recent burst (`steward` + `audit` on the same day, `symphony` earlier) is already testing the "this repo should stay small" design goal. Adding further commands before the inventory is truthful in the README would compound the drift.
- **Polish `parallel_command.rs` as the centerpiece.** It is the largest file and does most of what the tool is used for, but its scope and complexity are load-bearing; piecemeal refactor of its 7853 LOC would likely destabilize the working path without proving value. Research first, then slice.

## DX assessment (first-run honesty)

`autodev` is developer-facing tooling. The README promises a lifecycle — `corpus → gen → loop → qa → health → review → ship` — and a detailed per-command guide. Evaluating first-run friction:

- **T0 friction.** `cargo install --path . --root ~/.local` is one line; the binary lands as `~/.local/bin/auto`. Good, but assumes `~/.local/bin` is on `PATH`.
- **Discoverability.** `auto --help` is driven by `clap` with doc-comment subcommand descriptions. `steward` and `audit` appear in `--help` output because `main.rs:74-87` has doc comments — so the CLI itself is more truthful than the README. This is a positive signal.
- **First useful action.** For a repo with no `genesis/` yet, `auto corpus` is the obvious starting point. It writes `genesis/ASSESSMENT.md`, `genesis/SPEC.md`, etc. — the exact corpus shape this document represents. There is no hello-world smaller than a full corpus run, which is expensive (Claude opus with `xhigh`). A cheaper preview mode (`--dry-run`) exists but only stubs the invocation.
- **Honest examples.** The typical-flow block is concrete and copy-pasteable. However, it assumes `claude`, `codex`, and `gh` are already installed and authenticated; failure modes for missing agents are not walked through.
- **Error clarity.** Spot checks show errors are wrapped through `anyhow::Context` in most places; `kimi_backend::preflight_kimi_cli` catches the silent "LLM not set" mode explicitly. `audit_command.rs:1023` bails with a specific message about `--use-kimi-cli`, which is good.
- **Time-to-meaningful-success-moment.** Running `auto corpus` to completion on a mid-sized repo is measured in minutes under a `claude-opus-4-7` budget — not instantaneous, but the observability-to-stdout (phase markers, elapsed timings, claude PID) documented at README:138-141 is genuine, so operators are not flying blind.

Net: the DX is strong for the 13 documented commands but silent about `steward`, `audit`, and `symphony`. The biggest honesty gap a new user would hit is trying to discover the lifecycle and finding that the README's command inventory does not match `auto --help`.
