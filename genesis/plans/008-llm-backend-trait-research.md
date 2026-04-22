# Plan 008 — Research: LlmBackend trait consolidation

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

**Shape: Research-only.** This plan produces a written decision, not a code change. Implementation, if any, is a follow-on plan created after the decision lands.

## Purpose / Big Picture

The repo currently reaches LLM backends through three differently-shaped modules: `src/claude_exec.rs` (375 lines, stream-handling and PID lifecycle), `src/codex_exec.rs` (685 lines today, shrinking to under 400 after Plan 003, wraps a subprocess and parses stdout progress frames), and `src/kimi_backend.rs` (324 lines, mostly pure helpers — model resolution, arg construction, error parsing, a preflight). The `pi` CLI is invoked ad-hoc from `nemesis.rs` and `bug_command.rs` without a dedicated module. Command modules currently select a backend by reading booleans (`--use-kimi-cli`, `--use-pi`) and branching at every call site.

The question this plan answers is: does the codebase now have enough shared structure across backends to justify a small `LlmBackend` trait, or is the duplication still below the "rule of three" threshold? If a trait is warranted, in what shape, with what methods, and across which call sites?

The operator cost of getting this wrong in either direction is real. A premature trait locks in the wrong abstraction and has to be undone later; an indefinitely deferred one leaves every new command reinventing selection logic. The output of this plan is a short decision document with a clear recommendation, not a code change.

An operator reading the result knows one of three things: "a trait is warranted and it looks like this, and plan NNN will implement it"; or "a trait is not warranted yet, here is the observation threshold that should trigger re-opening the question"; or "a trait is warranted for a narrower slice (e.g., only argument assembly), and plan NNN will implement that narrower slice."

## Requirements Trace

- **R1.** A written survey of every call site that invokes a backend CLI, captured under `genesis/research/008-llm-backend-survey.md`. The survey includes: caller file, function, backend (claude / codex / kimi-cli / pi), selection mechanism (flag or environment), arguments built, output parsed.
- **R2.** A tabulated comparison of the four backend invocation shapes across axes: argument assembly, stdout streaming vs. collect-all, error extraction, PID tracking, cancellation, timeout, environment override.
- **R3.** A recommendation section stating one of three outcomes, each with a written rationale: full trait, scoped trait (argument assembly only, or error parsing only), or no trait at this time.
- **R4.** If the recommendation is "full trait" or "scoped trait", a sketch of the trait: method signatures, associated types, error type, test scaffolding pattern. The sketch is illustrative; actual implementation is a follow-on plan.
- **R5.** If the recommendation is "no trait at this time", a written re-opening trigger: a specific observable condition (e.g., "when a fourth backend is added" or "when `bug_command.rs` or `nemesis.rs` grows past 4000 lines of backend-coupled code") that should cause this question to be revisited.
- **R6.** An explicit Decision Log entry in this plan classifying the recommendation as **Mechanical**, **Taste**, or **User Challenge**. Trait shape is almost always Taste; the classification is itself a visible decision.

## Scope Boundaries

- **Producing:** one research note at `genesis/research/008-llm-backend-survey.md` plus this plan's own Decision Log.
- **Not producing:** any code change, any file under `src/`, any new dependency.
- **Not promising:** that a follow-on implementation plan will be created. The recommendation may be "no trait."
- **Not covering:** agent CLI selection for non-code tasks (e.g., Linear `gh` wrappers). Those are out of scope and have different shape.
- **Not deciding:** quota routing between backends. That belongs to `quota_selector.rs` and is a separate concern.

## Progress

- [ ] Call-site survey written.
- [ ] Comparison table written.
- [ ] Recommendation drafted.
- [ ] Decision Log classification applied.
- [ ] Research note committed.

## Surprises & Discoveries

None yet. Potential surprises worth logging during the survey:
- A backend invocation pattern that does not fit the expected "build args, spawn, parse stdout" shape (e.g., something that shells out through a pipeline).
- A call site that already builds its own local trait-like abstraction inline.
- A third backend in progress in an uncommitted branch that would change the count.

## Decision Log

- **2026-04-21 — Research, not implementation, at this phase.** Taste. The current duplication is across two-to-three call shapes, not four-plus; Rule-of-Three suggests a trait may be premature. Research output forces the question to be answered visibly before code changes.
- **2026-04-21 — Survey file lives under `genesis/research/`, not `genesis/plans/`.** Mechanical. `genesis/plans/` is reserved for ExecPlans. Research outputs are a different artifact shape and belong in a sibling directory.

## Outcomes & Retrospective

None yet.

## Context and Orientation

- `src/claude_exec.rs` — 375 lines. Live entry points include `spawn_claude`, stream readers, PID lifecycle. Stream-json parsing.
- `src/codex_exec.rs` — 685 lines before Plan 003, target under 400 after. Live entry points: `run_codex_exec`, `run_codex_exec_with_env`, `spawn_codex`.
- `src/kimi_backend.rs` — 324 lines. Pure helpers: `resolve_kimi_cli_model`, `resolve_kimi_bin`, `kimi_exec_args`, `extract_final_text`, `parse_kimi_error`, `preflight_kimi_cli`, `validate_kimi_model`. No process lifecycle — callers drive the process and use these helpers.
- `src/nemesis.rs` — 2921 lines. Consumes both codex and kimi backends; has its own `pi`-style code paths.
- `src/bug_command.rs` — 3533 lines. Similar story. Also consumes both.
- `src/audit_command.rs` — 1154 lines. Currently hard-requires `--use-kimi-cli` (see line 1023 `bail!`).
- `src/steward_command.rs` — 676 lines. Uses the same dispatch pattern.
- `src/main.rs` — for each command's `--use-kimi-cli` / `--use-pi` flag definition.

