# Thermo-Nuclear Findings — `generation.rs` + `audit_everything.rs`

Reviewed 12,037 lines. Counts: 2 P1, 6 P2, 9 P3, 4 P4, 3 P5, 2 P6, plus 4 correctness risks.

## P1 — File sprawl

**generation.rs (6,156)** -> `generation/`:
```
mod.rs           ~340   run_corpus/run_gen/run_reverse, GenerationMode, stage printing
planning_root.rs ~190   root resolution + staging
phase_runner.rs  ~330   codex/claude process spawning
prompts.rs       ~720   the 5 giant prompt builders + consts (pure, zero IO)
corpus_verify.rs ~520
spec_verify.rs   ~440   includes the 4 domain-specific lint_* spec checks
plan_verify.rs   ~620   plan-task validators + block extraction
root_sync.rs     ~310   sync-to-root logic
markdown.rs      ~120   the section parser
```

**audit_everything.rs (5,881)** -> `audit_everything/`:
```
mod.rs         ~200   the phase match
manifest.rs    ~260   the 30-field data model + StageStatus
run_paths.rs   ~190   RunPaths + every *_path helper
worktree.rs    ~120   worktree/pause/merge
inventory.rs   ~330   file enumeration + grouping
phases.rs      ~430   the 6 async phase drivers
workers.rs     ~280   work-stealing pools
remediation.rs ~620   the scheduler + dependency graph
file_quality.rs ~380  the rerate/deliverable gate
prompts.rs     ~700   ~9 prompt builders + skill-policy classifiers
status.rs      ~470   status rendering
git.rs         ~190   all git plumbing
context.rs     ~120   context bundle
```

## P2 — Spaghetti

- generation.rs:234-435 `run_corpus` 201-line procedure -> extract `print_corpus_summary`, `run_corpus_verify_only`.
- generation.rs:502-768 `run_generation` 266 lines, four bolted-on modes -> split into sync_only / full + shared summary.
- audit_everything.rs:899-1065 `run_remediation_lanes` 166-line scheduler loop; status-write repeated 5x -> extract `dispatch_ready_lanes`, `harvest_lane_result`, `set_task_status`.
- generation.rs:1935-2210 & 2447-2735 prompt builders are 130-275-line `format!` walls.
- audit_everything.rs:4457-4654 `write_run_status_markdown` 197 lines of `push_str`.
- generation.rs:3039-3104 `verify_generated_implementation_plan` builds an IIFE closure per task block -> named function.

## P3 — Code-judo

- generation.rs:1331 vs claude_exec.rs:297: duplicated AND divergent Claude classifier -> delete `author_phase_uses_claude_model`.
- generation.rs:1388-1456 `run_claude_prompt` hand-rolls `Command::new("claude")` -> route through `claude_exec`.
- Three independent markdown section parsers across both files -> one shared `markdown` module.
- audit_everything.rs:2460-2463 single-variant `enum GroupPhase { Synthesis }` -> delete enum + `phase` param from 3 fns.
- audit_everything.rs:3059-3062 `build_final_review_prompt` is `#[allow(dead_code)]` forwarder -> delete.
- generation.rs:437-492 three near-identical verify wrappers -> inline thin ones.
- audit_everything.rs:2050-2059 `next_ready_remediation_task_index` test-only wrapper -> tests call real fn.
- generation.rs:3179-3182,3396 four `let _ = plan_task_field_line_value(...)` no-op statements -> delete.
- generation.rs:275,539,724 `println!("review pass: ...")` verbatim 3x -> fold into helper.

## P4 — Types & boundaries

- audit_everything.rs:156-187 `EverythingManifest` 30-field god struct; path fields `String` -> `PathBuf`; split into `RunIdentity` + `PipelineState`.
- `in_place` bare bool threaded everywhere -> `RunMode { InPlace, Worktree }` enum.
- generation.rs plan validators stringly-typed -> `PlanField` enum / reuse task_parser constants.
- `anyhow` everywhere for a state machine with well-defined failure categories -> typed `AuditError`.

## P5 — Layering

- generation.rs: parse/orchestrate/prompt-build/process-spawn tangled -> P1 split fixes.
- audit_everything.rs: git shelled 3 ways -> consolidate into `git.rs`.
- audit_everything.rs:2202 & 2616 `if claude_route {...} else {...}` with 8 identical args copy-pasted -> one `run_phase_backend`.

## P6 — Cosmetic

- generation.rs:1105 `run_logged_codex_review` doc admits name is "historical" -> rename.
- audit_everything.rs:2787 `skill_summary` 28-arm match — acceptable.

## Bugs / correctness risks

1. Divergent Claude classifier: `author_phase_uses_claude_model` treats "" as Claude + uses `.contains()`; `looks_like_claude_model` treats "" as non-Claude + uses `.starts_with()`. Empty `--model` routes differently. One is wrong.
2. Lenient gate swallows validation (generation.rs:3056-3066): `AUTO_LENIENT_GATE=1`/`AUTO_LENIENT_DEPS=1` degrades ALL per-task validation to `eprintln!` warnings.
3. No-op statements (generation.rs:3179-3182,3396) read as enforcement but discard pure calls — misleading dead code.
4. Glob pathspec (audit_everything.rs:1751) `:(exclude)audit/everything/*/files/**` may disagree with non-globbed `rm --cached` on nested runs.

## Top refactor moves

1. Split audit_everything.rs into `audit_everything/` — SAFE.
2. Split generation.rs into `generation/` — SAFE.
3. Delete `author_phase_uses_claude_model`, route Claude through `claude_exec` — RISKY (changes routing).
4. Extract shared `markdown` section-parser module — SAFE.
5. Refactor `run_remediation_lanes` into dispatch/harvest/set_task_status — RISKY.
6. Delete dead `GroupPhase` enum + `build_final_review_prompt` — SAFE.
7. Collapse `run_generation` four-mode body — RISKY.
8. Unify codex-vs-claude branch into `run_phase_backend` — SAFE.
9. Type manifest path fields as `PathBuf`, add `RunMode` enum — RISKY (changes serialized manifest).
10. Remove no-op `let _` validation statements + stale comments — SAFE.
