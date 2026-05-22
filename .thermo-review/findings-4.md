# Thermo-Nuclear Findings — Batch 4 (orchestration commands)

Files audited (all four breach the 1k-line rule):

| File | Lines |
|---|---|
| `src/symphony_command.rs` | 3,243 |
| `src/completion_artifacts.rs` | 2,818 |
| `src/super_command.rs` | 2,380 |
| `src/review_command.rs` | 2,196 |
| **Total** | **10,637** |

Tone note: these are not bad files. The data modeling is mostly clean, error
messages are good, and the test coverage is dense. The defect is structural —
four monoliths, each carrying 3-5 unrelated responsibilities, plus duplicated
orchestration scaffolding that `super_command` should own once.

---

## Priority 1 — File sprawl

### P1 `src/symphony_command.rs:1-3243` — one file holds a GraphQL client, a Codex planner, a YAML workflow renderer, markdown parsing, and reconciliation logic

Five cohesive clusters that barely touch each other. Cross-cluster coupling is
low (clusters communicate only through `SymphonyTask` / `LinearIssue`).

Proposed module tree (`symphony/`):

```
symphony/
  mod.rs            run_symphony dispatch + re-exports                    (~60)
  linear/
    queries.rs      the 9 GraphQL const strings (lines 36-280)            (~250)
    client.rs       LinearGraphqlClient + parse_project/issue/state/      (~430)
                    blocker + required/optional_string + normalize_name
    model.rs        LinearIssue/Blocker/State/Team/Project structs        (~120)
  task.rs           TaskStatus, SymphonyTask, parse_tasks, parse_task_    (~260)
                    header, task_field_body/_line_value/_excerpt,
                    strip_list_bullet, single_line_excerpt, render_*digest/brief
  sync.rs           run_sync, DeterminedSyncPlan, reconcile_completed/    (~620)
                    completion, completed_plan_issue_updates,
                    mark_tasks_done_in_plan, backfill_review_entries,
                    issue_task_id*, fallback_task_priorities,
                    validate_schedule_dag, normalize_planner_response
  planner.rs        determine_sync_plan, build_sync_planner_prompt,       (~330)
                    run_codex_planner, planner_command, PlannerResponse,
                    extract_agent_message/planner_json, read_stream
  workflow.rs       render_workflow, run_foreground, WorkflowRenderSpec,  (~620)
                    render_workflow_markdown, resolve_* helpers,
                    validate_* / shell_quote / yaml_double_quote
```

Each piece lands well under 1k lines. Narrowest seam to cut first:
`linear/queries.rs` — 245 lines of pure `const` strings with zero logic.

### P1 `src/completion_artifacts.rs:1-2818` — verification-receipt evaluation, audit-manifest matching, artifact hashing, and footer codec stacked in one file

Production code ~1,650 lines, tests ~1,160. The production half still exceeds
1k and contains four independent subsystems.

Proposed module tree (`completion/`):

```
completion/
  mod.rs            TaskCompletionEvidence, CompletionGap*, public         (~330)
                    inspect_task_completion_evidence,
                    assess_task_completion_gap, ensure_host_review_handoff,
                    review_contains_task, default_review_doc, render_host_*
  verification.rs   VerificationPlan, verification_plan,                   (~260)
                    executable_commands_from_verification_step,
                    backtick_fragments, looks_like_executable_command,
                    is_env_assignment, truncate_verification_narrative,
                    verification_step_looks_external, strip_list_bullet
  receipt/
    model.rs        VerificationReceipt + 5 nested structs + Source enum   (~110)
    inspect.rs      inspect_verification_receipt,                          (~420)
                    verification_receipt_content_problem,
                    *_command_matches/_passed/_superseded/_zero_tests
    freshness.rs    verification_receipt_freshness_problem*,               (~280)
                    current_git_commit/_dirty_state/_plan_hash,
                    git_commit_is_ancestor, command_stdout,
                    shared_receipt_freshness_problem
    footer.rs       footer codec: *_commit_footer,                         (~200)
                    legacy_*_backfill_footer, git_verification_receipt_footers,
                    parse_verification_receipt_footer,
                    compact_receipt_json_for_footer, prune_receipt_*
  artifacts.rs      declared_completion_artifacts, declared_artifact_path,  (~250)
                    *_relative_path_is_safe, current_declared_artifact_hashes,
                    artifact_hash, collect_artifact_dir_entries,
                    sha256_hex, same_path, *_mutable_handoff
  audit.rs          AuditManifest*, unresolved_owned_audit_findings,       (~180)
                    audit_owned_path_patterns, audit_owned_pattern_matches,
                    wildcard_match, *_token_looks_path_like,
                    summarize_unresolved_audit_findings
```

