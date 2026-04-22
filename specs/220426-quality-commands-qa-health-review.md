# Specification: Quality commands — `auto qa`, `auto qa-only`, `auto health`, `auto review`

## Objective

Lock the four quality-surface commands so each produces a well-defined durable report, applies the documented rebase posture where the current code actually mutates and pushes, and distinguishes "report only" runs from mutating runs. `auto qa` fixes bounded issues and writes `QA.md`; `auto qa-only` produces the same report without touching code; `auto health` produces a repo-wide `HEALTH.md` with a 0-10 score and lane sub-scores; `auto review` drains `COMPLETED.md` and `REVIEW.md`, appending `ARCHIVED.md`, `WORKLIST.md`, and `LEARNINGS.md` entries as it goes.

## Evidence Status

### Verified facts (code)

- `src/main.rs:67-73` declares `Qa`, `QaOnly`, `Health`, `Review`.
- `src/main.rs:927-960` `QaArgs`: default model `gpt-5.4`, default reasoning effort `high`, default tier `QaTier::Standard`, default Codex binary `codex`, default `max_iterations = 1`.
- `src/main.rs:962-991` `QaOnlyArgs`: same defaults as `QaArgs`, no iteration count (report-only by nature).
- `src/main.rs:993-1014` `HealthArgs`: default model `gpt-5.4`, default reasoning effort `high`.
- `src/main.rs:720-800+` `ReviewArgs`: default model `gpt-5.4`, default reasoning effort `high`.
- `auto qa` and `auto review` call `util::sync_branch_with_remote` before work and `util::push_branch_with_remote_sync` before push (`src/qa_command.rs:124-190`, `src/review_command.rs:203-270,395-415`).
- `auto qa-only` and `auto health` are report-only wrappers today: they validate an optional branch override, write prompt logs, invoke Codex, and do not call `sync_branch_with_remote` or push (`src/qa_only_command.rs:59-128`, `src/health_command.rs:40-109`).
- Artifact contract (per `corpus/SPEC.md` §"Artifact shapes"):
  - `auto qa` / `auto qa-only` → overwrite `QA.md` each pass.
  - `auto health` → overwrite `HEALTH.md` each pass; include a 0-10 score and per-lane sub-scores.
  - `auto review` → write `REVIEW.md` consumption, append to `ARCHIVED.md`, append/update `WORKLIST.md`, append to `LEARNINGS.md`; read and clear `COMPLETED.md` entries.
- `src/review_command.rs` owns a `StaleBatchTracker` for re-introduced items (per `corpus/DESIGN.md` §"AI-slop risk").
- QA depth tiers (`QaTier`): `Quick` (critical/high only), `Standard` (adds medium), `Exhaustive` (adds polish/cosmetic). Declared on `QaArgs::tier` and `QaOnlyArgs::tier` (`src/main.rs:958-990`).

### Verified facts (docs)

- `README.md:46-53` documents default branch = currently checked-out branch, default models/effort for all four commands, and that `ship` targets the resolved base branch.
- `README.md:623-675` documents QA tiers, matching `QaTier` enum values.
- `README.md:46-47`, `48-49` documents the `standard` tier default for both `qa` and `qa-only`.

### Recommendations (corpus)

- `corpus/DESIGN.md` §"Decisions to recommend" proposes a `--json` output mode for `qa`, `health`, `ship`, `audit` to enable CI integration; current code emits markdown only.
- `corpus/plans/011-integration-smoke-tests.md` targets end-to-end smoke tests for `qa`, `health`, `ship` (no coverage exists today in `qa_command`, `qa_only_command`, `health_command`; `ship_command` has one test per `corpus/ASSESSMENT.md` test-gaps table).

### Hypotheses / unresolved questions

- The exact HEALTH.md lane schema (which sub-scores are always present vs. model-chosen per repo) is not source-verified in this spec pass.
- Whether `auto review` ever moves items back from `ARCHIVED.md` to `REVIEW.md` if a later run catches a regression is unknown; no evidence cited either way.

## Acceptance Criteria

- `auto qa` and `auto qa-only` default to model `gpt-5.4`, reasoning effort `high`, tier `standard`.
- `auto qa` default `max-iterations` is `1`; a higher value runs the fix cycle that many times before stopping.
- `auto qa` may write code changes; `auto qa-only` must not modify tracked files outside `.auto/qa-only/` and `QA.md`.
- Both commands overwrite `QA.md` on each run; prior content is not preserved in that file (operators diff via git history).
- `auto health` writes `HEALTH.md` once per run and includes a numeric score in 0-10 range plus per-lane sub-scores; the same file is overwritten on subsequent runs.
- `auto health` does not modify code or other artifacts.
- `auto review` reads `COMPLETED.md` entries, compares each against live repo evidence, appends approved items to `ARCHIVED.md`, appends still-open issues to `WORKLIST.md`, and appends durable knowledge to `LEARNINGS.md`.
- `auto review` writes pending handoff sections into `REVIEW.md` and consumes prior `REVIEW.md` entries once actioned.
- `auto review` uses `StaleBatchTracker` to flag re-introduced items that were just archived in a prior pass.
- `auto qa` and `auto review` call rebase-before-work when `origin/<branch>` exists and rebase-before-push when they push.
- `auto qa-only` and `auto health` do not currently rebase or push; if report-only commands should also sync before reading, that is a follow-on decision rather than a current-state fact.
- All four commands default to the currently checked-out branch; `--branch` overrides.
- Writes of `QA.md`, `HEALTH.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, `LEARNINGS.md` go through `util::atomic_write`.
- Missing `codex` binary produces a named-dependency error, non-zero exit.

## Verification

- Fixture repo with a seeded `COMPLETED.md` plus a code change that matches one entry and contradicts another; run `auto review`; assert matched entry lands in `ARCHIVED.md` and contradicted entry lands in `WORKLIST.md`.
- Fixture repo with no tracked lint issues: run `auto qa`; assert `QA.md` exists and `auto qa-only` in a second fixture run produces no diff outside `QA.md` and `.auto/qa-only/`.
- Snapshot-test `HEALTH.md` output shape: must include a score string matching `/[0-9]{1,2}/10/` and at least one lane sub-score heading.
- Unit test for `StaleBatchTracker` re-introduction detection exists in `src/review_command.rs` test module (~30 tests; corpus count).
- Add smoke tests per `corpus/plans/011-integration-smoke-tests.md` that exercise `auto qa`, `auto health`, `auto review` against a hermetic fixture with a stubbed Codex binary.

## Open Questions

- Should the `--json` output mode proposed in `corpus/DESIGN.md` ship as a parallel file (for example, `HEALTH.json`) or a CLI flag that swaps stdout to JSON? Product decision.
- What should `auto review` do with a `COMPLETED.md` entry whose evidence cannot be evaluated (referenced file deleted) — archive, worklist, or hold for operator?
- Should `auto health` have its own tier knob like `auto qa`, or stay single-tier?
- Does `auto qa --max-iterations N` count iterations even if the first pass finds no issues? Behavior should be documented explicitly.
- Should report-only commands (`auto qa-only`, `auto health`) rebase before reading, or should they remain side-effect-light and report on the currently checked-out tree exactly as found?
