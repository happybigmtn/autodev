# ASSESSMENT - autodev

Review date: 2026-04-23.

This assessment treats the current working tree as the source of truth. The archived previous corpus under `.auto/fresh-input/genesis-previous-20260423-201851` was used as historical context only. Several findings from that snapshot have since been implemented or superseded, so they are not carried forward as current facts.

Independent Codex review update: the authoring pass recorded one failing full-suite test, `quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error`. This review reran that exact test with `cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error -- --exact`, and it passed. Full `cargo test` also passed with 377 tests, 0 failed.

## How Might We

How might we make `auto` a trustworthy operator console for planning and executing agent work in real repositories, so a human can see what the tool will touch, what evidence proves progress, which credentials are active, and whether generated plans are true before the tool commits or pushes anything?

The code already answers part of that question: it is no longer a small prompt wrapper. It is a repo-ops CLI with planning corpus generation, implementation-plan synthesis, sequential and parallel agent execution, quota-account credential swapping, Linear/Symphony integration, audit and bug pipelines, and release/review/QA prompts. The next phase should make that reality safer and easier to operate rather than adding more surface area.

## Target Users, Success Criteria, and Constraints

Primary users are hands-on engineering operators who run coding agents against real checkouts. They need high-confidence repo state, narrow task execution, recoverable checkpoints, truthful planning artifacts, and clear failure modes.

Secondary users are future contributors to this CLI. They need honest onboarding, fast local smoke tests, and a command architecture that does not require reading a 7,000-line module before making a safe change.

Success looks like:

- `auto corpus` produces a corpus that is honest about current code and does not revive historical findings as active work.
- `auto gen`, `auto loop`, and `auto parallel` share the same task-contract interpretation.
- Credential swapping always restores the previous account and does not leave active provider auth in a surprising state.
- Generated verification commands prove work without accepting malformed shell snippets, zero-test filters, or directory greps as sufficient evidence.
- First-run operators can get to a meaningful success moment without already knowing every external dependency.

Major constraints:

- The CLI intentionally operates on real repository roots and may commit or push.
- The tool relies on external CLIs: `codex`, `claude`, `pi`, and `gh` are listed in `AGENTS.md`; Symphony and Linear flows also require API credentials.
- Several commands intentionally run models with permission or sandbox bypass flags. That is powerful, but it raises the standard for preflight, logging, checkpointing, and redaction.
- The working tree is currently dirty outside `genesis/`; those edits are treated as operator-owned and were not changed by this corpus pass.

## What The Project Says It Is Vs What The Code Shows

The README describes `autodev` as a lightweight repo-root planning and execution toolchain and documents the binary as `auto`. That is directionally true, but understated.

The code shows a mid-sized autonomous repo-ops system. `src/main.rs` wires sixteen top-level commands: `corpus`, `gen`, `reverse`, `bug`, `loop`, `parallel`, `qa`, `qa-only`, `health`, `review`, `steward`, `audit`, `ship`, `nemesis`, `quota`, and `symphony`.

The live implementation includes:

- strict planning-corpus generation and validation in `src/generation.rs`;
- corpus loading and snapshots in `src/corpus.rs`;
- sequential execution in `src/loop_command.rs`;
- large multi-lane orchestration in `src/parallel_command.rs`;
- credential/account routing in `src/quota_*.rs`;
- Linear-backed Symphony rendering and sync in `src/symphony_command.rs` and `src/linear_tracker.rs`;
- bug, nemesis, audit, QA, review, steward, and ship prompt pipelines.

The repo is developer-facing and operator-facing. Its user experience is primarily terminal commands, generated markdown, logs, receipts, and status output rather than a web UI.

## What Works

