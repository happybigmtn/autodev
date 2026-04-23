# GENESIS-REPORT - corpus refresh

## Refresh Summary

This corpus was refreshed on 2026-04-23 against the current working tree. The previous snapshot under `.auto/fresh-input/genesis-previous-20260423-201851` was read as history, not truth. The current codebase and dirty working tree are authoritative.

The generated output lives under `genesis/`. No corpus content is printed here beyond this report, and no root implementation files were modified by the corpus authoring pass.

## Major Findings

1. The repo is now a mid-sized operator CLI, not a small prompt wrapper. It has sixteen commands, multiple backend routes, quota credential swapping, parallel lane orchestration, and Linear/Symphony integration.
2. Root planning truth is stale. `IMPLEMENTATION_PLAN.md`, `ARCHIVED.md`, `WORKLIST.md`, and specs need reconciliation before operators can trust the active queue.
3. Validation truth changed during review. The authoring pass recorded a full `cargo test` failure in `quota_usage::tests::codex_cli_refresh_surfaces_human_refresh_error`, but this Codex review reran that exact test and then full `cargo test`; both passed. Current evidence is 377 passed, 0 failed.
4. The highest-severity code risk is quota credential restore. Claude runs can back up `.claude.json` without restoring it through the normal restore path.
5. Credential profile capture/copy remains too permissive around symlinks, stale files, and owner-only storage.
6. Symphony workflow rendering interpolates shell/YAML values without enough validation or quoting.
7. Verification proof remains a known weak point. `WORKLIST.md` still names malformed generated verification commands and false-positive proof patterns.
8. Backend invocation policy is split across wrappers and direct command spawn paths, making dangerous-mode behavior harder to audit.
9. The meaningful user-facing design surface is terminal UX: command help, status, logs, generated markdown, and receipts.
10. First-run DX needs a no-model local success path and command-specific requirement checks.

## Recommended Direction

Build an operator-trust kernel around the existing command surface.

The recommended next state is not more commands. It is a safer, more legible lifecycle:

`corpus -> gen -> loop/parallel/symphony -> review/ship`, with shared task parsing, transactional credentials, quoted workflow rendering, risk-classed verification, redacted logs, and first-run smoke tests.

## Top Next Priorities

1. Reconcile root planning truth and stale docs.
2. Fix quota credential restore/profile hardening and keep the quota usage error-surfacing regression covered.
3. Harden Symphony workflow rendering against hostile shell/YAML scalars.
4. Gate the security baseline before touching execution contracts.
5. Harden verification commands and shared task parsing.
6. Add a no-model first-run success path and CI-installed-binary proof.

## Not Doing

- Adding a seventeenth command.
- Rewriting the CLI into a daemon, service, plugin framework, or workspace.
- Promoting `genesis/` to the active root implementation queue.
- Treating the previous corpus snapshot as current truth.
- Encrypting quota credentials at rest in this phase. Owner-only permissions, restore correctness, stale-file pruning, and symlink safety come first; encryption is a separate user challenge.
- Refactoring `parallel_command.rs` broadly before security and evidence gates pass.
- Replacing the current provider CLIs.
- Building a web UI or TUI.

## Focus-Seed Response

No focus seed was supplied. Priority order was therefore determined by full-repo evidence. If a future focus seed emphasizes model defaults or command lifecycle, the quota restore bug, credential copy safety, and Symphony workflow rendering still outrank most other topics because they affect credential integrity and generated executable content.

## Decision Audit Trail

| Decision | Classification | Rationale |
|---|---|---|
| Treat root `IMPLEMENTATION_PLAN.md` plus `specs/` as active planning surface | Mechanical | No root `PLANS.md` or root `plans/` exists; the code treats `genesis/` as generated corpus input. |
| Generate `DESIGN.md` | Mechanical | The repo has meaningful user-facing terminal, log, markdown, and receipt surfaces. |
| Put planning truth reconciliation first | Taste | It is not the highest security issue, but it prevents stale docs from misdirecting every later slice. |
| Put quota restore/profile hardening before verification-contract work | Mechanical | Credential restore is the highest-severity verified code risk. |
| Treat Symphony shell/YAML rendering as Phase 1 security work | Mechanical | It emits executable workflow text from string inputs. |
| Add checkpoint plans after every phase | Mechanical | The operator requested explicit checkpoint or decision-gate plans after meaningful boundaries. |
| Keep backend invocation unification as research first | Taste | There are enough direct spawn paths to justify design work, but a rushed shared runner could destabilize live commands. |
| Do not silently change dangerous-mode defaults | User Challenge | Changing default trust behavior affects operator workflows and must be explicit. |
| Do not decide encryption-at-rest now | User Challenge | Encryption introduces key management and recovery policy choices beyond the evidence in this pass. |
| Keep `steward` lifecycle replacement as a decision gate | User Challenge | Whether `steward` should supersede `corpus + gen` for mid-flight repos is product direction, not a mechanical cleanup. |
| Treat full-suite validation as currently green | Mechanical | The named quota failure from the authoring pass now passes targeted review, and full `cargo test` passed with 377 tests. |

## Review Discipline Applied

CEO: The premise was challenged. The repo should focus on trustworthy execution of current commands rather than breadth.

Design: CLI information architecture, state coverage, user journeys, accessibility, responsive terminal behavior, and AI-slop risk are covered in `DESIGN.md`.

Engineering: The plan set orders architecture work after concrete safety fixes and evidence-contract hardening.

DX: The plan set includes a first-run path, hermetic smoke tests, CI fidelity, and installed-binary proof.

## Generated Artifacts

- `genesis/ASSESSMENT.md`
- `genesis/SPEC.md`
- `genesis/PLANS.md`
- `genesis/GENESIS-REPORT.md`
- `genesis/DESIGN.md`
- `genesis/plans/001-master-plan.md` through `genesis/plans/012-release-readiness-and-command-lifecycle-gate.md`