`wildcard_match` is a generic glob matcher with no completion context — it
could even drop to `crate::util`.

### P1 `src/super_command.rs:1-2380` — the orchestrator also embeds the audit-harvest pipeline and the deterministic gate parser

Production code ~2,000 lines. The audit-harvest subsystem (lines ~985-1435,
~450 lines) and the deterministic-gate plan parser (lines ~1549-1908, ~360
lines) are independent of the stage-runner core.

Proposed module tree (`super_cmd/`):

```
super_cmd/
  mod.rs            run_super stage loop, SuperArgs entry                  (~330)
  manifest.rs       SuperManifest, SuperStage, SuperRepoRecord,            (~330)
                    prepare_super_run, hydrate_super_args_from_manifest,
                    load/write_manifest, super_stage_terminal*,
                    push_stage/push_skipped, append_status_log,
                    write_super_cross_repo_manifest, repo_record
  stages.rs         run_super_corpus_review, run_super_execution_gate,     (~430)
                    validate_super_execution_gate_report, run_super_codex_phase,
                    build_super_*_prompt, build_super_focus,
                    build_super_generation_args, require_nonempty_file,
                    write_super_branch_*/final_*
  audit_harvest.rs  run_super_audit_phase, run_super_audit_harvest,        (~480)
                    run_audit_harvest_standalone, harvest_audit_findings,
                    resolve_latest_audit_run_id, chunk_findings_for_codex,
                    compress_finding_for_harvest, build_audit_harvest_prompt,
                    collect_paths_from_audit_rows,
                    audit_generated_plan_against_operator_bans
  gate.rs           DeterministicGateSummary, SuperParallelDecision,       (~430)
                    verify_parallel_ready_plan, verify_super_snapshot_ready_plan,
                    verify_super_task, SuperPlanSection, SuperTaskBlock,
                    extract_super_task_blocks, parse_super_task_header,
                    first_super_task_field_line, verification_looks_broad_or_malformed
```

`audit_harvest.rs` is fully self-contained (needs only `run_super_codex_phase`,
re-exported from `stages.rs`); cut it first.

### P1 `src/review_command.rs:1-2196` — REVIEW.md queue parsing, plan-harvest, stale-batch triage, and the iteration runner share one file

Production code ~1,500 lines. Three cohesive clusters plus the runner.

Proposed module tree (`review/`):

```
review/
  mod.rs            run_review iteration loop, DEFAULT_REVIEW_PROMPT,      (~430)
                    DIRECT_REVIEW_QUEUE_REVIEW_CLAUSE
  queue.rs          extract_review_items, is_bullet_review_item_start,     (~330)
                    looks_like_review_identity, is_review_bullet_item,
                    write_queue, ensure_review_doc(s), has_reviewable_items,
                    select_review_batch*, item_identity, batch_identity_set
  harvest.rs        harvest_completed_plan_items_for_review,               (~340)
                    extract_completed_plan_items, CompletedPlanItem,
                    is_top_level_plan_task_header, completed_plan_task_id,
                    render_completed_plan_review_item,
                    handoff_completed_items_to_review_queue,
                    append_review_items_preserving_doc,
                    collect_historical_review_docs, PlanReviewHarvestResult
  triage.rs         StaleTriageResult, mechanically_triage_stale_review_   (~140)
                    items, stale_followup_path, append_stale_review_followups,
                    ensure_trailing_blank_line
  annotate.rs       extract_cited_paths, build_live_tree_annotation,       (~180)
                    format_batch_block, append_reference_repo_clause
  progress.rs       IterationSnapshot, format_iteration_summary,           (~260)
                    TrackedRepoState, RepoProgress, collect_tracked_repo_states,
                    summarize_repo_progress, path_size, short_sha, signed_delta,
                    resolve_reference_repos, discover_sibling_git_repos
```

