# Thermo-Nuclear Findings — Report 3

Scope: `src/bug_command.rs` (3,951 lines), `src/audit_command.rs` (3,765 lines),
`src/nemesis.rs` (3,098 lines). Total reviewed: **10,814 lines**.

All three are LLM-driven audit/bug pipelines. All three are >3x past the 1k-line
rule. The headline structural defect is not just size — it is that the three
files re-implement the *same pipeline scaffolding* three times: process spawning
for Codex/PI/Kimi, stderr capture, timeout handling, JSON repair, schema
validation, fenced-block extraction, prompt ethos wrapping, output-dir
archiving. A reviewer cannot hold any one of these files in working memory, and
a bug fixed in one copy stays alive in the other two.

Findings are in priority order. `file:line` cites the live tree on
`thermo-nuclear-refactor`.

---

## Priority 1 — File sprawl (the 1k-line rule)

### [P1] bug_command.rs:1 — 3,951-line file holds 5 LLM phases + 2 JSON engines + chunker -> split into a `bug/` module tree.

`bug_command.rs` mixes seven unrelated concerns in one file: orchestration, the
five phase runners, repo chunking, a process-backend layer, a hand-rolled JSON
repair state machine, schema/prompt text, and report writing. Proposed tree
(`src/bug/`, `mod.rs` re-exports `run_bug`):

- `bug/mod.rs` — `run_bug` orchestration only: arg-to-`PhaseConfig` wiring,
  `apply_bug_profile`, `set_default_effort`, the per-chunk fix-on-verify loop,
  the final-review wiring, terminal banner printing. **~430 lines** (lines
  202–566).
- `bug/pipeline.rs` — the read-only chunk pipeline: `run_read_only_chunk_pipelines`,
  `run_read_only_chunk_pipeline`, the five `run_*_phase` /
  `load_or_run_*_phase` / `run_*_phase_once` functions, `clear_skeptic_phase_outputs`,
  the `try_resume_*` family, `derive_*` reducers. **~900 lines** (lines 568–1320,
  2447–2557).
- `bug/types.rs` — `RepoChunk`, `FileCandidate`, `BugFinding`, `BugIdRewrite`,
  `SkepticVerdict`, `AcceptedFinding`, `FixResult`, `ReviewResult`,
  `FinalReviewResult`, `ChunkOutcome`, `PhaseConfig`. **~110 lines** (lines
  41–147).
- `bug/chunker.rs` — `collect_repo_chunks`, `push_repo_chunk`,
  `build_file_candidate`, `risk_notes_for_file`, `top_level_scope`,
  `should_audit_path`, `slugify`, `write_chunk_manifest`, `write_bug_pre_index`.
  **~220 lines** (lines 1812–2027).
- `bug/prompts.rs` — `build_finder_prompt`, `build_skeptic_prompt`,
  `build_fix_prompt`, `build_final_review_prompt`, `build_review_prompt`,
  `build_bug_json_repair_prompt`, `render_prompt_files`, the six
  `*_json_schema()` constants. **~520 lines** (lines 2029–2311, 3126–3226).
- `bug/validate.rs` — `normalize_and_validate_finder_findings`,
  `normalize_finder_findings`, `validate_findings`, `finding_has_grounded_evidence`,
  `validate_accepted_findings`, `validate_skeptic_verdicts`, `validate_fix_results`,
  `validate_final_review_results`, `validate_review_results`,
  `validate_bug_id_coverage`. **~280 lines** (lines 2321–2636).
- `bug/report.rs` — `write_bug_summary`, `prepare_output_dir`,
  `prepare_bug_output_dir`. **~150 lines** (lines 3278–3485).
- Backend layer (`LlmBackend`, `select_backend`, `run_backend_prompt`,
  `run_backend_prompt_with_fallback`, `is_kimi_model`, `print_*_header`,
  `read_stream`, `append_stderr_log`, `configure_pi_env`,
  `prune_bug_phase_pi_state`) and the JSON-repair engine **must not land in a
  `bug/` submodule** — they are shared with `nemesis.rs` and belong in new
  top-level crates (see P3 below).

Seams are clean: `chunker` -> `pipeline` -> `report` only pass plain data
types; `validate` and `prompts` are pure given `types`.

### [P1] audit_command.rs:1 — 3,765-line file holds the audit run, finding verification, AND a full git-worktree resolution engine -> split into an `audit/` module tree.

