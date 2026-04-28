# Backend Invocation Policy

Date: 2026-04-23

Status: Inventory gate for `AD-001`

Spec: `specs/230426-backend-invocation-policy-and-model-routing.md`

## Policy Gate

This document is an inventory and policy gate only. `AD-001` does not change
command construction, provider routing, model defaults, reasoning-effort
defaults, quota behavior, context-window settings, timeout handling, or
dangerous permission flags.

Future backend refactors must preserve the behavior recorded here unless a
later task explicitly owns the behavior change and updates this policy.

## Shared Backend Wrappers

| Surface | Binary and argv shape | Model and effort source | Quota behavior | Output and logs | Timeout or futility | Dangerous posture |
| --- | --- | --- | --- | --- | --- | --- |
| `src/codex_exec.rs` shared model runner | Codex models use `<codex_bin> exec --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check --cd <repo> -m <model> -c model_reasoning_effort="<effort>"`; `run_codex_exec_max_context` also adds `-c model_context_window=1000000`. Kimi aliases route to `kimi-cli --yolo --print --output-format stream-json -m <resolved>`. MiniMax aliases route to `pi --model <resolved> --thinking <effort> --mode json -p --no-session --tools read,bash,edit,write,grep,find,ls` | Caller supplies `model`, `reasoning_effort`, and `codex_bin`; shared routing resolves Kimi and MiniMax aliases through `src/kimi_backend.rs` / `src/pi_backend.rs` | Codex uses `quota_exec::run_with_quota(Provider::Codex, ...)` when Codex quota accounts are configured; Kimi and MiniMax do not use quota routing | Prompt is written by callers. Codex stdout is streamed through `codex_stream`; Kimi/MiniMax stdout is streamed through PI-style rendering and optionally mirrored to a caller-provided stdout log. Stderr is captured and appended to the caller-provided stderr log. Optional worker pid file is written and cleared. | No internal timeout. Caller owns retry or timeout. Kimi and PI error frames are parsed and surfaced as command errors. | Codex bypasses approvals and sandbox; Kimi uses `--yolo`; PI uses write-capable tools. |
| `src/claude_exec.rs` shared Claude runner | `claude -p --verbose --dangerously-skip-permissions --model <resolved_model> --effort <resolved_effort> --output-format stream-json [--max-turns <n>]` | Caller supplies `model`, `effort`, and optional `max_turns`; non-Claude or empty model resolves to `opus`, empty effort resolves to `high` | Uses `quota_exec::run_with_quota(Provider::Claude, ...)` when Claude quota accounts are configured; otherwise spawns `claude` directly | Prompt is written by callers. Stdout is streamed through Claude stream rendering and optionally mirrored to a caller-provided stdout log. Stderr is captured and appended to the caller-provided stderr log. Optional worker pid file is written and cleared. | Futility detection kills Claude after consecutive empty tool results. Default threshold is `CLAUDE_FUTILITY_THRESHOLD`; review can pass `CLAUDE_FUTILITY_THRESHOLD_REVIEW`. Synthetic exit marker is `137`. No wall-clock timeout. | Always skips Claude permissions. |

## Generation

| Surface | Binary and argv shape | Model and effort source | Quota behavior | Output and logs | Timeout or futility | Dangerous posture |
| --- | --- | --- | --- | --- | --- | --- |
| `src/generation.rs` author routing through `run_logged_author_phase` | Routes Claude-like models to the direct Claude path below; all other model names route to shared max-context execution, which may select Codex, Kimi, or MiniMax by model alias | `CorpusArgs` and `GenerationArgs` currently default authoring to `gpt-5.5` with `xhigh`; explicit operator flags are passed through | Codex authoring uses shared Codex quota behavior. Kimi/MiniMax authoring does not use quota routing. Claude authoring does not use `src/claude_exec.rs`, so it does not use quota routing | Writes `.auto/logs/<phase>-<timestamp>-prompt.md`. Shared authoring writes stdout and stderr logs beside the prompt. Claude authoring writes a response file only when stdout is non-empty. | Shared path has no timeout. Direct Claude path has `--max-turns` but no stream futility detector. | Inherits the selected backend's dangerous posture. Claude path skips permissions. |
| `src/generation.rs` direct Claude authoring, `run_claude_prompt` | `claude -p --verbose --dangerously-skip-permissions --model <model> --effort <reasoning_effort> --max-turns <n>` | Same authoring `model`, `reasoning_effort`, and `max_turns` supplied to generation | None; this path directly spawns `claude` | Writes prompt log before spawn, captures all stdout and stderr with `wait_with_output`, writes non-empty stdout to a response file, and includes stderr in the failure message | No wall-clock timeout and no shared futility detection | Skips Claude permissions. |
| `src/generation.rs` independent review | Shared max-context execution; model aliases may select Codex, Kimi, or MiniMax | `codex_review_model` / `--review-model` and `codex_review_effort` / `--review-effort`; current defaults are `gpt-5.5` and `xhigh` | Codex uses shared quota behavior; Kimi/MiniMax do not | Writes prompt log, stderr log, and requires the requested report path to be created and non-empty | No internal timeout | Inherits selected backend posture. |