- CLI dispatch and help are centralized in `src/main.rs`, with clear argument defaults and a build-provenance long version from `src/util.rs` and `build.rs`.
- `auto corpus` encodes an unusually strict corpus contract: required docs, full ExecPlan section validation, checkpoint plan expectations, absolute repo-root sanitization, and Codex review hooks.
- `auto gen` and `auto reverse` can load the planning corpus and synchronize generated specs/plans back into root planning surfaces.
- Non-Claude authoring models route through Codex in `src/generation.rs`; Claude-family model names remain explicit Claude routes.
- `auto parallel` has substantial recovery machinery: lane state, tmux orchestration, landing, partial follow-ups, health status, and Linear best-effort sync.
- `util.rs` provides atomic writes, git checkpointing, remote sync, and generated-path exclusions.
- CI exists under `.github/workflows/ci.yml` with fmt, clippy, and tests on push and pull request.
- Many pure parser and state-machine surfaces have unit tests. Full `cargo test` passed with 377 tests in the binary target, and the previously reported quota usage test passes under targeted review.

## What Is Broken

- The root validation baseline in docs is stale. Root docs still cite older validation counts and task states, while this review's current validation evidence is full `cargo test`: 377 passed, 0 failed. Docs that claim the old `333`-test baseline are stale.
- Claude quota credential restore is incomplete on the normal path. `swap_credentials` backs up both `.claude/` and `.claude.json`, but `restore_credentials` only restores the `.claude/` directory, and `restore_and_update_state` disarms the guard before calling the restore function.
- Symphony workflow rendering interpolates `base_branch`, `model`, and `reasoning_effort` into shell/YAML contexts without typed validation and without consistently using the existing `shell_quote` helper.
- Root planning truth is inconsistent. `IMPLEMENTATION_PLAN.md`, `ARCHIVED.md`, and specs disagree about which tasks are complete and what the current validation baseline is.
- The tracked `genesis/` corpus files were deleted before this pass, leaving the repo temporarily without a usable checked-in corpus. This pass rebuilds the corpus but should not be treated as the active root queue.

## What Is Half-Built

- Verification evidence is stronger in `auto parallel` than in `auto loop`. The loop receipt policy exists as a decision document, but enforcement is still prompt-oriented.
- Completion evidence parsing is broad enough to normalize shell/network/destructive-looking commands as verification proof. It does not execute those commands, but it can bless risky proof text without a risk class.
- Markdown task parsing is duplicated across generation, loop, parallel, review, completion artifacts, and Symphony. Behavior has drifted; for example, blocked tasks and partial/dependency handling differ by command.
- Backend execution policy is split across shared wrappers and command-specific spawn paths. Some paths bypass `src/codex_exec.rs` or `src/claude_exec.rs`, which makes safety mode, quota routing, logging, and context-window behavior hard to audit globally.
- First-run DX depends on the operator already knowing which commands require a git remote, provider auth, Linear auth, Docker, browser tools, or `kimi-cli`.

## Tech Debt Inventory

| Item | Severity | Evidence | Next action |
|---|---:|---|---|
| Root planning drift | High | `IMPLEMENTATION_PLAN.md` and `ARCHIVED.md` disagree on completed tasks and test counts | Plan 002 |
| Quota credential restore gap | Critical | `src/quota_exec.rs` backs up `.claude.json` but does not restore it in `restore_credentials` | Plan 003 |
| Quota capture/copy hardening | High | `copy_auth_to_profile` uses raw `fs::copy`, preserves symlinks, and does not prune stale profile files | Plan 003 |
| Symphony workflow injection risk | High | `src/symphony_command.rs` interpolates shell/YAML scalars raw in workflow text | Plan 004 |
| Checkpoint staging includes `genesis/` | High | `CHECKPOINT_EXCLUDE_RULES` excludes `.auto`, `bug`, `nemesis`, and `gen-*`, not `genesis` | Plan 005 gate decision |
| Verification command false proof | High | `WORKLIST.md` names malformed generated receipts and false-positive proof paths | Plan 006 |
| Duplicated task parsing | Medium | plan/task parsing appears in generation, loop, parallel, review, Symphony, and completion artifacts | Plan 007 |
| Backend spawn duplication | Medium | direct model launches exist outside wrapper modules | Plan 008 |
| Large orchestration modules | Medium | `parallel_command.rs`, `generation.rs`, `bug_command.rs`, and `nemesis.rs` mix parsing, prompting, state, and IO | Research after Plan 009 |
| Weak first-run smoke | Medium | no top-level integration tests prove `auto --help`, dry-run corpus, incomplete corpus errors, or installed binary behavior | Plan 010 and Plan 011 |

