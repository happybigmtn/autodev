# Restore Formatting And Command Surface Baseline

## Priority Decision

P0. This is the first implementation slice. Score: very high operator value, high design clarity, medium engineering leverage, strongest evidence, and high parallel executability. It outranks `auto status`, quota hardening, and documentation cleanup because no operator can trust a repo that fails its advertised formatting and live command-surface checks. It is intentionally narrower than "make every test green."

## User / Operator Outcome

An operator can run the smallest advertised validation baseline and see that formatting and public command discovery are truthful. This gives later workers a clean start without turning every red validator, receipt, and lane test into one oversized assignment.

## Evidence

- `cargo fmt --check` failed on `src/spec_command.rs`, `src/super_command.rs`, and `src/task_parser.rs`.
- `cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture` failed because live Clap output includes `audit-harvest`.
- `src/main.rs` exposes `audit-harvest` as a command, and README/help smoke expectations lag that surface.
- The current worktree was already dirty before this corpus rebuild. This plan should preserve unrelated dirty quota, receipt, backend, and prompt work rather than flattening it into a formatting commit.

## Scope Boundary

Do not rewrite orchestration architecture. Do not fix generated-plan validators, receipt semantics, ship gates, lane routing, or quota persistence in this slice. Do not run rustfmt over unrelated dirty files unless those files are already in this slice or the operator explicitly accepts that formatting-only scope. Keep README/doc edits to the minimum command-surface truth needed by tests and CI help smoke.

## Implementation Slice

Goal: restore rustfmt and command-surface truth as the smallest validation baseline.

Dependencies: none.

Files likely to modify:

- `src/spec_command.rs`
- `src/task_parser.rs`
- `src/super_command.rs`
- `src/main.rs`
- `README.md`
- `.github/workflows/ci.yml` if CI help smoke should include `audit-harvest` or `doctor`.

Tests to add or modify:

- Update the command-surface test only after confirming whether `audit-harvest` is an intentional public command.
- Add or update help smoke only for live public commands.

Approach:

1. Run `cargo fmt --check` and apply the minimal rustfmt changes needed for the named files. If rustfmt wants to touch unrelated dirty files, inspect first and keep the commit scope explicit.
2. Decide whether `audit-harvest` is public. If it is public, update the command-surface test, README command list, and help smoke expectations. If it is not public, hide it deliberately and test that choice.
3. Run the targeted command-surface test and help commands.
4. Leave generated-plan validator failures to plan 003 and receipt/lane failures to plan 005.

## Verification

From the repository root:

    cargo fmt --check
    cargo test tests::top_level_command_surface_matches_live_enum
    cargo run -- --help
    cargo run -- audit-harvest --help

Expected observation: fmt passes, the live command surface and tests agree, and help output includes the intended public commands. Remaining generated-plan, receipt, lane, and quota failures are deferred to their own slices.

## Deferred

- Generated-plan/spec-task proof validator strictness.
- Receipt/ship/lane evidence semantics.
- New `auto status` design.
- Release packaging.
- Historical spec cleanup unrelated to the failing validators.
- Quota persistence hardening unless it blocks full test success.
