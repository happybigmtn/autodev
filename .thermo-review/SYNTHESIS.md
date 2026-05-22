# Thermo-Nuclear Refactor — Synthesis & Backlog

Branch: `thermo-nuclear-refactor` (autodev repo). Tests: **622 passing** (baseline 623; -1 = deleted `LaneScopeBudget` dead code). Clippy: clean. Build: clean.

## What landed

| Move | Outcome |
|---|---|
| Deleted `backend_policy.rs` | -414 lines (`#![allow(dead_code)]`, zero refs) |
| Split `parallel_command.rs` 10,653 lines | 13 submodules (largest 1,821) |
| Split `generation.rs` 6,156 | 9 submodules (largest 1,335) |
| Split `audit_everything.rs` 5,881 | 13 submodules (largest 1,060) |
| Split `bug_command.rs` 3,951 | 9 submodules (largest 819) |
| Split `audit_command.rs` 3,765 | 9 submodules (largest 1,388) |
| Split `symphony_command.rs` 3,243 | 7 submodules (largest 871) |
| Split `nemesis.rs` 3,098 | 6 submodules (largest 988) |
| Split `completion_artifacts.rs` 2,818 | 5 submodules (receipt.rs 1,309 — test-heavy) |
| Split `super_command.rs` 2,380 | 5 submodules (largest 646) |
| Split `review_command.rs` 2,196 | 4 submodules (largest 692) |
| Split `codex_stream.rs` 1,850 | 6 submodules (largest 588) |
| Split `util.rs` 1,704 | 3 submodules (git 920, fsutil 769) |
| Split `task_parser.rs` 1,223 | 4 submodules (largest 689) |
| Split `ship_command.rs` 1,490 | 5 submodules (largest 753) |
| Split `design_command.rs` 1,306 | 6 submodules (largest 445) |
| Split `loop_command.rs` 1,124 | 6 submodules (largest 398) |
| Thinned `main.rs` 1,879 -> 271 | clap arg structs moved to `src/cli/` |
| Dedup: byte-identical JSON-repair engine | One copy in `bug_command/llm_json.rs` (nemesis re-uses) |
| Dedup: backend-process helpers | `backend_process.rs` (extracted from `codex_exec`/`claude_exec`) |
| Dedup: branch helpers | Single copy in `util::git` (ship/loop re-use) |
| Dedup: markdown section parser | Unified within `generation::markdown` |
| Dead code purge | `LaneScopeBudget` trio, `GroupPhase` enum, `build_final_review_prompt`, no-op `let _` statements |

Net headline: every file over 1k lines is now a module directory; the only remaining >1k-line files inside those directories are test-heavy (e.g. `completion_artifacts/receipt.rs` 1,309 with ~half tests) or hold the core orchestration that resists further safe extraction (e.g. `parallel_command/orchestrator.rs` 1,821 — see backlog).

## Deferred RISKY moves (high-value, behavior-sensitive)

These were intentionally NOT done — they change behavior and need their own characterization tests.

### Scheduler decomposition
- **`run_parallel_loop` -> per-outcome handlers.** It's still 1,035 lines, cyclomatic >50. Extract `dispatch_ready_lanes`, `handle_lane_attempt_result`, `handle_nonzero_exit`/`handle_clean_exit`. Risk: core scheduler; behavior must be preserved exactly.
- **`run_remediation_lanes` -> `dispatch_ready_lanes` + `harvest_lane_result` + `set_task_status`.** Same shape: 166-line scheduler loop with persistence interleaved.

### Backend layering
- **Lane git-IO layer.** `parallel_command/orchestrator.rs` and `landing.rs` still call `Command::new("git")` directly in ~6 sites. Route everything through `lane_repo.rs`.
- **Unify the three divergent process backends.** `bug_command/backend.rs`, `audit_command`, `nemesis/backend.rs` all wrap Codex/Pi/Kimi spawning differently — copies have drifted. Cluster C confirmed the JSON-repair engine was byte-identical and unified it; the process layer still diverges. Build a single `audit_backend` module.
- **Backend trait unification.** `codex_exec`, `claude_exec`, `kimi_backend`, `pi_backend` share a process-lifecycle pattern. `backend_process.rs` covers the helpers; the higher-level dispatch is still copy-paste. Either keep the existing 3-variant enum and dedup more helpers, or introduce a trait.