`audit_command.rs` contains three near-independent programs behind one
`run_audit` entry: (a) the file-by-file audit run, (b) `--verify-findings`
reporting, (c) `--resolve-findings`, which is a multi-pass parallel git-clone /
lane / cherry-pick orchestrator. (c) alone is ~1,000 lines and shares nothing
with (a) except `ManifestEntry`. Proposed tree (`src/audit/`):

- `audit/mod.rs` — `run_audit` dispatch + the audit-run worker loop:
  `AuditWorkerContext`, `spawn_audit_worker`, `run_audit_worker`,
  `apply_verdict`, `apply_patch`, `commit_scoped`, `record_worklist_entry`,
  `append_retire_candidate`, `write_progress_snapshot`. **~700 lines** (lines
  248–624, 626–700, 2434–2726).
- `audit/manifest.rs` — `Manifest`, `ManifestEntry`, `EntryStatus`,
  `FileVerdict`, `initial_manifest`, `reconcile_manifest_with_tree`,
  `plan_audit_queue`, `write_manifest`, `mark_entry`. **~280 lines** (lines
  119–159, 702–813, 2334–2372).
- `audit/verify.rs` — `FindingVerificationReport/Entry/Result`,
  `verify_audit_findings`, `build_finding_verification_report`,
  `audit_entry_requires_closure`, `manifest_entry_requires_closure`,
  `clean_artifact_verdict*`, `write_finding_verification_report`. **~270 lines**
  (lines 161–190, 815–1011).
- `audit/resolve.rs` — the entire finding-resolution engine: the
  `FindingResolution*` types, `resolve_audit_findings`,
  `resolve_audit_findings_pass`, `build_finding_resolution_lanes`,
  `finding_architecture_key`, all `finding_resolution_*` path helpers,
  `clone_finding_resolution_lane_repo`, `commit/stage/land/fetch/cherry_pick`
  helpers, `prune_*`, `run_finding_resolution_lane`,
  `build_finding_resolution_prompt`, `rerun_only_drifted_audit`,
  `preflight_finding_resolution_roots`, `cargo_guard_wrapper_script`,
  `prepare_finding_resolution_lane_env`, `resolve_real_cargo`,
  `resolve_auto_executable`. **~1,200 lines** (lines 200–246, 1013–2230) — still
  over 1k; split it again: `audit/resolve/lanes.rs` (lane build + assignment +
  worktree paths), `audit/resolve/git.rs` (clone/stage/commit/fetch/cherry-pick/land),
  `audit/resolve/env.rs` (cargo guard wrapper + env + executable resolution),
  `audit/resolve/mod.rs` (the two pass drivers + status writer). Each ~300 lines.
- `audit/files.rs` — `enumerate_tracked_files`, `matches_any`, `glob_match`,
  `glob_match_recursive`, `build_file_prompt`, `file_artifact_dir`, `slugify`,
  `sha256_hex`, `now_iso8601`, `first_line`, `literal_git_pathspec`,
  `repo_relative_pathspec`. **~250 lines** (lines 2244–2429, 2596–2613).
- Auditor process layer (`run_auditor*`, `run_auditor_codex`,
  `run_auditor_kimi`, `read_stream`, `is_kimi_model`) -> shared backend crate
  (P3).

### [P1] nemesis.rs:1 — 3,098-line file holds the run, a duplicated JSON engine, and a markdown plan parser -> split into a `nemesis/` module tree.

Proposed tree (`src/nemesis/`):

- `nemesis/mod.rs` — `run_nemesis` orchestration, `apply_nemesis_profile`,
  `set_default_effort`, `resolve_auditor_model`, the `ensure_nemesis_*_config`
  validators, `validate_nemesis_backend_binaries`,
  `validate_nemesis_execution_contract`, `validate_backend_binary`,
  `ensure_executable_available`, `maybe_prepare_output_dir`, `prepare_output_dir`,
  `annotate_output_recovery`, `print_phase_header`, `nonempty_file`. **~620 lines**
  (lines 281–712, 752–896, 1024–1076).
- `nemesis/prompts.rs` — the three `DEFAULT_NEMESIS_*` prompt constants,
  `build_audit_prompt`, `build_review_prompt`, `build_implementation_prompt`,
  `build_finalizer_prompt`, `build_nemesis_results_repair_prompt`,
  `render_prompt_outputs`. **~360 lines** (lines 26–152, 898–1022, 1697–1740).
