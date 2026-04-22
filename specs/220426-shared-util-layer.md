# Specification: Shared util layer — git, atomic write, checkpoint, repo layout

## Objective

Preserve `src/util.rs` as the single home for cross-command primitives: git repo-root resolution, remote sync and rebase, auto-checkpoint with excluded paths, atomic file writes, repo layout guarantees, and timestamp slugging. Every command reads from this layer for those concerns. Future consolidation (branch-picker, reference-repo discovery, prompt-log helper) lands here rather than growing per-command duplicates.

## Evidence Status

### Verified facts (code — `util.rs`)

- File is 1,138 lines in the reviewed checkout.
- `CLI_LONG_VERSION` (`src/util.rs:9-17`) concatenates `CARGO_PKG_VERSION`, `AUTODEV_GIT_SHA`, `AUTODEV_GIT_DIRTY`, `AUTODEV_BUILD_PROFILE`.
- `CHECKPOINT_EXCLUDE_RULES` (`src/util.rs:54-60`): `CheckpointExcludeRule::Root(".auto")`, `PathPrefix(".claude/worktrees")`, `Root("bug")`, `Root("nemesis")`, `TopLevelPrefix("gen-")`.
- Public functions verified in `src/util.rs`:
  - `git_repo_root()` (line 62) via `git rev-parse --show-toplevel`.
  - `git_stdout()` (77).
  - `run_git()` (97).
  - `git_status_short_filtered()` (118) — applies `CHECKPOINT_EXCLUDE_RULES`.
  - `repo_name()` (125).
  - `auto_checkpoint_if_needed()` (133).
  - `sync_branch_with_remote()` (197) — runs `git pull --rebase --autostash origin <branch>` when the remote branch exists.
  - `push_branch_with_remote_sync()` (239).
  - `ensure_repo_layout()` (324) — creates `.auto/` and `.auto/logs/`.
  - `timestamp_slug()` (383) — format `YYYYMMDD-HHMMSS` via `Utc::now().format("%Y%m%d-%H%M%S")`.
  - `atomic_write()` (404) — writes to `.<filename>.tmp-<pid>-<nanos>` and renames; cleans up on failure.
  - `copy_tree()` (445).
  - `list_markdown_files()` (476).
  - `prune_old_entries()` (503).
  - `truncate_file_to_max_bytes()` (538).
  - `opencode_agent_dir()` (551) — `.auto/opencode-data/`.
  - `prune_pi_runtime_state()` (558).
  - `clear_and_recreate_dir()` (582).
- Temp-file recovery: `atomic_write_failure` cleanup runs on both write and rename failures (NEM-F4, verified in `COMPLETED.md`), with two tests covering both paths.
- `ensure_repo_layout_with` collects all failures rather than short-circuiting (NEM-F6, verified).
- Inline tests begin at `src/util.rs:608` and cover checkpoint exclusions, staging, remote sync/rebase, `push_branch_with_remote_sync`, and `atomic_write` cleanup (`src/util.rs:714-1091`). The remaining util-layer gap is not "are there tests?" but the missing-parent / non-git tmpdir / rapid-collision coverage tracked by TASK-014.

### Verified facts (cross-file usage)

- Checkpoint is called by mutating commands (`loop`, `qa`, `review`, `ship`, `bug`, `nemesis`, `parallel`, `audit`) per `corpus/SPEC.md` item 3.
- Rebase-before-work and rebase-before-push same callers via `sync_branch_with_remote` / `push_branch_with_remote_sync` (`corpus/SPEC.md` item 5).
- `atomic_write` is the uniform write path for persistent artifacts (`corpus/SPEC.md` item 4).

### Recommendations (corpus)

- Extract duplicated branch-resolution, reference-repo discovery, and prompt-log-path helpers into `util.rs` (or a new small module) per `corpus/plans/007-shared-util-extraction.md`. Current duplicates:
  - Branch resolution: `loop_command.rs`, `review_command.rs`, `parallel_command.rs`, `bug_command.rs` each reimplement "current branch → `origin/HEAD` → `main`/`master`/`trunk`".
  - Reference-repo discovery: `generation.rs`, `loop_command.rs`, `parallel_command.rs`, `review_command.rs`, `bug_command.rs` each copy `resolve_reference_repos` / `discover_sibling_git_repos`.
  - Prompt logging: `loop_command.rs`, `parallel_command.rs`, `review_command.rs`, `bug_command.rs`, `nemesis.rs`, `audit_command.rs` each write `.auto/logs/<command>-<timestamp>-prompt.md` with their own helper.
