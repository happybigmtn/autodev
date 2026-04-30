# Assessment

## How Might We

How might we turn `auto` from a powerful model-orchestration CLI into a production-trustworthy autonomous development control plane, where an operator can run corpus generation, plan generation, scheduler execution, evidence review, and release gating without guessing which markdown file, receipt, lane, or backend owns the truth?

## Target Users

- Engineering operator: wants to run `auto corpus`, `auto gen`, `auto super`, `auto parallel`, and `auto ship` without babysitting ambiguous state.
- Repository maintainer: wants root ledgers, generated corpora, receipts, and CI to agree.
- Worker agent: needs exact, machine-readable tasks, dependencies, evidence requirements, and recovery instructions.
- New contributor: needs a short first-run path with honest prerequisites and visible success.

## Success Criteria

- A cleared queue stays cleared unless fresh evidence says otherwise.
- A pending row is executable only when dependencies, ownership, and evidence requirements are deterministic.
- A completed row has current receipts, artifacts, and review handoff or an explicit accepted evidence class.
- Credential profiles and saved state cannot escape intended directories.
- `auto ship` runs release gates against the current synced tree and rechecks after model work.
- First-run setup gets a new operator to `auto doctor`, installed binary smoke, and a meaningful non-mutating command quickly.

## Repo Constraints

- The product is a Rust CLI, not a service-backed web application.
- The binary name is `auto`; required external tools include `claude`, `codex`, `pi`, and `gh`.
- Generated/runtime state lives under `.auto/`, `bug/`, `nemesis/`, `gen-*`, and related audit/cache paths that must not be treated as normal source.
- Root ledgers are the active execution surface; `genesis/` is subordinate planning input unless an operator promotes it.
- Model-backed phases are inherently non-deterministic, so host-side validators, receipts, and dirty-state guards must own canonical truth.
- CI currently covers Rust formatting, clippy, tests, install, and help smoke, but not live model execution.

## Project Claims Vs Code Reality

