# Repository Assessment

## Executive Summary

Autodev says it is a lightweight repo-root planning and execution toolchain for AI-assisted development. The code shows something more consequential: a Rust autonomous development control plane with model-backed planning, generation, review, design, QA, audit, parallel execution, quota routing, completion receipts, and release gating.

How might we make this control plane production-ready without losing the operator sovereignty and evidence discipline that make it useful? The answer is not a new product layer. It is to harden the existing runtime truth: credentials, corpus generation, dependency parsing, receipts, root-plan reconciliation, and release proof.

## Target Users, Success Criteria, and Constraints

Target users:

- Operators running real repositories through `auto corpus`, `auto gen`, `auto parallel`, `auto review`, `auto audit`, and `auto ship`.
- Engineering leads who need durable evidence and a trustworthy queue.
- Agent workers receiving prompts, task contracts, and write boundaries.
- Maintainers releasing the `auto` binary itself.

Success looks like:

- A fresh operator can install, run `auto doctor`, understand required tools, and see a meaningful success moment.
- A corpus run cannot destroy the only planning root on failure.
- A parallel run cannot execute dependency-blocked work or corrupt credentials.
- A completion claim has receipts tied to the current tree and artifacts.
- A release gate catches stale docs, stale receipts, dirty trees, missing install proof, and incomplete review handoff.

Constraints:

- Rust CLI, binary name `auto`, package version currently `0.2.0`.
- Runtime/generated state includes `.auto/`, `bug/`, `nemesis/`, and `gen-*`.
- Source-controlled planning truth currently lives in root planning docs and dated specs, not root `PLANS.md`.
- Model backends intentionally use powerful edit permissions, so prompt provenance, state isolation, and receipts are the real trust boundary.

## What It Says vs What Code Shows

| Area | Claim | Code reality | Assessment |
| --- | --- | --- | --- |
| Product identity | Lightweight planning/execution toolchain | 21-command autonomous control plane | README top-level framing should be updated to match runtime ambition. |
| Command count | Older specs say 16 or 17 | `src/main.rs` and README show 21 | Older specs and previous snapshots are stale. |
| First-run proof | Some dated specs say no doctor/install proof | `auto doctor` and CI install smoke exist | Specs need reconciliation. |
| Planning root | Previous genesis snapshot existed | `genesis/` was empty/deleted before this refresh, and failed corpus generation can leave only a skeleton `genesis/plans/` directory | Corpus rollback and non-empty validation are production blockers. |
| Active queue | Root implementation plan plus worklist | `IMPLEMENTATION_PLAN.md` has stale/partial rows; `WORKLIST.md` has live evidence tasks; `COMPLETED.md` is empty | Need root truth reconciliation before parallel launch. |
| Release proof | `auto ship` gates release | Receipts are not bound to current tree/artifact hashes | Good gate, insufficient freshness proof. |

## What Works

- Central CLI routing and help generation in `src/main.rs`.
- Stronger `auto gen` task contract validation in `src/generation.rs`.
- Shared parser foundation in `src/task_parser.rs`.
- `auto parallel` has tmux lane orchestration, status, preflight, salvage, drift audit, and worker prompt discipline.
- Receipt writer captures output tails and zero-test summaries.
- Completion evidence rejects zero-test executable receipts.
- `auto ship` has a mechanical release gate and bypass reason trail.
- `auto doctor` gives a no-model first-run preflight.
- CI runs format, clippy, tests, locked install, and selected help smoke tests.
- Quota capture now rejects symlinked credential files and writes sensitive files owner-only.
- Symphony workflow rendering rejects hostile branch/model/effort values.

## What Is Broken

- Quota profile paths interpolate raw account names, allowing path traversal risks.
- Quota credential swaps are not locked for the child process lifetime.
- `auto corpus` can leave a skeleton `genesis/plans/` after interruption or model failure.
- Planning roots with empty `plans/` directories can be accepted later.
- Dependency parsing misses common bare task references and treats missing dependency IDs as resolved.
- Symphony plan reconciliation can corrupt partial rows.
- Salvage notes can point at lane repos that have since been reset.
- Receipts are not tied to current commit, dirty state, plan hash, or artifact hashes.
- `auto review` can write plan/review docs before branch validation.
- `auto loop` can choose dependency-blocked work as first actionable.

## What Is Half-Built

- Backend invocation policy exists as an inventory but is not a full drift guard against actual spawn sites.
- `auto doctor` is useful but does not cover all high-value help surfaces or structured status.
- Report-only/dry-run semantics are inconsistent across corpus/spec/design/health/qa/review surfaces.
- `auto audit --everything` is mature, but status/pause/unpause can create run state and remediation can break dependency ordering.
- `nemesis` exposes `audit_passes`, but the reviewed code does not use it as a real multi-pass runtime control.
- Release gating is strong in shape but weak on proof freshness.