- Harden `timestamp_nanos_opt().unwrap_or_default()` usage at `src/util.rs:413` so the atomic-write temp-name never collides at `0` under pathological system clocks (`corpus/ASSESSMENT.md` §"Tech debt inventory").

### Hypotheses / unresolved questions

- Whether `CheckpointExcludeRule` variants are the complete grammar or get extended later (for example, glob support) is not source-verified in this pass; the enum today is `Root`, `PathPrefix`, `TopLevelPrefix`.
- Whether `ensure_repo_layout` should add directories for any newer command (`audit/`, `nemesis/`) is not source-verified; those commands may create their own dirs on first write.

## Acceptance Criteria

- `util::git_repo_root()` returns the repo toplevel for any cwd inside a Git work tree and returns an error with a clear "not inside a git repo" message otherwise.
- `util::auto_checkpoint_if_needed()` stages and commits tracked + untracked changes outside `CHECKPOINT_EXCLUDE_RULES` with a machine-parseable commit message prefix (for example, `autodev: ...`); no-ops cleanly when the worktree is clean.
- `CHECKPOINT_EXCLUDE_RULES` remains the single declared list of checkpoint exclusions (`.auto`, `.claude/worktrees`, `bug`, `nemesis`, `gen-*`). Adding a new disposable dir requires updating this constant, not adding ad-hoc filters in callers.
- `util::sync_branch_with_remote()` runs `git pull --rebase --autostash origin <branch>` when `origin/<branch>` exists; if it does not exist, the function is a no-op without error.
- `util::push_branch_with_remote_sync()` calls `sync_branch_with_remote()` once before pushing; push failures surface with actionable context.
- `util::atomic_write()` writes to a temp file named `.<filename>.tmp-<pid>-<nanos>` and `rename()`s to the final path on success; on failure (write or rename), the temp file is removed.
- `util::timestamp_slug()` returns a string matching `YYYYMMDD-HHMMSS` for stable lexicographic ordering at one-second precision.
- `util::ensure_repo_layout()` creates `.auto/` and `.auto/logs/` (and any other always-required dirs) and collects failures into a single error surface (NEM-F6 behavior retained).
- Callers for branch resolution, reference-repo discovery, and prompt logging may move to shared helpers in `util.rs` under a follow-on plan; until then, no caller is allowed to silently diverge on the marker set or path conventions.
- `CLI_LONG_VERSION` continues to embed package version + SHA + dirty flag + profile; `auto --version` renders them as four labeled lines.

## Verification

- `cargo test -p autodev util` passes all existing tests (30+ per corpus claim; run and verify).
- Add (if missing) a test that calls `atomic_write` with a target path whose parent does not exist and asserts the parent directory is created and the final file is written.
- Add a test for `sync_branch_with_remote` on a repo with no `origin/<branch>`; assert a clean no-op return.
- Add a test covering the temp-filename collision surface: call `atomic_write` twice in rapid succession on the same path; assert both complete without leaving stray `.tmp-` files.
- Negative test: `git_repo_root` from a tmpdir that is not a Git repo returns the documented error.

## Open Questions

- Should `CheckpointExcludeRule` gain glob support, or does the current `Root` / `PathPrefix` / `TopLevelPrefix` set cover every realistic exclusion?
- Should `timestamp_slug()` include milliseconds or nanoseconds as a tiebreaker to guarantee uniqueness for multiple archive/prompt paths created in the same second?
- Should `ensure_repo_layout` create command-specific dirs (`audit/`, `nemesis/`, `bug/`) eagerly, or leave that to the commands that write them?
- Is there value in a `util::atomic_append` primitive for the append-only artifacts (`WORKLIST.md`, `LEARNINGS.md`, `ARCHIVED.md`)?
- Should `util.rs` be split into `util/git.rs`, `util/fs.rs`, `util/log.rs` once Plan 007 lands, or stay one file until it crosses a concrete LOC threshold?
