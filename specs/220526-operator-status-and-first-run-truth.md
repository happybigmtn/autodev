# Specification: Operator Status And First-Run Truth

## Objective

Make no-model operator status truthful enough that a maintainer can run the binary, understand what is ready or blocked, and choose the next safe command without invoking a model.

This is P1 because it improves operator clarity after the underlying validation and evidence facts stabilize. It outranks historical spec cleanup because it affects the first command a user runs.

## Source Of Truth

- Runtime owner modules/APIs: `src/doctor_command.rs::run_doctor`, `src/doctor_command.rs::build_doctor_report`, `src/parallel_command.rs::run_parallel_status`, `src/quota_status.rs`, `src/main.rs` command and subcommand definitions.
- UI consumers: `auto doctor` stdout, `auto parallel status` stdout, `auto quota status` stdout, README quickstart and command guide, CI help smoke.
- Generated artifacts: none for `auto doctor`; `.auto/parallel/**` and `.auto/parallel/live.log` are status inputs for `auto parallel status`; quota config/state files are status inputs for `auto quota status`.
- Retired/superseded surfaces: treating missing model tools as first-run baseline failures; stale help probes that omit live public commands; scattered status facts that force operators to infer readiness from unrelated reports.

## Evidence Status

Verified facts grounded in code or docs:

- `src/doctor_command.rs:13-25` defines required autodev layout and help surfaces currently probed by doctor. The help probe list includes `--help`, `corpus`, `gen`, `design`, `super`, `parallel`, `quota`, and `symphony`; it omits `doctor`, `audit-harvest`, `ship`, `health`, and others.
- `src/doctor_command.rs:26-43` lists `codex`, `claude`, `pi`, and `gh` as optional capability checks with workflow descriptions.
- `src/doctor_command.rs:48-55` prints a doctor report and returns an error only when required checks fail.
- `src/doctor_command.rs:99-132` builds doctor required checks, optional tool checks, version probe, and help probes without invoking models.
- `src/doctor_command.rs:148-239` checks planning root health, queue task counts, and latest generated snapshot state.
- `src/doctor_command.rs:484-530` renders `required`, `capabilities`, `model/network`, and `next steps`.
- `README.md:68-69` says `auto doctor` is read-only and no-model, and missing `codex`, `claude`, `pi`, and `gh` are capability warnings rather than baseline first-run failures.
- `AGENTS.md:21` says required tools on PATH are `claude`, `codex`, `pi`, and `gh`, which conflicts with doctor/README treating those as capability warnings for first-run baseline.
- `src/main.rs:315-327` defines `auto quota status`.
- `src/main.rs:63-122` has no top-level `status` command; current no-model status surfaces are command-specific.
- `tests/parallel_status.rs:6` and `tests/performance_status.rs:6` contain integration coverage for `auto parallel status`.

Recommendations for the intended system:

- Keep `auto doctor` no-model and read-only.
- Split baseline readiness from execution/model readiness in output and docs.
- Reuse existing status helpers and runtime checks instead of duplicating queue, quota, lane, or receipt rules in presentation code.
- Update AGENTS/README/doctor wording so missing model tools are hard failures only when the chosen model-backed workflow requires them.
- Add a no-model top-level `auto status` only if it can aggregate existing runtime-owned facts without becoming a second source of truth.

Hypotheses / unresolved questions:

- It is unresolved whether the product needs a top-level `auto status` command or whether `auto doctor`, `auto parallel status`, and `auto quota status` should remain separate.
- It is unresolved whether doctor should probe all public help surfaces or only first-run critical surfaces.

## Runtime Contract

`src/doctor_command.rs` owns first-run baseline readiness. `src/parallel_command.rs` owns parallel lane state. `src/quota_status.rs` owns quota account state. No presentation surface may manually rederive these facts from duplicated constants when a runtime helper exists.

If a required baseline fact is absent, doctor must fail closed with a concrete next action. If an optional model tool is absent, doctor must warn without failing baseline readiness.

## UI Contract

Terminal output must show the operator:

- What baseline repo/binary facts passed or failed.
- What capability tools are missing and which workflows are unavailable.
- Whether planning root, queue, and generated snapshot state are usable.
- Which command is safe next.

README and AGENTS must not tell operators that optional model tools are required for no-model first-run success. CI help smoke must not be the only place command-surface truth is checked.

Production UI must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; it must consume runtime helpers/generated contracts or render an explicit unavailable/error state when runtime truth is missing.

## Generated Artifacts

none for `auto doctor`

Status inputs consumed by related commands:

- `.auto/parallel/**`
- `.auto/parallel/live.log`
- quota config under the platform config directory, currently rooted by `QuotaConfig::config_dir()`
- quota state JSON under `QuotaState::state_path()`

## Fixture Policy

Doctor/status tests may use temp repos, synthetic `genesis/`, synthetic `IMPLEMENTATION_PLAN.md`, temp quota homes, and fake lane roots. Production status code must read the live checkout, live `.auto/` state, and live quota config/state, not bundled sample data.

## Retired / Superseded Surfaces

- `AGENTS.md` hard-requirement wording for `claude`, `codex`, `pi`, and `gh` as a first-run baseline, unless the operator decides those tools truly must be installed before any no-model command.
- Help-surface probes that omit commands intentionally documented as public first-run or lifecycle surfaces.
- Any status output that infers completion or readiness from stale generated snapshots instead of runtime-owned state.

## Acceptance Criteria

- `auto doctor` remains no-model and read-only.
- `auto doctor` exits nonzero only for failed required baseline checks.
- Missing `codex`, `claude`, `pi`, or `gh` appears as capability warning unless the operator selected a workflow needing that tool.
- `auto doctor` reports planning-root provenance, queue counts, and generated snapshot state from live files.
- README and AGENTS agree on baseline-required tools vs optional workflow capabilities.
- Help probes include every public command chosen for first-run smoke coverage.
- If a top-level `auto status` is added, it reuses doctor/parallel/quota helpers rather than duplicating readiness logic.

## Verification

Run from `/home/r/coding/autodev`:

```bash
cargo test doctor_command::tests
cargo test parallel_status
cargo test performance_status
cargo run -- doctor
cargo run -- parallel status
cargo run -- quota status
```

Grep proof:

```bash
rg -n "HELP_SURFACES|OPTIONAL_TOOLS|required tools|capability warnings|auto doctor|auto parallel status|auto quota status" src/doctor_command.rs src/main.rs README.md AGENTS.md tests/parallel_status.rs tests/performance_status.rs
```

## Review And Closeout

A reviewer should run `auto doctor` in the live checkout and confirm it does not invoke models, network APIs, Linear, GitHub, Symphony, Docker, browser automation, or tmux sessions. The reviewer should compare the output to README and AGENTS wording.

Closeout must include grep proof that optional tool wording is consistent and that any added status command calls existing runtime helpers for canonical facts.

## Open Questions

- Should `auto status` be added as a top-level aggregator, or should `doctor`, `parallel status`, and `quota status` remain the operator model?
- Should doctor probe every public help surface or a curated first-run subset?
- Are `claude`, `codex`, `pi`, and `gh` truly required by repo policy, or only by specific model-backed workflows?
