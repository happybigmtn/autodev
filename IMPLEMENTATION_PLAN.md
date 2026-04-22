# IMPLEMENTATION_PLAN

Verified current-state baseline (2026-04-22, branch `main`):
- `cargo test`: 333 passed (single suite)
- `cargo clippy --all-targets --all-features -- -D warnings`: clean
- `cargo fmt --check`: clean
- `auto --version`: prints exactly `auto <ver>` / `commit:` / `dirty:` / `profile:` (4 lines)
- 16 CLI commands wired (`src/main.rs:52-96`); `.github/` and `audit/` directories absent; `steward/` empty.

## Priority Work

- [~] `TASK-002` Add `### auto steward`, `### auto audit`, `### auto symphony` detailed-guide subsections to README

    Spec: `specs/220426-readme-truth-pass.md`
    Why now: `README.md` has detailed `###` subsections for the 13 historical commands (`README.md:86-849`) but none for the three commands added since the README was last truthful. Operators currently have no entry-point doc for these three commands.
    Codebase evidence: existing subsection headers at `README.md:86,180,258,303,374,442,483,567,607,677,724,770,849`; concrete artifact lists in `src/steward_command.rs:21-28` (DRIFT/HINGES/RETIRE/HAZARDS/STEWARDSHIP-REPORT/PROMOTIONS), `src/audit_command.rs:131-138` (verdict shape), `src/main.rs:834` (audit doctrine default `audit/DOCTRINE.md`), `src/symphony_command.rs:402-417` (subcommand entry), `src/main.rs:111-118` (symphony Sync/Workflow/Run).
    Owns: `README.md`
    Integration touchpoints: none
    Scope boundary: three new subsections (`### auto steward`, `### auto audit`, `### auto symphony`) inserted in the same Purpose/What it reads/What it produces/Defaults pattern used by existing subsections. Each must list the actual deliverables/artifact paths from the source-verified evidence above. No edits to Defaults section, no edits to PR/CI prose.
    Acceptance criteria: each new subsection has Purpose, What it reads, What it produces (with concrete file paths from the evidence), and Defaults; `auto steward` subsection lists all six STEWARD_DELIVERABLES; `auto audit` subsection states the doctrine default `audit/DOCTRINE.md` and the six verdict variants `CLEAN`/`DRIFT-SMALL`/`DRIFT-LARGE`/`SLOP`/`RETIRE`/`REFACTOR`; `auto symphony` subsection enumerates the three subcommands with one-line purposes.
    Verification: `rg -n '^### .*auto (steward|audit|symphony)' README.md`; `rg -n 'DRIFT.md|HINGES.md|RETIRE.md|HAZARDS.md|STEWARDSHIP-REPORT.md|PROMOTIONS.md' README.md`; `rg -n 'audit/DOCTRINE.md' README.md`.
    Required tests: none (docs-only)
    Completion artifacts: `README.md`
    Dependencies: TASK-001
    Estimated scope: S
    Completion signal: three new subsections present and grep checks pass.

