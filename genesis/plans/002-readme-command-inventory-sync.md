# Plan 002 — README command inventory sync

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

## Purpose / Big Picture

The README opens by saying `auto` owns "thirteen commands" and lists them by name. The `Command` enum in `src/main.rs` lines 52-96 has sixteen variants: the thirteen listed commands plus `Steward`, `Audit`, and `Symphony`. An operator who relies on the README cannot discover three of the installed commands without reading Rust source. This plan makes the README honest about what the binary ships.

The user-visible gain: `auto --help` and the README now agree on the command inventory. Operators coming to this repo can pick up `auto steward`, `auto audit`, and `auto symphony` without reading `main.rs`.

## Requirements Trace

- **R1.** README command inventory lists all sixteen commands from the `Command` enum.
- **R2.** README top-of-file count claim ("thirteen commands") is updated or removed.
- **R3.** Each of `steward`, `audit`, `symphony` has a detailed-guide section in the README, matching the shape of the existing per-command sections (Purpose / What it reads / What it writes / What it actually does / When to run it / Useful flags).
- **R4.** The "Design Goal" paragraph is revised so its scope statement includes every command in the enum, or explicitly names the commands it considers side lanes that do not alter the small-repo principle.
- **R5.** `auto bug` default model claim is corrected from MiniMax to Kimi `kimi-coding/k2p6`, matching the change in commit `639d953`.
- **R6.** No source-code changes. The change is docs-only.

## Scope Boundaries

