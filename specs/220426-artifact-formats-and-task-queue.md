# Specification: Artifact formats and task-queue protocol

## Objective

Lock the shared on-disk contract every command reads or writes: the `- [ ]` / `- [!]` / `- [x]` / `- [~]` task-queue markers in `IMPLEMENTATION_PLAN.md`, the required sections on `specs/*.md`, the verification-receipt JSON shape under `.auto/symphony/verification-receipts/`, and the handoff loop between `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, and `LEARNINGS.md`. These formats are the integration surface across `gen`, `loop`, `parallel`, `review`, `qa`, `ship`, `nemesis`, `audit`, and `steward`, and they must remain parseable by all of them without ad-hoc dialects.

## Evidence Status

### Verified facts (code)

- Plan-level required sections declared as constants in `src/generation.rs:108-116`: `IMPLEMENTATION_PLAN_HEADER` (`# IMPLEMENTATION_PLAN`), `SPEC_OBJECTIVE_HEADER` (`## Objective`), `SPEC_ACCEPTANCE_CRITERIA_HEADER` (`## Acceptance Criteria`), `SPEC_VERIFICATION_HEADER` (`## Verification`), `REQUIRED_PLAN_SECTIONS = ["## Priority Work", "## Follow-On Work", "## Completed / Already Satisfied"]`.
- Spec filename shape: `ddmmyy-<topic-slug>[-<counter>].md` (`src/generation.rs:1754`).
- Task markers parsed identically across `loop_command.rs`, `parallel_command.rs`, `review_command.rs`, `generation.rs` (per `corpus/SPEC.md` §"Task queue protocol"):
  - `- [ ]` pending and runnable,
  - `- [!]` blocked (skipped by `loop`),
  - `- [x]` completed,
  - `- [~]` partially done / historical gap.
- Verification receipt path `.auto/symphony/verification-receipts/<task_id>.json` (`src/completion_artifacts.rs:123`) with shape defined by `TaskCompletionEvidence` (`src/completion_artifacts.rs:13-20`): `has_review_handoff`, `verification_receipt_path`, `verification_receipt_present`, `verification_receipt_status`, `declared_completion_artifacts`, `missing_completion_artifacts`.
- `completion_artifacts.rs` has ~13-16 tests covering receipt validation and narrative-only rejection.
- Artifact-role lifecycle per `corpus/DESIGN.md` §"State coverage" table:
  - `genesis/` — corpus, disposable, wiped-on-fresh-run, archive into `.auto/fresh-input/`.
  - `specs/` — durable, dated slugs, dedupes by stem within a day.
  - `IMPLEMENTATION_PLAN.md` — durable, append/consume queue.
  - `COMPLETED.md` → `REVIEW.md` → `ARCHIVED.md` — handoff chain.
  - `WORKLIST.md` — review/audit/qa/ship append, operator drains.
  - `LEARNINGS.md` — review/qa/ship append-only.
  - `QA.md`, `HEALTH.md`, `SHIP.md` — overwritten each pass.
  - `bug/`, `nemesis/` — disposable, archive on fresh run.
  - `audit/` — durable between runs via manifest hash resume.
- Atomic writes through `util::atomic_write` (`src/util.rs:404`) with temp filename `.{filename}.tmp-{pid}-{nanos}`.

### Verified facts (generation-side guarantees)

- `SpecSyncSummary { appended_paths, skipped_count }` at `src/generation.rs:55` counts dedup skips at write time.
- `auto gen` plan-merge behavior: still-open items from prior plan are re-appended to the new plan; completed items are not (`corpus/DESIGN.md`; README:238).
- `auto reverse` writes specs only; it does not touch `IMPLEMENTATION_PLAN.md` (`corpus/SPEC.md` row).

### Recommendations (corpus)

- Consolidate task-marker parsing into a single helper in `util.rs` or a new small module so the format has one source of truth (`corpus/plans/007-shared-util-extraction.md`).
- Consider a shared "artifact" trait or write-through helper for the append-only artifacts (`WORKLIST.md`, `LEARNINGS.md`, `ARCHIVED.md`) if the three converge on identical append semantics.

### Hypotheses / unresolved questions

- Semantic-duplicate spec detection is advisory today; two specs identical except for whitespace in the same run may both be written (`corpus/DESIGN.md` §"AI-slop risk").
- Precise behavior for a `- [~]` task on a subsequent `auto loop` pass (retry, skip, escalate) is not uniformly documented across callers.

