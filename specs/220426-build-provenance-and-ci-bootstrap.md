# Specification: Build provenance and GitHub Actions CI bootstrap

## Objective

Hold the build-time provenance pipeline honest (`build.rs` must capture git SHA, dirty flag, and cargo profile for every binary) and introduce a minimal GitHub Actions CI so the `AGENTS.md` validate block (`cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check`) is enforced on every push. The repo currently has no `.github/` directory; a tool that enforces discipline on other repos should enforce its own.

## Evidence Status

### Verified facts (code / repo state)

- `build.rs` exists and sets three `cargo:rustc-env=` variables:
  - `AUTODEV_GIT_SHA` from `git rev-parse --short HEAD`, with fallback `"unknown"` (`build.rs:8`).
  - `AUTODEV_GIT_DIRTY` from `git_dirty_flag()`: `"clean"`, `"dirty"`, or `"unknown"` when `git status --porcelain` cannot run or fails (`build.rs:9,46-58`).
  - `AUTODEV_BUILD_PROFILE` from the `PROFILE` env var, with fallback `"unknown"` (`build.rs:10`).
- Rerun triggers registered when a Git dir is discoverable: `.git/HEAD`, `.git/packed-refs`, current branch ref, `.git/index` (`build.rs:16-33`). If `git rev-parse --git-dir` fails, `emit_git_rerun_markers()` returns without panicking.
- `CLI_LONG_VERSION` at `src/util.rs:9-17` consumes those three env vars; `auto --version` renders them.
- `AGENTS.md` lists the Validate block: `cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check` (per `corpus/ASSESSMENT.md` §"Documentation staleness" row for AGENTS.md: "Accurate").
- No `.github/` directory exists at the repo root (`corpus/ASSESSMENT.md`; verified by explicit `ls .github` returning "No such file or directory").
- No `tests/` top-level directory exists; tests are inline `#[cfg(test)]` modules only.
- `Cargo.toml` fixes edition `2021`, pinned direct dependencies (list in `corpus/ASSESSMENT.md` §"Cargo.toml dependencies").

### Verified facts (validation state)

- `corpus/ASSESSMENT.md` flags that `cargo test` and `cargo clippy -D warnings` passing currently is **not verified** in this planning pass; Plan 010 prerequisite is "make warnings zero, then add CI."
- `corpus/plans/010-ci-github-actions-bootstrap.md` is the CI-bootstrap plan.
- `corpus/plans/011-integration-smoke-tests.md` adds the end-to-end smoke tests that CI should enforce after bootstrap.

### Recommendations (corpus)

- Pin GitHub Actions to SHA hashes with version comments, per the user's global dev standards file (`CLAUDE.md` §"GitHub Actions": `actions/checkout@<sha> # vX.Y.Z`, `persist-credentials: false`).
- Lint workflows with `actionlint` and scan with `zizmor` before committing (`CLAUDE.md` §"GitHub Actions").
- CI matrix should at minimum exercise `ubuntu-latest` on stable Rust; expanding to macos is out of scope for the first bootstrap.

### Hypotheses / unresolved questions

- Release-tarball / no-`.git` fallback is source-verified in `build.rs`: SHA and dirty flag fall back to `"unknown"`, and Git rerun markers are skipped when `git rev-parse --git-dir` fails.
- Whether any integration test today truly passes without a live `codex` / `claude` binary is unverified; current tests are unit-level and presumably hermetic, but Plan 011 is where end-to-end smoke tests would change that.

## Acceptance Criteria

### Build provenance (`build.rs`)

- Compiling `autodev` in a clean Git work tree yields `AUTODEV_GIT_DIRTY == "clean"`.
- Compiling with a modified tracked file yields `AUTODEV_GIT_DIRTY == "dirty"`.
- `AUTODEV_GIT_SHA` is the short commit hash of `HEAD` at build time.
- `AUTODEV_BUILD_PROFILE` equals `"debug"` for `cargo build`, `"release"` for `cargo build --release`.
- `auto --version` prints the package version on the first line, followed by lines `commit: <sha-or-unknown>`, `dirty: clean|dirty|unknown`, `profile: debug|release|unknown`.
- If the build environment has no `git` on `PATH`, compilation still succeeds; `AUTODEV_GIT_SHA` falls back to a documented placeholder (for example, `"unknown"`). If this fallback is not currently implemented, it is flagged as a follow-on.

### GitHub Actions CI bootstrap

- A `.github/workflows/ci.yml` file exists at the repo root once Plan 010 lands.
- The workflow triggers on `push` to any branch and on `pull_request` against `main`.
- The workflow runs on `ubuntu-latest` with stable Rust installed via `actions-rs/toolchain` or an equivalent pinned action.
- The workflow executes, in order: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`.
- All actions used in the workflow are pinned to commit SHAs with a trailing version comment (for example, `actions/checkout@<sha>  # v4.1.1`) per the user global dev standards.
- `persist-credentials: false` is set on every `actions/checkout` invocation.
- The workflow file is linted clean by `actionlint` and scanned clean by `zizmor` before commit.
- Before CI is enabled, `cargo clippy --all-targets --all-features -- -D warnings` must pass locally — Plan 010 rescue path explicitly says "make warnings zero, then add CI."
- A follow-on plan (Plan 011) adds integration smoke tests for `auto qa`, `auto health`, `auto ship`; CI can adopt those jobs once they exist.

## Verification

- `cargo build` then run `./target/debug/auto --version`; assert the four-line output includes package version, `commit:`, `dirty:`, and `profile:` labels with non-empty values.
- `git commit --allow-empty` then rebuild; `auto --version` shows the new SHA.
- Push a branch; GitHub Actions reports `fmt`, `clippy`, `test` steps green.
- Run `actionlint .github/workflows/` locally — zero findings.
- Run `zizmor .github/workflows/` locally — zero findings.
- Intentionally introduce a warning, push; assert CI fails with the expected clippy output.
- Intentionally introduce an unformatted file, push; assert CI fails at `cargo fmt --check`.

## Open Questions

- Should CI matrix expand to `macos-latest` for contributor-friendliness, or stay Linux-only to keep the CI bill small?
- Should CI cache `~/.cargo/registry` and `target/` via `actions/cache`? Defaulting to enabled with SHA-pinned cache action is recommended.
- Should `build.rs` emit an `AUTODEV_BUILD_DATE` env var alongside the others, or is SHA sufficient?
- Should `auto --version --verbose` surface the full 40-char SHA rather than the short one?
- Should CI also run an end-to-end `auto gen` against a hermetic fixture, or wait for Plan 011 integration smoke tests?