- [~] `TASK-014` Add tmpdir / missing-parent / rapid-collision regression tests for `util::atomic_write`

    Spec: `specs/220426-shared-util-layer.md`
    Why now: spec asks for three explicit test cases on `atomic_write` (tmpdir-not-a-git-repo behavior, missing parent dir auto-create, rapid-succession collision tiebreaker). Existing tests at `src/util.rs:1032-1091` cover only rename-failure cleanup and write-failure cleanup. The collision-tiebreaker test is the one most likely to catch a real bug — the temp filename uses `Utc::now().timestamp_nanos_opt().unwrap_or_default()` (`src/util.rs:413`), which can collide if two threads call into the same directory in the same nanosecond on systems where `timestamp_nanos_opt` returns `None`.
    Codebase evidence: `src/util.rs:404-426` (atomic_write), `src/util.rs:413` (`unwrap_or_default()` on nanos), `src/util.rs:1032-1091` (existing tests).
    Owns: `src/util.rs`
    Integration touchpoints: none (test-only addition)
    Scope boundary: add three tests inside the existing `#[cfg(test)] mod tests` block. Do NOT change `atomic_write` runtime behavior; if the rapid-collision test surfaces a real bug, open a separate task to fix it. Stay within the standard library and `tempfile`-style temp-dir creation already used elsewhere in the file.
    Acceptance criteria: tests exercise the three scenarios; the missing-parent test confirms `atomic_write` calls `create_dir_all`; the rapid-collision test spawns a small fixed number of threads writing to the same path and verifies all writes complete and the final file matches the last writer's bytes (or — if the test surfaces a deterministic collision — fails clearly so we file a real-fix task).
    Verification: `cargo test util::tests::atomic_write_creates_missing_parent_dir`; `cargo test util::tests::atomic_write_handles_rapid_succession_collisions`; `cargo test util::tests::atomic_write_works_outside_git_repo`.
    Required tests: `atomic_write_creates_missing_parent_dir`, `atomic_write_handles_rapid_succession_collisions`, `atomic_write_works_outside_git_repo`
    Completion artifacts: `src/util.rs`
    Dependencies: TASK-011
    Estimated scope: S
    Completion signal: three new tests pass; spec acceptance for the shared util layer test ask is closed.

- [~] `TASK-016` Tag `v0.2.0` once the priority + first follow-on cluster is verified clean

    Spec: `specs/220426-release-ship.md`
    Why now: `Cargo.toml` is still on `0.1.0`; once the visible drift (README, CI, dead code, hardening) is closed, cutting a `0.2.0` annotated tag locks the verified baseline. Spec frames this as a preservation contract; the only new work here is the actual tag.
    Codebase evidence: `Cargo.toml:3` (`version = "0.1.0"`), `build.rs:8-62` provenance already wired, `src/util.rs:9-17` `CLI_LONG_VERSION`.
    Owns: `refs/tags/v0.2.0`
    Integration touchpoints: `Cargo.toml`, `Cargo.lock`, `COMPLETED.md`.
    Scope boundary: bump `Cargo.toml` version to `0.2.0`, regenerate `Cargo.lock`, append a `## v0.2.0` section to `COMPLETED.md` summarizing the closed task IDs, and create the annotated tag locally. Do NOT push the tag in this task — `auto ship` (or a separate operator step) handles publishing and PR plumbing per spec.
    Acceptance criteria: `Cargo.toml` reads `version = "0.2.0"`; `cargo build` regenerates `Cargo.lock` cleanly; `git tag -l v0.2.0` returns `v0.2.0`; the tag's annotation message lists TASK-001..TASK-011 plus any closed follow-ons; `auto --version` first line reads `auto 0.2.0`.
    Verification: `cargo build && ./target/debug/auto --version | head -1` (must read `auto 0.2.0`); `git tag -l v0.2.0` returns `v0.2.0`; `git cat-file -p v0.2.0` shows annotated message with task list.
    Required tests: none (release-mechanics only; `cargo test` regression already covered by prior checkpoints)
    Completion artifacts: `Cargo.toml`, `Cargo.lock`, `COMPLETED.md`, `refs/tags/v0.2.0`
    Dependencies: TASK-011
    Estimated scope: S
    Completion signal: annotated `v0.2.0` tag exists locally with the closed task list, `auto --version` confirms the bump.

## Completed / Already Satisfied

