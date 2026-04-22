# Specification: Hardening pipelines — `auto bug` and `auto nemesis`

## Objective

Pin the two hardening commands to stable, multi-phase contracts.

`auto bug` runs a chunked finder → skeptic → reviewer → fixer → Codex finalizer pipeline with model defaults that reflect current code (Kimi primary, Codex finalizer pinned). Output lands under `bug/` with durable `BUG_REPORT.md`, `verified-findings.json`, and `implementation-results.json`.

`auto nemesis` now runs Kimi-first by default: Kimi draft audit → Kimi synthesis → Kimi fixer → Codex finalizer, archives prior output before wipe, supports `--report-only` to stop before implementation/finalizer, and syncs verified Nemesis findings back into root `specs/` and `IMPLEMENTATION_PLAN.md` after output verification. Legacy PI / MiniMax paths remain explicit opt-ins.

## Evidence Status

### Verified facts (code — `auto bug`)

- `src/main.rs:61` declares `Bug`; `src/main.rs:400-572` holds `BugArgs` (approximate range).
- Default model resolution verified at `src/main.rs:514-572`:
  - `--finder-model` default `k2.6`, `--finder-effort` default `high` (doc comment: "Kimi k2.6 by default").
  - `--skeptic-model` default `k2.6`, `--skeptic-effort` default `high`.
  - `--fixer-model` default `k2.6`, `--fixer-effort` default `high`.
  - `--reviewer-model` default `k2.6`, `--reviewer-effort` default `high`.
  - `--finalizer-model` default `gpt-5.4`, `--finalizer-effort` default `high` (doc comment: "stays pinned to gpt-5.4").
- `--use-kimi-cli` defaults to `true` (`src/main.rs:570-571`); `--no-use-kimi-cli` reverts to the legacy PI binary. `pi_bin` default is `pi`, `kimi_bin` default is `kimi-cli`.
- Chunk size default `24` (`src/main.rs:494-496`).
- Commit `639d953` made Kimi primary for the `auto bug` finder; README's "MiniMax finder by default" claim (README:39) is stale and contradicts code.
- `bug_command.rs` ~3,500 LOC, ~16-18 tests per corpus ASSESSMENT.
- Bug output artifacts: `bug/BUG_REPORT.md`, `bug/verified-findings.json`, `bug/implementation-results.json` (`corpus/SPEC.md` §"Artifact shapes").

### Verified facts (code — `auto nemesis`)

- `src/main.rs:91` declares `Nemesis`; `NemesisArgs` includes `--report-only` (`src/main.rs:1090-1092`), `--audit-passes` default `1` (`src/main.rs:1118-1121`), and `--use-kimi-cli` default `true` (`src/main.rs:1136-1138`).
- Current default models in `src/main.rs:1065-1116`:
  - Draft audit: `--model = "k2.6"`, `--reasoning-effort = "high"`.
  - Synthesis: `--reviewer-model = "k2.6"`, `--reviewer-effort = "high"`.
  - Fixer/implementation: `--fixer-model = "k2.6"`, `--fixer-effort = "high"`.
  - Finalizer: `--finalizer-model = "gpt-5.4"`, `--finalizer-effort = "high"`.
- Backend selection in `src/nemesis.rs:637-659` routes Kimi-family models through `kimi-cli` when `--use-kimi-cli=true`, through PI when explicitly requested, and non-Kimi/non-PI models through Codex. `--minimax` is an explicit legacy opt-in for the auditor model (`src/nemesis.rs:700-710`).
- `prepare_output_dir` archives the previous `nemesis/` into `.auto/fresh-input/nemesis-previous-<timestamp>/` before wipe (`corpus/SPEC.md` item 6; NEM-F1 `COMPLETED.md` verified).
- NEM-F findings NEM-F1..NEM-F10 are all confirmed applied in code per `COMPLETED.md` cross-check.
- `nemesis.rs` ~2,900 LOC, ~26-27 tests per corpus ASSESSMENT.
- Nemesis artifacts: `nemesis/nemesis-audit.md`, `nemesis/IMPLEMENTATION_PLAN.md`, `nemesis/implementation-results.json`, `nemesis/implementation-results.md` (`corpus/SPEC.md` §"Artifact shapes"). `draft-nemesis-audit.md` is written for two-phase auditor diffs (`README.md:391`).
- `auto nemesis` syncs the verified Nemesis spec into root `specs/` and appends unchecked Nemesis tasks to root `IMPLEMENTATION_PLAN.md` after output verification (`src/nemesis.rs:586-587`). This happens for both normal and `--report-only` runs today; `--report-only` skips implementation/finalizer, not root sync.

### Verified facts (docs)

- `README.md:54-55`: **Stale** — claims `auto nemesis` runs a PI audit pair by default, then a `gpt-5.4` `high` implementation pass. Code now defaults audit/synthesis/fixer to Kimi `k2.6` via `kimi-cli` and uses Codex `gpt-5.4` only as finalizer.
- `README.md:39`: **Stale** — claims MiniMax finder for `auto bug`; code now defaults to Kimi. This is the primary bug-command drift per `corpus/ASSESSMENT.md` §"Documentation staleness".

### Recommendations (corpus)

- Update `README.md:39` and the detailed `auto bug` section to reflect Kimi-primary defaults (`corpus/plans/002-readme-command-inventory-sync.md`).
- Research a shared `LlmBackend` trait consolidating Codex / Pi / Kimi dispatch across `bug_command.rs` and `nemesis.rs` (`corpus/plans/008-llm-backend-trait-research.md`). Research-only until a third caller arrives.

