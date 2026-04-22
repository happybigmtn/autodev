# Plan 007 — Shared utilities: branch resolution, reference-repo discovery, prompt logging

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

## Purpose / Big Picture

Three helpers appear nearly identically in four to six modules:

1. **Branch resolution.** "Use the current branch if it is one of `main`, `master`, `trunk`; otherwise consult `origin/HEAD`; otherwise scan for a reasonable default." Present in `loop_command.rs`, `parallel_command.rs`, `review_command.rs`, `bug_command.rs`, `qa_command.rs`, and others. Identical logic, different copies.
2. **Reference-repo discovery.** Auto-discovering sibling git repos under the parent directory, then merging with explicit `--reference-repo` entries. Present in `loop_command.rs`, `parallel_command.rs`, `review_command.rs`, `bug_command.rs`, and `generation.rs`.
3. **Prompt logging.** Every agent-invoking command writes a timestamped prompt log under `.auto/logs/<command>-<timestamp>-prompt.md`. Each module implements its own `log_prompt()`.

Consolidating them into a small set of shared helpers in `src/util.rs` (or a new sibling module) reduces duplicated code, lets bugs be fixed in one place, and gives a canonical answer to "how does `auto` pick a branch" that the README can link to.

The operator gains (a) fewer ways for subtly different branch-picking logic to drift across commands, and (b) a single log-path convention to document. An external observer sees the total source-line count drop by roughly 400-800 LOC and `rg 'fn resolve_loop_branch' src/` return one definition instead of several.

## Requirements Trace

- **R1.** A single function in `src/util.rs` (or a new small module) resolves the working branch according to the documented algorithm: prefer current branch if it is `main`/`master`/`trunk`; otherwise `origin/HEAD`; otherwise scan for `main`/`master`/`trunk`.
- **R2.** A single function in `src/util.rs` resolves the effective reference-repo list: auto-discovered siblings plus `--reference-repo` entries, deduplicated.
- **R3.** A single function in `src/util.rs` writes a prompt log under `.auto/logs/<command>-<timestamp>-prompt.md` with consistent format (header with command, timestamp, repo root; body with the prompt text).
- **R4.** Every caller of the old per-module helpers is updated to use the shared helper. The per-module helper definitions are removed.
- **R5.** Behavior is preserved. Existing tests continue to pass, and new tests for the shared helpers are added.
- **R6.** No public CLI-surface change. The three shared helpers are crate-internal; their introduction is invisible to operators.

## Scope Boundaries

- **Changing:** `src/util.rs`, `src/loop_command.rs`, `src/parallel_command.rs`, `src/review_command.rs`, `src/bug_command.rs`, `src/qa_command.rs`, `src/qa_only_command.rs`, `src/health_command.rs`, `src/ship_command.rs`, `src/audit_command.rs`, `src/generation.rs`, `src/nemesis.rs`, `src/steward_command.rs` — wherever the three helpers appear.
- **Not changing:** the semantics of branch resolution, reference-repo discovery, or prompt logging.
- **Not introducing:** new error types, new logging frameworks, or a new module system. Stay in `src/util.rs` unless the helper grows beyond ~150 LOC combined, in which case a single new file `src/shared_helpers.rs` is acceptable.
- **Not consolidating:** the `LlmBackend` enum duplication between `bug_command.rs` and `nemesis.rs` — that is Plan 008's research scope.

## Progress

- [ ] Inventory the three helper patterns across all command modules.
- [ ] Draft the shared signatures.
- [ ] Implement in `src/util.rs` with dedicated tests.
- [ ] Migrate callers one command at a time.
- [ ] Remove the per-module duplicates after each migration is green.
- [ ] Run `cargo test` and `cargo clippy -D warnings` between migrations.
- [ ] Final commit squash (optional).

## Surprises & Discoveries

None yet. Possible surprise: one module's branch resolution differs subtly (e.g., allows `develop` in addition to `main`/`master`/`trunk`). If so, that is a semantic variation that deserves its own decision in the Decision Log before being consolidated.

## Decision Log

- **2026-04-21 — Consolidate into `util.rs` rather than a new module.** Taste. `util.rs` is already the shared-helpers home; introducing a second module adds organization overhead without benefit. If the file grows past ~1500 LOC after this plan, split at that point.
- **2026-04-21 — Migrate one command per commit.** Taste. Reduces the surface of any one diff and keeps `cargo test` green between steps. Alternative of one monolithic commit is rejected as too risky.
- **2026-04-21 — Preserve current behavior exactly; log semantic variations for a follow-on plan.** Mechanical. If two modules differ (e.g., one tolerates `develop`), the consolidation picks the majority rule and logs the minority as a surprise; product-direction change is out of scope here.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Target call sites (non-exhaustive, to be confirmed by inventory):