- `specs/220426-cli-command-surface.md` — 16-command CLI surface verified at `src/main.rs:52-96`; nested `Quota` (`Status`/`Select`/`Accounts`/`Reset`/`Open` plus `AccountsCommand` `Add`/`List`/`Remove`/`Capture`) and `Symphony` (`Sync`/`Workflow`/`Run`) trees confirmed at `src/main.rs:289-340` and `:111-118`; `auto --version` emits exactly four lines (`auto <ver>` / `commit:` / `dirty:` / `profile:`) per `src/util.rs:9-17` and direct binary check; both unit tests in `src/main.rs:1190-1213` pass.
- `specs/220426-shared-util-layer.md` (preservation half) — every named util function is present at the expected line in `src/util.rs` (`git_repo_root` :62, `auto_checkpoint_if_needed` :133, `sync_branch_with_remote` :197, `push_branch_with_remote_sync` :239, `ensure_repo_layout` :324, `timestamp_slug` :383, `atomic_write` :404), `CLI_LONG_VERSION` :9, `CHECKPOINT_EXCLUDE_RULES` :54 (5 elements). Test file already covers checkpoint exclusion, atomic_write rename/write failure cleanup, sync rebase semantics, and `auto_checkpoint_if_needed` conflict-recovery (`src/util.rs:903-1099`). Remaining test gaps tracked in TASK-014.
- `specs/220426-bug-and-nemesis-hardening.md` (current behavior) — bug defaults verified at `src/main.rs:485-572` (Kimi `k2.6` `high` finder/skeptic/reviewer/fixer, `gpt-5.4` `high` finalizer, `--use-kimi-cli` default `true`, `chunk_size` 24); Nemesis defaults verified at `src/main.rs:1055-1138` and `src/nemesis.rs:637-710` (Kimi `k2.6` `high` audit/synthesis/fixer via `kimi-cli`, Codex `gpt-5.4` `high` finalizer, `--minimax` legacy opt-in); archive-then-wipe via `prepare_output_dir`/`maybe_prepare_output_dir`/`annotate_output_recovery` and `--report-only` implementation/finalizer short-circuit are recorded as fixed in `COMPLETED.md`. Spec's open questions (`--resume`, Codex 137 handling, report-only root-sync semantics) remain explicit hypotheses and stay off the priority queue.
- `specs/220426-build-provenance-and-ci-bootstrap.md` (part a only) — `build.rs` correctly emits `AUTODEV_GIT_SHA`, `AUTODEV_GIT_DIRTY`, `AUTODEV_BUILD_PROFILE` with rerun triggers on `.git/HEAD`, `packed-refs`, branch ref, `index`; tarball-without-`.git` fallback returns `"unknown"` (verified at `build.rs:8` and `:46-62`); `--version` output confirms three env vars all populated. CI workflow (part b) is queued as TASK-007/008.
- `specs/220426-agent-backend-execution.md` (current behavior) — `claude_exec`, `codex_exec`, `kimi_backend`, `pi_backend` wrappers all expose the spec'd entry points (`run_claude_exec` `:19`, `run_claude_with_futility` `:46`, `run_codex_exec` `:30`, `run_codex_exec_with_env` `:56`, kimi/pi resolve helpers); futility marker `137` and thresholds `8`/`16` (`src/codex_stream.rs:16,22`); kimi defaults (`KIMI_CLI_DEFAULT_MODEL`/`KIMI_CLI_MODEL_ENV`) and PI defaults present. Cleanup of `codex_exec.rs` dead tmux scaffold queued as TASK-003. `LlmBackend` trait is research-only and intentionally not queued.
- `specs/220426-artifact-formats-and-task-queue.md` — `util::atomic_write` matches spec exactly (`.{filename}.tmp-{pid}-{nanos}` pattern, parent auto-create, temp cleanup on both write and rename failure); plan-marker contract `- [ ]`/`- [!]`/`- [x]`/`- [~]` honored across `loop_command`, `parallel_command`, `review_command`. The shared task-marker helper and shared writer trait remain spec-flagged research and stay off this queue.
- `specs/220426-audit-doctrine-pass.md` (runtime behavior) — six verdict variants dispatched at `src/audit_command.rs:797-889`; doctrine missing-file bail at `:164-173`; `--use-kimi-cli` requirement bail at `:1050`; manifest hashes (`audited_doctrine_hash`/`audited_rubric_hash`) and resume modes (`Fresh`/`Resume`/`OnlyDrifted`) implemented at `:223-253`; per-file artifacts under `audit/files/<sha256[..16]>/` confirmed at `:732-734`. Test-coverage gap queued as TASK-004; `touched_paths`/`escalate` consumption explicitly deferred per spec.
- `specs/220426-execution-loop-and-parallel.md` (current behavior) — `auto loop` queue parsing, branch-resolution, sync-before-rebase, sync-before-push, `auto_checkpoint_if_needed`, `--max-iterations` exit, blocked-skip semantics all in `src/loop_command.rs`; `auto parallel` 5-lane tmux orchestration, status command, and verification-receipt path resolution via `completion_artifacts::inspect_task_completion_evidence` all in `src/parallel_command.rs`. Loop-side receipt enforcement decision queued as TASK-012.
- `specs/220426-planning-pipeline-corpus-gen-reverse.md` — `PlanningCorpus` shape matches spec (`src/corpus.rs:10-23`); `REQUIRED_PLAN_SECTIONS` constants, archive-then-wipe to `.auto/fresh-input/`, gen filename pattern `ddmmyy-<slug>[-counter].md`, and three-section plan validator all live in `src/generation.rs` (lines 109-115, 668, ~1754); reverse never mutates `IMPLEMENTATION_PLAN.md`.
- `specs/220426-quality-commands-qa-health-review.md` — `qa_command` and `review_command` wire `sync_branch_with_remote` / `push_branch_with_remote_sync`, while `qa_only_command` and `health_command` intentionally remain report-only wrappers with no rebase/push today; all four default to `gpt-5.4` `high` where applicable, QA/QA-only default to `standard` tier, write prompt logs through `atomic_write`, and `review_command::run_review` drains `COMPLETED.md` → `REVIEW.md` → `ARCHIVED.md`/`WORKLIST.md`/`LEARNINGS.md` with stale-batch detection via inline `HashMap<Vec<String>, usize>` (`src/review_command.rs:213,241`). The spec's `--json` output, `auto health` tier knob, and report-only rebase decision are explicit research items.
- `specs/220426-quota-routing-and-credential-hardening.md` (rotation half) — multi-account multiplexer, `fd_lock`-serialized swaps, `WEEKLY_FLOOR_PCT=10` / `SESSION_FLOOR_PCT=25` / `EXHAUSTION_COOLDOWN_HOURS=1` floors, primary-account preference with session-headroom fallback, and `auto quota select` persistence are all in place. Hardening half (perm tightening + body scrubbing) queued as TASK-005/006/015. Encryption-at-rest is explicitly out of scope per spec.
- `specs/220426-release-ship.md` — `auto ship` resolves base branch via `--base-branch`/`origin/HEAD`/`KNOWN_PRIMARY_BRANCHES`, runs Codex prep with the documented prompt, writes prompt log via `atomic_write`, syncs and pushes per `sync_branch_with_remote`/`push_branch_with_remote_sync`, and one regression test `default_ship_prompt_includes_operational_release_controls` is in place (`src/ship_command.rs`). Spec acceptance items that depend on the Codex agent's prompt-time behavior (atomic SHIP.md write, gh advisory, codex hard-fail) are noted as prompt-contract items rather than enforceable Rust gates.
- `specs/220426-steward-mid-flight-reconciliation.md` (current behavior) — six STEWARD_DELIVERABLES enumerated at `src/steward_command.rs:21-28`; `--skip-finalizer` short-circuit at `:163-167`; `auto_checkpoint_if_needed` invocations at `:88-96`, `:153-161`, `:202-213`; prompt log via `atomic_write` at `:106-112`; `verify_steward_deliverables` post-check at `:269-285`. The "no planning surface" enforcement gap is queued as TASK-010; deliverable-write atomicity is a Codex-side concern, not a Rust gate.
- `specs/220426-symphony-linear-orchestration.md` (current behavior) — three subcommands wired (`Sync`/`Workflow`/`Run`); GraphQL queries and four drift categories (`missing`/`stale`/`terminal`/`completed_active`) implemented in `src/linear_tracker.rs:16-263`; sync planner, workflow renderer, and foreground orchestrator implemented in `src/symphony_command.rs`. Hardcoded operator path queued as TASK-009; GraphQL surface dedupe decision queued as TASK-013.
- `specs/050426-nemesis-audit.md` (NEM-F1..NEM-F10) — all ten findings recorded as resolved in `COMPLETED.md` with primary fix commit `2079927`.