## Tech Debt Inventory

- Very large modules, especially `src/parallel_command.rs`, concentrate scheduler, lane, salvage, prompt, and report logic.
- Dated specs encode historical truth next to current truth without an explicit staleness marker.
- Multiple commands maintain similar report-only/write-boundary semantics independently.
- Task schema enforcement differs across `auto spec`, `auto gen`, `auto super`, `auto loop`, Symphony, and review flows.
- Runtime state paths, durable artifacts, and source-controlled control docs are explained in scattered places.
- Model/default observability can hardcode GPT-5.5 xhigh in review prompts even when CLI overrides are supplied.
- Filesystem copy/archive helpers need stronger symlink-boundary semantics before they are used as production recovery primitives.

## Security Risks

- High: quota account names are not path-bounded before becoming profile paths used by add/remove/capture/select flows.
- High: quota credential activation is global and not protected for the full model child process lifetime.
- Medium: Claude activation can leave mixed active credentials when the selected profile lacks files present in the active directory.
- Medium: `auto quota open` does not classify quota/auth failures or rotate the way shared wrappers do.
- Medium: repository archive/copy helpers can follow symlinks while copying planning or runtime trees.
- Medium: Kimi and PI prompt transport puts full prompts in argv, exposing sensitive repo prompts to process listings and risking argv-size failures.
- Medium: dangerous model execution flags are intentional but increase the importance of prompt boundaries, dirty-state checks, and receipts.

## Test Gaps

- Quota path traversal, provider-lock lifetime, mixed active credential cleanup, and quota-open retry behavior.
- Corpus rollback after model failure and rejection of empty planning roots.
- Corpus/archive symlink behavior and root-boundary preservation.
- Bare dependency references, external dependency semantics, missing dependency blockers, loop dependency filtering, and audit cycle-breaker dependency safety.
- Symphony marking `[~]` rows, git-ref completion artifacts, review stale follow-up parser visibility, and review branch-mismatch no-write behavior.
- Receipt current-commit mismatch, dirty-state mismatch, artifact hash mismatch, and corrupted receipt handling.
- Actual shell/Python receipt wrapper execution in CI.
- Report-only write-boundary enforcement, required `QA.md`/`HEALTH.md` output, and dry-run preview semantics across commands.
- Super gate parity with the richer `auto gen` task contract.

## Documentation Staleness

- `specs/220426-cli-command-surface.md` is stale where it describes the older command count.
- `specs/230426-first-run-ci-and-installed-binary-proof.md` is stale where it says there is no doctor/install proof.
- `specs/230426-planning-corpus-and-generation.md` contains older planning-surface assumptions.
- `docs/decisions/backend-invocation-policy.md` does not fully cover newer command modules.
- `README.md` is mostly current at the top, but deeper design and CI sections lag current commands and exact CI help probes.
- `IMPLEMENTATION_PLAN.md` still contains stale release/version evidence around `TASK-016`.

## Prior Claims and Plans

| Claim or plan | Current status | Evidence reviewed | Required action |
| --- | --- | --- | --- |
| Command surface has 16/17 commands | Stale | `src/main.rs`, README | Update specs/docs when promoted. |
| Command surface has 21 commands | Verified | `src/main.rs`, README | Keep as current product truth. |
| `auto doctor` missing | Stale | `src/doctor_command.rs`, local help behavior reported | Reconcile old spec. |
| CI installed binary proof missing | Stale | `.github/workflows/ci.yml` | Reconcile old spec; add missing help/receipt smoke. |
| Quota symlink rejection/owner-only writes | Mostly implemented | `src/quota_config.rs`, `src/quota_exec.rs` | Do not reopen completed work; address remaining path/lease risks. |
| Shared parser blocked-task preservation | Partly implemented | `src/task_parser.rs`, `src/generation.rs` | Extend to dependency truth and all consumers. |
| AD-014 Symphony/receipt checkpoint | Still active evidence task | `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, Symphony/receipt code | Convert to concrete plan slice. |
| TASK-016 v0.2.0 tag | Stale/partial | `Cargo.toml`, git tag, root plan, `COMPLETED.md`, receipt tail | Reconcile root plan and completion artifact semantics; the tag exists, but the active row still says `0.1.0`, `COMPLETED.md` lacks the expected release section, and the tag annotation omits `TASK-014` while archive prose says through `TASK-015`. |
| Previous genesis snapshot | Historical context only | archived `.auto/fresh-input/...` | Do not copy forward as truth. |

## Code Review Coverage

Files and areas read directly or through focused review:

- Root control docs: `AGENTS.md`, `README.md`, `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `ARCHIVED.md`, `REVIEW.md`, `COMPLETED.md`.
- Build and CI: `Cargo.toml`, `Cargo.lock`, `build.rs`, `.github/workflows/ci.yml`.
- Main and commands: `src/main.rs`, `src/spec_command.rs`, `src/generation.rs`, `src/corpus.rs`, `src/design_command.rs`, `src/super_command.rs`, `src/parallel_command.rs`, `src/review_command.rs`, `src/loop_command.rs`, `src/audit_command.rs`, `src/audit_everything.rs`, `src/qa_only_command.rs`, `src/health_command.rs`, `src/nemesis.rs`, `src/ship_command.rs`, `src/symphony_command.rs`, `src/doctor_command.rs`, `src/book_command.rs`, `src/steward_command.rs`.
- State and evidence: `src/task_parser.rs`, `src/completion_artifacts.rs`, `src/state.rs`, `src/util.rs`, `scripts/run-task-verification.sh`, `scripts/verification_receipt.py`, `tests/parallel_status.rs`.
- Backends/security: `src/backend_policy.rs`, `src/codex_exec.rs`, `src/claude_exec.rs`, `src/kimi_backend.rs`, `src/pi_backend.rs`, `src/quota_config.rs`, `src/quota_exec.rs`, `src/quota_accounts.rs`, `src/quota_status.rs`, `src/quota_usage.rs`, `src/quota_patterns.rs`.
- Planning history: all `specs/`, `docs/decisions/`, archived previous genesis snapshot, recent git log and tag state.

