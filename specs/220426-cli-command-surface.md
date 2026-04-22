# Specification: CLI command surface and version provenance

## Objective

Lock the `auto` CLI's top-level command surface, clap argument layout, subcommand trees, and embedded-binary provenance so operators, downstream commands, and CI can rely on a stable dispatch contract. The binary must advertise sixteen commands that all reach a real module, and `auto --version` must emit package version plus git SHA, dirty flag, and build profile so release artifacts are self-identifying.

## Evidence Status

### Verified facts (code / build)

- `src/main.rs:52-96` declares exactly sixteen variants on the `Command` enum: `Corpus`, `Gen`, `Reverse`, `Bug`, `Loop`, `Parallel`, `Qa`, `QaOnly`, `Health`, `Review`, `Steward`, `Audit`, `Ship`, `Nemesis`, `Quota`, `Symphony`.
- `src/main.rs:1-31` `mod` declarations reach every command module referenced by the enum.
- Quota surface has a nested subcommand tree (`QuotaSubcommand` with `Status`, `Select`, `Accounts`, `Reset`, `Open` at `src/main.rs:290-301`) and an `AccountsCommand` inner tree (`Add`, `List`, `Remove`, `Capture` at `src/main.rs:331-340`).
- Symphony has a nested subcommand tree (`SymphonySubcommand` with `Sync`, `Workflow`, `Run` at `src/main.rs:111-118`).
- `auto --version` uses `CLI_LONG_VERSION` from `src/util.rs:9-17`, which concatenates `CARGO_PKG_VERSION`, `AUTODEV_GIT_SHA`, `AUTODEV_GIT_DIRTY`, and `AUTODEV_BUILD_PROFILE`.
- `build.rs` sets `AUTODEV_GIT_SHA` (from `git rev-parse --short HEAD`, fallback `"unknown"`), `AUTODEV_GIT_DIRTY` (`"clean"` / `"dirty"` from `git status --porcelain`, fallback `"unknown"` when git is unavailable or errors), and `AUTODEV_BUILD_PROFILE` (from cargo `PROFILE`). It registers rerun triggers on `.git/HEAD`, `.git/packed-refs`, branch ref, and index when a Git dir is discoverable.
- Every subcommand variant has a clap `/// doc comment` used as its `--help` description (`src/main.rs:53-95`).
- `Cargo.toml` pins the binary name to `auto` under `[[bin]] path = "src/main.rs"`.

### Verified facts (docs / naming drift)

- `README.md:11` claims "`auto` owns thirteen commands" and the bulleted inventory (`README.md:13-25`) lists only: `corpus`, `gen`, `reverse`, `bug`, `nemesis`, `quota`, `loop`, `parallel`, `qa`, `qa-only`, `health`, `review`, `ship`. `steward`, `audit`, and `symphony` are not in that list; `README.md:536` mentions `auto symphony` in a prose sentence but the top-level inventory does not.
- `genesis/` planning corpus (`gen-20260422-040815/corpus/SPEC.md`, `ASSESSMENT.md`, `GENESIS-REPORT.md`) treats README command-count drift as the #1 operator-visible issue.

### Recommendations (intended future direction from corpus)

- Group `auto --help` output by cluster (Planning / Execution / Quality / Hardening / Release / Infrastructure) using `clap`'s `next_help_heading`, per `corpus/DESIGN.md` §"Decisions to recommend".
- README inventory and detailed per-command guide should reflect the sixteen real commands, per `corpus/PLANS.md` Plan 002.
- Preflight / `auto doctor` for agent-CLI presence is noted as a DX gap in `corpus/ASSESSMENT.md` but is explicitly out of scope for this pass (no such command exists).

### Hypotheses / unresolved questions

- Whether `steward` should supersede `corpus + gen` for mid-flight repos is under research per `corpus/plans/012-command-lifecycle-reconciliation-research.md`; a positive finding would retire commands from this inventory, not add to it.
- Whether `symphony` is a permanent part of the surface or an experiment is flagged `Unverified` in `corpus/ASSESSMENT.md` §"Assumption ledger".

## Acceptance Criteria

- `auto --help` lists sixteen top-level subcommands matching the `Command` enum in `src/main.rs:52-96`.
- `auto <subcommand> --help` renders the subcommand's clap doc comment as its description for all sixteen commands.
- `auto --version` output contains four lines: the `CARGO_PKG_VERSION` value, then `commit: <short-sha-or-unknown>`, `dirty: clean|dirty|unknown`, and `profile: debug|release|unknown`.
- `auto quota --help` lists the subcommands `status`, `select`, `accounts`, `reset`, `open`.
- `auto quota accounts --help` lists `add`, `list`, `remove`, `capture`.
- `auto symphony --help` lists `sync`, `workflow`, `run`.
- Every `Command` variant dispatches to a function in the module named after it (for example, `Command::Audit` dispatches into `audit_command`).
- Invoking `auto <subcommand>` with a required-but-missing argument prints a clap error message naming the argument and exits non-zero; it does not panic.
- Binary provenance is regenerated when `HEAD` moves: a rebuild after `git commit` produces a different `AUTODEV_GIT_SHA` in `auto --version` output.
- No `Command` variant is declared in `main.rs` without a reachable handler; `cargo build` fails if any dispatch arm is unwired.

## Verification

- Build with `cargo build --release` and run `target/release/auto --help`; assert the visible subcommand names match the enum variants.
- Run `target/release/auto --version`; assert four-line output with the documented labels.
- Make a trivial edit, run `cargo build`, then `auto --version` twice (once clean, once with a tracked-file dirty state); assert `dirty:` flips from `clean` to `dirty` without re-invoking `cargo clean`.
- Run `auto <each command> --help` in a shell loop and assert exit code 0 for all sixteen.
- Run `cargo test -p autodev main` to exercise the two argument-parsing unit tests currently in `src/main.rs`; add coverage for at least one representative argument on each command as a follow-on.
- Manual smoke: `auto quota --help`, `auto quota accounts --help`, `auto symphony --help` show nested trees.

## Open Questions

- Should `auto --help` group the sixteen commands by lifecycle cluster using `clap::Command::next_help_heading`? (Recommended by `corpus/DESIGN.md`; not implemented.)
- Should the README explicitly document the existing `dirty: unknown` fallback for builds where `build.rs` cannot reach `git` (for example, release tarballs or stripped build environments)?
- Should `auto symphony run` keep the current `--symphony-root <path>` / `AUTODEV_SYMPHONY_ROOT` resolution contract, or add more explicit preflight diagnostics for an unbuilt Symphony checkout? The operator-specific hardcoded default was removed by TASK-009.
