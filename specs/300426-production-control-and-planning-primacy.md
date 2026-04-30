# Specification: Production Control And Planning Primacy

## Objective

Make the production-readiness campaign safe to promote by keeping active queue truth, generated corpus truth, release gates, and operator status output in one explicit hierarchy.

The current generated snapshot is planning input. It must not become executable truth until an operator promotes accepted slices into the root ledgers.

## Source Of Truth

- Runtime owners: `src/main.rs`, `src/generation.rs`, `src/super_command.rs`, `src/parallel_command.rs`, `src/loop_command.rs`, `src/review_command.rs`, `src/steward_command.rs`, `src/state.rs`.
- Queue and planning owners: root `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `WORKLIST.md`; planning corpus under `genesis/`; generated snapshots under `gen-*`.
- UI consumers: terminal help, `auto doctor`, `auto gen`, `auto super`, `auto parallel status`, `auto review`, `auto steward`, `auto ship`, README lifecycle prose, generated spec and plan snapshots.
- Generated artifacts: `gen-*/specs/*.md`, `gen-*/IMPLEMENTATION_PLAN.md`, `gen-*/corpus/**`, `.auto/state.json`, `.auto/super/*/manifest.json`, `.auto/logs/*`.
- Retired/superseded surfaces: stale generated snapshots under older `gen-*` directories, root specs whose topic is replaced by a promoted same-day spec, and any root queue row that presents `genesis/` as active execution truth before promotion.

## Evidence Status

Verified facts grounded in code or primary repo files:

- The compiled CLI is named `auto` and the Cargo package is `autodev` version `0.2.0`, verified by `Cargo.toml` and `rg -n "name = \"autodev\"|version = \"0.2.0\"|name = \"auto\"" Cargo.toml`.
- `src/main.rs` owns the public command enum for corpus, gen, spec, design, super, reverse, bug, loop, parallel, qa, qa-only, health, book, doctor, review, steward, audit, ship, nemesis, quota, and symphony, verified by `rg -n "enum Command|Corpus\\(|Parallel\\(|Ship\\(|Quota\\(" src/main.rs`.
- `genesis/PLANS.md` lists twelve numbered plans and dependency order, verified by `find genesis/plans -maxdepth 1 -type f -name '[0-9][0-9][0-9]-*.md'`.
- `.auto/state.json` currently stores `planning_root` as `/home/r/Coding/autodev/genesis`, verified by `nl -ba .auto/state.json`.
- The root `IMPLEMENTATION_PLAN.md` now contains the promoted production-race worker queue with 26 priority rows and 4 follow-on rows, verified by `rg -n "^- \\[ \\]" IMPLEMENTATION_PLAN.md` and `auto parallel status`.
- `src/generation.rs` already builds prompts that require generated specs to name source of truth, runtime/UI/generated/fixture/retired surfaces, acceptance, verification, and closeout, verified by `rg -n "Required output contract|Source Of Truth|Runtime Contract|Review And Closeout" src/generation.rs`.

Recommendations for the intended system:

- Treat `genesis/` and future `gen-*` snapshots as subordinate until `auto gen --sync-only --output-dir <gen-dir>` or a manual operator promotion writes reviewed root specs and ledgers; for this pass, the reviewed root specs and root `IMPLEMENTATION_PLAN.md` are the active execution truth.
- Add an explicit production-control gate that prints whether launch, resume, promotion, or release is safe, unsafe, or waived.
- Preserve the current markdown-ledger architecture; add host-side validators and status summaries before introducing new storage.

Hypotheses / unresolved questions:

- This execution gate uses reviewed root `IMPLEMENTATION_PLAN.md` and root specs plus `.auto/super/20260430-180207/EXECUTION-GATE.md` as the current promotion artifact; the future default promotion artifact remains a product decision.
- Performance targets for large queues are not settled by current code; they need measured fixture evidence before becoming requirements.
- Root specs dated `220426`, `230426`, and `300426` overlap this snapshot; retirement should happen only when the operator promotes replacements.

## Runtime Contract

- `src/generation.rs` owns corpus loading, generated spec verification, generated plan verification, snapshot-only mode, sync-only mode, and root sync.
- `src/super_command.rs` owns the production-race orchestration gate before `auto parallel`.
- `src/parallel_command.rs` and `src/loop_command.rs` own execution against the root queue only.
- `src/state.rs` may remember prior planning roots and outputs, but saved state is runtime-generated local JSON and must be treated as untrusted until repo containment is validated.
- If the current root queue cannot be parsed, if the root queue is empty, or if the generation output cannot be verified, production launch must fail closed and say which truth source is missing.

## UI Contract

- The UI is terminal output plus markdown ledgers. It must display which surface is authoritative: root ledger, generated snapshot, saved state, receipt, report, or operator waiver.
- Terminal help and README prose must not imply that `genesis/` is active queue truth by default.
- Status output must not duplicate scheduler eligibility rules in prose; it must render the same host-side parser/gate result used by `auto parallel`, `auto super`, and `auto loop`.
- Generated specs may recommend future work, but generated text must label unimplemented future behavior as recommendation or hypothesis.
- Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- `gen-20260430-184141/specs/*.md` is this generated spec snapshot.
- `gen-20260430-184141/IMPLEMENTATION_PLAN.md` is this generated execution-plan snapshot; the reviewed root `IMPLEMENTATION_PLAN.md` copy is now the active queue truth for this gate.
- `gen-20260430-184141/corpus/**` is the copied planning context already present in the output directory.
- Future promotion refreshes root `specs/*.md`, root `IMPLEMENTATION_PLAN.md`, `.auto/state.json`, and `.auto/logs/*`.
- `auto gen --snapshot-only` writes a reviewable snapshot; `auto gen --sync-only --output-dir gen-20260430-184141` or reviewed manual root edits promote accepted rows after review.

## Fixture Policy

- Fixture corpora, fake receipts, and synthetic queue files belong in Rust tests or temporary directories only.
- Production code must not import generated snapshot files as fallback truth for active queue state.
- Snapshot corpora may be copied for review, but root execution must parse current root ledgers and current receipts.

## Retired / Superseded Surfaces

- Do not delete older root specs automatically. On promotion, tombstone only the root specs whose topic is replaced by a generated same-day spec and update any root plan references.
- Archive obsolete `gen-*` snapshots only after confirming `.auto/state.json` no longer points to them.
- Treat `.auto/super/*` and `.auto/parallel/*` as run evidence, not active doctrine.

## Acceptance Criteria

- `auto gen --snapshot-only` can produce a snapshot without changing root ledgers.
- `auto gen --sync-only --output-dir gen-20260430-184141` refuses to sync if any generated spec or plan violates the host-side generation validators.
- `auto super` cannot launch `auto parallel` from an empty or unpromoted root queue without an explicit operator decision recorded as a waiver.
- `auto parallel status` states whether current work is live, stale, blocked, safe to launch, or unsafe to launch.
- Root `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, and `COMPLETED.md` do not contradict the selected promotion mode.

## Verification

- `rg -n "Required output contract|verify_generated_specs|verify_generated_implementation_plan|sync_verified_generation_outputs" src/generation.rs`
- `rg -n "verify_parallel_ready_plan|EXECUTION-GATE|run_super" src/super_command.rs`
- `rg -n "run_parallel_status|refresh_parallel_plan_or_last_good|ready_parallel_tasks" src/parallel_command.rs`
- `rg -n "^- \\[ \\]" IMPLEMENTATION_PLAN.md`
- `auto parallel status`
- `cargo test generation::tests`
- `cargo test super_command::tests`
- `cargo test --test parallel_status`

## Review And Closeout

- A reviewer first proves current truth with `git status --short`, `find genesis/plans ... | wc -l`, `rg -n "^- \\[ \\]" IMPLEMENTATION_PLAN.md`, `auto parallel status`, and `nl -ba REVIEW.md`.
- The reviewer then runs the focused generation/super/parallel tests above and confirms failures name the exact invalid surface.
- Grep proof must show no promoted root queue row points workers at `genesis/` or `gen-*` as active truth unless the row is explicitly a generation/promotion task.
- Closeout must record the chosen promotion mode and whether this snapshot is still subordinate or has been synced into root specs and the root implementation plan.

## Open Questions

- Should promotion write a dedicated `PROMOTION.md`/manifest for auditability?
- Should `auto doctor` include active planning primacy in its required checks?
- Should stale root specs be tombstoned at sync time or left for a separate review pass?
