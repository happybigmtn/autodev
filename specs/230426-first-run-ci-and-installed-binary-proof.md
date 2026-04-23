# Specification: First-Run CI And Installed Binary Proof

## Objective
Give operators and contributors a trustworthy first-success path that proves the repository builds, validates, installs, and exposes the expected `auto` binary before live models, credentials, Linear, Symphony, or release workflows are required.

## Evidence Status

### Verified Facts

- `Cargo.toml` declares package `autodev` version `0.2.0` in `Cargo.toml:2-3`.
- `Cargo.toml` declares the binary name `auto` with path `src/main.rs` in `Cargo.toml:8-10`.
- `build.rs` embeds `AUTODEV_GIT_SHA`, `AUTODEV_GIT_DIRTY`, and `AUTODEV_BUILD_PROFILE` in `build.rs:12-14`.
- `CLI_LONG_VERSION` exposes package version plus embedded build metadata in `src/util.rs:11-19`.
- The CI workflow runs `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` in `.github/workflows/ci.yml:30-36`.
- The current CI workflow does not contain `cargo install`, `which auto`, or `auto --version`; this is backed by `rg -n "cargo install|which auto|auto --version" .github/workflows/ci.yml README.md`, which found those commands in README but not the workflow.
- README documents `auto --version` as version plus embedded git commit and dirty/clean status in `README.md:271`.
- README documents `cargo install --path . --root ~/.local` in `README.md:1162`.
- `auto health` is a model-backed health pass: it writes health prompt artifacts under `.auto/health` and `.auto/logs` in `src/health_command.rs:63-72`, then invokes Codex in `src/health_command.rs:84`.
- `HealthArgs` defaults to model `gpt-5.5`, reasoning effort `high`, and Codex binary `codex` in `src/main.rs:1098-1121`.
- There is no `Doctor` command variant in the command enum in `src/main.rs:53-99`.
- Plan 010 recommends a no-model success path and hermetic smoke tests in `genesis/plans/010-first-run-doctor-and-hermetic-smoke-tests.md:7`.
- Plan 011 recommends installed-binary proof after first-run smoke coverage in `genesis/plans/011-ci-fidelity-and-installed-binary-proof.md:1` and `genesis/PLANS.md:38-39`.

### Recommendations

- Add a no-model doctor or preflight mode that checks required local tools, repo layout, generated/corpus path expectations, help text, and installation status without invoking Codex, Claude, Kimi, PI, Linear, or Symphony.
- Extend CI or release checks to install the binary into a temporary root and run the PATH-resolved binary for version and help smoke.
- Keep `auto health` as model-backed unless a compatibility path is explicitly added; do not relabel current health behavior as no-model.
- Document the first success path with exact commands and expected evidence files.

### Hypotheses / Unresolved Questions

- It is unresolved whether first-run should be implemented as `auto doctor`, `auto health --preflight`, or `auto --self-test`.
- It is unresolved whether installed-binary proof belongs in CI, local release scripts, `auto ship`, or all three.
- It is unresolved which external tools should be required for baseline first success versus optional feature-specific checks.

## Acceptance Criteria

- A first-run command succeeds without live model credentials, network access to model providers, Linear credentials, or a Symphony checkout.
- The first-run command reports missing `codex`, `claude`, `pi`, and `gh` as capability-specific warnings or failures instead of panicking.
- CI or release validation installs `auto` into a temporary root and runs the installed binary by PATH, not only `cargo run`.
- Installed-binary proof records `auto --version` output with package version, git SHA, dirty state, and build profile.
- Help smoke covers at least `auto --help`, `auto corpus --help`, `auto gen --help`, `auto parallel --help`, `auto quota --help`, and `auto symphony --help`.
- Model-backed `auto health` remains clearly documented as model-backed until a no-model mode exists.
- README install instructions and CI validation commands stay consistent with the actual workflow.

## Verification

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo install --path . --root /tmp/autodev-install-proof`
- `PATH=/tmp/autodev-install-proof/bin:$PATH auto --version`
- `PATH=/tmp/autodev-install-proof/bin:$PATH auto --help`
- Add and run no-model first-run smoke tests for missing corpus, incomplete corpus, and generated output directory handling.

## Open Questions

- Should CI prove installed binary behavior on every pull request or only on release branches?
- Should first-run preflight create artifacts, or should it be read-only by default?
- Should the first-run command verify optional browser, tmux, and Symphony dependencies, or defer them to feature-specific checks?