Generation routing must continue to send Claude-like names through Claude and
non-Claude names through the shared model runner. The shared runner keeps Codex
models on Codex, routes Kimi aliases through `kimi-cli`, and routes MiniMax
aliases through `pi`.

## Commands Using Shared Wrappers

| Surface | Binary and argv shape | Model and effort source | Quota behavior | Output and logs | Timeout or futility | Dangerous posture |
| --- | --- | --- | --- | --- | --- | --- |
| `src/loop_command.rs` | `--claude` selects shared Claude; otherwise shared model runner | `LoopArgs.model`, `LoopArgs.reasoning_effort`, optional Claude `max_turns`, and `codex_bin` | Shared wrapper behavior for the selected provider | Writes `.auto/logs/loop-<timestamp>-prompt.md`; stderr log is `.auto/loop/codex.stderr.log` by default even when a non-Codex provider is selected | Claude can return futility marker `137`; loop tracks consecutive failures and distinguishes futility. Shared model runner has no timeout. | Inherits selected wrapper dangerous flags. |
| `src/parallel_command.rs` serial loop worker | `--claude` selects shared Claude with lane env; otherwise shared model runner with lane env | Host or lane config supplies `model`, `reasoning_effort`, optional Claude `max_turns`, and `codex_bin` | Shared wrapper behavior for selected provider | Writes loop prompt under `.auto/logs`; stderr goes to the run-root log path. Extra lane env is passed through. | Claude futility marker is detected and classified. Shared model runner has no timeout. | Inherits selected wrapper dangerous flags. |
| `src/parallel_command.rs` lane worker | `--claude` selects shared Claude with lane env and worker pid path; otherwise shared model runner with lane env and worker pid path | Lane config supplies `model`, `reasoning_effort`, optional Claude `max_turns`, and `codex_bin` | Shared wrapper behavior for selected provider | Writes per-attempt prompt, stdout log, stderr log, and worker pid under the lane run root | Claude futility marker is detected by the host. Shared model runner has no timeout. | Inherits selected wrapper dangerous flags. |
| `src/review_command.rs` | `--claude` selects shared Claude with review futility threshold; otherwise shared max-context model runner | `ReviewArgs.model`, `ReviewArgs.reasoning_effort`, optional Claude `max_turns`, and `codex_bin` | Shared wrapper behavior for selected provider | Writes `.auto/logs/review-<timestamp>-prompt.md`; stderr log is `.auto/review/codex.stderr.log` by default | Claude uses the review futility threshold. Shared model runner has no timeout. Review also has stale-batch handling outside the backend runner. | Inherits selected wrapper dangerous flags. |
| `src/qa_command.rs` | Shared model runner | `QaArgs.model`, `QaArgs.reasoning_effort`, and `codex_bin` | Codex uses shared quota behavior; Kimi/MiniMax do not | Writes prompt under `.auto/logs`; stderr log is `.auto/qa/codex.stderr.log` by default | No internal timeout | Inherits selected backend posture. |
| `src/qa_only_command.rs` | Shared model runner | `QaOnlyArgs.model`, `QaOnlyArgs.reasoning_effort`, and `codex_bin` | Codex uses shared quota behavior; Kimi/MiniMax do not | Writes prompt under `.auto/logs`; stderr log is `.auto/qa-only/codex.stderr.log` by default | No internal timeout | Inherits selected backend posture. |
| `src/health_command.rs` | Shared model runner | `HealthArgs.model`, `HealthArgs.reasoning_effort`, and `codex_bin` | Codex uses shared quota behavior; Kimi/MiniMax do not | Writes prompt under `.auto/logs`; stderr log is `.auto/health/codex.stderr.log` by default | No internal timeout | Inherits selected backend posture. |
| `src/ship_command.rs` | Shared model runner | `ShipArgs.model`, `ShipArgs.reasoning_effort`, and `codex_bin` | Codex uses shared quota behavior; Kimi/MiniMax do not | Writes prompt under `.auto/logs`; stderr log is `.auto/ship/codex.stderr.log` by default | No internal timeout | Inherits selected backend posture. |
| `src/steward_command.rs` | Shared model runner for steward and finalizer phases | `StewardArgs.model` and `reasoning_effort`; finalizer uses `finalizer_model` and `finalizer_effort`; both use `codex_bin` | Codex uses shared quota behavior; Kimi/MiniMax do not | Writes prompts under `.auto/logs`; stderr logs are under the steward output directory | No internal timeout | Inherits selected backend posture. |
| `src/super_command.rs` | Shared max-context model runner for super-only review gates | `SuperArgs` phase model and effort values plus `codex_bin` | Codex uses shared quota behavior; Kimi/MiniMax do not | Writes prompt and stderr logs under the super run root | No internal timeout | Inherits selected backend posture. |

