# Specification: Audit Nemesis And Report-Only Lifecycle

## Objective

Make audit, nemesis, QA-only, health, design, review, and book lifecycle commands honest about what they read, wrote, proved, skipped, and blocked.

## Source Of Truth

- Runtime owners: `src/audit_command.rs`, `src/audit_everything.rs`, `src/nemesis.rs`, `src/qa_only_command.rs`, `src/health_command.rs`, `src/design_command.rs`, `src/review_command.rs`, `src/book_command.rs`, `src/bug_command.rs`.
- Report owners: `.auto/audit-everything/**`, `audit/`, `bug/`, `nemesis/`, `.auto/design/**`, `.auto/qa-only/**`, `.auto/health/**`, `QA.md`, `HEALTH.md`, `REVIEW.md`, `WORKLIST.md`, root specs and root plan only when a command is explicitly mutating.
- UI consumers: terminal status, run manifests, `RUN-STATUS.md`, final reviews, CODEBASE-BOOK, `QA.md`, `HEALTH.md`, `REVIEW.md`, root specs, root plan.
- Generated artifacts: audit manifests, per-file verdicts, remediation plan files, final review, file-quality reports, CODEBASE-BOOK, nemesis audit/spec/plan/results, QA/health reports, design reports, bug pipeline JSON/markdown.
- Retired/superseded surfaces: report-only modes that mutate root planning truth, advertised flags that do not match behavior, model-only lifecycle proof without fixture coverage, and unlabeled model observations.

## Evidence Status

Verified facts grounded in code or primary repo files:

- `audit_everything` owns `.auto/audit-everything`, latest-run, pause files, staged run state, file-quality thresholds `9.0` accept and `10.0` target, verified by `rg -n "PROFESSIONAL_AUDIT_DIR|LATEST_RUN_FILE|FILE_QUALITY_ACCEPT_SCORE|FILE_QUALITY_TARGET_SCORE" src/audit_everything.rs`.
- `audit_everything` writes manifests and `RUN-STATUS.md`, verified by `rg -n "write_manifest|write_run_status_markdown|RUN-STATUS" src/audit_everything.rs`.
- `audit_command` is the legacy file-by-file doctrine audit and owns verdict application plus worklist/retire behavior, verified by `rg -n "DEFAULT_INCLUDE_GLOBS|FileVerdict|apply_verdict|WORKLIST" src/audit_command.rs`.
- `NemesisArgs` exposes `--report-only` and `--audit-passes`, verified by `rg -n "report_only|audit_passes" src/main.rs src/nemesis.rs`.
- `nemesis.rs` has functions that sync nemesis spec to root and append nemesis plan to root, verified by `rg -n "sync_nemesis_spec_to_root|append_nemesis_plan_to_root|report_only" src/nemesis.rs`.
- `qa_only_command`, `health_command`, and `design_command` are report-style surfaces named by the corpus as stronger report-only patterns, verified by `rg -n "QA.md|HEALTH.md|DESIGN-REPORT|report-only" src/qa_only_command.rs src/health_command.rs src/design_command.rs`.

Recommendations for the intended system:

- Declare allowed write sets for every report-only mode and enforce them with dirty-state snapshots.
- Decide whether `auto nemesis --report-only` is truly report-only, renamed, or explicitly "no implementation but may sync planning artifacts".
- Make `--audit-passes` either run multiple auditor passes with clear artifacts or fail as unsupported.
- Add model-free lifecycle fixture tests for critical command families.
- Label report evidence as host receipt, model observation, external blocker, operator waiver, or generated artifact.

Hypotheses / unresolved questions:

- Existing operators may rely on nemesis report-only writing root specs/plans; changing it is a user-sovereignty decision.
- The minimum fixture set for `audit --everything` may be expensive; use model-free stubs and narrow manifests first.
- Whether `bug` belongs under report-only lifecycle depends on how much of its pipeline is in scope for this campaign.