| Area | Project says | Code shows | Status |
| --- | --- | --- | --- |
| Product shape | Lightweight repo-root planning and execution toolchain. | Rust CLI binary `auto` with 21 commands in `src/main.rs` and CI smoke for major help surfaces. | Verified. |
| Control truth | Runtime truth in `src/`; planning truth in root ledgers/specs/receipts; `genesis/` is input unless promoted. | `DESIGN.md`, `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, and generation discovery support this. No root `PLANS.md` exists. | Verified. |
| Corpus generation | Complete genesis corpus with required files and full ExecPlans. | `run_corpus` validates generated outputs; this working tree now has 12 numbered ExecPlans. The authoring pass reported a degraded pre-refresh `genesis/`; independent review verified the current corpus, saved-state pointer, and archived snapshots rather than relying on that prior filesystem claim. | Current corpus verified; pre-refresh condition is recorded as authoring-pass evidence. |
| Generation | `auto gen` uses saved planning root or `genesis`. | `src/state.rs` stores raw `PathBuf`; `src/generation.rs` reuses saved paths and later removes planning roots during corpus prep. | Half-built safety. |
| Scheduler | `auto parallel` is the production scheduler with receipts and lane recovery. | Strong worker prompts and receipt checks exist, but split-brain ledger conventions and last-good plan fallback can mislead execution. | Works with safety gaps. |
| Completion evidence | Receipts, artifacts, and review handoff prove completion. | `completion_artifacts.rs` enforces this, but current root rows are checked while `REVIEW.md` is intentionally empty, creating demotion risk. | Inconsistent. |
| Quota routing | Credential profile safety and account routing. | Symlink and chmod protections exist, but account names become path components and failover can retry after progress. | High-risk gap. |
| Design gate | Report-only by default with GO/NO-GO artifacts. | `design_command.rs` enforces artifacts and write boundaries, but verdict parsing accepts any matching line. | Mostly works. |
| Release | `auto ship` gates release readiness. | Gate is evaluated before sync and not rerun after model pass. | Needs hardening. |
| CI | Formatting, clippy, tests, installed binary smoke. | `.github/workflows/ci.yml` runs `cargo fmt --check`, clippy with warnings as errors, `cargo test`, `cargo install --locked`, and help smoke. | Strong baseline. |

## What Works

- CLI command surface is explicit and tested in `src/main.rs`.
- CI validates formatting, clippy, tests, install, and major help surfaces.
- `auto corpus` prompt/validation now requires focus briefs, assessment/spec/report/index, and full ExecPlan envelopes.
- `auto design`, `auto qa-only`, and `auto health` enforce report-only write boundaries through dirty-state snapshots.
- `auto audit --everything` has a mature resumable manifest, status/pause phases, file-quality rerating, and merge gates.
- `completion_artifacts.rs` centralizes most task completion evidence checks.
- `parallel_command.rs` contains stale-lane recovery, worker receipt instructions, and status rendering.
- `util.rs` has checkpoint exclusions for `.auto`, `bug`, `nemesis`, `gen-*`, audit caches, and `.claude/worktrees`.
- Quota credential copying rejects symlinked sources and writes copied credential files with owner-only permissions.

## Broken Or Half-Built

- The authoring pass reported a degraded pre-refresh `genesis/` while `.auto/state.json` pointed at it; this independent review verified the current corpus is complete, the saved state still points at `genesis/`, and runtime corpus preparation can remove the planning root before replacement validation.
- `load_planning_corpus` accepts a planning root with an empty `plans/` directory; generated-output verification catches this later, but input loading can still produce a zero-plan corpus.
- Saved `.auto/state.json` paths are raw `PathBuf`s and can steer later generation toward absolute or outside-repo planning roots.
- `prepare_planning_root_for_corpus` removes the planning root after archiving without a containment gate.
- Quota account names are interpolated into profile paths without slug validation.
- Quota failover can retry a write-capable model invocation after detecting worker progress.
- Current checked root rows plus an empty `REVIEW.md` can conflict with completion evidence rules and cause mass demotion.
- `auto parallel` can continue from a last-good plan snapshot when current plan refresh fails.
- Lane resume identity does not persist a stable task-body hash as a hard resume guard.
- `auto ship` evaluates release gates before branch sync and does not rerun the gate after model execution.
- GO/PASS verdict parsing in design, audit, and book accepts any matching line rather than exactly one terminal verdict.
- Kimi/PI prompt delivery paths put full prompts in argv and have weaker timeout/preflight parity.
- Large orchestrators (`generation.rs`, `parallel_command.rs`, `audit_everything.rs`, `audit_command.rs`) mix policy, IO, prompts, scheduling, and validation in files that are hard to review end to end.

## Tech Debt Inventory

- Duplicated receipt freshness logic between release and completion surfaces.
- Duplicated backend invocation paths for generation Claude execution versus shared Claude wrapper.
- Markdown remains the main contract for queue rows, review handoffs, design reports, QA, health, and ship notes.
- Active plan truth is spread across `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, receipts, and generated corpora.
- Older specs still mention obsolete command counts even though README and `src/main.rs` now agree on 21 commands.
- Report-only semantics differ across commands; `nemesis --report-only` can still update root planning artifacts.
- Status output is strong in places but not backed by a single machine-readable run manifest for `auto parallel`.
- Verification lint is narrow and does not cover enough shell, `rg`, wrapper, and narrative-proof cases.

## Security Risks

- High: quota account names can escape the profile namespace through `/`, `..`, absolute-like segments, or control characters.
- High: saved `.auto/state.json` planning roots can influence destructive corpus operations unless constrained to the repository root or explicitly supplied.
- High: quota failover after detected progress can duplicate model side effects.
- High: Kimi/PI prompt-in-argv paths can leak prompts on multi-user systems.
- Medium: quota config/state writes are owner-only but not atomic and can follow destination symlinks.
- Medium: declared artifact paths are joined without an explicit containment check, so absolute or parent-relative paths need hard rejection.
- Medium: raw provider refresh stderr/stdout can leak sensitive text if not passed through one sanitizer.
- Medium: automatic dirty checkpointing preserves work, but production scheduling needs tighter ownership boundaries before committing arbitrary dirt.