## Security Risks

| Risk | Severity | Evidence | Status |
|---|---:|---|---|
| Claude credential restore can leave selected profile active | Critical | `src/quota_exec.rs` backup pair includes `.claude.json`; restore path omits it | Verified from code |
| Credential profile capture is not symlink-safe | High | `src/quota_config.rs` recreates symlinks during recursive copy | Verified from code |
| Agent execution uses sandbox/approval bypass flags | High | Codex wrapper uses `--dangerously-bypass-approvals-and-sandbox`; Claude wrapper uses `--dangerously-skip-permissions`; Kimi uses `--yolo` | Verified from code, intentional but under-controlled |
| Auto checkpoints can stage sensitive untracked paths | High | `stage_checkpoint_changes` stages all non-ignored, non-excluded untracked files | Verified from code |
| Symphony workflow shell/YAML injection | High | branch/model/reasoning values are interpolated without typed validators | Verified from code |
| Logs can persist raw stderr and provider output | Medium | stderr appenders exist across Codex, Claude, bug, stream rendering, and parallel logs | Verified from code |
| Quota state load-modify-save is not uniformly transaction-locked | Medium | swap paths lock provider credentials; status/reset/select paths still deserve a state-lock audit | Verified from code pattern, needs focused proof |

## Test Gaps

The test suite is broad but mostly unit-level. Current review evidence re-establishes a green full-suite baseline: 377 tests passed. The gaps below remain because the suite is still light on integration and security-boundary coverage.

| Area | Current coverage | Gap |
|---|---|---|
| `util.rs` | strong unit coverage for atomic writes, checkpoints, sync, push, and exclusions | secret-looking checkpoint classification is absent |
| `generation.rs` | strong validation/prompt tests | integration tests with fake model binaries are still missing |
| `parallel_command.rs` | extensive parser/status/recovery tests | live tmux multi-lane behavior is not hermetic in CI |
| `completion_artifacts.rs` | receipt and narrative evidence tests | command risk classes and false-proof fixtures remain open |
| `quota_*` | selector/state/config tests plus usage tests | restore path, profile pruning, symlink rejection, and concurrent state mutation need stronger tests |
| `symphony_command.rs` | parser/render tests | hostile shell/YAML scalar golden tests are missing |
| `qa`, `qa-only`, `health`, `ship` | mostly prompt text tests or no tests | first-run and fake-model smoke tests are missing |
| installed binary | no current proof in this pass | `cargo install --path . --root ~/.local` was not run |

## Documentation Staleness

| Document | Current status |
|---|---|
| `AGENTS.md` | Accurate on build basics and required tools; validate block omits `cargo fmt --check`, which CI runs. |
| `README.md` | Much fresher than the archived corpus, but still contains wording that implies `--model` picks a Claude model for corpus even though non-Claude values route through Codex. It also overstates `origin` as a global runtime requirement. |
| `IMPLEMENTATION_PLAN.md` | Stale active queue. It includes old validation counts and leaves tasks open that `ARCHIVED.md` describes as completed. |
| `ARCHIVED.md` | Useful completion ledger, but should not be treated as proof without receipts or code validation. |
| `WORKLIST.md` | Current and important. It names verification-command synthesis and false-positive proof gaps. |
| `specs/220426-*.md` | Mixed. Some specs describe code that has moved on, especially CI, README truth, release status, and audit backend defaults. |
| `genesis/` | Rebuilt by this pass. It is a planning corpus, not the active root implementation queue. |

## Implementation-Status Table For Prior Claims And Plans