### Hypotheses / unresolved questions

- Whether `auto nemesis --resume` exists is **unverified** — `corpus/DESIGN.md` §"Artifacts as information system" explicitly notes nemesis has no resume. Do not assume resume.
- Whether Codex futility exit (137) during the implementer phase short-circuits or retries is not source-verified in this pass.

## Acceptance Criteria

### `auto bug`

- Default run uses Kimi (`k2.6`) for finder, skeptic, reviewer, and fixer; Codex (`gpt-5.4`) for the finalizer pass; all at `high` effort.
- `--use-kimi-cli=true` (default) routes Kimi phases through `kimi-cli --yolo`; `--no-use-kimi-cli` routes through the legacy PI binary; PI binary resolution uses `--pi-bin` default `pi`.
- `--finder-model`, `--skeptic-model`, `--reviewer-model`, `--fixer-model`, `--finalizer-model` each accept overrides; effort flags follow the same pattern.
- The finder pass splits the reviewed surface into chunks with `--chunk-size` (default `24` files or equivalent unit).
- Output directory defaults to `<repo>/bug` and may be overridden.
- `bug/BUG_REPORT.md`, `bug/verified-findings.json`, `bug/implementation-results.json` are written through `util::atomic_write`.
- `auto bug --allow-dirty` permits runs on a dirty worktree; otherwise the command exits with a dirty-state error.
- `auto bug --dry-run` previews the chunk plan and exits without invoking any model.
- Missing `kimi-cli` binary when `use-kimi-cli=true` yields a non-zero exit with a named-dependency error; falling back to PI requires explicit `--no-use-kimi-cli`.

### `auto nemesis`

- Default run performs Kimi audit (`k2.6` high) → Kimi synthesis (`k2.6` high) → Kimi fixer (`k2.6` high) → Codex finalizer (`gpt-5.4` high).
- `--report-only` stops after audit + synthesis; implementation/fixer and Codex finalizer are skipped. Current code still syncs the verified Nemesis spec and unchecked tasks into root `specs/` / `IMPLEMENTATION_PLAN.md`.
- `--kimi` is now a no-op clarity flag for the default auditor family; `--minimax` explicitly opts the auditor back into the legacy MiniMax path.
- `prepare_output_dir` archives any pre-existing `nemesis/` into `.auto/fresh-input/nemesis-previous-<timestamp>/` before wiping and writing the new run.
- Output artifacts on a completed run: `nemesis/draft-nemesis-audit.md`, `nemesis/draft-IMPLEMENTATION_PLAN.md`, `nemesis/nemesis-audit.md`, `nemesis/IMPLEMENTATION_PLAN.md`, `nemesis/implementation-results.json`, `nemesis/implementation-results.md`, and `nemesis/final-review.md` when implementation runs.
- Unresolved findings are appended to root `IMPLEMENTATION_PLAN.md` and a dated audit spec snapshot is written under `specs/` after output verification. If non-mutating report-only semantics are desired, that is a future behavior change.
- NEM-F1..NEM-F10 hardening behaviors remain intact (archive-then-wipe, checkpoint excludes, atomic staging, repo-layout collection, absent-remote short-circuit, file-pair verification, time-precise spec filename, zero-task plan short-circuit).
- Missing `kimi-cli`, `pi`, or `codex` binaries as required by the chosen phase mix produce named-dependency non-zero exits.

### Shared

- Both commands use `util::auto_checkpoint_if_needed` before running when the worktree has tracked dirty state outside `CHECKPOINT_EXCLUDE_RULES`.
- Both commands write prompt logs per phase under `.auto/logs/<command>-<timestamp>-<phase>-prompt.md`.
- Both commands report elapsed timing per phase to stdout.

## Verification

- `cargo test -p autodev bug_command` and `cargo test -p autodev nemesis` pass (≈44 combined tests).
- Test the MiniMax-to-Kimi switch: assert `BugArgs::finder_model` default string is `"k2.6"` (not `"minimax"`).
- Test the Nemesis Kimi-first default: assert default `NemesisArgs` route audit/synthesis/fixer to `k2.6` and finalizer to `gpt-5.4`.
- Fixture test for `auto nemesis --report-only`: assert `nemesis/nemesis-audit.md` is written, implementation/finalizer artifacts are absent, and root spec/plan sync behavior matches the documented current contract.
- Archive-then-wipe test: pre-create `nemesis/` with a dummy file, run `auto nemesis`, assert a copy exists under `.auto/fresh-input/nemesis-previous-*` and the dummy file no longer exists in `nemesis/`.
- Dry-run test for `auto bug --dry-run`: assert no files under `bug/` are created and no model was invoked.
- README reconciliation: CI or test-like check fails if `README.md:39` still says "MiniMax finder by default"; follow-on per `corpus/plans/002-*`.

## Open Questions

- Does `auto nemesis` need a `--resume` story, or is "delete + re-run" the supported recovery?
- Should `auto bug` and `auto nemesis` share a `LlmBackend` trait today, or wait for a third caller per `corpus/plans/008-*`?
- Should `auto bug` emit a chunk-level manifest like `auto audit` does, to support partial resumes across long chunk runs?
- What is the exact semantics for when `auto nemesis` appends to root plan — does it dedupe against existing entries, or is duplication an operator concern?
- Should `--report-only` remain a "no implementation/finalizer" mode that still syncs root spec/plan artifacts, or should it become fully non-mutating outside `nemesis/`?