Independent review update on 2026-04-30:

- Rechecked repo instructions, README lifecycle text, root queue docs, CI, command definitions, corpus/generation code, quota execution, task parsing, receipts, ship gate, and representative quality-command write boundaries.
- Verified the generated corpus has 12 numbered plans and no absolute repository-root paths.
- Found and corrected non-runnable generated plan commands such as multi-filter `cargo test` invocations and nonexistent `auto gen --dry-run` / `auto ship --dry-run` examples.
- Did not run long integration suites or model-backed commands during this document review.

## Assumption Ledger

| Assumption | Status | Proof or next proof |
| --- | --- | --- |
| `auto` version is `0.2.0` | Verified, with release-ledger drift | `Cargo.toml` and `v0.2.0` tag; root `TASK-016` and `COMPLETED.md` still need reconciliation. |
| Root `PLANS.md` governs ExecPlans | False in current checkout | `rg --files -g 'PLANS.md' -g 'plans/**'` returned none. |
| `genesis/` is active planning truth | False unless promoted | Root docs and state point to root planning files; corpus is subordinate. |
| The old genesis snapshot is accurate | False/stale | It predates command and hardening changes. |
| Quota can safely run parallel lanes | Not proved; risk found | Requires Plan 002. |
| Receipts prove current release readiness | Not proved; risk found | Requires Plan 006. |
| `auto parallel` can be launched safely now | Not proved; likely no | Requires checkpoint plans and root queue reconciliation. |

## Focus Response

The operator focus emphasized production readiness, semantic consistency, scheduler safety, runtime/design sync, resumability, implementation quality, verification receipts, and agent usability. Code reality agrees with the focus: the repository already has the major workflow surfaces, but several trust boundaries are too soft for production-scale parallel execution.

Non-focused risks that outrank polish:

- Credential safety in quota execution.
- Atomicity of the corpus root.
- Dependency parsing correctness.
- Receipt freshness and release evidence binding.

The focus changed plan ordering by moving credential/corpus/scheduler evidence ahead of docs and DX, while still preserving a DX plan because this is a developer-facing CLI.

## Opportunity Framing

Recommended direction: production hardening of the current autonomous control plane.

Rejected direction: build a dashboard or new product shell. Reason: the terminal control plane is already the product surface and has more urgent trust gaps.

Rejected direction: generate a large greenfield backlog. Reason: root planning truth already exists; the useful move is reconciliation and high-leverage hardening.

Rejected direction: immediately execute `auto parallel`. Reason: unsafe credential leases, lossy dependencies, stale receipts, and stale root rows make a launch premature.

Rejected direction: docs-only cleanup. Reason: stale docs matter, but code-level safety defects can cause false execution or credential corruption.

## DX Assessment

First-run friction is improving. `auto doctor` provides a no-model success path, CI installs the binary, and README's top-level command list is close to code truth. The fastest path can produce a meaningful success moment with `auto --version` and `auto doctor`.

The gaps are sharp but tractable:

- Required versus optional tools are described inconsistently.
- Dry-run output sometimes means no writes and sometimes means prompt/state writes without model execution.
- Help smoke tests do not cover all important operator commands.
- Error clarity around stale corpus roots, stale receipts, missing dependencies, and unsafe quota state needs to be more deterministic.
- Copy-paste onboarding should be honest about the real work: planning and execution are evidence-bound workflows, not one-command magic.