- **Changing:** `README.md` at the repository root.
- **Not changing:** `src/` files, `AGENTS.md`, `specs/`, `docs/`, `genesis/` (except that this plan's `Progress` section will be updated), `COMPLETED.md`, `IMPLEMENTATION_PLAN.md`.
- **Not adding:** new commands, new artifact types, new flags.
- **Not rewriting:** any per-command section whose current text is already accurate against code.

## Progress

- [ ] Read current README inventory and detailed-guide entries to confirm drift.
- [ ] Draft replacement inventory table (sixteen rows).
- [ ] Draft `auto steward` detailed-guide section.
- [ ] Draft `auto audit` detailed-guide section.
- [ ] Draft `auto symphony` detailed-guide section.
- [ ] Correct `auto bug` finder default-model claim.
- [ ] Update "Design Goal" paragraph to reflect current enum or side-lane framing.
- [ ] Write edits into `README.md` via `apply_patch`.
- [ ] Re-read README front-to-back to verify internal consistency.
- [ ] Commit.

## Surprises & Discoveries

None yet. During authoring, record anything that contradicts the corpus findings (e.g., if `symphony` is discovered to be deprecated and not just undocumented, that is a surprise worth logging).

## Decision Log

- **2026-04-21 — Docs-only change; no source-code edits in this plan.** Mechanical. The drift is purely a docs freshness problem.
- **2026-04-21 — Keep the "side lanes" framing.** Taste. The README currently distinguishes lifecycle commands from side lanes. `steward` fits lifecycle (alternative to `corpus + gen`); `audit` fits side lanes (quality); `symphony` is infrastructure. The revised inventory should preserve that split rather than collapse to a flat list.
- **2026-04-21 — Do not change the Design Goal's scope sentence wholesale.** Taste. The operator's stated principle ("this repo should stay small") is their voice. Update the list of commands inside it; do not relitigate the principle.

## Outcomes & Retrospective

None yet.

## Context and Orientation

- `README.md` — full tool-facing prose. Current "thirteen commands" claim is at line 11. Command bulleted list runs lines 13-25. Detailed command guide runs lines 84-918. "Design Goal" paragraph is lines 1078-1082.
- `src/main.rs` — `Command` enum at lines 52-96. Doc comments on `Steward` (lines 74-80), `Audit` (lines 81-87), `Symphony` (line 95).
- `src/steward_command.rs` — `auto steward` implementation. Writes `DRIFT.md`, `HINGES.md`, `RETIRE.md`, `HAZARDS.md`, `STEWARDSHIP-REPORT.md`, `PROMOTIONS.md`. Two Codex `gpt-5.4` passes.
- `src/audit_command.rs` — `auto audit` implementation. Reads `audit/DOCTRINE.md`. Writes `audit/MANIFEST.json` and `audit/files/<hash-prefix>/{verdict.json,patch.diff,...}`. Uses `kimi-cli` by default; bails without `--use-kimi-cli`.
- `src/symphony_command.rs` — `auto symphony` has subcommands `Sync`, `Workflow`, `Run`. Reads `IMPLEMENTATION_PLAN.md`, pushes to Linear.app via GraphQL.
- `src/bug_command.rs` — `auto bug` current defaults. Finder phase default was changed from MiniMax to Kimi in commit `639d953` (2026-04-21).
- `docs/audit-doctrine-template.md` — the doctrine-file template `auto audit` expects.

## Plan of Work

1. **Survey the current README** to list every claim that must change. Include line numbers in a working note.
2. **Draft a new inventory block** replacing lines 13-25. Prefer a bulleted list with one-line descriptions, matching existing style.
3. **Draft a new "How To Think About The Commands"** paragraph that acknowledges sixteen commands: seven lifecycle + nine side lanes / infrastructure. Keep the existing seven-step lifecycle intact.
4. **Author three new per-command sections** — `auto steward`, `auto audit`, `auto symphony` — following the established template.
5. **Revise the `auto bug` "Default model layout"** subsection (README:340-345) so the finder line says Kimi `kimi-coding/k2p6` rather than MiniMax `minimax/MiniMax-M2.7-highspeed`.
6. **Revise "Design Goal"** to list all sixteen commands or to add a short sentence acknowledging the side lanes.
7. **Re-read top-to-bottom** after all edits to catch references to "thirteen," "twelve side lanes," etc.
8. **Commit** with a single commit message describing the docs sync.

## Implementation Units

**Unit 1 — Inventory block and count claim.**
- Goal: lines 11-25 of README list all sixteen commands; "thirteen commands" is corrected.
- Requirements advanced: R1, R2.
- Dependencies: none.
- Files to create or modify: `README.md`.
- Tests to add or modify: none.
- Approach: `apply_patch`. Replace the "thirteen commands" sentence with "`auto` owns the following commands:" and the bulleted list with the full sixteen.
- Test scenarios: none (docs-only).
- Test expectation: none -- docs-only change.

**Unit 2 — Per-command guide sections for steward, audit, symphony.**
- Goal: the detailed-guide portion of the README includes a section for each new command, matching the existing template.
- Requirements advanced: R3.
- Dependencies: Unit 1 landed first so the inventory references exist.
- Files to create or modify: `README.md`.
- Tests to add or modify: none.
- Approach: add three new `###` sections placed where they belong in lifecycle order.
- Test scenarios: none.
- Test expectation: none -- docs-only change.

**Unit 3 — Bug finder default correction.**
- Goal: the `auto bug` default-model block reflects Kimi primary.
- Requirements advanced: R5.
- Dependencies: none.
- Files to create or modify: `README.md`.
- Tests to add or modify: none.
- Approach: `apply_patch`. Replace the line "finder: MiniMax `minimax/MiniMax-M2.7-highspeed` with `high`" with "finder: Kimi `kimi-coding/k2p6` with `high`".
- Test scenarios: none.
- Test expectation: none -- docs-only change.

**Unit 4 — Design goal sentence.**
- Goal: the list of commands inside the Design Goal paragraph matches the enum, or is replaced by a shorter sentence that does not require enumeration.
- Requirements advanced: R4.
- Dependencies: Units 1-3.
- Files to create or modify: `README.md`.
- Tests to add or modify: none.
- Approach: preserve the "this repo should stay small" sentence; update the enumeration to sixteen commands or to reference the CLI help.
- Test scenarios: none.
- Test expectation: none -- docs-only change.

## Concrete Steps

From the repository root:

1. Read the current README inventory:
   ```
   sed -n '1,60p' README.md
   sed -n '1070,1085p' README.md
   ```
2. Read the current `auto bug` default-model layout:
   ```
   grep -n 'MiniMax-M2.7-highspeed' README.md
   ```
3. Apply the edits for Units 1, 3, 4 first. Leave Unit 2 (three new sections) last because it is the largest edit.
4. After edits, confirm the corrected inventory renders correctly:
   ```
   sed -n '1,60p' README.md
   ```
5. Confirm every command in `src/main.rs` appears in the README:
   ```
   grep -oE 'fn run_(corpus|gen|reverse|bug|nemesis|loop|parallel|qa|qa_only|health|review|ship|steward|audit|symphony|quota)' src/*.rs | sort -u
   ```
   For each entry, verify the matching section exists in `README.md` with a `### auto <name>` heading or similar.
6. Stage and commit:
   ```
   git add README.md
   git commit -m "docs: sync command inventory with current Command enum"
   ```

## Validation and Acceptance

- **Observable 1.** `grep -c 'thirteen commands' README.md` returns `0`.
- **Observable 2.** `grep -cE '^- `auto (steward|audit|symphony)`' README.md` returns `3` (or the equivalent count once rendered in the inventory).
- **Observable 3.** `grep -c '### `auto steward`' README.md` returns `1`; same for `### auto audit` and `### auto symphony`.
- **Observable 4.** `grep 'finder: Kimi' README.md` returns at least one line; `grep 'finder: MiniMax' README.md` returns `0`.
- **Observable 5.** Manual read-through reveals no dangling "twelve" / "thirteen" / "fourteen" command-count claims.

This plan does not need new `cargo test` cases because no code changes. `cargo check` should still pass, confirming the edit touched nothing unintended.

## Idempotence and Recovery

- Edits are applied via `apply_patch` on `README.md`. Re-running an already-applied patch produces no diff because the context no longer matches; re-run is safe but a no-op.
- If the edits corrupt the file, `git checkout -- README.md` reverts to HEAD.
- If only a subset of units completes before interruption, subsequent runs apply only the remaining units.

## Artifacts and Notes

- Commit hash (to be filled after landing).
- Before/after `wc -l README.md` noted as evidence of non-destructive growth.
- Link to the previous version in git history for anyone who wants to diff.

## Interfaces and Dependencies

- **Depends on:** nothing except the current repository working tree.
- **Used by:** operators reading `README.md`. Plan 005 gate verifies this plan's acceptance before Phase 2 begins.
- **External:** none. No agent CLI, no network, no build.
