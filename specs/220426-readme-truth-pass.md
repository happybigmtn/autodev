# Specification: README truth-pass and docs-code alignment

## Objective

Close the #1 operator-visible gap: the README currently claims "thirteen commands" and documents only those thirteen, while `src/main.rs:52-96` has sixteen. `steward`, `audit`, and `symphony` are invisible to any operator who reads the README alone. Default-model claims for `auto bug` and `auto nemesis` are also stale (README says MiniMax/PI defaults; code says Kimi-first with Codex finalizers). A focused truth-pass updates the inventory bullet list, the detailed command guide, and the per-command default-model lines so that the README matches `auto --help` and code constants. Depth changes (detailed rewrites, new sections) are out of scope for this pass.

## Evidence Status

### Verified facts (README vs code drift)

- `README.md:11` asserts "`auto` owns thirteen commands" — verified to contradict the 16-variant `Command` enum at `src/main.rs:52-96`.
- `README.md:13-25` bulleted inventory lists: `corpus`, `gen`, `reverse`, `bug`, `nemesis`, `quota`, `loop`, `parallel`, `qa`, `qa-only`, `health`, `review`, `ship`. Missing: `steward`, `audit`, `symphony`.
- `README.md:536` mentions `auto symphony` in prose inside the `auto loop` description but not in the inventory.
- `README.md:39`: "`auto bug` runs MiniMax finder, Kimi skeptic/reviewer, and a final `gpt-5.4` `high` implementation pass by default." Code defaults (`src/main.rs:514-520`) set `--finder-model = "k2.6"` (Kimi). Commit `639d953` (title: "bug + nemesis: kimi-cli primary, fix-on-verify, Codex finalizer") intentionally switched the finder primary.
- `README.md:54-55`: "`auto nemesis` runs a PI audit pair by default, then a `gpt-5.4` `high` implementation pass." Code defaults now set audit/synthesis/fixer to Kimi `k2.6` and finalizer to Codex `gpt-5.4` (`src/main.rs:1065-1116`, `src/nemesis.rs:637-710`).
- README detailed guide section (`README.md:84-918`) has no subsection for `auto steward`, `auto audit`, or `auto symphony` (detailed guide only covers the thirteen advertised commands).
- `auto symphony` and `auto audit` and `auto steward` have doc-comments on their `Command` variants in `src/main.rs:74-95`, so `auto --help` already describes them correctly; only the README is stale.

### Verified facts (code)

- `src/main.rs:74-80` (steward doc-comment) explicitly frames steward as an alternative to `corpus + gen` for mid-flight repos.
- `src/main.rs:81-87` (audit doc-comment) names the verdict set and describes doctrine-driven operation.
- `src/main.rs:95` (symphony) has a doc-comment "Sync implementation-plan items into Linear and run the local Symphony runtime".
- AGENTS.md is documented as "Accurate" in `corpus/ASSESSMENT.md` §"Documentation staleness", so it does not need changes in this pass.
- `IMPLEMENTATION_PLAN.md` at repo root is an empty skeleton (three headers, no tasks) per `corpus/ASSESSMENT.md` §"What is broken".

### Recommendations (corpus)

- Add a one-line "what is this" column to the README inventory (sixteen rows, one sentence each) before touching the detailed command guide; this is cheaper than rewriting the detailed guide and closes the primary discovery gap (`corpus/DESIGN.md` §"Decisions to recommend" item 2).
- For each of `steward`, `audit`, `symphony`, add a section to the detailed command guide covering purpose, inputs, outputs, default models, flags. Scope note: these sections can cite the relevant new spec files (220426-*) rather than duplicate their content.
- Per `corpus/plans/002-readme-command-inventory-sync.md`: confine this pass to inventory + default-model corrections; a larger rewrite of the detailed guide is a follow-on.

### Hypotheses / unresolved questions

- Whether the README should also document `steward`'s six-deliverable artifact set, or keep that in the spec-level docs and just point to the spec file.
- Whether the `salvage/` directory (operator scratch, per `corpus/ASSESSMENT.md` §"What is broken") should be called out in the README or added to `.gitignore`.

## Acceptance Criteria

- `README.md:11` no longer says "thirteen commands"; the count matches the real surface (sixteen).
- The top-level inventory bullet list in `README.md:13-25` adds `auto steward`, `auto audit`, and `auto symphony` (in a sensible order, for example placed near lifecycle neighbors).
- The same inventory list includes a one-line "what is this" purpose next to each of the sixteen entries, derived from the `Command` variant doc-comment in `src/main.rs:52-96`.
- `README.md:39` default-model line for `auto bug` reflects code reality: "Kimi `k2.6` finder, Kimi skeptic/reviewer, Kimi fixer (by default), and a final `gpt-5.4` `high` Codex finalizer pass."
- `README.md:54-55` default-model line for `auto nemesis` reflects code reality: Kimi `k2.6` audit, synthesis, and fixer by default, followed by a Codex `gpt-5.4` `high` finalizer when implementation runs.
- The detailed command guide gains three subsections: `### auto steward`, `### auto audit`, `### auto symphony`. Each subsection covers purpose, inputs, outputs (artifact file list), default models / flags, and when to prefer this command over its siblings.
- For each of the three new subsections, the artifact list matches the real artifact set:
  - `auto steward`: `DRIFT.md`, `HINGES.md`, `RETIRE.md`, `HAZARDS.md`, `STEWARDSHIP-REPORT.md`, `PROMOTIONS.md`.
  - `auto audit`: `audit/DOCTRINE.md` (input), `audit/MANIFEST.json`, `audit/files/<hash-prefix>/{verdict.json,patch.diff,response.log,prompt.md}`.
  - `auto symphony`: `WORKFLOW.md`, Linear project operations (`sync`, `workflow`, `run` subcommands).
- `README.md` no longer says `auto parallel` uses the tmux helpers in `codex_exec.rs`; if the tmux-session description stays (it does at line 42-43), it refers to the real tmux integration in `parallel_command.rs`.
- Typical-flow block (lifecycle) remains truthful — no command is added to the flow that is not in the inventory.
- No new claims are added to the README that the code does not support (no "phantom features").

## Verification

- `grep -E "thirteen|fourteen|fifteen|sixteen" README.md` returns the correct count only.
- `rg "^- \`auto " README.md | wc -l` returns at least sixteen inventory rows.
- Manual diff between `README.md` default-model paragraphs and the relevant `clap` doc-comments in `src/main.rs` — no contradictions remain.
- Every `auto <command>` named in the detailed command guide appears in `auto --help` output and vice versa.
- `corpus/plans/002-readme-command-inventory-sync.md` acceptance is satisfied; follow-on plans (Plan 009, Plan 012) pick up the lifecycle-reconciliation narrative from here.
- `grep -n "PI audit pair by default\\|MiniMax finder" README.md` returns no stale default-model claims.

## Open Questions

- Should the "thirteen commands" prose style be dropped entirely in favor of "the current command set" to reduce recurring drift when the count changes again?
- Should the README link directly into the `gen-*/specs/220426-*.md` spec files for deeper per-command detail, or stay self-contained?
- Should the `IMPLEMENTATION_PLAN.md` skeleton at the repo root be populated (with the planned tasks from the corpus) as part of the truth pass, or left to the first real `auto gen` run?
- Should the README gain a "Current defaults change log" section so future model default switches (like the MiniMax → Kimi move) are documented in one place rather than chasing line edits?