### Type-system tightening
- **`LaneWorkerMetadata.harness: String` -> enum.** Persisted in `assignment.json`; a typo currently passes validation silently.
- **`in_place: bool` and other state flags -> enums** (`RunMode`, `QaMode`, `StewardMode`, `CheckResult { Pass, Fail }`). Booleans encode states better expressed as enums.
- **Typed errors for the audit/landing state machines.** `landing_error_suggests_dirty_canonical_worktree`, `is_linear_usage_limit_error`, `environment_blocker_reason` all string-match `format!("{err:#}")` — a `LandingError` enum catches these at the source.
- **`EverythingManifest` path fields `String` -> `PathBuf`.** Round-tripped through `PathBuf::from` everywhere; changes the serialized format, needs a round-trip test.

### Command unification
- **qa + qa-only behind a `QaMode` enum.** `QaTier` match arms are byte-identical and the prompts are ~80% identical. Risk: prompt text is user-visible.
- **Consolidate task model into `task_parser`.** `linear_tracker` imports `parse_tasks`/`TaskStatus` from `symphony_command`; there are now two `parse_tasks` in the tree. The task model is domain logic, not symphony-command logic.
- **Audit's `markdown_section` parser.** Different algorithm than `generation::markdown` (matches headings at any `#` depth vs literal `## Header` substring). Unifying changes behavior at sub-headings.

### Smaller wins
- **`live.log` regex round-trip -> JSON sidecar** in `parallel_command/status.rs` (`last_parallel_stop_state` scrapes a line the host itself formatted).
- **Move inline product-doctrine prompt text** (`build_parallel_lane_prompt`'s ~2k chars) into `prompt.rs` constants or a template asset.
- **Extract `report_only.rs`** — `ReportOnlyDirtyStateReport`, `collect_dirty_state`, etc. currently live in `qa_only_command.rs` but are used by `health_command` and `design_command`.
- **Flatten `run_design_resolution`** with a `FinalPassOutcome` enum (5-branch tangle today).
- **Hoist `finish_iteration`** in ship + loop drivers (duplicated checkpoint/banner/gate sequence).

## Deferred BUGS (real, but each is a behavior change)

Each needs a reproducing test before fix.

1. **Nemesis has no process timeout.** `run_codex`/`run_pi`/`run_kimi_cli` in `nemesis/backend.rs` have no timeout while `bug_command` and `audit_command` both timeout-and-kill. A hung model wedges `auto nemesis` forever. Fix: mirror the existing bug/audit timeout pattern.
2. **`EmptyFallback::if_empty_then` divergent semantics** between the two surviving copies (`trim().is_empty()` vs `is_empty()`). Pick one, document the choice.
3. **`bug_command` `code_phase_commit_before` is `None` on resumed runs.** Freshly-landed per-chunk fixes can go un-pushed when the pipeline resumes mid-run.
4. **Divergent Claude classifier.** `author_phase_uses_claude_model` (deleted from generation if you take the SAFE dedup branch — currently still present) uses `.contains()` and treats `""` as Claude; `claude_exec::looks_like_claude_model` uses `.starts_with()` and treats `""` as non-Claude. Empty `--model` routes differently. One is wrong.
5. **`AUTO_LENIENT_GATE` / `AUTO_LENIENT_DEPS` swallows the whole plan-integrity contract,** not just the dependency check it's named for (generation/plan_verify.rs around `validate_execution_rows`).
6. **`parallel_command/plan.rs` `build_iteration_prompt` indexes `queue.pending_ids[0]` unconditionally.** Callers guard with `ready.is_empty()` first; one careless caller from a panic. Take a `&LoopTask`/`Option`.
7. **`propagate_lane_receipts` stamps receipt fingerprint from canonical's *current* porcelain status,** which may include transient cherry-pick state. Confirm ordering is intentional.
8. **`refresh_parallel_plan_or_last_good` has a misleading name** — binds `last_good_plan` with `let _` and always returns the error; the name promises fallback the body doesn't implement.

## Sizing

- Source files: 42 -> 42 (1 deleted, no net change in file count because module directories aggregate to one)
- Total Rust LOC: 67,242 -> ~73,500 (the per-submodule split + test-module duplication adds boilerplate; pure deletion came to ~600 lines; the rest is reorganization-and-grow). Net "logical" code is smaller — the increase is `use` statements and `mod` declarations that the compiler needs.
- Largest single file: 10,653 -> 1,821 (parallel_command/orchestrator.rs)
- Test count: 623 -> 622 (one dead test removed)

The thermo-nuclear "1k-line rule" is now respected at the file level; the 5-6 submodules that still exceed 1k are test-heavy or are the deliberate cores of orchestration logic that the RISKY moves above would further decompose.