## Direct Multi-Provider Pipelines

| Surface | Binary and argv shape | Model and effort source | Quota behavior | Output and logs | Timeout or futility | Dangerous posture |
| --- | --- | --- | --- | --- | --- | --- |
| `src/bug_command.rs` Codex backend | `<codex_bin> exec --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check --cd <repo> -m <model> -c model_reasoning_effort="<effort>" -c model_context_window=1000000` | Per-phase `PhaseConfig`; default Codex model is `gpt-5.5`, default effort is `high`; code-writer phases require `gpt-5.5` with `high` or `xhigh` | None; this path directly spawns Codex and does not use `src/codex_exec.rs` | Captures Codex JSON stdout into a string and appends stderr to `bug/bug.stderr.log`, truncated to `BUG_STDERR_LOG_MAX_BYTES` | Finder, skeptic, reviewer chunk phases use 30-minute timeout; implementation and final review use 90-minute timeouts; no futility detector | Bypasses Codex approvals and sandbox. |
| `src/bug_command.rs` PI backend | `<pi_bin> --model <resolved_model> --thinking <thinking> --mode json -p --no-session --tools read,bash,edit,write,grep,find,ls <prompt>` | `PiProvider::detect` and `resolve_model`; Kimi default is `kimi-coding/k2p6`, MiniMax default is `minimax/MiniMax-M2.7-highspeed`; effort maps to PI `--thinking` | None | Sets `PI_CODING_AGENT_DIR` and `OPENCODE_CODING_AGENT_DIR`; captures stdout through PI rendering with heartbeat and appends stderr to `bug.stderr.log` | Same bug phase timeouts as Codex. PI errors parsed from JSON stdout. | Tool allowlist includes write-capable tools. |
| `src/bug_command.rs` Kimi CLI backend | `<kimi_bin> --yolo --print --output-format stream-json -m <resolved_model> [--thinking|--no-thinking] -p <prompt>` | Kimi model is resolved by `src/kimi_backend.rs`; `--use-kimi-cli` is currently true by default; effort strings map to Kimi thinking on/off | None | Optional preflight runs before touching chunks. Runtime captures stdout through PI rendering with heartbeat, appends stderr to `bug.stderr.log`, parses final assistant text from stream-json | Same bug phase timeouts as Codex. Kimi errors and `LLM not set` are parsed from stdout. | Uses `--yolo`, which auto-approves Kimi actions. |
| `src/nemesis.rs` Codex backend | `<codex_bin> exec --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check --cd <repo> -m <model> -c model_reasoning_effort="<effort>" -c model_context_window=1000000` | `NemesisBackend::Codex`; default Codex Nemesis model is `gpt-5.5`, default effort is usually `high`; finalizer must be Codex | None | Prompts are written under `.auto/logs`; non-empty audit/review responses are written to `.auto/logs`; implementation/finalizer responses are written under `nemesis/`; stderr is captured for failure text, not appended to a central shared-wrapper log | No wall-clock timeout and no futility detector | Bypasses Codex approvals and sandbox. |
| `src/nemesis.rs` PI backend | `<pi_bin> --model <resolved_model> --thinking <thinking> --mode json -p --no-session --tools read,bash,edit,write,grep,find,ls <prompt>` | `PiProvider::detect` and `resolve_model`; `--kimi` and `--minimax` can opt into legacy non-Codex auditor models | None | Sets PI/OpenCode agent dirs, streams stdout with heartbeat, parses PI errors from stdout | No timeout. On PI failure, `run_nemesis_backend` falls back to Codex `gpt-5.5` with `high`. | Tool allowlist includes write-capable tools. |
| `src/nemesis.rs` Kimi CLI backend | `<kimi_bin> --yolo --print --output-format stream-json -m <resolved_model> [--thinking|--no-thinking] -p <prompt>` | Kimi model resolved by `src/kimi_backend.rs`; `--use-kimi-cli` is true by default for explicit Kimi phases | None | Streams stdout with heartbeat, parses final assistant text from stream-json, parses Kimi errors from stdout | No timeout. On Kimi failure, `run_nemesis_backend` falls back to Codex `gpt-5.5` with `high`. | Uses Kimi `--yolo`. |
| `src/audit_command.rs` Codex auditor | `<codex_bin> exec --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check --cd <repo> -m <model> -c model_reasoning_effort="<effort>" -c model_context_window=1000000` | `AuditArgs.model` and `AuditArgs.reasoning_effort`; current defaults are `gpt-5.5` and `high` | None; this path directly spawns Codex and does not use `src/codex_exec.rs` | Captures rendered Codex stdout as the auditor response and captures stderr only for failure text | 30-minute per-file timeout (`AUDITOR_TIMEOUT_SECS`) | Bypasses Codex approvals and sandbox. |
| `src/audit_command.rs` Kimi auditor | `<kimi_bin> --yolo --print --output-format stream-json -m <resolved_model> [--thinking|--no-thinking] -p <prompt>` | `AuditArgs.model` must look like Kimi and `--use-kimi-cli` must be enabled; Kimi model resolved by `src/kimi_backend.rs` | None | Preflights Kimi before audit. Captures stdout with heartbeat, parses final assistant text, and uses stderr/stdout only for failures | 30-minute per-file timeout | Uses Kimi `--yolo`. |

