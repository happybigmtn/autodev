# Plan 010 — GitHub Actions CI bootstrap

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

## Purpose / Big Picture

The repository has no CI. `AGENTS.md` declares the validate commands (`cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check`), but nothing mechanical enforces them on push. Every green signal to date has been a local `cargo test` run by whoever last touched the tree. This is the correct early-stage choice; it stops being correct once the repo has Phase 1 and Phase 2 hygiene and is about to grow integration tests (Plan 011) that nobody wants to remember to run by hand.

This plan adds a single GitHub Actions workflow that runs the three validate commands on push and on pull request. The workflow is deliberately minimal: no matrix, no release artifacts, no caching beyond what `actions/cache` provides for `~/.cargo` and `target/`. It is the smallest honest CI that enforces the existing AGENTS.md contract.

The operator impact is one new file under `.github/workflows/`, a CI badge opportunity on the README (optional, out of scope here), and a green check mark on future PRs.

## Requirements Trace

- **R1.** A file at `.github/workflows/ci.yml` runs `cargo build`, `cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo fmt --all -- --check` on every push to `main` and on every pull request targeting `main`.
- **R2.** The workflow pins `actions/checkout` to a SHA hash with a version comment (per the global standard in `~/.claude/CLAUDE.md`). Same for any other action used.
- **R3.** The workflow uses `dtolnay/rust-toolchain@stable` (or equivalent SHA-pinned action) and installs `rustfmt` and `clippy` components.
- **R4.** `persist-credentials: false` is set on the checkout step.
- **R5.** The workflow runs on Ubuntu (`ubuntu-latest`). No matrix across OSes in this pass; cross-platform concerns are a separate plan if they arise.
- **R6.** `actionlint` and `zizmor` produce no errors on the workflow file, verified locally before commit.
- **R7.** A passing run is visible in the GitHub Actions tab after the commit lands.

## Scope Boundaries

- **Creating:** `.github/workflows/ci.yml`.
- **Not creating:** release workflows, deploy workflows, nightly matrix, cross-platform matrix, coverage reporting.
- **Not adding:** `rust-toolchain.toml`. The repo does not pin a toolchain today; keeping policy in the workflow is sufficient for the current scale.
- **Not adding:** CI caching beyond `actions/cache` on `~/.cargo` and `target/`. More elaborate caching is a follow-up.
- **Not adding:** badges to the README. Optional follow-up.
- **Not changing:** AGENTS.md, Cargo.toml, or any source file.

## Progress

- [ ] Workflow file authored.
- [ ] `actionlint` passes.
- [ ] `zizmor` passes.
- [ ] Commit and push.
- [ ] Visible green run on GitHub Actions.

## Surprises & Discoveries

None yet. Potential surprises:
- `cargo clippy -D warnings` turns up a warning that was latent on the local machine because of a different clippy version. In this case, either fix the warning in a follow-up plan, or narrow the clippy command for the CI introduction itself and open a tracking ticket.
- A `cargo fmt --check` failure because the tree has never been formatted uniformly. If so, run `cargo fmt --all` locally, commit the formatting pass in a separate preceding commit, then add CI.

## Decision Log

- **2026-04-21 — Single workflow, no matrix.** Taste. Matrix'ing across OSes or Rust versions doubles or triples the wall clock without adding proportionate safety for a tool that is developed on and run from Linux dev machines. A Windows/macOS matrix can be added later if the surface warrants it.
- **2026-04-21 — No `rust-toolchain.toml`.** Taste. Pinning toolchain in-repo creates a second source of truth with the workflow and the developer environment. The workflow's `@stable` selector matches `AGENTS.md` intent and is the simpler choice.
- **2026-04-21 — SHA-pin every third-party action.** Mechanical. Required by the global standard. Version comment next to the SHA keeps it readable.
- **2026-04-21 — `persist-credentials: false` is always on.** Mechanical. `zizmor` would flag otherwise; it matches global-standard guidance against credential reuse.

## Outcomes & Retrospective

None yet.

## Context and Orientation

- `AGENTS.md` — source of truth for the validate commands. The workflow reflects what is already documented there.
- `Cargo.toml` — confirms `edition = "2021"`, no MSRV pin.
- `~/.claude/CLAUDE.md` (global) — specifies `actionlint` and `zizmor` as the GitHub Actions linters and security auditor.
- `genesis/ASSESSMENT.md` — notes the missing CI as a Phase 3 item.

## Plan of Work

1. Author `.github/workflows/ci.yml` in the shape described under Implementation Units.
2. Run `actionlint` and `zizmor` on the new file locally.
3. Fix any findings and re-run.
4. Commit, push, confirm the workflow runs green on the feature branch.
5. If the initial run fails because of a latent warning or a formatting delta, address in a preceding commit (do not weaken the workflow).

## Implementation Units

**Unit 1 — Authoring.**
- Goal: a single workflow file that runs the four validate steps on push and PR.
- Requirements advanced: R1, R2, R3, R4, R5.
- Dependencies: none (Plan 009 gates this, but authoring does not require other code changes).
- Files to create or modify: `.github/workflows/ci.yml`.
- Tests to add or modify: none -- infra.
- Approach: write YAML with `actions/checkout@<sha>`, `dtolnay/rust-toolchain@<sha>` with components, `actions/cache@<sha>` for cargo state, and the four `cargo` commands as separate named steps.
- Test expectation: none -- the workflow itself is the test.