- `src/loop_command.rs` — `resolve_loop_branch` or similar; `discover_sibling_git_repos`; prompt-log helper.
- `src/parallel_command.rs` — its own branch and reference-repo logic; prompt log per lane.
- `src/review_command.rs` — branch resolution and reference-repo merge; prompt log.
- `src/bug_command.rs` — branch resolution; prompt log per phase.
- `src/qa_command.rs`, `src/qa_only_command.rs`, `src/health_command.rs`, `src/ship_command.rs` — branch resolution; prompt log.
- `src/nemesis.rs` — branch resolution (inside implementation phase); prompt log.
- `src/audit_command.rs` — prompt log.
- `src/generation.rs` — reference-repo discovery for `auto gen` when a reference repo is declared; prompt log.

Anchors in `src/util.rs`:
- `CHECKPOINT_EXCLUDE_RULES` constant (~line 16)
- `atomic_write` (~lines 404-426)
- `sync_branch_with_remote` (~line 205)
- `push_branch_with_remote_sync` (~elsewhere)

Directory for prompt logs: `.auto/logs/` (created by `ensure_repo_layout_with`).

Commands for inventory:
```
rg -n 'fn .*branch|fn resolve_loop_branch|fn default_branch|fn resolve_working_branch' src/
rg -n 'discover_sibling|sibling_git_repos|resolve_reference_repos' src/
rg -n 'log_prompt|prompt_log|auto/logs' src/
```

## Plan of Work

1. **Inventory.** Run the three ripgreps above and list every definition and call. Identify any semantic variance.
2. **Design signatures.** Propose function signatures in a short doc-comment block inside `src/util.rs`:
   - `pub(crate) fn resolve_working_branch(repo_root: &Path, explicit: Option<&str>) -> Result<String>`
   - `pub(crate) fn resolve_reference_repos(repo_root: &Path, explicit: &[PathBuf]) -> Result<Vec<PathBuf>>`
   - `pub(crate) fn log_prompt(repo_root: &Path, command: &str, prompt: &str) -> Result<PathBuf>`
3. **Implement.** Write the three functions. For each, write at least two tests before migrating callers.
4. **Migrate callers one module at a time.** For each command module, replace its local helper with a call to the shared helper. Keep the old definition until the call-site migration compiles, then remove.
5. **Run `cargo test` between commands.** Catch any semantic drift immediately.
6. **Final cleanup.** Remove unused imports. Ensure all removed helpers are gone.
7. **Commit in small increments.** One commit per command module is acceptable; bulk-squash at the end if desired.

## Implementation Units

**Unit 1 — Inventory and semantic-variance note.**
- Goal: a short bulleted list of every existing helper with a note on whether it matches the proposed canonical shape.
- Requirements advanced: R1, R2, R3 (understanding before changing).
- Dependencies: none.
- Files to create or modify: none (working notes).
- Tests to add or modify: none.
- Approach: ripgrep plus manual inspection.
- Test expectation: none -- research step.

**Unit 2 — Introduce `util::resolve_working_branch`.**
- Goal: a single function that resolves the working branch per the documented algorithm.
- Requirements advanced: R1, R5.
- Dependencies: Unit 1.
- Files to create or modify: `src/util.rs`.
- Tests to add or modify: add `resolve_working_branch_*` tests covering: current branch is `main` → returns `main`; current branch is feature branch with `origin/HEAD` set → returns the origin default; no origin/HEAD, current branch is `develop`, `main` exists → returns `main`; no fallback available → errors.
- Approach: port the most thorough existing implementation into `util.rs` with a doc comment.
- Test scenarios: the four above.
- Test expectation: the new tests pass.

**Unit 3 — Migrate branch callers.**
- Goal: every command module calls `util::resolve_working_branch`.
- Requirements advanced: R1, R4.
- Dependencies: Unit 2.
- Files to create or modify: the command modules listed in Context.
- Tests to add or modify: no new tests; existing module tests must stay green.
- Approach: one module at a time; commit between.
- Test scenarios: `cargo test` stays green per migration.
- Test expectation: existing tests unchanged in pass/fail.