## Test Gaps

- No test rejects unsafe quota account names across add, capture, remove, select, status, config load, and state load.
- No state containment test covers corrupted `.auto/state.json` with absolute or outside-repo paths.
- No regression proves checked rows plus accepted empty-review convention will not mass-demote unexpectedly.
- No integration test covers last-good plan fallback versus fail-closed production dispatch.
- No lane resume test rejects a changed task body under the same task id.
- No release test proves `auto ship` gates after sync and again after model execution.
- No shared verdict parser test rejects mixed GO/NO-GO or PASS/NO-GO reports.
- No model-free end-to-end fixture harness exercises `qa`, `qa-only`, `health`, `review`, `design --resolve`, `nemesis --report-only`, `ship`, and `audit --everything`.
- No CI check asserts tracked completion artifacts for `[x]` rows still exist.

## Documentation Staleness

- README is closer to current code than older specs and now documents 21 commands.
- Older specs still cite 13, 16, or 17 commands and should be marked historical or refreshed.
- Some archived genesis warnings around `TASK-016` are superseded by current `COMPLETED.md` and receipts.
- Root `DESIGN.md` accurately frames terminal/markdown as the user-facing surface.
- `AGENTS.md` documents checkpoint exclusions but under-documents extra runtime boundaries like `.claude/worktrees` and audit caches.
- Report-only wording should be sharpened for commands that still update root planning artifacts.

## Prior Claim / Plan Status

| Claim or plan family | Current implementation status | Evidence reviewed |
| --- | --- | --- |
| Design gate blockers reconciled | Root rows are checked and `REVIEW.md` is empty; runtime still has verdict parser and ledger-demotion risks. | `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `src/design_command.rs`, `src/completion_artifacts.rs`. |
| Genesis is ready planning input | Current generated corpus is structurally ready as planning input; the degraded pre-refresh state is authoring-pass evidence, not a current filesystem fact. | `find genesis`, `.auto/state.json`, `git ls-tree HEAD genesis`, current corpus shape checks. |
| Root queue ready for auto parallel | No executable unchecked rows exist. New campaign must be promoted first. | `IMPLEMENTATION_PLAN.md`, `REVIEW.md`. |
| Quota safety hardened | Partially true: symlink/source copying is hardened; account-name and retry-after-progress gaps remain. | `src/quota_config.rs`, `src/quota_accounts.rs`, `src/quota_exec.rs`. |
| Receipt freshness bound to release evidence | Partially true: strong checks exist; release ordering and duplicated logic remain risks. | `src/completion_artifacts.rs`, `src/ship_command.rs`. |
| CI production baseline | Strong for Rust and help smoke; lacks end-to-end model-free workflow harness. | `.github/workflows/ci.yml`. |
| Archived genesis plan set | Useful historical sequencing, not current truth. Some release-ledger claims are superseded. | `.auto/fresh-input/genesis-previous-20260430-180207/`. |

## Code Review Coverage

Direct line reads or targeted source review covered:

- Entry and CLI routing: `src/main.rs`, `Cargo.toml`, `build.rs`, `.github/workflows/ci.yml`.
- Corpus and generation: `src/generation.rs`, `src/corpus.rs`, `src/state.rs`.
- Scheduler and task truth: `src/parallel_command.rs`, `src/loop_command.rs`, `src/super_command.rs`, `src/steward_command.rs`, `src/task_parser.rs`, `src/completion_artifacts.rs`, `src/verification_lint.rs`, `tests/parallel_status.rs`.
- Backend and quota: `src/backend_policy.rs`, `src/claude_exec.rs`, `src/codex_exec.rs`, `src/codex_stream.rs`, `src/kimi_backend.rs`, `src/pi_backend.rs`, `src/quota_config.rs`, `src/quota_accounts.rs`, `src/quota_exec.rs`, `src/quota_state.rs`, `src/quota_selector.rs`, `src/quota_patterns.rs`, `src/quota_status.rs`, `src/quota_usage.rs`.
- Quality and release: `src/audit_everything.rs`, `src/audit_command.rs`, `src/qa_command.rs`, `src/qa_only_command.rs`, `src/review_command.rs`, `src/design_command.rs`, `src/nemesis.rs`, `src/book_command.rs`, `src/health_command.rs`, `src/ship_command.rs`, `src/audit_rubric.md`.
- Shared utilities: `src/util.rs`.
- Docs and ledgers: `README.md`, `DESIGN.md`, `AGENTS.md`, `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `COMPLETED.md`, root specs, backend decision docs, and the archived previous genesis snapshot.