**Unit 2 — Local static validation.**
- Goal: `actionlint` and `zizmor` emit zero findings.
- Requirements advanced: R6.
- Dependencies: Unit 1.
- Files to create or modify: `.github/workflows/ci.yml` (if findings require fixes).
- Tests to add or modify: none.
- Approach: run each tool; address any finding before commit.
- Test expectation: none.

**Unit 3 — Commit and observe.**
- Goal: first run on GitHub reaches green.
- Requirements advanced: R7.
- Dependencies: Unit 2.
- Files to create or modify: none (commit only).
- Tests to add or modify: none.
- Approach: commit, push to a feature branch, open a PR or push to `main` per the repo's branch discipline, watch the run complete.
- Test expectation: the run exits with success on all four `cargo` steps.

## Concrete Steps

From the repository root:

1. Confirm no workflow file exists yet:
   ```
   ls .github/workflows 2>/dev/null || echo "no workflows dir yet"
   ```
2. Create `.github/workflows/ci.yml` with the following structure. SHA placeholders below must be replaced with current pinned SHAs before commit (look up the latest tagged release of each action and its corresponding SHA from GitHub):
   ```yaml
   name: CI

   on:
     push:
       branches: [main]
     pull_request:
       branches: [main]

   permissions:
     contents: read

   jobs:
     build-test-lint:
       runs-on: ubuntu-latest
       steps:
         - name: Checkout
           uses: actions/checkout@<SHA>  # v4.x.x
           with:
             persist-credentials: false

         - name: Install Rust toolchain
           uses: dtolnay/rust-toolchain@<SHA>  # stable as of YYYY-MM-DD
           with:
             toolchain: stable
             components: rustfmt, clippy

         - name: Cache cargo registry and target
           uses: actions/cache@<SHA>  # v4.x.x
           with:
             path: |
               ~/.cargo/registry
               ~/.cargo/git
               target
             key: ${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}
             restore-keys: |
               ${{ runner.os }}-cargo-

         - name: Format check
           run: cargo fmt --all -- --check

         - name: Build
           run: cargo build --all-targets --all-features

         - name: Test
           run: cargo test --all-targets --all-features

         - name: Clippy
           run: cargo clippy --all-targets --all-features -- -D warnings
   ```
3. Run local static checks:
   ```
   actionlint .github/workflows/ci.yml
   zizmor .github/workflows/ci.yml
   ```
4. Address any findings. Common expected findings: unpinned action (fix with SHA), missing `permissions:` block (already included above).
5. Pre-commit sanity on the local tree:
   ```
   cargo fmt --all -- --check
   cargo build
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   ```
   If `cargo fmt --check` fails, run `cargo fmt --all` as a separate preceding commit before adding CI.
6. Commit:
   ```
   git add .github/workflows/ci.yml
   git commit -m "ci: add GitHub Actions workflow for build, test, clippy, fmt"
   ```
7. Push to a feature branch, open a PR, confirm the workflow runs green.

## Validation and Acceptance

- **Observable 1.** `.github/workflows/ci.yml` exists and contains four named steps: fmt check, build, test, clippy.
- **Observable 2.** Every third-party action is SHA-pinned with a version comment.
- **Observable 3.** `persist-credentials: false` appears on the checkout step.
- **Observable 4.** `actionlint .github/workflows/ci.yml` exits 0.
- **Observable 5.** `zizmor .github/workflows/ci.yml` exits 0.
- **Observable 6.** On push, the GitHub Actions "CI" run reaches green on all steps.
- **Observable 7.** On a PR that intentionally introduces a `cargo fmt`, `clippy`, or `test` failure, the run goes red. (Verified once, then reverted, as the fail-before-fix check.)

## Idempotence and Recovery

- Rerunning the workflow on the same commit produces the same outcome (modulo caching; cache hits are incidental).
- If the workflow file is committed with a broken YAML, the initial run fails loudly. Fix with a follow-up commit; no rollback needed.
- If a future clippy release introduces a new lint that breaks CI, either fix the lint or add a narrowly-scoped `#[allow(...)]` with a justification comment in the source (preferred: fix). Do not relax `-D warnings`.

## Artifacts and Notes

- Pinned SHAs at time of authoring (fill in at commit):
  - `actions/checkout`: (tag vX.Y.Z → SHA)
  - `dtolnay/rust-toolchain`: (tag → SHA)
  - `actions/cache`: (tag vX.Y.Z → SHA)
- `actionlint` version used locally: (to be filled).
- `zizmor` version used locally: (to be filled).
- First green run URL: (to be filled).
- Commit hash: (to be filled).

## Interfaces and Dependencies

- **Depends on:** Plan 009 checkpoint (Phase 3 gate) verifies that Phase 2 landed clean. Technically the workflow could be authored before that, but it would immediately enforce assertions that Phase 2 was about to satisfy. Running after the gate avoids a red-then-green sequence on the first CI run.
- **Used by:** Plan 011 (integration smoke tests) will add a fifth step or a second job under the same workflow.
- **External:** GitHub Actions runtime; `actionlint` and `zizmor` for local validation.
