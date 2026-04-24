# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `AD-014` Symphony and receipt evidence checkpoint

  Spec: `specs/230426-symphony-workflow-and-linear-sync.md`
  Why now: Workflow rendering and completion evidence both affect unattended execution safety; future Linear sync or external Symphony runtime changes should wait until these local deterministic proofs are recorded.
  Codebase evidence: `src/symphony_command.rs` already reconciles terminal Linear issues through `inspect_task_completion_evidence` before marking local tasks done; `src/completion_artifacts.rs` is the shared evidence gate; `scripts/run-task-verification.sh` is the receipt-producing wrapper.
  Owns: `REVIEW.md`
  Integration touchpoints: `src/symphony_command.rs`, `src/completion_artifacts.rs`, `scripts/run-task-verification.sh`, `.auto/symphony/verification-receipts`
  Scope boundary: Validation and handoff only; do not call Linear, render a live external Symphony workflow, or launch Symphony runtime.
  Acceptance criteria: `REVIEW.md` records hostile workflow render test outcomes, zero-test receipt outcomes, any intentionally untested live Linear or Symphony surfaces, and a go/no-go decision for adapter migration work.
  Verification: `cargo test symphony_command::tests::workflow_render_rejects_hostile_branch`; `cargo test symphony_command::tests::workflow_render_rejects_hostile_model_and_effort`; `cargo test completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests`; `cargo test completion_artifacts::tests::inspect_task_completion_evidence_requires_wrapper_for_executable_verification`; `rg -n "AD-014|Symphony|zero-test|receipt" REVIEW.md`
  Required tests: `symphony_command::tests::workflow_render_rejects_hostile_branch`, `symphony_command::tests::workflow_render_rejects_hostile_model_and_effort`, `completion_artifacts::tests::inspect_task_completion_evidence_rejects_zero_cargo_tests`, `completion_artifacts::tests::inspect_task_completion_evidence_requires_wrapper_for_executable_verification`
  Completion artifacts: `REVIEW.md`
  Dependencies: `AD-011`, `AD-013`
  Estimated scope: XS
  Completion signal: Review handoff records local green proof or specific blockers for workflow rendering and receipt evidence.

- [~] `TASK-016` Tag `v0.2.0` once the priority + first follow-on cluster is verified clean

    Spec: `specs/220426-release-ship.md`
    Why now: `Cargo.toml` is still on `0.1.0`; once the visible drift (README, CI, dead code, hardening) is closed, cutting a `0.2.0` annotated tag locks the verified baseline. Spec frames this as a preservation contract; the only new work here is the actual tag.
    Codebase evidence: `Cargo.toml:3` (`version = "0.1.0"`), `build.rs:8-62` provenance already wired, `src/util.rs:9-17` `CLI_LONG_VERSION`.
    Owns: `refs/tags/v0.2.0`
    Integration touchpoints: `Cargo.toml`, `Cargo.lock`, `COMPLETED.md`.
    Scope boundary: bump `Cargo.toml` version to `0.2.0`, regenerate `Cargo.lock`, append a `## v0.2.0` section to `COMPLETED.md` summarizing the closed task IDs, and create the annotated tag locally. Do NOT push the tag in this task — `auto ship` (or a separate operator step) handles publishing and PR plumbing per spec.
    Acceptance criteria: `Cargo.toml` reads `version = "0.2.0"`; `cargo build` regenerates `Cargo.lock` cleanly; `git tag -l v0.2.0` returns `v0.2.0`; the tag's annotation message lists TASK-001..TASK-011 plus any closed follow-ons; `auto --version` first line reads `auto 0.2.0`.
    Verification: `cargo build && ./target/debug/auto --version | head -1` (must read `auto 0.2.0`); `git tag -l v0.2.0` returns `v0.2.0`; `git cat-file -p v0.2.0` shows annotated message with task list.
    Required tests: none (release-mechanics only; `cargo test` regression already covered by prior checkpoints)
    Completion artifacts: `Cargo.toml`, `Cargo.lock`, `COMPLETED.md`, `refs/tags/v0.2.0`
    Dependencies: TASK-011
    Estimated scope: S
    Completion signal: annotated `v0.2.0` tag exists locally with the closed task list, `auto --version` confirms the bump.


## Follow-On Work

