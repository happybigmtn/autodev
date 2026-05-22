# Thermo-Nuclear Findings — Remaining Command Modules

Reviewed 9,076 lines across 11 files. Counts: P1 x3, P2 x6, P3 x7, P4 x4, P5 x3, P6 x3. No outright bugs.

## P1 — File sprawl

**ship_command.rs (1,490)** -> `src/ship/`:
```
mod.rs    ~120  run_ship, run_ship_in_repo, ShipGatePhase, enforce_ship_gate
prompt.rs ~75   DEFAULT_SHIP_PROMPT_TEMPLATE, render_default_ship_prompt
gate.rs   ~330  ShipGateReport, VerificationReceipt*, evaluate_ship_gate, receipt loaders, check_*
branch.rs ~65   resolve_base_branch, parse_origin_head_branch, git_branch_exists
```

**design_command.rs (1,306)** -> `src/design/`:
```
mod.rs       ~190  run_design, run_super_design_module, DesignManifest, run_design_codex_phase
resolve.rs   ~280  run_design_resolution, run_design_parallel_pass, write_design_*_status
promotion.rs ~110  promote_design_plan_items_to_root_queue, extract_unchecked_design_plan_item_blocks
verify.rs    ~110  verify_design_artifacts, require_design_go, design_report_is_go
prompt.rs    ~250  build_design_prompt, build_design_parallel_prompt, DESIGN_ARTIFACTS
```

**loop_command.rs (1,124)** -> `src/loop_cmd/`:
```
mod.rs    ~260  run_loop driver, RepoProgress, TrackedRepoState, collect/summarize repo progress
prompt.rs ~120  DEFAULT_LOOP_PROMPT_TEMPLATE, render_default_loop_prompt, build_iteration_prompt
queue.rs  ~110  LoopQueueSnapshot, inspect_loop_queue, parse_loop_queue, reconcile/mark task helpers
refs.rs   ~110  resolve_reference_repos, discover_sibling_git_repos
branch.rs ~70   resolve_loop_branch, pick_loop_branch, parse_origin_head_branch, git_branch_exists
```

## P2 — Spaghetti

- design_command.rs:261-371 `run_design_resolution` 111-line driver, 5-branch tangle -> `FinalPassOutcome` enum.
- ship_command.rs:646-790 `run_ship_in_repo` 144 lines -> extract `finish_iteration`, `print_iteration_banner`.
- loop_command.rs:179-346 `run_loop` ~167 lines -> `describe_exit(code)`.
- design_command.rs:944-991 `verify_design_artifacts` nests 4 deep -> `let Err ... else`.
- doctor_command.rs:152-244 `check_planning_health` 92 lines, three checks -> split.
- linear_tracker.rs:224-263 `coverage_drift` nests 3 deep -> `classify_pending_task`.

## P3 — Code-judo

- A: qa_command.rs vs qa_only_command.rs parallel structures; `QaTier` match arms byte-identical; prompts ~80% identical -> unify behind `QaMode` enum. RISKY (prompts user-visible).
- B: qa_only_command.rs wrongly owns shared report-only infra (`ReportOnlyDirtyStateReport`, `collect_dirty_state`, etc.) used by health/design -> move to `report_only.rs`. SAFE.
- C: doctor_command.rs:328-371 repeatedly spelunks `toml::Value` -> parse once into `AutodevManifest`. SAFE.
- D: `KNOWN_PRIMARY_BRANCHES`, `parse_origin_head_branch`, `git_branch_exists`, `git_ref_exists` verbatim-duplicated in ship + loop -> hoist to `crate::util`. SAFE.
- E: design_command.rs:573-633 `run_super_design_module` duplicates `run_design` phase sequence -> fold. RISKY.
- G: ship_command.rs:480-526 `record_ship_gate_blockers_with_verdict` one-line forwarder; `record_ship_gate_bypass` is `#[cfg(test)]` in production body -> inline/move. SAFE.

## P4 — Types & boundaries

- A: boolean `report_only`/`apply`/`skip_qa` -> enums (`StewardMode`, `QaMode`).
- B: doctor `RequiredCheck { passed: bool, action: Option<String> }` allows nonsense -> `CheckResult { Pass, Fail { action } }`.
- C: linear_tracker.rs:91-124 `LinearIssue` and `TrackedIssue` structurally identical -> merge.
- D: spec/steward use `anyhow::bail!` for distinct validation failures; tests assert on prose -> `SpecValidationError` enum.

## P5 — Layering

- A: linear_tracker.rs:10 imports `parse_tasks`/`task_contract_fingerprint`/`TaskStatus` from `symphony_command` — there are now TWO `parse_tasks` in the tree -> consolidate task model into `task_parser`.
- B: giant prompt constants live inside orchestration files — P1 splits fix the big three; advisory for spec/steward/health/qa.
- C: doctor_command.rs defines its clap struct locally — minor.

## P6 — Cosmetic

`TrackedRepoState::new` is a `#[cfg(test)]` constructor in production code. Prompt step labels `99999`/`9999991` sort-last hack needs a comment. `qa_only_dirty_state_report` one-line forwarder — drop.

## Top refactor moves

1. Unify qa + qa-only behind `QaMode` enum + one prompt template — RISKY.
2. Split ship_command.rs -> `ship/{mod,prompt,gate,branch}` — SAFE.
3. Split design_command.rs -> `design/{mod,resolve,promotion,verify,prompt}` — SAFE.
4. Split loop_command.rs -> `loop_cmd/{mod,prompt,queue,refs,branch}` — SAFE.
5. Extract shared report-only infra -> `report_only.rs` — SAFE.
6. Hoist duplicated branch helpers into `crate::util` — SAFE.
7. Consolidate task model so linear_tracker depends on task_parser — RISKY.
8. Flatten `run_design_resolution` with `FinalPassOutcome` enum — RISKY.
9. Extract `finish_iteration`/`checkpoint_and_banner` in ship + loop — RISKY.
10. Merge `LinearIssue`/`TrackedIssue`; parse Cargo manifest once -> `AutodevManifest` — SAFE.