## Acceptance Criteria

### Plan format (`IMPLEMENTATION_PLAN.md`)

- The file starts with `# IMPLEMENTATION_PLAN` at the top of the document.
- The file contains the three top-level headers in this order: `## Priority Work`, `## Follow-On Work`, `## Completed / Already Satisfied`.
- Task rows use exactly one of the four markers: `- [ ]` (pending), `- [!]` (blocked), `- [x]` (completed), `- [~]` (partial / historical).
- Task bodies contain labeled metadata blocks that the command set recognizes: at minimum `Spec:`, `Acceptance:`, `Verification:`, `Dependencies:`, `Completion signal:`. Other labels (for example, `Owned surfaces:`, `Scope:`, `Evidence:`, `Estimated scope:`) may appear and must not break parsing.
- A task marked `- [!]` is never auto-converted to `- [ ]` or `- [x]` by `auto loop` or `auto parallel`; only the operator (or an explicit unblock pass) may transition it.
- A task marked `- [~]` is re-visitable by subsequent `auto loop` / `auto parallel` runs without being treated as completed.

### Spec format (`specs/*.md`)

- Each spec file starts with `# Specification:` as its first heading.
- Each spec file contains every required section: `## Objective`, `## Acceptance Criteria`, `## Verification`, `## Evidence Status`, `## Open Questions`.
- Acceptance criteria are flat bullets, not prose paragraphs.
- Filename pattern matches `ddmmyy-<topic-slug>[-<counter>].md`.
- Same-day same-slug collisions are either deduplicated (no re-write) or get a `-<counter>` suffix.

### Verification receipt shape (`.auto/symphony/verification-receipts/<task_id>.json`)

- The receipt is valid JSON that deserializes into `TaskCompletionEvidence`.
- `verification_receipt_status` is a string naming the receipt's state (for example, `"passed"`, `"failed"`, `"skipped"`).
- A task whose plan body has an executable `Verification:` step and whose receipt is missing or has `verification_receipt_present == false` must not be marked `- [x]` by `auto parallel`.

### Handoff chain (completed → review → archive/worklist/learnings)

- `COMPLETED.md` receives entries when `auto loop` finishes a task it could declare complete but has not yet been reviewed.
- `auto review` drains `COMPLETED.md` entries into `REVIEW.md` as handoff sections.
- Reviewed items land in `ARCHIVED.md` (passed) or `WORKLIST.md` (regressions / follow-ups) with the review verdict attached.
- Durable institutional knowledge surfaced during review, qa, or ship is appended to `LEARNINGS.md`.
- All six files are written through `util::atomic_write`.

### Overwrite-per-pass files

- `QA.md`, `HEALTH.md`, `SHIP.md` are overwritten in full on each pass; operators rely on git history for diffs.

### Disposable-with-archive files

- `genesis/`, `bug/`, `nemesis/` are archived into `.auto/fresh-input/<name>-<timestamp>/` before a fresh run wipes them.

## Verification

- `cargo test -p autodev generation` exercises the required-section check and plan-merge preserve-open behavior.
- `cargo test -p autodev completion_artifacts` locks the receipt contract.
- Property-test (`proptest` if introduced per global guide, or fixture-based): feed random plan bodies using the four markers and assert all command parsers converge on the same set of runnable tasks.
- Fixture test: seed a plan with a `- [!]` task, run `auto loop`; assert the task remains `- [!]` after the run.
- Fixture test: write a spec file missing `## Evidence Status`; assert `auto gen` flags it during the generation contract check.
- Cross-command fuzzing: generate a fresh plan with `auto gen`, mutate its tasks, run `auto loop` once, run `auto review` once; assert the end state is internally consistent (no duplicate `- [x]` rows, no lost tasks).

## Open Questions

- Should `- [~]` have a machine-readable reason field in the task body so the next run knows whether to retry automatically?
- Should the task-marker regex be extracted to a shared constant (Plan 007 research territory)?
- Should the spec dedupe rule compare canonicalized content (strip whitespace, normalize bullets) rather than exact bytes to catch semantic duplicates?
- Is there a need for a machine-readable "plan manifest" summarizing Priority / Follow-On / Completed counts, to let CI gate on plan health?
- Do the append-only artifacts (`WORKLIST.md`, `LEARNINGS.md`, `ARCHIVED.md`) benefit from a shared writer trait, or is duplication tolerable at current scale?