**Unit 4 — Introduce `util::resolve_reference_repos`.**
- Goal: a single function that discovers sibling repos and merges with explicit entries.
- Requirements advanced: R2, R5.
- Dependencies: Unit 3.
- Files to create or modify: `src/util.rs`.
- Tests to add or modify: tests covering: tempdir with two sibling `.git` dirs → both returned; with an explicit `--reference-repo` overlap → deduplicated; no siblings → empty.
- Approach: port from the module with the most thorough implementation (likely `loop_command.rs` or `review_command.rs`).
- Test scenarios: the three above.
- Test expectation: the new tests pass.

**Unit 5 — Migrate reference-repo callers.**
- Goal: every module that has a `discover_sibling_*` or equivalent helper delegates to `util::resolve_reference_repos`.
- Requirements advanced: R2, R4.
- Dependencies: Unit 4.
- Files to create or modify: command modules.
- Tests to add or modify: existing tests stay green.
- Approach: one module at a time.
- Test scenarios: `cargo test` stays green.
- Test expectation: existing tests unchanged.

**Unit 6 — Introduce `util::log_prompt`.**
- Goal: a single function that writes a prompt log file with a shared header.
- Requirements advanced: R3, R5.
- Dependencies: none (independent of Units 2-5).
- Files to create or modify: `src/util.rs`.
- Tests to add or modify: test verifies file-path format (`.auto/logs/<command>-<timestamp>-prompt.md`), header includes the command name, body contains the full prompt, `atomic_write` is used.
- Approach: read current per-module shapes; pick the most thorough header format; use `atomic_write`.
- Test scenarios: one prompt-log test.
- Test expectation: the new test passes.

**Unit 7 — Migrate prompt-log callers.**
- Goal: every command module calls `util::log_prompt`.
- Requirements advanced: R3, R4.
- Dependencies: Unit 6.
- Files to create or modify: command modules.
- Tests to add or modify: existing tests stay green.
- Approach: one module at a time.
- Test expectation: existing tests unchanged.

## Concrete Steps

From the repository root:

1. Inventory:
   ```
   rg -n 'fn .*branch' src/ | rg -v '//|#\['
   rg -n 'discover_sibling|sibling_git_repos|resolve_reference_repos' src/
   rg -n 'log_prompt|prompt_log' src/
   rg -n '.auto/logs' src/
   ```
2. Check current line count of the affected modules:
   ```
   wc -l src/loop_command.rs src/parallel_command.rs src/review_command.rs src/bug_command.rs src/qa_command.rs src/ship_command.rs src/health_command.rs src/audit_command.rs src/generation.rs src/nemesis.rs src/steward_command.rs
   ```
3. Work through Units 2-7 in order. Between each unit run:
   ```
   cargo build
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   ```
4. Final commit summary message (or a series of commits):
   ```
   git commit -m "util: extract resolve_working_branch / resolve_reference_repos / log_prompt"
   ```

## Validation and Acceptance

- **Observable 1.** `rg -n 'fn resolve_working_branch' src/` returns exactly one definition.
- **Observable 2.** `rg -n 'fn resolve_reference_repos' src/` returns exactly one definition.
- **Observable 3.** `rg -n 'fn log_prompt' src/` returns exactly one definition (or at most one per shared-helpers file if the plan ends up splitting).
- **Observable 4.** `cargo test` reports no regressions; at least three new tests exist for the new helpers.
- **Observable 5.** `cargo clippy --all-targets --all-features -- -D warnings` stays clean.
- **Observable 6.** Post-change `wc -l` on the affected modules is smaller than pre-change.

Fail-before-fix: on the pre-change baseline, Observations 1-3 return multiple definitions.

## Idempotence and Recovery

- Each unit is a small edit; `git diff` reviews one migration at a time.
- If a migration breaks tests, revert the specific module's migration commit via `git revert` or `git checkout` and investigate.
- Rerunning the plan with units already applied is a no-op.

## Artifacts and Notes

- Pre-change line counts: (to be filled).
- Post-change line counts: (to be filled).
- New test names: (to be filled).
- Commit hashes per unit: (to be filled).

## Interfaces and Dependencies

- **Depends on:** Plan 005 gate passed.
- **Used by:** Plan 008 can compare duplication metrics across `bug_command.rs` and `nemesis.rs` more cleanly once this plan has landed.
- **External:** `cargo`. No agent CLI, no network.
