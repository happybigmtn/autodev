# Specification: Validation And Command Surface Baseline

## Objective

Restore the smallest trustworthy local baseline for `auto`: formatting passes, the public Clap command surface is intentional, and README/CI/help smoke do not contradict the live binary.

This is P0 because a red formatter and a stale command-surface test block every later worker from knowing whether failures are caused by their slice. It outranks broad documentation cleanup because the failing proof is executable and user-visible now.

## Source Of Truth

- Runtime owner modules/APIs: `Cargo.toml` for package and binary identity; `src/main.rs` for `Cli`, `Command`, command args, and dispatch; `.github/workflows/ci.yml` for CI validation commands.
- UI consumers: `auto --help`, per-command `--help`, README command list, CI installed-binary smoke logs, `auto doctor` help probes.
- Generated artifacts: none. Help output is rendered by Clap at runtime and must not be treated as a generated contract file.
- Retired/superseded surfaces: README's stale "twenty-one commands" claim if `audit-harvest` stays public; the stale expected list in `tests::top_level_command_surface_matches_live_enum`; CI help smoke that omits any intended public no-model command.

## Evidence Status

Verified facts grounded in code or commands:

- `Cargo.toml:1-10` declares package `autodev`, version `0.2.0`, edition `2021`, and binary `auto` at `src/main.rs`.
- `.github/workflows/ci.yml:29-36` runs `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- `.github/workflows/ci.yml:38-52` installs the binary and smokes `auto --version`, `auto --help`, `auto corpus --help`, `auto gen --help`, `auto design --help`, `auto super --help`, `auto parallel --help`, `auto quota --help`, and `auto symphony --help`; it does not currently smoke `auto doctor --help` or `auto audit-harvest --help`.
- `src/main.rs:63-122` defines the live top-level `Command` enum, including `Doctor`, `AuditHarvest`, and `Symphony`.
- `src/main.rs:1680-1712` dispatches `Doctor` to `doctor_command::run_doctor`, `AuditHarvest` to `super_command::run_audit_harvest_standalone`, and `Symphony` to `symphony_command::run_symphony`.
- `README.md:11-34` says `auto` owns twenty-one commands and lists no `auto audit-harvest` entry.
- `src/main.rs:1723-1735` hard-codes the expected command list and omits `audit-harvest`.
- Command evidence from this generation pass: `cargo fmt --check` exited 1 and reported diffs in `src/spec_command.rs`, `src/super_command.rs`, and `src/task_parser.rs`.
- Command evidence from this generation pass: `cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture` exited 101 because actual commands included `audit-harvest` while the expected list did not.

Recommendations for the intended system:

- Decide explicitly whether `audit-harvest` is public. If public, update README, command-surface tests, and CI help smoke. If private, hide it deliberately through Clap and add a test proving it is hidden.
- Apply minimal rustfmt changes only to files in the formatting failure set unless the operator accepts a wider formatting-only pass.
- Add CI smoke for `auto doctor --help`; add `auto audit-harvest --help` only if public.

Hypotheses / unresolved questions:

- It is unresolved whether `audit-harvest` should remain a public top-level command or become a hidden implementation subcommand of `auto super`.
- It is unresolved whether CI should smoke every public command or only first-run and lifecycle entry points.

## Runtime Contract

The canonical command list is the Clap `Command` enum in `src/main.rs`. Tests and documentation may render or verify that list, but they must not invent a separate product surface.

Formatting is a hard fail-closed gate: if `cargo fmt --check` is red, the baseline is not green and later feature slices must not claim full validation.

If command-surface truth is absent or ambiguous, runtime must fail closed in tests rather than letting README, CI, and help drift independently.

## UI Contract

The terminal UI is the product UI for this surface. README command lists, CI smoke logs, `auto --help`, and `auto doctor` must agree on which commands are public.

Presentation code must not duplicate command constants in a way that can silently drift from Clap. If a manual expected list remains, it must be a deliberate regression guard that is updated in the same change as any public command addition or removal.

Production UI must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; it must consume runtime helpers/generated contracts or render an explicit unavailable/error state when runtime truth is missing.

## Generated Artifacts

none

## Fixture Policy

Command-surface tests may use Clap parsing and synthetic argv only. Production README/CI/help behavior must come from the live binary, not fixture command lists, copied help output, or old generated snapshots.

## Retired / Superseded Surfaces

- README command count that omits `audit-harvest`, if `audit-harvest` is public.
- `tests::top_level_command_surface_matches_live_enum` expected list without `audit-harvest`, if `audit-harvest` is public.
- CI help smoke that skips intended public first-run commands.

## Acceptance Criteria

- `cargo fmt --check` exits 0.
- `cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture` exits 0.
- `cargo run -- --help` lists exactly the intended public top-level commands.
- README command count and command list match the intended public top-level commands.
- CI help smoke includes every command chosen as part of the first-run or public command contract.
- If `audit-harvest` is public, `cargo run -- audit-harvest --help` exits through Clap help successfully.
- If `audit-harvest` is private, `cargo run -- --help` does not list it and a test proves the hidden behavior.

## Verification

Run from `/home/r/coding/autodev`:

```bash
cargo fmt --check
cargo test tests::top_level_command_surface_matches_live_enum -- --nocapture
cargo run -- --help
cargo run -- doctor --help
```

Run this only if `audit-harvest` is public:

```bash
cargo run -- audit-harvest --help
```

Use grep proof to catch drift normal tests might miss:

```bash
rg -n "audit-harvest|twenty-one commands|HELP_SURFACES|top_level_command_surface_matches_live_enum" README.md src/main.rs src/doctor_command.rs .github/workflows/ci.yml
```

## Review And Closeout

A reviewer should independently compare `src/main.rs:63-122`, `cargo run -- --help`, README's command list, and CI smoke commands. The review is not complete if it only updates README or only fixes the test.

The closeout proof must include the formatter command and the targeted command-surface test. It must also include grep evidence showing the old stale command count or omitted `audit-harvest` expectation was removed or intentionally hidden.

## Open Questions

- Should `audit-harvest` remain a public operator command, or should it be hidden behind `auto super`?
- Should CI smoke all public commands, or only the first-run and high-traffic lifecycle commands?
