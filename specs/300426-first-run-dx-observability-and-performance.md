# Specification: First-Run DX Observability And Performance

## Objective

Give a new operator a fast, non-mutating first success and give production operators truthful status, observability, and scale evidence before running large model-backed workflows.

## Source Of Truth

- Runtime owners: `src/main.rs`, `src/doctor_command.rs`, `src/parallel_command.rs`, `src/audit_everything.rs`, `src/health_command.rs`, `src/qa_only_command.rs`, `src/util.rs`.
- Documentation owners: `README.md`, `AGENTS.md`, `.github/workflows/ci.yml`, generated specs and plans.
- UI consumers: `auto --help`, `auto doctor`, `auto parallel status`, audit status, README quickstart, CI logs, installed binary smoke.
- Generated artifacts: `.auto/logs/*`, `.auto/parallel/*`, `.auto/audit-everything/*/RUN-STATUS.md`, `QA.md`, `HEALTH.md`, CI installed-binary output.
- Retired/superseded surfaces: long undifferentiated onboarding before first proof, status output that does not name safe/unsafe next action, and performance claims without measured fixtures.

## Evidence Status

Verified facts grounded in code or primary repo files:

- `auto doctor` is no-model and checks repo root, autodev layout, binary provenance, help surfaces, and optional tools `codex`, `claude`, `pi`, and `gh`, verified by `rg -n "AUTODEV_REQUIRED_LAYOUT|HELP_SURFACES|OPTIONAL_TOOLS|model/network" src/doctor_command.rs`.
- `doctor_command` help surfaces include `auto --help`, corpus, gen, design, super, parallel, quota, and symphony, verified by `rg -n "HELP_SURFACES|doctor_checks_expected_help_surfaces" src/doctor_command.rs`.
- CI installs the binary and smokes `auto --version`, `auto --help`, corpus, gen, design, super, parallel, quota, and symphony help, verified by `nl -ba .github/workflows/ci.yml`.
- README describes defaults, provider routing, first preflight commands, snapshot-only, and sync-only generation flow, verified by `rg -n "auto doctor|snapshot-only|sync-only|Defaults|Provider notes" README.md`.
- `audit_everything` owns manifest-backed run status and file-quality thresholds, verified by `rg -n "write_run_status_markdown|print_status|FILE_QUALITY_ACCEPT_SCORE|DEFAULT_FILE_QUALITY_PASS_LIMIT" src/audit_everything.rs`.
- `parallel_command` status summarizes tmux, host pids, lanes, warnings, and stale recovery, verified by `rg -n "run_parallel_status|tmux:|host pids|lanes:|health" src/parallel_command.rs`.

Recommendations for the intended system:

- Add a short README quickstart that starts with `cargo build` or `cargo install --path . --root ~/.local`, then `auto doctor`, then one non-mutating status command.
- Extend `auto doctor` to report active planning truth, corpus health, root queue summary, and whether model-backed commands are safe to start.
- Add deterministic performance fixtures for large plans and audit status before publishing exact targets.
- Keep optional tools as capabilities, not baseline first-run failures.

Hypotheses / unresolved questions:

- Exact performance targets for plan size, lane count, and audit manifest size need measurement on named hardware/context.
- Whether CI should run benchmark-style fixtures depends on runtime cost.
- The best first-success command after `auto doctor` may be `auto parallel status`, `auto corpus --verify-only`, or a new status command.

## Runtime Contract

- `doctor_command` owns first-run required checks and optional capability checks.
- `parallel_command` owns live/stale scheduler observability.
- `audit_everything` owns audit progress truth and `RUN-STATUS.md`.
- `util` owns binary provenance and checkpoint exclusions that keep generated/runtime state out of normal commits.
- If required layout or binary provenance checks fail, `auto doctor` exits non-zero. Missing model tools remain warnings unless a specific workflow requires them.

## UI Contract

- First-run UI must be copy-pasteable and non-mutating.
- Status output must state the safe next action or the blocker, not just dump raw state.
- Help, README, and doctor output must not disagree about required versus optional tools.
- Performance numbers must be labeled as measured evidence with command, machine/context, input size, and date.
- Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- CI installed-binary smoke logs.
- `.auto/logs/*` prompt/stderr/stdout logs.
- `.auto/parallel/*` status/log artifacts.
- `.auto/audit-everything/*/RUN-STATUS.md` and `MANIFEST.json`.
- Future performance fixture output under test temp dirs or a documented `.auto/perf/` location if added.

## Fixture Policy

- Doctor tests may create temp repos and fake optional tools.
- Status/performance tests may create synthetic run roots, manifests, and queue files.
- Production code must not use fixture queue/audit/status data as fallback truth.

## Retired / Superseded Surfaces

- Retire docs that present `codex`, `claude`, `pi`, and `gh` as required for no-model first success.
- Retire status prose that omits safe/unsafe next action.
- Retire unmeasured performance targets.

## Acceptance Criteria

- A fresh checkout can run `auto doctor` without invoking model providers, network APIs, Docker, browser automation, tmux, Linear, or GitHub.
- `auto doctor` reports binary provenance, help-surface health, optional tool capabilities, active planning root, corpus health, and queue summary.
- README quickstart reaches a non-mutating success in a small number of commands.
- CI keeps installed binary smoke for the major help surfaces.
- `auto parallel status` and audit status name safe launch/resume/recovery state.
- At least one deterministic fixture measures large-plan or audit-status behavior before any exact performance target is documented as a requirement.

## Verification

- `cargo test doctor_command::tests`
- `cargo test --test parallel_status`
- `cargo test audit_everything::tests`
- `cargo test util::tests::checkpoint_status_ignores_autodev_generated_dirs`
- `cargo run --quiet -- doctor`
- `cargo run --quiet -- parallel status`
- `rg -n "auto doctor|snapshot-only|sync-only|Provider notes|Smoke installed auto binary" README.md .github/workflows/ci.yml src/doctor_command.rs`

## Review And Closeout

- A reviewer runs `auto doctor` locally and summarizes required failures versus optional warnings without leaking secrets.
- A reviewer checks README quickstart commands against the compiled binary help.
- Grep proof must show CI still smokes the installed binary help surfaces.
- Closeout records any measured performance fixture with command, input size, elapsed time, machine/context, and whether the number is a target or observation.

## Open Questions

- Should active planning truth live in `auto doctor`, `auto status`, or both?
- What input sizes define "large plan" and "large audit" for this repo?
- Should optional provider auth checks be added behind a flag so first-run remains non-mutating and no-model?