Terms used below:
- **Call site** — a location that invokes an LLM backend to run a prompt and collect output.
- **Selection** — how a call site chooses which backend to invoke.
- **Shape** — the structural pattern of the invocation (arg assembly, process spawn, stdout handling, error surface).

## Plan of Work

1. Enumerate every call site that talks to a backend. Use `rg` to find calls to `run_codex_exec_with_env`, `spawn_claude`, `kimi_exec_args`, and any direct `Command::new("pi")`, `Command::new("kimi")`, `Command::new("kimi-cli")` invocations in `src/`.
2. For each call site, record the columns listed under R1.
3. Build the comparison table per R2.
4. Draft the recommendation per R3.
5. If the recommendation is yes-trait, sketch the trait per R4.
6. If no-trait, write the re-opening trigger per R5.
7. Classify and commit.

## Implementation Units

**Unit 1 — Call-site survey.**
- Goal: produce a populated table of every backend invocation in `src/`.
- Requirements advanced: R1.
- Dependencies: none.
- Files to create or modify: `genesis/research/008-llm-backend-survey.md`.
- Tests to add or modify: none -- research only.
- Approach: `rg` for known entry-point function names; for each match, open the file and extract the columns; populate table.
- Test expectation: none.

**Unit 2 — Shape comparison.**
- Goal: a side-by-side comparison of the four backend shapes on the seven axes listed in R2.
- Requirements advanced: R2.
- Dependencies: Unit 1.
- Files to create or modify: `genesis/research/008-llm-backend-survey.md` (append section).
- Tests to add or modify: none.
- Approach: read `claude_exec.rs`, `codex_exec.rs` (post-Plan-003), `kimi_backend.rs`; identify the relevant pattern for each axis.
- Test expectation: none.

**Unit 3 — Recommendation.**
- Goal: one of `{full trait, scoped trait, no trait}`, with rationale.
- Requirements advanced: R3, R4, R5, R6.
- Dependencies: Units 1 and 2.
- Files to create or modify: `genesis/research/008-llm-backend-survey.md` (append section); this plan's Decision Log.
- Tests to add or modify: none.
- Approach: weigh the shape-comparison against the Rule-of-Three; apply the criteria listed in "Recommendation criteria" below; write rationale.
- Test expectation: none.

## Concrete Steps

From the repository root:

1. Create the research file scaffold:
   ```
   mkdir -p genesis/research
   ```
2. Enumerate call sites:
   ```
   rg -nE 'run_codex_exec_with_env|spawn_claude|kimi_exec_args|run_codex_exec\b' src/
   rg -nE 'Command::new\("(pi|kimi|kimi-cli|claude|codex)"' src/
   ```
3. For each unique call site, populate the survey table.
4. Read the three backend modules end-to-end to produce the shape comparison.
5. Draft the recommendation against the criteria below.
6. Write the survey file and update this plan's Decision Log with the classification.
7. Commit:
   ```
   git add genesis/research/008-llm-backend-survey.md genesis/plans/008-llm-backend-trait-research.md
   git commit -m "research(backend): llm backend trait survey and recommendation"
   ```

### Recommendation criteria

- **Full trait** (`LlmBackend` with spawn / stream / cancel methods) is warranted only if: three or more call sites branch on backend at runtime AND the shape-comparison table shows six of seven axes aligning AND the error types can be unified without forcing callers to downcast.
- **Scoped trait** (just `build_args` or just `parse_error`) is warranted if one axis aligns strongly across backends but others do not.
- **No trait** is the default. Two call shapes are not enough. The re-opening trigger goes into writing.

## Validation and Acceptance

- **Observable 1.** `genesis/research/008-llm-backend-survey.md` exists and contains the three sections: Call-site survey, Shape comparison, Recommendation.
- **Observable 2.** The Decision Log on this plan file has a new dated entry with the classification (Mechanical / Taste / User Challenge).
- **Observable 3.** If the recommendation is full or scoped trait, a new plan file (e.g., `genesis/plans/NNN-llm-backend-trait.md`) is either declared in `genesis/PLANS.md` index with a title and shape, or explicitly deferred with a note. If no-trait, `genesis/PLANS.md` is updated to record the deferral.
- **Observable 4.** `git log -1 genesis/research/008-llm-backend-survey.md` shows the research commit.
- **Observable 5.** No file under `src/` has been modified by this plan.

## Idempotence and Recovery

- Rerunning the survey produces the same output unless the codebase has changed. If the codebase has changed, update the existing survey file -- do not duplicate.
- If the recommendation is later overturned (e.g., by a new requirement), append a new Decision Log entry rather than editing the prior one. Leaves audit trail.
- If the research note is committed and a mistake is found later, amend or replace in a new commit; never rewrite published history.

## Artifacts and Notes

- Call-site count (to be filled after survey).
- Backends covered (to be filled — expect four: claude, codex, kimi-cli, pi).
- Recommendation summary (to be filled): `{full | scoped | no-trait}`.
- Re-opening trigger if `no-trait` (to be filled).
- Commit hash (to be filled).

## Interfaces and Dependencies

- **Depends on:** Plan 007 (shared utility extraction). Extracting the branch, reference-repo, and prompt-log helpers first gives the survey a cleaner view of what duplication is backend-shaped vs. general-utility-shaped. Running this research before 007 would conflate the two.
- **Used by:** Plan 009 (Phase 2 checkpoint gate) verifies that this plan has produced a recommendation before unlocking Phase 3.
- **External:** none. Pure reading of existing source.