## Provider Helper Modules

| Surface | Responsibility | Model and binary source | Output and errors | Policy notes |
| --- | --- | --- | --- | --- |
| `src/kimi_backend.rs` | Resolves Kimi binary and model, builds `kimi-cli` argv, parses stream-json final text, parses Kimi errors, and performs Kimi preflight | Default resolved Kimi CLI model is `kimi-code/kimi-for-coding`; `FABRO_KIMI_CLI_MODEL` can override the model; `FABRO_KIMI_CLI_BIN`, `~/.npm-global/bin/kimi-cli`, `~/.local/bin/kimi-cli`, or `kimi-cli` select the binary | Preflight uses text output and `--final-message-only`; runtime callers parse stream-json. `LLM not set` is treated as a model/config error | The helper intentionally does not spawn runtime phases except preflight. Runtime callers own timeout, logs, and fallback. |
| `src/pi_backend.rs` | Detects Kimi/MiniMax PI providers, maps aliases, resolves `pi` binary, and parses PI JSON errors | `FABRO_PI_BIN`, bundled locations, or bare `pi` select the binary. Default models are `kimi-coding/k2p6` and `minimax/MiniMax-M2.7-highspeed` | Runtime callers parse stdout with `parse_pi_error` | The helper does not decide whether PI should remain a supported backend. |

## Symphony And Quota Entrypoints

| Surface | Binary and argv shape | Model and effort source | Quota behavior | Output and logs | Timeout or futility | Dangerous posture |
| --- | --- | --- | --- | --- | --- | --- |
| `src/symphony_command.rs` sync planner | If quota accounts exist: `<current auto exe> quota open codex exec ...`; otherwise: `<codex_bin> exec ...`. Both add `--json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check --cd <repo> -m <model> -c model_reasoning_effort="<effort>"` | `planner_model` and `planner_reasoning_effort` | Manual quota-open path instead of shared `run_codex_exec`; no quota when config has no Codex accounts | Captures Codex planner stdout and stderr into strings, with heartbeat on stdout | No internal timeout | Bypasses Codex approvals and sandbox. |
| `src/symphony_command.rs` external Symphony runtime | `<symphony_bin> --i-understand-that-this-will-be-running-without-the-usual-guardrails --logs-root <logs_root> [--port <port>] <workflow>` | Symphony runtime is external; the rendered workflow contains the worker model and reasoning effort | The rendered workflow command uses `auto quota open codex ... app-server` | Rust inherits stdout and stderr. Symphony logs go under the selected `logs_root`, with live log path printed as `<logs_root>/log/symphony.log` | Rust waits for process exit; no extra timeout | Explicitly runs Symphony without usual guardrails. |
| `src/symphony_command.rs` rendered Codex worker command | `env CARGO_TARGET_DIR=<shared> auto quota open codex --config shell_environment_policy.inherit=all --config model_reasoning_effort=<effort> --model <model> app-server` | `SymphonyRunSpec.model` and `SymphonyRunSpec.reasoning_effort` | Always rendered through `auto quota open codex` | Output handling is owned by Symphony, not this Rust process | Runtime limits are rendered into workflow fields such as read and wall-clock timeouts | Approval policy and sandbox are rendered into the Symphony workflow. |
| `src/quota_exec.rs` quota open | `<provider label> <args...>` where provider label is currently `codex` or `claude` | All model, effort, and flags are supplied by the caller's args | Selects an account, swaps credentials, launches the provider CLI, records account state, and restores credentials | Inherits provider CLI stdout/stderr. Prints selected quota account to stderr. | No timeout in `run_quota_open`; caller/runtime owns duration | Dangerous posture is entirely inherited from passed args. |
| `src/quota_usage.rs` Codex refresh | `<codex_bin> exec --json --ephemeral --skip-git-repo-check --sandbox read-only --color never --cd <scratch> -c model_reasoning_effort="<refresh_effort>" <refresh_prompt>` with `CODEX_HOME=<profile_dir>` | Refresh effort constant and refresh prompt are internal to quota usage; binary comes from `AUTO_QUOTA_CODEX_BIN` or `codex` | Runs against a profile directory to refresh captured Codex credentials | Captures stdout and stderr from the refresh process | Timeout behavior is local to refresh helper logic, not part of shared backend wrappers | Read-only sandbox, no dangerous bypass flag. |

