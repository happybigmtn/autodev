# Specification: Planning pipeline — `auto corpus`, `auto gen`, `auto reverse`

## Objective

Preserve the planning pipeline contract: `auto corpus` rebuilds a disposable `genesis/` corpus from repo reality; `auto gen` turns that corpus into durable `specs/` plus `IMPLEMENTATION_PLAN.md`; `auto reverse` documents current behavior from code without rewriting the root plan queue. All three commands must remain archive-safe, must emit required spec sections, and must keep their per-command artifact contracts aligned with what `loop`, `parallel`, `review`, and `steward` already parse.

## Evidence Status

### Verified facts (code)

- `src/main.rs:54-59` declares `Corpus`, `Gen`, and `Reverse`. `Gen` and `Reverse` share the `GenerationArgs` type (`src/main.rs:57,59`).
- `src/main.rs:401-405` sets `corpus` defaults to `claude-opus-4-7` with effort `xhigh`. `src/main.rs:409-413` sets `gen` / `reverse` planner defaults to the same (`gpt-5.4` variant also available via CLI flags; see args block).
- `src/corpus.rs:10-23` defines `PlanningCorpus` with explicit output paths: `assessment_path`, `design_path`, `focus_path`, `idea_path`, `report_path`, `plans_index_path`, `spec_path`, `specs_index_path`, `spec_documents`, `primary_plans`, `support_documents`.
- Plan-level constants in `src/generation.rs:108-116` declare `# IMPLEMENTATION_PLAN` plus `## Priority Work`, `## Follow-On Work`, and `## Completed / Already Satisfied`; `src/generation.rs:1752-1768` is the prompt contract requiring generated specs to include `## Objective`, `## Evidence Status`, `## Acceptance Criteria`, `## Verification`, and `## Open Questions`.
- Spec filename pattern is `ddmmyy-<topic-slug>[-<counter>].md` (`src/generation.rs:1754` path formatter).
- Spec sync results are tracked by `SpecSyncSummary { appended_paths, skipped_count }` at `src/generation.rs:55`.
- `auto reverse` does not rewrite the root `IMPLEMENTATION_PLAN.md`; gen does. Corpus claim: "Reverse treats the codebase as truth, specs as documentation-only" (`corpus/SPEC.md` row for `reverse`).
- `auto corpus` archives the previous `genesis/` snapshot into `.auto/fresh-input/` before wiping (behavior confirmed in `corpus/SPEC.md` §"Archive-then-wipe" and runs via `generation.rs` layout).
- `auto corpus` supports `--focus "..."` (writes `genesis/FOCUS.md`) and `--idea "..."` (writes `genesis/IDEA.md`, also seeds a pre-corpus office-hours shaping pass). Described at `README.md:102-128,142-150`.
- Stage-by-stage observability to stdout is documented at `README.md:138-141`: binary provenance, repo root, prompt log path, Claude phase markers, Claude PID, cwd, elapsed timings.
- `genesis/` artifacts emitted when present: `ASSESSMENT.md`, `SPEC.md`, `PLANS.md`, `GENESIS-REPORT.md`, `DESIGN.md` (only for repos with meaningful UI surfaces), `FOCUS.md` / `IDEA.md` (only when flags used), `plans/*.md` (`README.md:102-112`).
- Specs under `specs/` survive across runs; plan queue is replaced each `auto gen` run, with still-open tasks from the prior plan appended back (`corpus/DESIGN.md` §"Artifacts as information system" + `README.md:238`).

### Recommendations (intended direction from corpus)

- `corpus/DESIGN.md` §"Decisions to recommend" proposes documenting when to use `steward` vs `corpus + gen` so operators choose the right entry point for mid-flight vs greenfield. Implementation is research-only in `corpus/plans/012-command-lifecycle-reconciliation-research.md`.
- Spec-duplicate detection (`SpecSyncSummary::skipped_count`) is described as advisory in `corpus/DESIGN.md` §"AI-slop risk"; tighter semantic-duplicate guarding is a future idea, not wired.

### Hypotheses / unresolved questions

