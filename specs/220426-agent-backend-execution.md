# Specification: Agent backend execution — Claude, Codex, PI, Kimi

## Objective

Keep the four agent-CLI backends (`claude`, `codex`, `pi`, `kimi-cli`) behind small, uniformly-shaped wrappers that handle spawn, env injection, stdout streaming, futility detection, quota routing, and error pattern extraction. Every command that runs a model must go through one of the backend helpers rather than invoking the CLI directly, and the wrappers must fail loudly (non-zero exit with a named-dependency error) when their underlying binary is missing or times out silently. Dead tmux scaffolding in `codex_exec.rs` must not ship as live code.

## Evidence Status

### Verified facts (code)

- `src/claude_exec.rs:16` `DEFAULT_CLAUDE_MODEL_ALIAS = "opus"`. Entry points: `run_claude_exec` (line 19), `run_claude_with_futility` (line 46), `run_claude_exec_with_env` (line 74). 4-5 tests per corpus ASSESSMENT.
- `src/claude_exec.rs:161` invokes Claude with `--dangerously-skip-permissions` (always on; no operator override).
- `src/codex_exec.rs:30` `run_codex_exec`; `:56` `run_codex_exec_with_env`. 2-4 tests per corpus ASSESSMENT.
- `src/codex_exec.rs:217-221` and `:430` invoke Codex with `--dangerously-bypass-approvals-and-sandbox` (always on).
- `src/codex_exec.rs:1` has `#![allow(dead_code)]` with a comment: "Tmux-backed Codex lane helpers are staged for CLI integration but are not yet wired into a live command path." ~400 of 685 LOC are dead per `corpus/ASSESSMENT.md` §"Half-built".
- Dead tmux helpers in `codex_exec.rs`: `TmuxCodexRunConfig` (lines 21-27), `run_codex_exec_in_tmux_with_env`, `spawn_codex_in_tmux`, `ensure_tmux_lanes`, `render_tmux_codex_script`, `wait_for_tmux_completion`. None are referenced by a live caller.
- `src/codex_stream.rs:16` `CLAUDE_FUTILITY_THRESHOLD: usize = 8`; `:22` `CLAUDE_FUTILITY_THRESHOLD_REVIEW: usize = 16`.
- `FUTILITY_EXIT_MARKER: i32 = 137` is declared at `src/claude_exec.rs:142`; this matches `corpus/SPEC.md` item 7.
- Empty tool-result tracking: `fn is_empty_tool_result` (`codex_stream.rs:1055`), `consecutive_empty_results: usize` on `ClaudeRenderState` (`codex_stream.rs:56`), `fn track_claude_tool_futility` (`codex_stream.rs:1037-1053`).
- `src/kimi_backend.rs:23` `KIMI_CLI_DEFAULT_MODEL = "kimi-code/kimi-for-coding"`; `:28` `KIMI_CLI_MODEL_ENV = "FABRO_KIMI_CLI_MODEL"`.
- `kimi_backend.rs` entry points: `resolve_kimi_cli_model` (line 33), `resolve_kimi_bin` (line 66) which searches `FABRO_KIMI_CLI_BIN`, `~/.npm-global/bin/kimi-cli`, `~/.local/bin/kimi-cli`, then falls back to `kimi-cli` on PATH. Also `preflight_kimi_cli`, `kimi_exec_args` (wraps `kimi-cli --yolo --print --output-format stream-json`), `parse_kimi_error`, `extract_final_text`. 9-11 tests.
- `src/pi_backend.rs:5-9` `PiProvider` enum (`Kimi`, `Minimax`). `default_model` returns `"kimi-coding/k2p6"` for `Kimi` and `"minimax/MiniMax-M2.7-highspeed"` for `Minimax`. `resolve_pi_bin` (line 63) searches `FABRO_PI_BIN`, `~/.npm-global/bin/pi`, `~/.local/bin/pi`. 8 tests.
- `kimi_backend.rs::preflight_kimi_cli` catches the silent "LLM not set" failure (`corpus/SPEC.md` §"What works").
- Quota routing gates through `quota_exec::is_quota_available(Provider::Claude)` at `src/claude_exec.rs:89`.

### Verified facts (docs)

- `README.md:43` says `auto parallel` launches a detached tmux session. The live tmux code is in `parallel_command.rs`, not `codex_exec.rs`; the `codex_exec.rs` dead tmux scaffolding is a stale plan artifact (`corpus/ASSESSMENT.md`).
- `README.md:949` names the CLI binaries (`claude`, `codex`, `pi`, `kimi-cli`) the tool expects on `PATH`.

### Recommendations (corpus)

- Delete the dead tmux scaffolding in `codex_exec.rs` per `corpus/plans/003-codex-exec-tmux-deadcode-removal.md`. `auto parallel`'s tmux integration remains in `parallel_command.rs`.
- Research-only: a shared `LlmBackend` trait consolidating Codex / Pi / Kimi dispatch (`corpus/plans/008-llm-backend-trait-research.md`). Hold until a third caller justifies the abstraction.

