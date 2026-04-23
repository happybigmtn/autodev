# First-Run Preflight

Date: 2026-04-23

Status: Accepted

Task: `AD-006`

## Context

`auto health` is a model-backed repo health pass today. It writes prompt and run
artifacts under `.auto/health` and `.auto/logs`, then invokes Codex with default
model `gpt-5.5`, reasoning effort `high`, and binary `codex`.

The first-run path needs a different contract: contributors should be able to
prove the local checkout, installed `auto` binary, help surfaces, and basic
capability posture before they have Codex, Claude, PI, Linear, GitHub, Symphony,
or network access configured.

The command enum currently has no `Doctor` variant. That absence is useful
evidence: this decision defines the public shape for the later implementation
task and does not relabel the current `auto health` behavior as no-model.

## Decision

Add a dedicated no-model command:

```text
auto doctor
```

`auto doctor` is the first-success command for a fresh checkout or freshly
installed binary. It is not an alias for `auto health`, and it must not invoke
Codex, Claude, Kimi, PI, Linear, GitHub, Symphony, or any model provider.

Rejected shapes:

- `auto health --preflight`: rejected because `health` already means a
  model-backed quality report with durable prompt artifacts. A preflight mode
  under the same noun would make first-run docs too easy to misread.
- `auto --self-test`: rejected because it hides the surface from the existing
  subcommand-oriented help model and does not leave room for command-specific
  diagnostics later.

## Required Checks

`auto doctor` should fail non-zero when the baseline local installation cannot
be trusted:

- repo root can be found from the current directory;
- required repo layout exists, including `Cargo.toml`, `src/main.rs`, `README.md`,
  and `AGENTS.md`;
- `Cargo.toml` declares package `autodev` and binary `auto`;
- the running binary can report `auto --version` with package version, git SHA,
  dirty state, and build profile metadata;
- core help surfaces are parseable: `auto --help`, `auto corpus --help`,
  `auto gen --help`, `auto parallel --help`, `auto quota --help`, and
  `auto symphony --help`;
- generated/runtime directories that may exist on later runs, such as `.auto/`,
  `bug/`, `nemesis/`, and `gen-*`, are treated as runtime state rather than
  required source layout.

Required failures must name the failing check and include the next local action,
for example `cargo build`, `cargo install --path . --root ~/.local`, or rerun
from the repository root.

## Optional Capability Checks

`auto doctor` should report these tools as capability warnings, not baseline
failures:

- `codex`: required for model-backed Codex commands such as `auto health`,
  `auto qa`, and generation paths that choose the Codex backend;
- `claude`: required for Claude-backed corpus and generation paths;
- `pi`: required for quota-aware account multiplexing;
- `gh`: required for GitHub-facing ship or review flows.

Missing optional tools should not panic and should not prevent a first-run
success. The output should say which workflows are unavailable until the tool is
installed or authenticated.

Browser, tmux, Docker, regtest, and Symphony runtime checks are deferred to the
feature-specific commands that actually need them. The baseline first-run
preflight should not fail because those heavier environments are absent.

## No-Network And No-Model Contract

`auto doctor` must be safe to run without credentials and without network access.
It may inspect local files, local environment variables, PATH resolution, and
the current executable. It must not:

- call model CLIs or provider APIs;
- test provider authentication by making live requests;
- call Linear or GitHub APIs;
- start Symphony, Docker, browser automation, tmux sessions, or regtest nodes;
- create commits, branches, checkpoints, or remote pushes.

## Output Categories

Human output should be grouped so a new operator can scan it quickly:

- `required`: pass/fail checks for repo layout, binary provenance, and help
  surfaces;
- `capabilities`: warnings for missing optional tools, grouped by affected
  workflows;
- `model/network`: explicit statement that no model or network calls were made;
- `next steps`: the shortest safe follow-up command, such as `cargo test` or the
  first model-backed command once credentials are configured.

The final line should be unambiguous: `doctor ok` when required checks pass, or
`doctor failed` when any required check fails.

## Artifact Policy

`auto doctor` is read-only by default. It should not write `.auto/doctor`,
verification receipts, prompt logs, or any other artifact during the baseline
first-run pass.

If a future mode needs machine-readable output, prefer stdout-only JSON through
an explicit flag such as `--json`; do not add default filesystem writes to the
first-run command.

## Consequences

`AD-007` should implement `auto doctor` as the no-model first-run command and
keep `auto health` documented as model-backed. The README should teach
`auto doctor` before `auto health`, and CI or release proof can later install the
binary and run the same help/version surfaces through the PATH-resolved `auto`.