- `nemesis/outputs.rs` — `VerifiedNemesisOutputs`, `verify_nemesis_outputs`,
  `draft_nemesis_outputs_valid`, `verify_nemesis_implementation_results*`,
  `NemesisFixResult`, `load_nemesis_fix_results`, `validate_nemesis_fix_results`,
  `fixed_nemesis_result_is_truthful_noop`, `repair_nemesis_implementation_outputs`.
  **~330 lines** (lines 186–200, 1388–1544, 1562–1695).
- `nemesis/plan.rs` — the markdown plan parser/merger: `PlanSection`,
  `PlanTaskBlock`, `EMPTY_PLAN`, `REQUIRED_PLAN_SECTIONS`,
  `load_unchecked_nemesis_task_ids`, `unchecked_nemesis_task_ids`,
  `sync_nemesis_spec_to_root`, `next_nemesis_spec_destination`,
  `append_nemesis_plan_to_root`, `append_new_open_tasks`, `normalize_root_plan`,
  `markdown_has_line`, `append_blocks_to_section`, `extract_plan_task_blocks`,
  `finalize_plan_block`, `parse_section_header`, `parse_plan_task_header`.
  **~360 lines** (lines 157–178, 1546–1560, 1998–2342).
- `nemesis/commit.rs` — `commit_nemesis_outputs_if_needed`,
  `nemesis_commit_pathspecs`, `push_unique_pathspec`, `repo_relative_path`,
  `restore_nemesis_commit_index`. **~110 lines** (lines 2060–2166).
- Backend layer (`NemesisBackend`, `select_backend`, `run_nemesis_backend`,
  `run_codex`, `run_pi`, `run_kimi_cli`, `configure_pi_env`, `read_stream`,
  `is_kimi_model`, `EmptyFallback`) and the JSON repair engine -> shared crates
  (P3).

---

## Priority 2 — Spaghetti / control-flow rot

### [P2] bug_command.rs:235 — `run_bug` is 332 lines, well over the 100-line limit.

`run_bug` (lines 235–566) does arg parsing, five `PhaseConfig` constructions,
profile application, kimi preflight, a 50-line banner, two near-duplicate
checkpoint/remote-sync branches (377–392), the read-only pipeline call, the
per-chunk fix loop, aggregate file writes, the final-review wiring with a
three-way `code_phase_commit_before` branch, and the cleanup/print tail. Extract:
`configure_bug_phases(&args) -> BugPhases`, `print_bug_banner(...)`,
`checkpoint_and_sync(...)` (kills the 377–392 duplication), `run_fix_loop(...)`,
`finalize_bug_run(...)`.

### [P2] bug_command.rs:1436 — `run_backend_prompt` is 278 lines with three copy-pasted spawn bodies.

The `Codex` / `Pi` / `KimiCli` arms (1446–1712) repeat the identical sequence:
build `TokioCommand`, spawn with `with_context`, take stdout/stderr, spawn two
capture tasks, `time::timeout(...)`, kill-on-timeout, await tasks,
`append_stderr_log`, bail-on-timeout, bail-on-failure. Only the arg vector and
the error parser differ. Collapse to one `spawn_and_capture(cmd, timeout,
stderr_log, label) -> CapturedOutput` helper plus three small arg-builders. This
removes ~150 lines and is the same shape as `nemesis::run_codex`/`run_pi`/
`run_kimi_cli` (see P3).

### [P2] audit_command.rs:1095 — `resolve_audit_findings_pass` is 273 lines.

Lines 1095–1368: manifest load, verification report, unresolved-path filtering,
lane build, three directory creations, banner, prune, a `.map()` that clones
repos and is itself ~20 lines, lane-status construction, the join-set landing
loop with two inline `write_finding_resolution_status` calls per iteration, the
re-audit call, and a final `match verify_audit_findings`. The
`write_finding_resolution_status` call appears **five times** in this one
function with slightly different `phase` strings — wrap a
`set_phase(&mut ctx, phase)` closure. Extract `prepare_lane_assignments`,
`drive_resolution_lanes`, `finalize_resolution_pass`.

### [P2] audit_command.rs:248 — `run_audit` is 376 lines.