## Assumption Ledger

| Assumption | Status | Proof or next proof |
| --- | --- | --- |
| The repo is Codex-first for current instructions. | Verified. | `AGENTS.md` exists and was provided; no root `CLAUDE.md` governance override was found. |
| Active planning truth is root ledgers, not `genesis/`. | Verified. | Root `DESIGN.md`; no root `PLANS.md` or root `plans/` directory. |
| `genesis/` needs review before promotion. | Verified. | Current `genesis/` has mandatory top-level files and 12 numbered plans, while root ledgers remain active truth. |
| Latest CI passed at current HEAD. | Reported by git-history reviewer, not locally rerun yet in this pass. | Re-run or check GitHub if release-critical. |
| Quota profile traversal is exploitable. | Verified from code path shape; needs test reproduction. | Add failing tests around unsafe account names. |
| Empty-review checked rows can mass-demote. | Strong code inference; needs regression test. | Build fixture with current ledgers and run completion audit. |
| `auto ship` gate ordering can accept stale proof after sync. | Verified from code order. | Add release fixture test. |
| Full production launch is safe after corpus generation. | Not proven. | Requires numbered plans and root queue promotion gate. |

## Focus Response

The operator focus correctly emphasizes release blockers, operator trust, verification evidence, first-run DX, scheduler safety, runtime/design sync, and maintainable execution contracts. The code supports the direction: `auto` already has substantial scheduler, receipt, audit, and release machinery. The code also says the next move is not blind parallel execution. The current root queue has no open rows, the generated corpus is subordinate until promoted, and high-severity safety gaps remain in quota/profile paths, saved state, completion evidence conventions, and release-gate ordering.

Non-focused risks that still outrank some focus items:

- Quota account path traversal outranks terminal copy polish.
- Saved planning-root containment outranks new generation throughput.
- Checked-row/empty-review demotion risk outranks adding more queue rows.
- Release-gate ordering outranks final release narrative.

## Opportunity Framing

Strongest direction: preserve `auto corpus`, `auto gen`, `auto super`, and `auto parallel` as the control primitives, but make their state, evidence, and release contracts deterministic before scaling execution. This uses the repo's real leverage: a mature CLI, CI, receipt machinery, report-only guards, and audit manifests.

Rejected direction: build a web dashboard now. The repo's actual user surface is terminal output and markdown ledgers; a dashboard would duplicate truth before truth is settled.

Rejected direction: run `auto parallel` immediately from current root state. The active queue is empty and the next campaign needs promotion; launching lanes would operate on stale or absent work.

Rejected direction: replace markdown ledgers wholesale with a database. That may be a future architecture discussion, but the next 14 days should harden the current contracts and add machine-readable manifests where risk is highest.

Rejected direction: docs-only cleanup. It would improve confidence language but would not fix path containment, demotion risk, receipt freshness, or release ordering.

## DX Assessment

First-run experience is credible but heavy. The repo has clear build commands, CI install smoke, `auto --help`, and `auto doctor`, but README length and command breadth make uncertainty high at T0. The fastest honest path should be: install, run `auto --version`, run `auto doctor`, run a non-mutating status or verify-only command, and see exactly which external tools or credentials are missing. Error clarity is good in several host-side gates but inconsistent across backend invocation paths. The onboarding examples are mostly honest about real work, but they need a compact learn-by-doing path that distinguishes report-only, mutating, queue-promoting, and release commands.