## Supporting Subprocesses

The following subprocess families are not model backends, but they affect
backend orchestration and must stay distinguished from LLM invocations in any
future abstraction:

- `src/util.rs`, `src/review_command.rs`, `src/ship_command.rs`,
  `src/steward_command.rs`, `src/loop_command.rs`, `src/parallel_command.rs`,
  `src/nemesis.rs`, and `src/audit_command.rs` run `git` for repository state,
  checkpointing, branch sync, diff checks, and test setup.
- `src/parallel_command.rs` runs `sh`, arbitrary host programs, `agent-browser`,
  `tmux`, and `kill` for host preflight, browser readiness, lane terminal
  management, and process cleanup.
- `src/symphony_command.rs` launches the external Symphony runtime and renders
  an external Codex worker command into the Symphony workflow. These are runtime
  process boundaries, not shared wrapper calls.

## Current Defaults That Must Not Drift Implicitly

- Generation authoring and independent review defaults currently use `gpt-5.5` with
  `xhigh`.
- Shared model runner Kimi aliases route to `kimi-cli`; MiniMax aliases route
  to `pi`.
- Loop, parallel, QA, QA-only, health, review, ship, steward, audit, bug, and
  Nemesis Codex defaults currently use `gpt-5.5` with `high` unless a phase has
  a more specific flag such as a finalizer or reviewer model.
- Claude wrapper defaults resolve non-Claude or empty model strings to `opus`
  and empty effort to `high`.
- Kimi CLI default model resolves to `kimi-code/kimi-for-coding`.
- PI defaults resolve Kimi to `kimi-coding/k2p6` and MiniMax to
  `minimax/MiniMax-M2.7-highspeed`.

## Unresolved Decisions

- Whether generation's direct Claude authoring path should move onto
  `src/claude_exec.rs` for quota routing and futility handling remains
  unresolved. This document records the split; it does not choose a refactor.
- Whether bug, Nemesis, and audit direct Codex paths should move onto
  `src/codex_exec.rs` remains unresolved because those pipelines have their own
  response parsing, timeouts, fallback, and log semantics.
- Whether Kimi CLI and PI should become one provider abstraction or remain
  pipeline-specific backends remains unresolved.
- Whether dangerous flags should be surfaced in user-facing help, dry-run
  output, or normalized backend-policy telemetry remains unresolved.
- Whether every Codex and Claude invocation should opt into quota routing when
  accounts exist remains unresolved; current behavior is mixed and intentionally
  recorded as mixed.
- Whether direct backend stderr should be normalized into shared log files is
  unresolved. Current direct paths often capture stderr for failures instead of
  appending it to the shared wrapper logs.

## Verification Contract

The policy inventory is considered current for `AD-001` when both commands
below pass:

```bash
rg -n "Command::new|TokioCommand::new|run_codex_exec|run_claude_exec|run_claude_prompt|run_logged_author_phase|kimi-cli|pi_bin|dangerously|--yolo|quota open" src
rg -n "src/generation.rs|src/codex_exec.rs|src/claude_exec.rs|src/kimi_backend.rs|src/pi_backend.rs|src/bug_command.rs|src/nemesis.rs|src/audit_command.rs|src/symphony_command.rs|src/parallel_command.rs" docs/decisions/backend-invocation-policy.md
```

This document intentionally records unresolved decisions instead of implying a
single provider abstraction.