---

## Priority 2 — Spaghetti / control-flow rot

### P2 `src/symphony_command.rs:422-638` — `run_sync` is a ~215-line function
Does config resolution, fetch, two reconciliation passes, planner dispatch, an
issue create/update loop, a relation diff loop, and seven conditional summary
`println!` blocks. Cyclomatic complexity well past 8. Extract `sync_issues`
(create/update loop, 483-536), `sync_blocker_relations` (relation loop,
538-595), `print_sync_summary` (597-636). `run_sync` then reads as named steps.

### P2 `src/symphony_command.rs:1930-2141` — `render_workflow_markdown` is a ~210-line function dominated by one `format!`
`before_run_hook` (1949-2003) is a 50-line vector of shell fragments; the
trailing `format!` is a ~105-line embedded template with 14 substitutions. Move
the template to a `const` with `{placeholder}` tokens; lift `before_run_hook`
into `fn render_before_run_hook(...) -> String`.

### P2 `src/completion_artifacts.rs:795-1030` vs `:1038-1148` — `inspect_verification_receipt` (~235 lines) and `verification_receipt_content_problem` (~110 lines) are near-identical
Both compute `missing`/`failed`/`zero_test`/`unsuperseded_failed` over
`receipt.commands` in the same order. Extract one
`receipt_command_problems(receipt, expected) -> Option<String>`; both callers
use it. (Also a P3 — see below.)

### P2 `src/super_command.rs:106-417` — `run_super` is a ~310-line function
Nine stages, each an `if super_stage_terminal { println resume-skip } else {
run; push_stage }` block, repeated verbatim ~9×, plus a `with_audit` sub-branch
with two nested stages and three early returns. Introduce a `run_stage` helper
(`name`, async body returning `Option<PathBuf>`) so each stage is one call.

### P2 `src/review_command.rs:230-437` — the `while` loop body in `run_review` is ~210 lines
Mixes batch selection, stale-batch detection + triage, prompt assembly, harness
dispatch, snapshot diffing, repo-progress classification, commit/push. Extract
`handle_stale_batch(...) -> ControlFlow` (254-295) and
`commit_iteration_progress(...)` (400-436).

### P2 `src/super_command.rs:1934-1988` — `audit_generated_plan_against_operator_bans` nests `filter`/`flat_map`/`filter` three deep
Pull the token test into a named
`fn extract_banned_path_tokens(line) -> impl Iterator<Item=&str>`.

---

## Priority 3 — Code-judo (aggressive simplification)

### P3 `src/completion_artifacts.rs:913-1027` ≈ `:1043-1147` — delete ~100 lines of duplicated receipt-problem logic
The `missing`/`failed`/`zero_test`/`unsuperseded_failed` computation exists
twice, byte-for-byte modulo return shape (`(bool, Option<String>)` vs
`Option<String>`). Collapse to one function so a fix to one path can no longer
be silently missed in the other.

### P3 `src/super_command.rs:949-983` ≈ `:1048-1083` — `run_super_audit_phase` duplicates `resolve_latest_audit_run_id`
Both contain the identical `latest-run` symlink read + `read_dir` mtime-max
fallback (~35 lines each). `run_super_audit_phase` should call
`resolve_latest_audit_run_id(&audit_root)`; only the error message differs.

### P3 `src/symphony_command.rs:1189-1220` & `:1269-1320` — `render_sync_task_digest` and `render_issue_task_brief` duplicate per-field extraction
Both pull Why-now/Owns/Touchpoints/Scope-boundary through
`single_line_excerpt(task_field_*(...))` with the same limits. A shared
`task_field_summary(task, field, next, limit)` removes the boilerplate.

### P3 `src/super_command.rs` — `#[allow(dead_code)]` items
`field_value_is_none` (1734-1738), `contains_path_like_token` (1788-1811),
`task_field_value` (1894-1900) are genuinely dead — delete them.
`cargo_test_line_is_package_wide` (1749) IS called by
`verification_looks_broad_or_malformed:1744` — the `#[allow]` is wrong; remove
only the attribute, keep the function. `verify_super_task_process_fields`
(1685-1732) is a 47-line validator no production path calls — see Bugs below.