## Runtime Contract

- Each lifecycle command owns its run root and must verify required reports before claiming success.
- Report-only mode may write only declared report/log/artifact paths and must fail if other files change.
- Mutating modes may edit root specs, root plan, worklist, or source only when flags and command names say so.
- `audit_everything` manifest and `RUN-STATUS.md` own audit progress truth.
- If a required report is missing/empty, a final verdict is not accepted, a report-only write boundary is violated, or an advertised flag is unsupported, the command must fail closed.

## UI Contract

- Terminal output must state report path, status, blockers, next step, and whether source/root planning files were changed.
- `QA.md`, `HEALTH.md`, design reports, audit final review, nemesis reports, and book review must label evidence sources.
- Report UIs must not duplicate runtime verdict parsing or file-quality thresholds; they must render runtime-owned constants and statuses.
- README/help must distinguish report-only, mutating, queue-promoting, and release commands.
- Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- `.auto/audit-everything/<run-id>/MANIFEST.json`, `RUN-STATUS.md`, `FINAL-REVIEW.md`, `CHANGE-SUMMARY.md`, `CODEBASE-BOOK/**`, `file-quality/**`.
- `audit/MANIFEST.json`, `audit/files/**/verdict.json`, `patch.diff`, and progress snapshots for legacy audit.
- `nemesis/**`, root synced nemesis specs/plans when enabled.
- `QA.md`, `HEALTH.md`, `.auto/design/**`, `.auto/qa-only/**`, `.auto/health/**`.
- `bug/**` pipeline reports and JSON if bug lifecycle is covered by the same write-boundary helper.

## Fixture Policy

- Lifecycle fixtures must use temp repos, stub model binaries, fake manifests, and synthetic report files.
- Production code must not import fixture reports as current QA, health, audit, or nemesis truth.
- Stubs must be clearly scoped to tests and must not be discovered through normal model binary resolution.

## Retired / Superseded Surfaces

- Retire report-only behavior that mutates root specs or root implementation plan without explicit naming.
- Retire advertised flags that are no-ops or partial without an unsupported error.
- Retire model report claims that are not labeled as model observations.

## Acceptance Criteria

- Every report-only command has an allowed write-set test that fails on source/root planning mutation.
- `auto nemesis --report-only` has an explicit settled contract and tests matching that contract.
- `auto nemesis --audit-passes 2` either produces two auditor-pass artifacts or fails before model work with unsupported messaging.
- `auto audit --everything status` and final review use manifest truth and exact verdict/evidence labels.
- Critical lifecycle command families have at least one model-free fixture path with stubbed backend output.
- Reports distinguish host receipts, model observations, external blockers, and operator waivers.

## Verification

- `cargo test audit_everything::tests`
- `cargo test audit_command::tests`
- `cargo test nemesis::tests`
- `cargo test qa_only_command::tests`
- `cargo test health_command::tests`
- `cargo test design_command::tests`
- `cargo test book_command::tests`
- `rg -n "report_only|audit_passes|sync_nemesis_spec_to_root|append_nemesis_plan_to_root|RUN-STATUS|FILE_QUALITY_ACCEPT_SCORE" src/audit_everything.rs src/audit_command.rs src/nemesis.rs src/qa_only_command.rs src/health_command.rs src/design_command.rs`

## Review And Closeout

- A reviewer runs report-only dirty-state fixtures and confirms disallowed writes are caught.
- A reviewer runs a nemesis contract fixture for the chosen `--report-only` behavior and the chosen `--audit-passes` behavior.
- Grep proof must show command help, README, and runtime behavior agree on report-only versus mutating modes.
- Closeout records which lifecycle commands still lack model-free fixtures and why that risk is accepted or queued.

## Open Questions

- Should nemesis report-only be renamed to avoid breaking existing workflows?
- Should lifecycle write-boundary enforcement be a shared helper in `util`?
- Which lifecycle commands are required in CI versus local-only due to cost?
