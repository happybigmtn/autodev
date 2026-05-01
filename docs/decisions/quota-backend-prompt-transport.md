# Decision: quota backend prompt transport

## Status

Accepted for current runtime, with an explicit migration trigger.

## Context

`auto` routes Codex-family models through the Codex backend, Claude through its
own harness, Kimi-family models through `kimi-cli`, and MiniMax-family models
through `pi`. Quota routing swaps provider credentials before invoking those
backends. That makes prompt transport a credential-safety concern: prompts may
contain repository paths, operator instructions, or private context, and backend
argv handling differs by provider.

Current local evidence:

- `src/codex_exec.rs` and `src/claude_exec.rs` use backend-specific execution
  paths that avoid treating the full prompt as a quota-account display name.
- `src/kimi_backend.rs` builds `kimi-cli --yolo --print --output-format
  stream-json -m <model> -p <prompt>`.
- `src/pi_backend.rs` owns MiniMax/PI invocation and error parsing.
- `src/backend_policy.rs` documents backend command families and dangerous
  flags, but does not prove that Kimi or PI currently support a safer
  prompt-file or stdin contract.
- `src/quota_config.rs` now validates account slugs before profile path
  construction, so provider account identity and prompt text are separate
  runtime concepts.

## Decision

Keep the current Kimi and PI prompt transport until a provider-supported
stdin or prompt-file mode is proven by local help output or primary provider
documentation.

Do not infer support from generated specs, examples, or third-party snippets.
Any migration must include a checked-in decision update that cites the exact
provider command, version, and help/documentation evidence.

The accepted policy is:

- Codex and Claude remain the preferred backends for high-context repository
  automation when prompt confidentiality matters.
- Kimi may continue using `-p <prompt>` only as an explicit operator-visible
  limitation.
- PI/MiniMax may continue using its current invocation only as an explicit
  operator-visible limitation.
- Account names are configuration slugs, not display names. Migration must keep
  slugs stable and add any richer display name as a separate field.
- Unsafe or legacy account config names must fail closed at load, save, capture,
  selection, and state update boundaries.

## Migration Trigger

Move Kimi or PI prompts off argv only when all of these are true:

- The backend supports stdin or prompt-file input in current local help output
  or primary provider documentation.
- The implementation preserves streaming JSON/error parsing behavior.
- Tests prove argv no longer contains the full prompt.
- README provider notes and backend policy docs are updated in the same change.

## Verification

Use:

```bash
rg -n "Kimi|PI|argv|stdin|prompt file|migration" docs/decisions/quota-backend-prompt-transport.md
rg -n "kimi_exec_args|-p|parse_pi_error|resolve_pi_bin" src/kimi_backend.rs src/pi_backend.rs src/codex_exec.rs
rg -n "prompt transport|Kimi|MiniMax|PI" README.md
```
