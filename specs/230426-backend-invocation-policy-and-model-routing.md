# Specification: Backend Invocation Policy And Model Routing

## Objective
Inventory and normalize agent backend invocation so model names, reasoning effort, dangerous flags, quota routing, logging, context-window settings, and futility handling are explicit policy instead of scattered command construction.

## Evidence Status

### Verified Facts

- Generation authoring routes through `run_logged_author_phase` in `src/generation.rs:918-985`.
- Generation authoring uses direct `claude -p --verbose --dangerously-skip-permissions --model <model> --effort <effort> --max-turns <n>` for Claude-like models in `src/generation.rs:1075-1089`.
- The generation backend-routing test verifies non-Claude models such as `gpt-5.5` and `o3` use Codex authoring rather than Claude in `src/generation.rs:4012`.
- Shared Codex execution uses `codex exec --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check --cd <repo> -m <model>` and can set `model_context_window` in `src/codex_exec.rs:152-167`.
- Shared Claude execution constructs `claude` with `--dangerously-skip-permissions` and `--output-format stream-json` in `src/claude_exec.rs:157-167`.
- Symphony planner invocation uses either `auto quota open codex exec` or direct Codex execution and sets `model_reasoning_effort` in `src/symphony_command.rs:929-953`.
- `auto parallel` uses shared Claude and Codex execution wrappers for serial loop and worker lanes in `src/parallel_command.rs:1930-1958` and `src/parallel_command.rs:4918-4933`.
- Kimi backend argv is documented as `kimi-cli --yolo --print --output-format stream-json -m <model> -p <prompt>` in `src/kimi_backend.rs:92-106`.
- Kimi default model handling and `kimi-coding/k2p6` validation are in `src/kimi_backend.rs:19-44` and `src/kimi_backend.rs:319-326`.
- PI defaults include `kimi-coding/k2p6` and `minimax/MiniMax-M2.7-highspeed` in `src/pi_backend.rs:21-22`.
- `auto bug`, `auto nemesis`, and `auto audit` each build direct backend commands for Codex, Kimi, or PI paths in `src/bug_command.rs:1446-1687`, `src/nemesis.rs:1064-1287`, and `src/audit_command.rs:1052-1211`.
- The planning corpus says backend invocation policy is a research gate and should not change behavior or defaults until direct spawn paths, dangerous flags, quota routing, logs, context windows, and futility are inventoried in `genesis/plans/008-backend-invocation-policy-research.md:1` and `genesis/plans/008-backend-invocation-policy-research.md:106-108`.

### Recommendations

- Create a backend invocation inventory artifact that lists every direct `Command::new`, shared wrapper, quota-routed path, and provider-specific CLI.
- Define a policy struct for backend name, model, effort, sandbox or permission posture, quota routing, context window, JSON mode, logging path, timeout, and futility handling.
- Refactor only after the policy artifact is reviewed, because generation, bug, nemesis, audit, review, QA, health, ship, Symphony, loop, and parallel have different failure semantics.
- Preserve explicit operator model choices and avoid changing defaults as part of policy research.

### Hypotheses / Unresolved Questions

- It is unresolved whether generation's direct Claude path should use `claude_exec` for quota and futility behavior or remain separate because it needs verbose transcript handling.
- It is unresolved whether Kimi and PI paths should be represented under one provider abstraction or remain pipeline-specific backends.
- It is unresolved whether all dangerous or bypass flags should be visible in command help, logs, or a dry-run policy report.

## Acceptance Criteria

- A checked-in backend inventory lists every live backend invocation path with file path, command binary, arguments, model source, effort source, quota behavior, and output handling.
- The inventory explicitly distinguishes direct `claude`, direct `codex`, quota-routed Codex, `kimi-cli`, `pi`, git subprocesses, tmux subprocesses, and external Symphony runtime subprocesses.
- No model default changes occur in the backend-policy research implementation.
- Generation authoring still routes non-Claude model names to Codex.
- Kimi and MiniMax aliases keep their current validation and default mapping behavior until a separate operator-approved change.
- Dangerous flags such as Codex sandbox bypass and Claude skipped permissions are visible in policy documentation and tests.
- Commands that use quota routing preserve current human-refresh and quota-exhaustion behavior.

## Verification

- `rg -n "Command::new|TokioCommand::new|run_codex_exec|run_claude_exec|run_claude_prompt|run_nemesis_backend|run_auditor|kimi-cli|pi_bin|dangerously|--yolo|quota open" src`
- `cargo test generation_author_backend_uses_codex_for_non_claude_models`
- `cargo test exec_args_contain_yolo_and_print_and_stream_json`
- `cargo test minimax_alias_defaults_to_m27_highspeed`
- Add and run tests that serialize the backend inventory and fail when a new direct backend spawn lacks policy metadata.

## Open Questions

- Should backend policy be a runtime data model, a static audit artifact, or both?
- Should quota routing be opt-out for every Codex and Claude call when accounts exist?
- Should operator-visible logs record full argv with redaction or a normalized provider policy summary?
