# Specification: Receipt Artifact And Evidence Binding

## Objective

Make verification receipts, declared artifacts, completion handoffs, and release gates share one current-tree evidence contract.

## Source Of Truth

- Runtime owners: `src/completion_artifacts.rs`, `src/ship_command.rs`, `src/verification_lint.rs`, `src/task_parser.rs`, `src/parallel_command.rs`, `src/symphony_command.rs`.
- Evidence owners: `.auto/symphony/verification-receipts/*.json`, `scripts/run-task-verification.sh`, `scripts/verification_receipt.py`, `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `SHIP.md`, `QA.md`, `HEALTH.md`.
- UI consumers: `auto parallel` host completion reconciliation, `auto loop`, `auto symphony`, `auto ship`, `REVIEW.md`, release reports, worker prompts.
- Generated artifacts: verification receipt JSON, review handoff entries, ship blockers, `.auto/parallel/live.log`, declared completion artifacts.
- Retired/superseded surfaces: narrative proof presented as host receipt, declared artifact paths outside repo containment, duplicate receipt freshness logic, zero-test success, and unsuperseded failed command history.

## Evidence Status

Verified facts grounded in code or primary repo files:

- `TaskCompletionEvidence` tracks review handoff, verification receipt path/presence/status, declared completion artifacts, missing artifacts, and unresolved audit findings, verified by `rg -n "struct TaskCompletionEvidence|has_review_handoff|verification_receipt_present|declared_completion_artifacts" src/completion_artifacts.rs`.
- `verification_receipt_root` resolves receipts under `.auto/symphony/verification-receipts`, including host lookup from lane repo paths, verified by `rg -n "verification_receipt_root|symphony/verification-receipts" src/completion_artifacts.rs`.
- Completion receipt freshness checks compare current commit, dirty-state fingerprint, plan hash, declared artifact hashes, expected argv, failed commands, superseded failures, and zero-test status, verified by `rg -n "verification_receipt_freshness_problem|zero_test|expected_argv|supersedes|dirty-state fingerprint" src/completion_artifacts.rs`.
- `ship_command` duplicates receipt structs and freshness logic for the release gate, verified by `rg -n "struct VerificationReceipt|verification_receipt_freshness_problem|load_verification_receipts" src/ship_command.rs`.
- `verification_lint` rejects stale `cargo --lib`, multi-filter cargo test commands, and malformed recursive grep shapes, verified by `rg -n "cargo --lib|multi-filter|malformed grep|verify_commands_are_runnable" src/verification_lint.rs`.
- `declared_artifact_path` currently joins declared artifact strings to repo root and special-cases receipt paths, verified by `rg -n "fn declared_artifact_path|repo_root.join|strip_prefix" src/completion_artifacts.rs src/ship_command.rs`.
- `scripts/run-task-verification.sh` invokes `scripts/verification_receipt.py record`, and the writer records commit, dirty state, plan hash, expected argv, supersedes metadata, zero-test detection, and declared artifact hashes, verified by `rg -n "verification_receipt|commit|dirty_state|plan_hash|expected_argv|supersedes|zero_test|declared_artifact" scripts/run-task-verification.sh scripts/verification_receipt.py`.

Recommendations for the intended system:

- Extract shared evidence inspection for completion and release so freshness policy cannot drift.
- Validate declared artifact paths as repo-contained before hashing or accepting them.
- Add explicit evidence classes: host receipt, model narrative, review handoff, external evidence, operator waiver, archive record.
- Treat failed, stale, missing, zero-test, and unsuperseded failed receipt entries as blocking for receipt-required work.

Hypotheses / unresolved questions:

- Target repositories may not have Autodev's receipt wrapper and writer; missing target-repo receipt tooling must be labeled as an environment/tooling blocker rather than treated as local host proof.
- Some external/live evidence cannot be represented as a local file; the label and waiver format need an operator decision.
- Directory artifact hashing is implemented, but the acceptable directory size/performance bound is not measured.

## Runtime Contract

- `completion_artifacts` owns the canonical evidence model and should become the shared inspector for completion and release.
- `ship_command` must consume the shared inspector or a shared evidence module instead of maintaining parallel freshness logic.
- `task_parser` owns extraction of verification text and declared artifacts from task markdown.
- `verification_lint` owns host-side proof-command shape validation before workers run.
- If declared artifacts are outside the repo, missing, stale, mismatched, unhashable when required, or narrative-only where a host receipt is required, completion/release must fail closed.

## UI Contract

- `REVIEW.md` must say whether validation is host observed, model observed, external, waived, or blocked.
- `auto ship` must show release blockers using the same evidence terms as task completion.
- Worker prompts must show executable verification commands separately from narrative guidance.
- UI consumers must not reinterpret receipt JSON fields; they must call the shared inspector or render its structured result.
- Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- `.auto/symphony/verification-receipts/*.json`.
- Host synthesized `REVIEW.md` handoff entries.
- `SHIP.md` release blockers and bypass notes.
- `.auto/parallel/live.log` and salvage records when completion cannot be landed.
- Future generated schema/documentation for receipt JSON if the implicit contract becomes explicit.

## Fixture Policy

- Tests may create synthetic receipts, fake declared artifacts, and temp git repos.
- Production code must not accept fixture receipts or copied receipt excerpts as current task proof.
- Fixture external evidence must be marked as external and cannot satisfy receipt-required local commands.

## Retired / Superseded Surfaces

- Retire duplicate receipt freshness logic in `src/ship_command.rs` once shared evidence inspection exists.
- Retire artifact path acceptance that can escape the repo root or hash arbitrary host paths.
- Retire review prose that calls model-written "commands run" a host receipt.

## Acceptance Criteria

- Declared artifact paths that are absolute, contain traversal, or resolve outside the repo are rejected with a clear error.
- Completion and ship gates share the same receipt freshness result for commit, dirty state, plan hash, expected argv, failed commands, superseded commands, zero-test status, and declared artifact hashes.
- Narrative-only proof is labeled as non-receipt evidence and does not satisfy receipt-required executable verification.
- Zero-test cargo receipts fail completion and release.
- A corrected passing command can supersede a failed receipt entry only through explicit receipt metadata.
- External evidence and operator waivers are visible and never silently treated as host-created receipts.

## Verification

- `cargo test completion_artifacts::tests`
- `cargo test ship_command::tests`
- `cargo test verification_lint::tests`
- `rg -n "verification_receipt_freshness_problem|declared_artifact_path|zero_test|supersedes|expected_argv" src/completion_artifacts.rs src/ship_command.rs`
- `rg -n "verify_commands_are_runnable|cargo --lib|multi-filter|grep" src/verification_lint.rs`

## Review And Closeout

- A reviewer runs matching completion and ship fixtures against the same stale receipt and confirms both fail with compatible blocker text.
- Grep proof must show receipt freshness code has one owner after consolidation, or a failing parity test covers every duplicated field.
- A reviewer inspects one real receipt under `.auto/symphony/verification-receipts/` and confirms the parser handles current fields without hand-editing it.
- Closeout records any non-local evidence class used in a task and why a host receipt could not represent it.

## Open Questions

- Should receipt JSON get a formal schema file under `docs/` or stay test-enforced?
- How should operator waivers expire?
- Should directory artifact hashing impose a max file count or byte size?