The worker-pool loop (470–599) has the "spawn next on completion" pattern
written out **four times** (once on success, three times in error arms at
486–498). Extract a `refill(&mut join_set, &mut plan_iter, &ctx, &mut active)`
helper; the three error arms collapse to one.

### [P2] nemesis.rs:310 — `run_nemesis` is 402 lines.

Lines 310–712. It interleaves config building, four backend selections,
validation, prompt building + prompt-file writes, a banner, the audit/review
resume branches, output verification, the implementer branch, the finalizer
branch with its own resume check, root sync, commit, and a print tail. The
implementer block (565–670) is itself a 105-line nested arrow. Extract
`build_nemesis_backends`, `write_nemesis_prompts`, `run_audit_review_phase`,
`run_implementer_phase`, `run_finalizer_phase`.

### [P2] bug_command.rs:2910 / nemesis.rs:1768 — `escape_unescaped_quotes_in_json_strings` is a 112-line char-by-char state machine.

Cyclomatic complexity far over 8: nested `match` on `ch` inside `match
string_role` inside `while`, plus a `primitive_value` flag mutated across
scopes. It is correct-ish but unreviewable and, being duplicated, doubly so.
Beyond deduplication (P3), it wants property-based tests
(`proptest`: "any serde-serialized value survives a round trip through the
repairer unchanged").

### [P2] audit_command.rs:2273 — `glob_match_recursive` has a dead branch.

Lines 2284–2286: inside the `**/` loop, `if path.get(i) == Some(&b'/') { //
continue scanning }` has an empty body and a comment — it does nothing. The loop
already scans every `i`. Delete the dead `if`.

---

## Priority 3 — Code-judo (aggressive simplification)

### [P3] bug_command.rs:2638-3124 ≡ nemesis.rs:202-226,1742-1996 — the JSON-repair engine is duplicated byte-for-byte.

Confirmed with `diff`: `escape_unescaped_quotes_in_json_strings` and its 111
surrounding lines are **identical** between the two files. The full duplicated
set: enums `JsonRepairContext`, `ObjectParseState`, `ArrayParseState`,
`JsonStringRole`; functions `escape_unescaped_quotes_in_json_strings`,
`current_string_role`, `finish_string_token`, `finish_json_value`,
`advance_json_context_after_comma`, `context_expects_value`,
`is_likely_string_terminator`, `next_significant_char`,
`is_valid_array_value_start`, `valid_json_string_escape_at`,
`extract_fenced_json_block`, `extract_complete_json_value_prefix`. That is
~300 lines living twice. **Fix: new `src/llm_json.rs`** exporting
`repair_llm_json(content) -> Option<String>` plus `extract_fenced_json_block`.
`bug_command`'s `normalize_bug_pipeline_json_shapes` /
`normalize_bug_pipeline_value` / `ensure_array_field` stay bug-specific and call
into the shared primitives. `JSON_REPAIR_MAX_BYTES` (defined identically in both,
bug_command.rs:35 and nemesis.rs:158) moves to `llm_json.rs`.

### [P3] bug_command.rs:1436 ≡ nemesis.rs:1078 — the backend layer is a third copy of process-spawn-and-capture.

`LlmBackend` (bug) and `NemesisBackend` (nemesis) are the same three-variant
enum with the same `label`/`model`/`effort|variant`/`is_kimi_family` impl.
`select_backend` is near-identical (bug adds one kimi-first branch). `run_codex`
/ `run_pi` / `run_kimi_cli` and bug's `run_backend_prompt` arms are the same
spawn/capture/timeout/error-parse logic. `is_kimi_model` is defined **three
times verbatim** (bug_command.rs:1387, audit_command.rs:2778, nemesis.rs:747).
`configure_pi_env` is defined identically in bug_command.rs:1803 and
nemesis.rs:1366. `read_stream` is defined three times (bug:1773, audit:2965,
nemesis:1375). `EmptyFallback`/`if_empty_then` is defined twice (bug:3487,
nemesis:2344) — with a subtle behavior fork (see Bugs below). **Fix: a shared
`src/audit_backend.rs`** owning the backend enum, `select_backend`,
`run_backend_prompt` with timeout + codex fallback, `is_kimi_model`,
`configure_pi_env`, `read_stream`, `EmptyFallback`. Note `codex_exec.rs` and
`codex_stream.rs` already exist as the intended shared backend home — extend
them rather than adding a parallel module if their abstraction fits. This is the
single highest-leverage deduplication: it removes ~400 lines and collapses three
divergent process layers into one.

### [P3] bug_command.rs:695-1243 — the five phase runners are a parallel structure begging to unify.

`run_finder_phase`, `run_skeptic_phase_once`, `run_review_phase`,
`run_fix_phase_at`, `run_final_review_phase` all do: derive 3–5 artifact paths,
`build_*_prompt`, `atomic_write` the prompt, `select_backend`,
`print_*_header`, `run_backend_prompt_with_fallback`, `prune_bug_phase_pi_state`,
`atomic_write` the response, `load_json_file_with_backend_repair`, validate.
And each is mirrored by a `load_or_run_*` resume wrapper and a `try_resume_*`
function — three parallel functions per phase, ~15 functions of boilerplate.
A `Phase` descriptor (name, artifact filenames, prompt builder fn, schema,
validator fn) plus one generic `run_phase<T>` / `load_or_run_phase<T>` would
collapse this to ~200 lines. Marked RISKY because the phases are not perfectly
uniform (skeptic has a retry loop, fix writes verified-findings first).

### [P3] bug_command.rs:2766 — `json_repair_candidate` is a one-line single-use wrapper.

`fn json_repair_candidate(content) { extract_fenced_json_block(content).unwrap_or_else(|| content.to_string()) }`
— inline it into its two callers (`load_json_file`, `repair_llm_json`).

### [P3] nemesis.rs:156 / nemesis.rs:781 — `DEFAULT_NEMESIS_AUDIT_MODEL` and `resolve_auditor_model` are near-dead.

`DEFAULT_NEMESIS_AUDIT_MODEL` is `#[allow(dead_code)]`-tagged and equals
`DEFAULT_CODEX_NEMESIS_MODEL` ("gpt-5.5"). `resolve_auditor_model` exists only
to honor `--minimax`/`--kimi` legacy flags when the model is still the default;
the comments call them "legacy opt-in." If those flags are being retired, this
function and the constant are dead weight. Flag for the operator: confirm
`--minimax`/`--kimi` are still supported; if not, delete `resolve_auditor_model`,
the constant, and the flags (per the repo's "replace, don't deprecate" rule).

### [P3] audit_command.rs:2728 — `run_auditor` is a test-only shim.

`run_auditor` is `#[allow(dead_code)]` and called only from one test. Either
move the thin wrapper into the test module or have the test call
`run_auditor_labeled(.., None)` directly.

### [P3] audit_command.rs:933 / 945 — `clean_artifact_verdict` only ever runs as a sub-step of `clean_artifact_verdict_for_current_source`.

`clean_artifact_verdict` has no other caller. It is small, but the two-function
split adds a re-`read`/re-`exists` of `verdict.json` (945 reads metadata, 933
re-reads contents). Merge into one function that reads the verdict once.

### [P3] bug_command.rs:31-39 — phase-timeout constants are silently divergent from audit/nemesis.

`bug` uses 30/90/90-minute phase timeouts; `audit` uses a 30-minute auditor
timeout and a 4-hour resolution timeout; `nemesis`'s `run_codex`/`run_pi` have
**no timeout at all** — `child.wait().await` can hang forever. Once the backend
layer is unified (P3 above), nemesis inherits the timeout and this inconsistency
is a bug fix, not just cleanup. See Bugs.

---

## Priority 4 — Types & boundaries

### [P4] bug_command.rs:58-131 & nemesis.rs:186 — verdict/status fields are stringly-typed.

`BugFinding.impact`, `SkepticVerdict.decision`, `FixResult.status`,
`ReviewResult.verdict`/`confidence`, `FinalReviewResult.status`,
`NemesisFixResult.status`, `FileVerdict.verdict` are all `String`, then
re-validated with `match s.trim().to_ascii_lowercase().as_str()` scattered
across `validate_*` and `derive_*`. The same `"accepted"|"disproved"` /
`"fixed"|"deferred"|..."` literal sets appear in both the validator and the
deriver (e.g. bug_command.rs:2465 and 2509 both match skeptic decision).
`EntryStatus` in `audit_command.rs:140` is already a proper `#[serde]` enum —
that is the model. Convert the others to enums with `#[serde(rename_all =
"snake_case")]`; the `match other => bail!` validation collapses into serde
deserialization, and the duplicated literal lists vanish.

### [P4] bug_command.rs:1392 — `display_phase_model` vs `PhaseConfig` — model resolution leaks.

`PhaseConfig { model, effort }` carries a raw model string; whether it is a Pi
alias is recomputed on demand via `PiProvider::detect` in `display_phase_model`,
`select_backend`, and `ensure_code_writer_config`. The resolved backend identity
should be computed once. Minor.

### [P4] audit_command.rs:150-159 — `FileVerdict` is `#[allow(dead_code)]`.

`touched_paths` and `escalate` are deserialized but never read (`apply_verdict`
ignores both; there is no `Escalated`-from-verdict path despite
`EntryStatus::Escalated` existing). Either wire `escalate` into the apply path
or drop the fields and the `#[allow(dead_code)]`. Phantom feature.

### [P4] Three pipelines, three `anyhow`-only error stacks.

Every failure is `anyhow::Error` with string context. For an
operator-facing tool that's defensible, but the resume logic repeatedly does
`match validate_*(...) { Ok => reuse, Err(err) => println!("warning: ignoring
invalid ...") }` — swallowing typed failure categories into printed strings. A
small `ResumeArtifact` enum (`Fresh`, `Reusable(T)`, `Stale(reason)`) would make
the resume contract explicit instead of `Option<T>` + side-effecting `println!`.

---

## Priority 5 — Canonical layers

### [P5] All three — CLI parse / orchestration / backend exec / process IO are tangled in one file each.

There is no enforced layering. `run_bug` (orchestration) directly constructs
`TokioCommand` arg vectors deep inside `run_backend_prompt` (process IO).
`audit_command` reaches into git plumbing (`Command::new("git")` raw) in
~12 places while `util::run_git`/`git_stdout` exist and are used elsewhere in
the *same file* — inconsistent layering within one module
(e.g. raw `Command` at lines 1659, 1816, 1893, 1942, 1966, 1991, 2531, 2544
vs `run_git` at 1839, 1841, 1862, 1869). The P1/P3 splits enforce the layering:
`mod.rs` = orchestration, `*/prompts.rs` + `*/validate.rs` = pure logic,
`audit_backend.rs` + `llm_json.rs` = backend/IO. Route every raw git call
through `util::run_git`/`git_stdout` or a new `audit/resolve/git.rs`.

### [P5] audit_command.rs:248 — `run_audit` dispatches to a different binary's worth of code by flag.

`run_audit` immediately branches: `args.everything` -> another module,
`verify_findings` -> `verify_audit_findings`, `resolve_findings` ->
`resolve_audit_findings`, else the audit run. These are four distinct
subcommands wearing one trench coat. Once `audit/` is a module tree, `run_audit`
should be a 15-line dispatcher in `audit/mod.rs` and nothing else.

---

## Outright bugs

### [BUG] nemesis.rs:1219 `run_codex` / nemesis.rs:1295 `run_pi` — no timeout; a hung model hangs the run forever.

`bug_command::run_backend_prompt` wraps `child.wait()` in `time::timeout(...)`
and kills on expiry. `audit_command`'s auditor does the same. `nemesis::run_codex`
and `run_pi` and `run_kimi_cli` call `child.wait().await` with **no timeout** —
a wedged Codex/PI/Kimi process blocks `auto nemesis` indefinitely with no
recovery. Fix: give the nemesis backend the same timeout treatment (naturally
falls out of unifying the backend layer, P3).

### [BUG] nemesis.rs:2344-2356 — `EmptyFallback::if_empty_then` has divergent semantics from the bug_command copy.

`nemesis.rs` impl: `if self.trim().is_empty()`. `bug_command.rs:3491` impl:
`if self.is_empty()` (no `trim`). Same trait name, same method name, different
behavior: a whitespace-only stderr string falls back in nemesis but not in bug.
Whichever is intended, the two copies disagree — unify on one (the `trim`
version is the safer choice for "empty stderr" detection) when the backend layer
is deduplicated.

### [BUG] audit_command.rs:583 — progress snapshot 25-boundary write is redundant work.

`audited` is incremented at line 571 then tested with `is_multiple_of(25)` at
583. Combined with the final unconditional `write_progress_snapshot` at 607,
the 25-boundary write is redundant on runs that end exactly on a multiple of 25.
Not a correctness bug; advisory.

### [BUG] bug_command.rs:464-475 / 491-515 — `code_phase_commit_before` skips the trailing push on resumed runs.

When `resumed_final_review_results` is `Some`, `code_phase_commit_before` is
`None`, so the trailing push/checkpoint block (491–515) is skipped entirely on a
resumed run — even though the per-chunk fix loop above (404–441) may have just
landed new commits this run. On resume, freshly-landed chunk fixes can go
un-pushed. Confirm intended; if not, compute `commit_before` before the fix loop
unconditionally.

---

## Priority 6 — Cosmetic

- bug_command.rs:1387, audit_command.rs:2778, nemesis.rs:747 — `is_kimi_model`
  defined three times with an identical heuristic; one home.
- audit_command.rs:2232 `slugify` and bug_command.rs:1975 `slugify` are two
  different implementations of the same idea (audit's keeps no trailing-dash
  state; bug's tracks `last_dash`). Pick one, share it.
- audit_command.rs:1725 `chrono_like_now` is a one-line alias for
  `crate::util::timestamp_slug()` — inline it.
- audit_command.rs:1443 `finding_resolution_target_root` takes a `run_id`
  parameter then immediately does `let _ = run_id;` and ignores it. Drop it.

---

## Top refactor moves (ranked, highest leverage first)

1. **Extract `src/llm_json.rs`** — lift the byte-identical JSON-repair engine
   (~300 lines) out of `bug_command.rs` and `nemesis.rs`; both call the shared
   API. **SAFE** (pure code movement; `diff` proves the blocks are identical;
   mechanically verifiable by re-running the existing repair tests in both
   files).
2. **Extract a shared `audit_backend` layer** — unify `LlmBackend`/`NemesisBackend`,
   `select_backend`, `run_backend_prompt`/`run_codex`/`run_pi`/`run_kimi_cli`,
   `is_kimi_model` (x3), `configure_pi_env` (x2), `read_stream` (x3),
   `EmptyFallback` (x2). Removes ~400 lines and three divergent process layers.
   **RISKY** (the three copies have drifted — no nemesis timeout, divergent
   `if_empty_then`; unifying changes nemesis runtime behavior, intentionally).
3. **Split `audit_command.rs` into `audit/` with `resolve/` as its own subtree**
   — the finding-resolution engine (~1,200 lines) shares nothing with the audit
   run but `ManifestEntry`. **SAFE** (code movement; the resolution path is
   reached only via `--resolve-findings` and is well-isolated).
4. **Split `bug_command.rs` into the 8-file `bug/` tree** (mod, pipeline, types,
   chunker, prompts, validate, report + the two shared extractions). **SAFE**
   (cohesive clusters with data-only seams).
5. **Split `nemesis.rs` into the `nemesis/` tree** (mod, prompts, outputs, plan,
   commit + shared extractions). **SAFE** (`plan.rs` markdown parser is
   completely self-contained; `prompts.rs` is pure string formatting).
6. **Decompose `run_bug`, `run_audit`, `run_nemesis`, `resolve_audit_findings_pass`**
   (all 270–402 lines) into named sub-steps and kill the repeated
   spawn-refill / checkpoint-sync / `write_finding_resolution_status` snippets.
   **SAFE** (each extraction is a contiguous block lift; behavior preserved).
7. **Collapse `run_backend_prompt`'s three copy-pasted spawn arms** into one
   `spawn_and_capture` helper. **SAFE** (the three arms are structurally
   identical; only arg vectors differ).
8. **Add a nemesis backend timeout** — close the no-timeout hang bug. **RISKY**
   (changes behavior: a previously-hanging run now fails fast — the desired
   outcome, but it is a behavior change).
9. **Convert stringly-typed verdict/status fields to `#[serde]` enums**
   (`impact`, `decision`, `status`, `verdict`, `confidence`). Deletes the
   duplicated `match ... bail!` validation. **RISKY** (changes deserialization;
   malformed-but-previously-tolerated values now fail at parse time).
10. **Unify the five bug phase runners** behind a `Phase` descriptor + generic
    `run_phase<T>`/`load_or_run_phase<T>`. **RISKY** (phases are not perfectly
    uniform — skeptic retry loop, fix's pre-write — so the abstraction must be
    designed carefully, not mechanically lifted).