### P3 `src/super_command.rs:1278-1284` — two doc comments are braided across a function definition
Lines 1278-1280 ("Compress an analysis.json...") belong to
`compress_finding_for_harvest` at 1322; lines 1281-1284 belong to
`collect_paths_from_audit_rows` at 1285. Move each `///` block above its own
`fn`. Misleads any reader.

### P3 `src/super_command.rs:1119` & `:1203` — `plan_path` bound twice in `harvest_audit_findings`
`let plan_path = repo_root.join(IMPLEMENTATION_PLAN);` at 1119 and again at
1203 (shadowing, same value). Delete line 1203 — the 1119 binding is still in
scope.

### P3 `src/symphony_command.rs:1265-1267` — `task_field_excerpt` is a thin one-liner
`single_line_excerpt(task_field_body(...))`, used 3× only inside
`render_issue_task_brief`. Keep it (rule of three) but move it into the `task.rs`
submodule with its helpers, not floating among workflow code.

---

## Priority 4 — Types & boundaries

### P4 `src/super_command.rs:42-87` — `SuperStage.status` is stringly-typed
Compared against literals `"complete"`/`"skipped"`/`"launched"` in
`super_stage_terminal` (588-598). Model as an enum
(`Complete|Skipped|Launched|Failed`) with `serde` rename so the compiler
enforces an exhaustive match — exactly the bug class the file's own comment
(586-592) describes ("`launched` was previously treated as terminal").
`generation_mode`/`root_plan_status` are likewise `String`s pinned to consts.

### P4 `src/completion_artifacts.rs:33-38` — `CompletionGapKind::None` where `Option` belongs
`assess_task_completion_gap` returns `kind == None` when `missing_reasons` is
empty; callers must keep two fields in agreement. Either make the whole
assessment an `Option<CompletionGapAssessment>` or document `None` as the single
source of truth. Advisory.

### P4 `src/symphony_command.rs:31` — `RELATION_BLOCKS` is a stringly-typed relation kind
Fine for one relation type; becomes an enum if more appear. Advisory.

### P4 `src/review_command.rs:1129-1139` — `repo_forbids_legacy_review_trackers` is a fragile content-substring policy probe
Decides a major behavioral branch (direct-review-queue mode) by checking that
`AGENTS.md`/`WORKFLOW.md` contains five literal substrings simultaneously. Any
rewording silently flips the repo back to legacy three-file mode. A config
decision encoded as prose grep. Advisory — deserves an explicit marker file or
structured config key.

---

## Priority 5 — Canonical layers

### P5 `src/symphony_command.rs` — CLI orchestration, HTTP/GraphQL backend, and process spawning tangled in one layer
`run_sync` directly owns a `reqwest::Client`, directly spawns Codex via
`TokioCommand` in `run_codex_planner`, and renders YAML. No backend-exec
boundary. The P1 split fixes this: `linear/client.rs` is the IO layer,
`planner.rs` the process layer, `sync.rs` stays orchestration.

### P5 `src/super_command.rs` — `super` mixes in-process and out-of-process calls
`generation::run_gen`, `parallel_command::run_parallel`,
`design_command::run_super_design_module` are in-process; `run_super_audit_phase`
(895-945) shells out to `current_exe()`. The audit subprocess is deliberate
(doc comment 886-889 explains the clap-default rationale) — accept it but name
the inconsistency in module docs so it isn't "fixed" into an in-process call.

### P5 — duplicated orchestration scaffolding across all four files (cross-cutting)
`super_command` coordinates the others, yet each re-implements scaffolding:
- **Codex-phase invocation**: `symphony::run_codex_planner` (868-933),
  `super::run_super_codex_phase` (847-884), and `review`'s inline
  `run_codex_exec_max_context` call (359-369) each build a prompt path, write
  it, print a `prompt log:` line, spawn, check `status.success()` — three
  divergent copies.
- **Latest-run-id resolution**: duplicated within `super_command` (P3).
- **`git -C` wrappers**: `super::git_text` (761-766) and
  `completion_artifacts::command_stdout` (1389-1400) both wrap
  `crate::util::git_stdout`; `review` uses it directly. Pick one.