| Prior claim or plan theme | Current status | Evidence | Corpus response |
|---|---|---|---|
| Old corpus: no CI exists | Stale | `.github/workflows/ci.yml` exists and runs fmt/clippy/tests | Do not carry forward as active |
| Old corpus: README misses `steward`, `audit`, `symphony` | Mostly stale | README now documents the sixteen-command surface, though wording still needs cleanup | Narrow to wording and runtime requirement drift |
| Old corpus: dead tmux scaffold in `codex_exec.rs` | Stale | recent history removed that scaffold; current Codex wrapper is live | Do not plan deletion |
| Old corpus: quota file permissions absent | Partly stale | config/state saves use owner-only writes, but profile capture/copy and restore remain risky | Plan focused hardening |
| Root plan: old test baseline | Refuted | full `cargo test` passed with 377 tests; targeted quota usage rerun also passes | Plan 002 updates root planning truth |
| `WORKLIST.md`: malformed review verification command synthesis | Still open | no evidence that the worklist item was resolved | Plan 006 |
| `WORKLIST.md`: false-positive proof from zero-test filters or directory grep | Still open | no evidence that the worklist item was resolved | Plan 006 |
| `docs/decisions/loop-receipt-gating.md`: loop receipt gating deferred | Still relevant | loop enforcement remains weaker than parallel evidence checks | Plan 006/009 decision gate |
| Broad Codex `gpt-5.5` default migration | Current working-tree direction | dirty source/docs show broad default changes; actual routing must be validated by runtime tests | Treat as current but not release-proven |
| Root `plans/` governs ExecPlans | Not applicable | no root `PLANS.md`; no root `plans/` directory | Generated plans remain subordinate to root queue/specs |

## Code-Review Coverage List

Files read directly or through targeted ranges:

- Root instructions and docs: `AGENTS.md`, `README.md`, `ARCHIVED.md`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `WORKLIST.md`, `LEARNINGS.md`, `.gitignore`.
- Build and CI: `Cargo.toml`, `Cargo.lock`, `build.rs`, `.github/workflows/ci.yml`.
- Core CLI and state: `src/main.rs`, `src/state.rs`, `src/util.rs`, `src/corpus.rs`, `src/generation.rs`.
- Agent execution: `src/codex_exec.rs`, `src/claude_exec.rs`, `src/pi_backend.rs`, `src/kimi_backend.rs`, `src/codex_stream.rs`.
- Task execution and review: `src/loop_command.rs`, `src/parallel_command.rs`, `src/completion_artifacts.rs`, `src/review_command.rs`.
- Product commands: `src/audit_command.rs`, `src/bug_command.rs`, `src/nemesis.rs`, `src/steward_command.rs`, `src/qa_command.rs`, `src/qa_only_command.rs`, `src/health_command.rs`, `src/ship_command.rs`.
- Quota router: `src/quota_accounts.rs`, `src/quota_config.rs`, `src/quota_exec.rs`, `src/quota_patterns.rs`, `src/quota_selector.rs`, `src/quota_state.rs`, `src/quota_status.rs`, `src/quota_usage.rs`.
- Symphony/Linear: `src/symphony_command.rs`, `src/linear_tracker.rs`.
- Specs and decisions: `specs/220426-*.md`, `specs/050426-nemesis-audit.md`, `docs/decisions/loop-receipt-gating.md`, `docs/decisions/symphony-graphql-surface.md`, `docs/audit-doctrine-template.md`.
- Historical corpus: `.auto/fresh-input/genesis-previous-20260423-201851/*`.

Git history was reviewed for recent corpus/spec/hardening work, including the sequence that generated the first corpus, converted it into root specs and plans, added Codex authoring support, hardened quota permissions, added CI, and removed older dead code.

Independent Codex review spot-checks on 2026-04-23 inspected:

- repo and corpus shape: `git status --short`, `find genesis -type f -name '*.md'`, and absence of root `plans/`;
- corpus docs and plan headings: `genesis/GENESIS-REPORT.md`, `ASSESSMENT.md`, `SPEC.md`, `PLANS.md`, `DESIGN.md`, and representative numbered ExecPlans;
- root controls: `AGENTS.md`, `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `.gitignore`, `.github/workflows/ci.yml`, and targeted README/default greps;
- code evidence: targeted ranges in `src/main.rs`, `src/generation.rs`, `src/util.rs`, `src/quota_exec.rs`, `src/quota_config.rs`, `src/symphony_command.rs`, `src/codex_exec.rs`, `src/claude_exec.rs`, and `src/kimi_backend.rs`;
- validation evidence: `cargo test quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error -- --exact`, `cargo test -- --list`, and full `cargo test`.

## Assumption Ledger

| Statement | Status | Proof or next proof |
|---|---|---|
| The binary name is `auto` | Verified | `Cargo.toml` declares `[[bin]] name = "auto"` and `AGENTS.md` lists the CLI binary |
| The package version is `0.2.0` | Verified | `Cargo.toml` |
| The active root planning surface is `IMPLEMENTATION_PLAN.md` plus `specs/` | Verified by repo layout | no root `PLANS.md`; no root `plans/`; these docs exist and are referenced by commands |
| `genesis/` is a generated corpus, not root queue authority | Verified by code and layout | `auto corpus` writes it; `auto gen` consumes it; root queue remains separate |
| Current tests are green | Verified for unit suite | full `cargo test` passed with 377 tests; clippy/install proof not rerun |
| Broad `gpt-5.5` defaults are intended | Likely, but release decision still in progress | dirty code/docs show it; runtime validation and commit history should confirm before release |
| Quota restore bug is exploitable in normal use | High-confidence code finding | focused regression should prove selected-profile leakage and backup cleanup |
| Symphony shell/YAML injection can execute hostile input | Plausible risk | add golden tests before deciding exact exploitability |
| First-run operator flow is too sharp | Verified by docs/code review | missing command-specific doctor and hermetic smoke tests |

## Focus Response

No operator focus seed was supplied. Priority order therefore comes from full-repo review. If a focus seed later points at model defaults, review command synthesis, or Symphony, the quota credential restore bug and shell/YAML rendering risk still outrank most non-security work because they affect credential integrity and generated executable workflow text.

## Opportunity Framing

Recommended direction: build an operator-trust kernel around the existing command surface. That means truthful planning surfaces, safe credential/account handling, shared task and verification contracts, explicit backend execution policy, and first-run smoke tests.

Rejected direction 1: add more commands or backends now. The repo already has sixteen commands and several direct backend spawn paths. More surface would amplify current safety and DX debt.

Rejected direction 2: rewrite the CLI into a plugin system or service. The current single-binary Rust shape is valuable for local operators. The immediate problems are contracts and safety boundaries, not deployment architecture.

Rejected direction 3: make `genesis/` the active control plane. The code treats `genesis/` as corpus input to generation, while root docs hold implementation state. Promoting `genesis/` to queue authority would create two competing sources of truth.

Rejected direction 4: treat the archived previous corpus as truth. It is useful history, but many of its findings have been implemented or superseded.

## DX Assessment

First-run friction is moderate to high. A Rust contributor can build quickly with `cargo check`, `cargo build`, and `cargo test`, but an operator needs several external CLIs, model auth, a git repository, and sometimes a remote or Linear credentials depending on command. The README is detailed, but it does not yet make command-specific runtime requirements clear enough.

The fastest honest success moment should be:

1. Build or run `auto --version`.
2. Run a local doctor or dry-run command that proves required binaries and repo layout without calling a model.
3. Run a small corpus dry run or fixture-backed generation smoke test.

Current docs make the system look more turnkey than it is. Onboarding should be adjusted so copy-paste examples are honest about live model calls, potential commits/pushes, and generated artifact locations. Error clarity is strong in many code paths, but preflight is inconsistent across commands.

## Review Discipline Summary

CEO review: challenge the premise. The repo should not chase a larger autonomous platform until it proves trust in the existing command lifecycle.

Design review: the meaningful user surfaces are CLI help, status output, logs, generated markdown, and receipts. There is no browser UI, but information architecture and terminal accessibility matter.

Engineering review: fix security and contract seams before modular refactors. The main risk is not one missing abstraction; it is multiple commands interpreting the same plan, verification, and backend concepts differently.

DX review: build a command-specific first-run path and integration smoke tests. The system should teach operators what it will do before it asks them to trust live agents with repo writes.
