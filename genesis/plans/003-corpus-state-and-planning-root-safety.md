# Corpus State And Planning Root Safety

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` current as work proceeds. No root `PLANS.md` exists in this checkout; if one is added, maintain this plan in accordance with it.

## Purpose / Big Picture

This slice makes `auto corpus` and `auto gen` safe as production control primitives. The operator gains confidence that a failed corpus run cannot leave the planning root empty, that generation cannot silently trust a corrupted saved path, and that a planning corpus with zero numbered plans fails before producing misleading downstream work.

## Requirements Trace

- R1: Saved planning roots must be repository-contained or explicitly supplied and confirmed.
- R2: Corpus regeneration must preserve the previous corpus until a complete replacement passes validation.
- R3: Planning corpus loading must reject an empty primary plan set.
- R4: `auto gen` must explain whether it used an explicit CLI path, saved state, or the default `genesis/`.
- R5: The current `genesis/` directory must remain complete after this refresh.

## Scope Boundaries

This plan does not change plan content quality prompts except where needed for safety messaging. It does not promote `genesis/` into the active root queue. It does not alter `IMPLEMENTATION_PLAN.md` task semantics.

## Progress

- [x] 2026-04-30: Verified current `genesis/` has 12 numbered plans; degraded pre-refresh `genesis/` state is authoring-pass evidence, not current filesystem state.
- [x] 2026-04-30: Verified `.auto/state.json` points at `genesis/`.
- [x] 2026-04-30: Verified `src/state.rs` stores raw `PathBuf` planning roots.
- [x] 2026-04-30: Verified `prepare_planning_root_for_corpus` removes the planning root after archiving.
- [x] 2026-04-30: Verified `load_planning_corpus` does not reject an empty `plans/` directory.
- [ ] Add planning-root containment checks.
- [ ] Make corpus writes stage-and-swap.
- [ ] Reject empty primary plan sets everywhere a corpus is loaded.

## Surprises & Discoveries

- Generated-output verification already rejects zero numbered plans, but input corpus loading can still represent an empty primary plan set.
- `auto corpus` is stricter about output shape than `auto gen` is about saved planning-root provenance.

## Decision Log

- Mechanical: Saved state must be treated as untrusted because it is runtime-generated local JSON.
- Mechanical: Empty `plans/` should be an error, not a valid empty corpus.
- Taste: Prefer stage-and-swap over in-place cleanup because it preserves the previous corpus until the new one is complete.
- User Challenge: Production `auto gen` should probably prefer snapshot/review mode before mutating root ledgers, even though the control primitive remains `auto gen`.

## Outcomes & Retrospective

None yet.

## Context and Orientation

Relevant files:

- `src/state.rs`: loads and saves `.auto/state.json`.
- `src/generation.rs`: `run_corpus`, `run_generation`, planning-root selection, corpus preparation, generated-output verification.
- `src/corpus.rs`: `load_planning_corpus` and corpus snapshot emission.
- `src/util.rs`: `atomic_write`, `copy_tree`, repository layout helpers.
- `genesis/`: current planning corpus.
- `.auto/fresh-input/`: archive location for previous corpora.

Non-obvious terms:

- Planning root: directory containing `ASSESSMENT.md`, `SPEC.md`, `PLANS.md`, `GENESIS-REPORT.md`, and `plans/`.
- Stage-and-swap: write a complete replacement to a temporary sibling directory, validate it, then rename it into place.
- Saved state: `.auto/state.json`, which records prior planning roots and output dirs.

## Plan of Work

Add a shared planning-root resolver that records provenance: explicit CLI, saved state, or default. It must reject saved absolute/outside-repo paths unless an explicit CLI argument is supplied. For default `genesis/`, require repository containment. Update `run_generation` and corpus preparation to use this resolver.

Change corpus preparation so the old planning root is copied to the archive and left in place until the new staged root validates. After validation, atomically swap or carefully rename the staged root into place. If swap cannot be atomic across platforms, keep a durable recovery path that restores the archived corpus on failure.

Update `load_planning_corpus` to reject empty primary plan sets. Add tests for empty `plans/`, nested `corpus/plans/`, missing mandatory files, and saved outside-repo planning roots.

## Implementation Units

- Unit 1: Planning-root provenance and containment.
  - Goal: Prevent saved state from steering destructive operations outside approved locations.
  - Requirements advanced: R1, R4.
  - Dependencies: none.
  - Files to create or modify: `src/state.rs`, `src/generation.rs`, tests in `src/generation.rs` or a new test module.
  - Tests to add or modify: saved absolute path outside repository is rejected; explicit path reports provenance; default `genesis/` works.
  - Approach: Resolve paths relative to repository root where possible and reject outside-repo saved state by default.
  - Test scenarios: `.auto/state.json` with outside planning root fails with a clear error before deletion.

- Unit 2: Non-empty corpus loading.
  - Goal: Make empty planning corpora impossible to consume.
  - Requirements advanced: R3.
  - Dependencies: none.
  - Files to create or modify: `src/corpus.rs`, corpus tests.
  - Tests to add or modify: empty `plans/` returns an error; numbered markdown file returns one primary plan; support docs do not count.
  - Approach: After collecting primary paths, bail if empty.
  - Test scenarios: `auto gen --planning-root genesis` fails when `genesis/plans/` has no numbered files.

- Unit 3: Stage-and-swap corpus writes.
  - Goal: Preserve previous corpus until replacement validates.
  - Requirements advanced: R2, R5.
  - Dependencies: Unit 1.
  - Files to create or modify: `src/generation.rs`, possibly `src/util.rs`.
  - Tests to add or modify: simulated validation failure leaves previous corpus intact; archive copy still created; staged temp cleaned.
  - Approach: Generate into a temporary planning root, verify required files and numbered plans, then replace the live root.
  - Test scenarios: injected failure after partial generation does not leave `genesis/` empty.

- Unit 4: Operator-facing messages.
  - Goal: Make provenance and recovery visible.
  - Requirements advanced: R4.
  - Dependencies: Units 1-3.
  - Files to create or modify: `src/generation.rs`, README quickstart if needed.
  - Tests to add or modify: stdout snapshot or focused string tests for provenance messages.
  - Approach: Print planning-root source and archive/recovery path.
  - Test scenarios: `auto gen --snapshot-only` output says whether it used saved state or explicit path.

## Concrete Steps

From the repository root:

    rg -n "planning_root|prepare_planning_root_for_corpus|load_planning_corpus|latest_output_dir" src

Expected observation: all planning-root read/write paths.

    cargo test corpus
    cargo test generation

Expected observation before work: new empty-corpus and outside-state tests fail.

After implementation:

    cargo test corpus
    cargo test generation
    cargo clippy --all-targets --all-features -- -D warnings

Expected observation: corpus and generation safety tests pass.

## Validation and Acceptance

Acceptance requires a failing-before/passing-after test for empty `plans/`, a corrupted saved state path that fails before deletion, and a simulated corpus-generation failure that preserves the previous corpus. Operator output must say which planning root was used and why.

## Idempotence and Recovery

Repeated corpus runs should archive previous roots and either complete the swap or leave the old root intact. If a staged directory remains after interruption, the next run should identify it as stale and remove or archive it safely. If `.auto/state.json` is invalid, the command should fail closed with remediation instructions.

## Artifacts and Notes

- Evidence to fill in: test fixture for empty `genesis/plans/`.
- Evidence to fill in: recovery proof after staged corpus failure.
- Evidence to fill in: command output showing planning-root provenance.

## Interfaces and Dependencies

- CLI: `auto corpus`, `auto gen`, `auto reverse`, `auto super`.
- Files: `.auto/state.json`, `.auto/fresh-input/`, `genesis/`, generated `gen-*` outputs.
- Modules: `generation`, `corpus`, `state`, `util`.