### Hypotheses / unresolved questions

- Whether the futility-exit marker (137) is special-cased the same way by every caller (for example, does `auto loop` mark the task `- [~]` vs `- [ ]`?) is not uniformly verified in this pass.
- Whether `FABRO_KIMI_CLI_MODEL` env takes precedence over CLI flags is asserted by resolution order but not tested end-to-end here.

## Acceptance Criteria

### Claude backend (`claude_exec.rs`)

- Entry points `run_claude_exec`, `run_claude_with_futility`, `run_claude_exec_with_env` exist and remain the only ways any command calls Claude.
- Claude invocations always include `--dangerously-skip-permissions`.
- The Claude wrapper consumes quota routing via `quota_exec::is_quota_available(Provider::Claude)` and surfaces quota exhaustion through the quota rotation path, not as a crash.
- Futility detection is active: when consecutive empty-tool-result count reaches `CLAUDE_FUTILITY_THRESHOLD` (or the review-tier threshold `CLAUDE_FUTILITY_THRESHOLD_REVIEW`), the Claude process is killed and the wrapper returns exit code `137` (`FUTILITY_EXIT_MARKER`).

### Codex backend (`codex_exec.rs`)

- Entry points `run_codex_exec` and `run_codex_exec_with_env` remain the only live dispatch paths.
- Codex invocations always include `--dangerously-bypass-approvals-and-sandbox`.
- Dead tmux scaffolding is removed: `TmuxCodexRunConfig`, `run_codex_exec_in_tmux_with_env`, `spawn_codex_in_tmux`, `ensure_tmux_lanes`, `render_tmux_codex_script`, `wait_for_tmux_completion` are not present in `codex_exec.rs` after the cleanup; `#![allow(dead_code)]` at line 1 is removed or reduced to cover only provably-temporary items.
- No live caller in the repo imports any of the deleted tmux helpers.

### Kimi backend (`kimi_backend.rs`)

- `kimi-cli` binary resolution order is honored: `FABRO_KIMI_CLI_BIN` env > `~/.npm-global/bin/kimi-cli` > `~/.local/bin/kimi-cli` > `kimi-cli` on `PATH`.
- `FABRO_KIMI_CLI_MODEL` env overrides the default `KIMI_CLI_DEFAULT_MODEL = "kimi-code/kimi-for-coding"`.
- Kimi invocations use `kimi-cli --yolo --print --output-format stream-json` as argument shape.
- `preflight_kimi_cli` runs a trivial request and fails loudly when the CLI reports the "LLM not set" silent-failure state.
- `parse_kimi_error` surfaces a short, non-secret error string suitable for operator logs; token content must not appear in the error.

### PI backend (`pi_backend.rs`)

- `PiProvider::Kimi` default model is `"kimi-coding/k2p6"`; `PiProvider::Minimax` default model is `"minimax/MiniMax-M2.7-highspeed"`.
- `pi` binary resolution order: `FABRO_PI_BIN` env > `~/.npm-global/bin/pi` > `~/.local/bin/pi` > `pi` on `PATH`.
- `parse_pi_error` and `PiProvider::detect` report provider-recognized error classes (rate limit, quota, etc.) for quota rotation.

### Cross-backend

- Missing binary for any backend triggers a non-zero exit with a named-dependency error ("not found on PATH: `kimi-cli`"). No silent fallback to a different backend.
- All backends stream stdout/stderr to the operator through `codex_stream.rs` renderers; no backend buffers a long run silently.
- All backends write a prompt log under `.auto/logs/<command>-<timestamp>-prompt.md` before invoking the underlying CLI.

## Verification

- `cargo test -p autodev claude_exec`, `cargo test -p autodev codex_exec`, `cargo test -p autodev codex_stream`, `cargo test -p autodev kimi_backend`, `cargo test -p autodev pi_backend` all pass.
- Add a test that counts `#![allow(dead_code)]` attributes in `src/codex_exec.rs` after the Plan 003 cleanup and asserts zero (or the minimum justified).
- Add a grep-based check that `run_codex_exec_in_tmux_with_env` is no longer referenced anywhere in `src/`.
- Add a futility test: simulate 8 consecutive empty tool-result blocks; assert the wrapper returns exit code 137 and the Claude process is killed.
- Add a missing-binary test: point `FABRO_KIMI_CLI_BIN` at a non-existent path and assert the named-dependency error.
- Add a preflight test for `preflight_kimi_cli` recognizing the "LLM not set" message and surfacing it as a distinct error.

## Open Questions

- Should the `--dangerously-skip-permissions` / `--dangerously-bypass-approvals-and-sandbox` flags ever be opt-out? Today they are always-on; removing the flag for a specific safe command would require per-command plumbing.
- Should a shared `LlmBackend` trait land now (pending Plan 008 research) or continue as two separate enums in `bug_command.rs` and `nemesis.rs`?
- Should futility detection apply uniformly to `auto nemesis` implementer and `auto qa`, or stay tuned per command?
- Should Kimi and PI share a common error-shape type that quota rotation consumes directly?
