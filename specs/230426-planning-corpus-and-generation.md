# Specification: Planning Corpus And Generation Snapshot

## Objective
Define the operator-facing contract for `genesis/`, `auto corpus`, `auto gen`, `auto reverse`, and generated snapshot directories so planning intent can guide future work without overriding live-code truth or mutating root planning surfaces during snapshot-only generation.

## Evidence Status

### Verified Facts

- The current top-level CLI command enum includes `Corpus`, `Gen`, `Super`, `Reverse`, `Bug`, `Loop`, `Parallel`, `Qa`, `QaOnly`, `Health`, `Review`, `Steward`, `Audit`, `Ship`, `Nemesis`, `Quota`, and `Symphony`; this is 17 command variants in `src/main.rs:53-99`.
- `auto corpus` defaults its planning root to `<repo>/genesis`, its authoring model to `gpt-5.5`, its authoring effort to `xhigh`, and its authoring parallelism to `5` in `src/main.rs:393-444`.
- `auto gen` and `auto reverse` share `GenerationArgs`, including output directory, model, effort, Codex review settings, `--plan-only`, `--sync-only`, and `--parallelism` in `src/main.rs:452-499`.
- `load_planning_corpus` requires a `plans/` directory, reads optional corpus documents, loads existing spec documents, and stores support files in `src/corpus.rs:39-93`.
- `emit_corpus_snapshot` writes optional corpus documents, plans, support files, and existing spec documents under an output root in `src/corpus.rs:95-125`.
- `run_generation` emits a corpus snapshot, writes generated specs and an implementation plan, invokes Codex review unless skipped, and syncs generated outputs to root planning surfaces in `src/generation.rs:376-610`.
- The spec-generation prompt requires generated specs to live under `<output>/specs/`, start with `# Specification:`, use code as authoritative for current facts, and separate verified facts from recommendations and hypotheses in `src/generation.rs:1990-2085`.
- `verify_generated_specs` rejects missing `specs/`, empty spec output, missing `# Specification:`, missing required sections, and acceptance sections without bullets or structured criteria in `src/generation.rs:2227-2293`.
- `merge_generated_plan_with_existing_open_tasks` preserves parsed unchecked existing tasks by task ID when merging a generated plan in `src/generation.rs:3335-3356`.
- The current generation plan parser recognizes `[ ]`, `[~]`, `[x]`, and `[X]` task headers, but not `[!]`, in `src/generation.rs:3494-3509`.
- The planning corpus says the repo should stop letting stale root planning truth contradict code, and it orders root planning reconciliation before quota, Symphony, verification, parser, first-run, CI, and release gates in `genesis/plans/001-master-plan.md:9` and `genesis/PLANS.md:29-40`.
- `genesis/GENESIS-REPORT.md:11` says the repo has sixteen commands, while live `src/main.rs:53-99` shows 17 command variants; this is a corpus/code conflict and live code is authoritative for the snapshot.

### Recommendations

- Add a snapshot-only generation mode that writes under a requested output directory without syncing root `specs/` or root `IMPLEMENTATION_PLAN.md`; this follows the current task contract and avoids the root-sync behavior verified in `src/generation.rs:568-610`.
- Promote `genesis/` as supporting corpus input, not as an alternative active control plane, until root planning truth reconciliation is complete; this follows `genesis/plans/002-root-planning-truth-reconciliation.md:1` and `src/super_command.rs:416`.
- Treat the missing `[!]` branch in generation parsing as a preservation bug to fix before relying on generated plan merges for blocked tasks; the intended shared parser work is described in `genesis/plans/007-shared-task-parser-and-blocked-task-preservation.md:7`.

### Hypotheses / Unresolved Questions

- It is unresolved whether the right product shape is a new `--spec-only` or `--no-sync-root` flag, a dedicated subcommand, or a documented direct snapshot writer.
- It is unresolved whether root `specs/` dated `220426-*` should be replaced automatically by a generated `230426-*` snapshot or only through an explicit promotion command.
- It is unresolved whether `auto steward` should become the preferred ongoing planning command after `auto corpus` and `auto gen` create an initial corpus.

## Acceptance Criteria

- A snapshot-only generation path writes markdown specs only under the requested output directory and does not modify root `specs/`, root `IMPLEMENTATION_PLAN.md`, `genesis/`, or source files.
- Generated spec filenames match `^[0-9]{6}-[a-z0-9-]+\.md$` and use the snapshot date as the leading `ddmmyy` value.
- Every generated spec starts with `# Specification:` and contains `## Objective`, `## Evidence Status`, `## Acceptance Criteria`, `## Verification`, and `## Open Questions`.
- Each `## Evidence Status` section has separate subsections for verified facts, recommendations, and hypotheses or unresolved questions.
- Any current-state fact about commands, defaults, files, metrics, tests, or behavior includes a file path with line number, command, or primary-source citation.
- A generated plan merge preserves `[ ]`, `[~]`, and `[!]` existing tasks unless the replacement task with the same ID is explicitly present in the generated plan.
- Corpus statements that contradict live code are called out as conflicts and are not restated as verified current facts.

## Verification

- `rg -n "^# Specification:|^## Objective|^## Evidence Status|^## Acceptance Criteria|^## Verification|^## Open Questions" gen-20260423-210325/specs/*.md`
- `find gen-20260423-210325/specs -maxdepth 1 -type f -name '*.md' -printf '%f\n' | sort`
- `cargo test generation_prompt_makes_code_authoritative_for_current_state_facts`
- `cargo test generation::tests::merge_generated_plan_with_existing_open_tasks`
- Add and run a regression proving `[!]` tasks survive generated plan merge.

## Open Questions

- Should snapshot-only generation be a public CLI mode or an internal generation-pass contract?
- Should `genesis/` remain checkpoint-stageable, or should generated-corpus inputs be protected like `gen-*` outputs?
- Should a future generator refuse to continue when corpus command counts disagree with `src/main.rs`?
