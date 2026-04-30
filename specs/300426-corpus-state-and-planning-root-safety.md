# Specification: Corpus State And Planning Root Safety

## Objective

Make corpus generation and spec generation safe against empty plan sets, unsafe saved planning roots, and destructive in-place corpus refreshes.

## Source Of Truth

- Runtime owners: `src/generation.rs`, `src/corpus.rs`, `src/state.rs`, `src/util.rs`, `src/spec_command.rs`, `src/super_command.rs`.
- Planning owners: `genesis/`, `.auto/state.json`, `.auto/fresh-input/`, `gen-*`, root `specs/`, root `IMPLEMENTATION_PLAN.md`.
- UI consumers: `auto corpus`, `auto corpus --verify-only`, `auto gen`, `auto gen --snapshot-only`, `auto gen --sync-only`, `auto reverse`, `auto super`, generated review reports.
- Generated artifacts: `genesis/**`, `.auto/fresh-input/*`, `gen-*/corpus/**`, `gen-*/specs/*.md`, `gen-*/IMPLEMENTATION_PLAN.md`, `.auto/logs/*`, `.auto/state.json`.
- Retired/superseded surfaces: accepted empty `plans/` corpus roots, saved absolute planning roots outside the repo without explicit confirmation, and in-place deletion of a prior corpus before the replacement validates.

## Evidence Status

Verified facts grounded in code or primary repo files:

- `.auto/state.json` stores raw `planning_root` and `latest_output_dir` `PathBuf` values, verified by `rg -n "planning_root|latest_output_dir|PathBuf" src/state.rs` and `nl -ba .auto/state.json`.
- `load_planning_corpus` rejects a missing `plans/` directory but currently builds an empty `primary_plans` vector if no numbered plan files are found, verified by `rg -n "load_planning_corpus|primary_plans|collect_plan_paths" src/corpus.rs`.
- `is_primary_plan_file` treats files starting with an ASCII digit as primary plans, verified by `rg -n "is_primary_plan_file|is_ascii_digit" src/corpus.rs`.
- `run_generation` snapshots the loaded corpus into the output dir before authoring generated specs, verified by `rg -n "load planning corpus|emit_corpus_snapshot|generate specs" src/generation.rs`.
- `verify_generated_specs` enforces required generated spec sections, verified by `rg -n "REQUIRED_SPEC_SECTIONS|verify_generated_specs|generated_spec_has_section" src/generation.rs`.
- The current corpus has twelve numbered plans, verified by `find genesis/plans -maxdepth 1 -type f -name '[0-9][0-9][0-9]-*.md' | sort`.

Recommendations for the intended system:

- Validate saved planning roots as repo-contained unless the operator supplies and confirms an explicit external path.
- Reject an empty primary plan set in `load_planning_corpus`, not only after generated output is produced.
- Replace destructive corpus refresh with stage-and-swap: generate into a sibling temp directory, verify, then rename.
- Print planning-root provenance on `auto gen`: explicit CLI path, saved state, or default `genesis/`.

Hypotheses / unresolved questions:

- Existing workflows may rely on external planning roots. The confirmation UX and persistence policy are not decided.
- `auto reverse` may need a different empty-corpus policy than `auto gen`; current evidence does not prove that distinction.
- The retention policy for `.auto/fresh-input/` should follow existing `src/util.rs` pruning constants unless changed by a focused performance/storage decision.

## Runtime Contract

- `state` owns only persisted hints; it does not make a path trusted.
- `corpus` owns corpus loading and must fail closed when `plans/` is missing or primary plans are empty.
- `generation` owns corpus preparation, output-dir preparation, generated output verification, and sync.
- `util` owns atomic writes, copy-tree helpers, and generated/runtime state pruning.
- If a planning root is absent, outside policy, empty, malformed, or fails generated-output validation, no root specs or root plan files may be overwritten.

## UI Contract

- Terminal output must state which planning root was used and why.
- `auto gen` output must not imply saved state is authoritative without validation.
- README and help text must describe `genesis/` as planning input and `gen-*` as snapshots until sync.
- UI consumers must render corpus counts from `load_planning_corpus`/verification results, not from hard-coded plan counts.
- Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- `genesis/**` when `auto corpus` authors or verifies a corpus.
- `.auto/fresh-input/*` when previous corpus content is archived.
- `gen-*/corpus/**`, `gen-*/specs/*.md`, `gen-*/IMPLEMENTATION_PLAN.md` during generation.
- `.auto/state.json` after generation state is saved.
- `.auto/logs/*-prompt.md`, `*-stdout.log`, `*-stderr.log`, and review reports.

## Fixture Policy

- Empty-corpus, corrupt-state, and failed-generation fixtures belong in temp dirs inside tests.
- Production generation must not fall back to fixture corpora, older `gen-*` snapshots, or archived `.auto/fresh-input/*` without explicit operator recovery.
- Test corpora must use clearly synthetic titles and task ids so they cannot be mistaken for root execution work.

## Retired / Superseded Surfaces

- Retire empty primary plan sets as valid `PlanningCorpus`.
- Retire saved external planning roots that are silently accepted as current input.
- Retire direct root sync from unverified generated outputs.

## Acceptance Criteria

- `load_planning_corpus` fails with an actionable error when `plans/` exists but contains no numbered primary plan files.
- `auto gen` prints whether it used `--planning-root`, `.auto/state.json`, or default `<repo>/genesis`.
- A corrupt or external saved planning root fails before any deletion or root sync unless explicitly confirmed by the operator.
- A simulated corpus authoring failure preserves the previous valid `genesis/`.
- `auto corpus --verify-only` validates current corpus shape without mutating corpus files.
- `auto gen --snapshot-only` writes a complete `gen-*` tree and leaves root ledgers unchanged.

## Verification

- `cargo test corpus::tests`
- `cargo test generation::tests`
- `cargo test state::tests`
- `rg -n "load_planning_corpus|primary_plans|is_primary_plan_file" src/corpus.rs`
- `rg -n "prepare_planning_root_for_corpus|verify_corpus_outputs|verify_generated_specs|sync_verified_generation_outputs" src/generation.rs`
- `rg -n "planning_root|latest_output_dir" src/state.rs .auto/state.json`
- `find genesis/plans -maxdepth 1 -type f -name '[0-9][0-9][0-9]-*.md' | sort | wc -l`

## Review And Closeout

- A reviewer runs fixture tests for empty corpus, corrupt saved state, and failed stage-and-swap.
- Grep proof must show there is one policy helper for planning-root containment and that `auto corpus`, `auto gen`, `auto reverse`, and `auto super` use it or explicitly document why not.
- The reviewer checks `git status --short` before and after a failed corpus refresh fixture to prove the previous corpus survives.
- Closeout records any intentionally supported external-planning-root flow with its confirmation requirement.

## Open Questions

- Should external planning roots be allowed only by flag, or should saved external roots also be accepted after confirmation?
- Should `auto corpus --verify-only` update `.auto/state.json`?
- Should `auto gen` reject support-only corpora in reverse mode, or allow them as research input?