- **Resume/stage bookkeeping**: the `if terminal { resume-skip } else { run }`
  idiom is copy-pasted ~9× (P2).

Recommendation: a `crate::orchestration` module owning `CodexPhase`
(prompt-write + spawn + status-check + log lines) and the stage-runner helper.
`super_command` is the natural owner. Highest-leverage structural change in the
batch — removes duplication and establishes the missing backend-exec layer.

---

## Priority 6 — Cosmetic

- `src/symphony_command.rs:1222-1230` & `completion_artifacts.rs:1644-1652` —
  `strip_list_bullet` defined identically in both files; once shared modules
  exist, define once.
- `src/completion_artifacts.rs:271-284` — `AuditManifest`/`AuditManifestFile`
  declared mid-file between unrelated functions; move into `audit.rs`.

---

## Outright bugs / correctness risks

No confirmed runtime bugs. Two items to verify:

1. `src/super_command.rs:1568` `verify_parallel_ready_plan` — production calls
   it only via `verify_super_snapshot_ready_plan`; the bare function is
   otherwise test-only. Not dead (has a live caller) but the naming implies a
   root-plan path that no longer exists. Confirm intent during `gate.rs`
   extraction.
2. `src/super_command.rs:1685` `verify_super_task_process_fields` carries
   `#[allow(dead_code)]` and validates `PLAN_TASK_PROCESS_FIELDS`,
   UI-consumer/cross-surface coupling, and `Review/closeout:` quality — but is
   **not called by `verify_super_task`**. If it was meant to run as part of the
   deterministic gate, the gate is currently weaker than it looks: a real
   behavioral gap, not just dead code. Decide: wire it in (RISKY — strengthens
   the gate, may reject plans that pass today) or delete it (SAFE).

---

## Top refactor moves (ranked, highest leverage first)

1. **Extract a shared `crate::orchestration` module** (CodexPhase invocation +
   stage-runner helper); route `symphony`/`super`/`review` through it. Removes
   the three-way duplicated Codex-phase dance and the ~9× stage idiom, and
   establishes the missing backend-exec layer. — **RISKY** (changes the call
   path of every Codex invocation; behavior must be proven identical).
2. **Split `symphony_command.rs` into `symphony/`** (P1). Start with
   `linear/queries.rs` (245 lines of pure consts). — **SAFE**.
3. **Split `completion_artifacts.rs` into `completion/`** (P1), cutting
   `receipt/footer.rs` and `audit.rs` first. — **SAFE**.
4. **Deduplicate receipt-problem logic** — collapse
   `inspect_verification_receipt:913-1027` and all of
   `verification_receipt_content_problem` into one `receipt_command_problems`.
   Deletes ~100 lines; stops the two paths drifting. — **RISKY** (the copies
   return different shapes; merge must preserve each caller's early-return
   ordering).
5. **Split `super_command.rs` into `super_cmd/`** (P1), extracting
   `audit_harvest.rs` (self-contained) first, then `gate.rs`. — **SAFE**.
6. **Split `review_command.rs` into `review/`** (P1). — **SAFE**.
7. **Extract `run_stage` helper** in `super_command`; collapse `run_super`'s ~9
   hand-copied resume-skip branches. — **RISKY** (touches resume semantics; the
   `resume_helpers_skip_terminal_stages...` test must still pass).
8. **Decompose `run_sync`** into `sync_issues`/`sync_blocker_relations`/
   `print_sync_summary` (P2). — **SAFE** (mechanical extraction of contiguous
   blocks).
9. **Move `render_workflow_markdown`'s template to a `const`** and lift
   `before_run_hook` into its own function (P2). — **RISKY** (30+ workflow
   assertion tests pin the exact output; safe only if byte-identical).
10. **Delete the dead `#[allow(dead_code)]` items** in `super_command`
    (`field_value_is_none`, `contains_path_like_token`, `task_field_value`);
    fix the misplaced `#[allow]` on `cargo_test_line_is_package_wide`; decide
    `verify_super_task_process_fields`'s fate. — **SAFE** for the deletions;
    **RISKY** if `verify_super_task_process_fields` is wired in.
