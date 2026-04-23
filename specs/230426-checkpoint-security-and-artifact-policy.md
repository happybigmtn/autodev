# Specification: Checkpoint Security And Artifact Policy

## Objective
Define the security checkpoint and artifact-boundary policy that keeps generated state, credentials, worktree artifacts, and planning corpus files from being staged, committed, or executed accidentally.

## Evidence Status

### Verified Facts

- Checkpoint exclusion rules are defined as five rules in `src/util.rs:56`.
- The current checkpoint rules exclude `.auto`, `.claude/worktrees`, `bug`, `nemesis`, and top-level `gen-*` paths in `src/util.rs:56-62`.
- Checkpoint staging applies those exclusion rules while adding tracked and untracked files in `src/util.rs:351-385`.
- `genesis/` is not listed in `CHECKPOINT_EXCLUDE_RULES` in `src/util.rs:56-62`.
- `write_0o600_if_unix` is the repository helper for owner-only file writes on Unix in `src/util.rs:491-510`.
- The quota router currently writes config and state with the owner-only helper in `src/quota_config.rs:106-112` and `src/quota_state.rs:47`.
- Profile capture and execution-time recursive copy still preserve symlinks in `src/quota_config.rs:243-250` and `src/quota_exec.rs:77-84`.
- `auto audit` applies patches or appends planning entries and then uses `commit_all` in `src/audit_command.rs:799-935`.
- `commit_all` is the audit helper that stages broadly before committing in `src/audit_command.rs:927-935`.
- The planning corpus defines a Security Checkpoint Gate after planning truth, quota hardening, and Symphony workflow hardening in `genesis/plans/005-security-checkpoint-gate.md:7` and `genesis/PLANS.md:33`.
- Plan 005 names checkpoint policy in `src/util.rs` as part of its dependency surface in `genesis/plans/005-security-checkpoint-gate.md:143`.

### Recommendations

- Decide explicitly whether `genesis/` is active planning input that should be stageable or generated corpus input that needs an opt-in checkpoint path.
- Add a security gate that refuses to proceed to parser, verification, first-run, CI, or release work until quota restore and Symphony rendering tests pass.
- Add pathspec-based staging for audit and other automated commit paths that currently stage broadly.
- Extend sensitive-path checks to catch credential-like files, quota profiles, receipt JSON hand edits, generated workflow text, and external runtime state.

### Hypotheses / Unresolved Questions

- It is unresolved whether generated snapshot directories should remain excluded forever or become promotable artifacts through a specific command.
- It is unresolved whether broad staging is acceptable in `auto audit` when running in a clean branch with generated audit-only changes.
- It is unresolved whether receipt JSON files should be checkpoint-excluded, protected by validation, or both.

## Acceptance Criteria

- Security checkpoint output lists every currently excluded path class and explains why it is excluded.
- The checkpoint makes an explicit go/no-go decision for `genesis/` staging and records the decision in root planning docs.
- Automated commit paths use scoped pathspecs or produce an explicit exception when broad staging is required.
- Credential, auth, token, quota profile, generated runtime state, and receipt JSON paths are never silently included in checkpoint commits.
- The checkpoint blocks release-readiness work until quota restore tests, profile capture tests, and Symphony hostile-render tests pass or are recorded as blockers.
- The checkpoint reports any root planning document that contradicts live command inventory, version, or CI facts.

## Verification

- `rg -n "CHECKPOINT_EXCLUDE_RULES|gen-\\*|genesis|write_0o600_if_unix|commit_all|git add -A" src`
- `cargo test util::tests::write_0o600_if_unix_tightens_existing_file_before_write`
- Add and run tests for checkpoint staging of `genesis/`, `gen-*`, `.auto`, `bug`, `nemesis`, and quota profile paths.
- Add and run audit-path tests proving automated commits stage only intended files.

## Open Questions

- Should `genesis/` be treated like source-controlled planning input or generated corpus output?
- Should the security checkpoint be a command, a release gate section, or a CI job?
- Should manual operator overrides be recorded in `REVIEW.md`, `SHIP.md`, or a dedicated security checkpoint file?