- Whether `auto corpus --dry-run` is a full preview or stubs the invocation is flagged in `corpus/ASSESSMENT.md` §"DX assessment"; source evidence for the exact preview shape was not extracted in this pass.
- `DESIGN.md` is only emitted for repos with "meaningful UI surfaces"; the exact decision rule (file-type heuristic vs README directive) is not source-verified in this spec.

## Acceptance Criteria

- `auto corpus` writes every path in the `PlanningCorpus` struct set that applies to the run, plus `genesis/plans/*.md` when the planner produced them.
- `auto corpus` moves any pre-existing `genesis/` tree into `.auto/fresh-input/genesis-<timestamp>/` before writing new files; the new `genesis/` does not merge with the archived snapshot.
- `auto corpus --focus "<str>"` writes `genesis/FOCUS.md` and uses it as biasing context without skipping the full-repo sweep.
- `auto corpus --idea "<str>"` writes `genesis/IDEA.md` and runs a pre-corpus shaping pass that produces the office-hours-style brief documented in `README.md:142-150`.
- `auto corpus` prints stage markers to stdout: binary provenance line, repo root, prompt log path, claude phase start/finish markers with PID, and per-phase elapsed timings.
- `auto gen` writes one or more markdown files under `specs/` with filename shape `ddmmyy-<slug>[-N].md` and each file begins with `# Specification:` and includes `## Objective`, `## Acceptance Criteria`, `## Verification`.
- `auto gen` produces or updates `IMPLEMENTATION_PLAN.md` with the three required top-level sections declared in `src/generation.rs:108-116`.
- `auto gen` preserves still-open (`- [ ]`, `- [!]`) tasks from the previous plan by re-appending them to the new plan rather than silently dropping them.
- `auto gen` does not re-append tasks that were already completed (`- [x]`).
- `auto reverse` writes under `specs/` with the same filename shape and required sections as `auto gen` but does not overwrite, replace, or append to the root `IMPLEMENTATION_PLAN.md`.
- Every spec produced by `auto gen` / `auto reverse` includes `## Evidence Status` and `## Open Questions` (current behavior of the required-section check).
- Each run writes a prompt log under `.auto/logs/<command>-<timestamp>-prompt.md`.
- When invoked in a repo with no reachable `claude` binary on `PATH`, the command exits non-zero with a message that names `claude` as the missing dependency.
- `SpecSyncSummary::skipped_count` reports specs that matched an existing file with the same stem in the same day bucket; the skipped specs are not re-written.

## Verification

- Run `auto corpus` against a fixture repo with a pre-existing `genesis/`; confirm the old snapshot lands in `.auto/fresh-input/`.
- Run `auto corpus --focus "hardening"`; assert `genesis/FOCUS.md` exists with the normalized brief.
- Run `auto gen` twice back-to-back; assert still-open tasks survive and completed tasks do not re-appear.
- Run `auto reverse`; assert `IMPLEMENTATION_PLAN.md` modification time is unchanged and new `specs/*.md` files exist.
- Unit tests in `src/generation.rs` (43 tests per the Gathered-evidence report) cover markdown extraction, task-block parsing, and dated-slug collision.
- Grep the resulting spec files for `## Objective` / `## Acceptance Criteria` / `## Verification` / `## Evidence Status` / `## Open Questions`; all five must be present.
- Integration smoke: `cargo test -p autodev generation` passes; any added fixture test for the archive-then-wipe flow also passes.

## Open Questions

- Is `DESIGN.md` generation gated only on heuristic UI-surface detection, or does the planner model make that call? Not source-verified in this pass.
- Should `auto reverse` ever update `IMPLEMENTATION_PLAN.md` when the diff is purely "completed" (for example, mark a task `- [x]` if the code reflects it)? Current behavior is strict read-only; may deserve an explicit note in the README.
- How should `auto gen` resolve collisions where two specs generated in the same run serialize to identical slug-date stems with different content? `skipped_count` tracks it but the collision-resolution rule is advisory.
- `auto corpus --dry-run`: is it currently a cheap preview or a stub? Flagged in `corpus/ASSESSMENT.md` but not resolved.
