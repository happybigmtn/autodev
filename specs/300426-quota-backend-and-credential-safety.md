# Specification: Quota Backend And Credential Safety

## Objective

Harden quota-aware backend execution so account profiles, credential swapping, backend prompts, and retry behavior cannot corrupt credentials or duplicate model-backed side effects.

## Source Of Truth

- Runtime owners: `src/quota_config.rs`, `src/quota_accounts.rs`, `src/quota_exec.rs`, `src/quota_state.rs`, `src/quota_usage.rs`, `src/quota_status.rs`, `src/quota_selector.rs`, `src/quota_patterns.rs`, `src/backend_policy.rs`, `src/codex_exec.rs`, `src/claude_exec.rs`, `src/kimi_backend.rs`, `src/pi_backend.rs`.
- CLI owners: `src/main.rs` `QuotaArgs`, `QuotaSubcommand`, and backend/model flags on model-backed commands.
- UI consumers: `auto quota status`, `auto quota accounts add/list/remove/capture`, quota-router stderr lines, backend command logs, README provider notes.
- Generated artifacts: quota config TOML under the platform config dir, quota state JSON, captured profile directories, `.auto/logs/*`, backend stdout/stderr logs.
- Retired/superseded surfaces: raw account names as path components, retry-after-progress without recovery, unsanitized provider errors in operator output, and unsafe argv prompt paths where a stdin/file protocol is supported.

## Evidence Status

Verified facts grounded in code or primary repo files:

- `Provider` currently covers `Claude` and `Codex`, verified by `rg -n "enum Provider|Claude|Codex" src/quota_config.rs`.
- `QuotaConfig::profile_dir` constructs profile paths by joining `profiles_dir()` with `format!("{}-{name}", provider.label())`, verified by `rg -n "profile_dir|format!\\(\"\\{\\}-\\{name\\}\"" src/quota_config.rs`.
- `QuotaConfig::save` serializes TOML and writes with `write_0o600_if_unix`, verified by `rg -n "fn save|to_string_pretty|write_0o600_if_unix" src/quota_config.rs`.
- Credential capture rejects symlinked credential paths and writes copied files owner-only, verified by `rg -n "refusing to copy symlinked credential path|copy_credential_file|write_0o600_if_unix" src/quota_config.rs`.
- `run_with_quota` detects progress with `quota_output_has_agent_progress` but still continues to the next account on quota exhaustion, verified by `rg -n "quota_output_has_agent_progress|trying next|continue" src/quota_exec.rs`.
- `kimi_exec_args` passes prompts through `-p <prompt>`, verified by `rg -n "kimi_exec_args|-p|prompt.to_string" src/kimi_backend.rs`.
- PI/Kimi/Minimax model aliases are still present in backend routing, verified by `rg -n "PiProvider|kimi|Minimax|resolve_model" src/pi_backend.rs src/codex_exec.rs`.

Recommendations for the intended system:

- Introduce a single account-slug validator before config writes, profile path construction, state updates, account capture, account select, and account removal.
- Canonicalize and enforce that every profile path stays under `QuotaConfig::profiles_dir()`.
- Stop failover after detected worker progress unless the operator explicitly resumes a recovery artifact.
- Route all displayed provider errors through one sanitizer before printing or storing in logs meant for operators.
- Research Kimi/PI prompt delivery and use stdin or prompt files where supported; otherwise require an explicit unsafe argv limitation note.

Hypotheses / unresolved questions:

- Kimi CLI stdin or file prompt support is not verified in this repo; do not promise it before checking primary Kimi CLI documentation or local `kimi-cli --help`.
- PI prompt delivery capabilities are not verified; treat argv avoidance as a research task until proven.
- Exact state-file format changes should preserve backward compatibility for existing local quota configs.

## Runtime Contract

- `quota_config` owns account identity validation, profile path containment, profile capture, and owner-only config/profile writes.
- `quota_state` owns leases, cooldowns, selected-account state, and atomic persistence.
- `quota_exec` owns credential swap locks, provider selection, failover, credential restore, and retry-after-progress policy.
- Backend wrappers own command construction, but they must consume sanitized prompts and errors through shared helpers when available.
- If an account name is unsafe, if a profile path escapes the profile root, if a credential source is a symlink, or if quota exhaustion is detected after agent progress, execution must fail closed before another account is tried.

## UI Contract

- `auto quota` output must distinguish config errors, missing credentials, exhausted accounts, unavailable auth, and retry-after-progress stop conditions.
- UI text must render account slugs from validated config, not infer profile paths independently.
- Provider error text must be sanitized before terminal display and before durable operator-facing logs.
- README provider notes must describe only backend behavior that runtime code actually supports.
- Production UI/presentation must not duplicate runtime-owned catalogs, constants, risk classifications, settlement math, eligibility rules, or fixture fallback truth; when such concepts apply, it must render the owning runtime/gate result.

## Generated Artifacts

- Quota config TOML from `QuotaConfig::save`.
- Quota state file from `QuotaState::save`.
- Captured profile directories under `QuotaConfig::profiles_dir()`.
- `.auto/logs/*`, backend stdout/stderr logs, and quota-router stderr messages.
- Tests may generate temporary config/profile roots by overriding config/home environment where the existing test harness supports it.

## Fixture Policy

- Tests may create fake auth files and fake quota state in temp directories.
- Production code must never import fake provider credentials, fixture account names, or captured test profiles.
- Fixture provider errors must exercise the sanitizer but cannot be used as a claim about live provider wording unless sourced from primary provider output.

## Retired / Superseded Surfaces

- Retire raw `String` account names at path construction call sites in favor of a validated account slug type or helper.
- Retire retry-after-progress log-only behavior that says progress was detected and then tries the next account.
- Tombstone docs that recommend arbitrary account display names if the runtime slug policy becomes strict ASCII.

## Acceptance Criteria

- Unsafe account names such as `../x`, absolute paths, empty names, names with path separators, and non-slug punctuation fail before any config, state, or profile filesystem mutation.
- `QuotaConfig::profile_dir` or its replacement proves the resolved path remains under `profiles_dir()`.
- Config and state writes are owner-only and atomic enough that interrupted writes do not leave partial TOML or JSON accepted as valid current truth.
- Quota exhaustion after detected worker progress stops the run and prints a recovery message instead of retrying another account.
- A non-progress quota exhaustion may still fail over to another eligible account.
- Provider errors shown by quota status, quota execution, Kimi, PI, Claude, and Codex paths pass through shared sanitization.
- Kimi/PI prompt delivery is either moved off argv with test proof or documented as an explicit production limitation with an operator acknowledgement path.

## Verification

- `cargo test quota_config::tests`
- `cargo test quota_state::tests`
- `cargo test quota_selector::tests`
- `cargo test quota_patterns::tests`
- `cargo test quota_exec::tests`
- `cargo test kimi_backend::tests`
- `cargo test pi_backend::tests`
- `rg -n "profile_dir|quota_output_has_agent_progress|trying next|write_0o600_if_unix|refusing to copy symlinked" src/quota_config.rs src/quota_exec.rs`
- `rg -n "sanitize|error" src/quota_status.rs src/quota_usage.rs src/kimi_backend.rs src/pi_backend.rs src/codex_exec.rs src/claude_exec.rs`

## Review And Closeout

- A reviewer creates a temp quota config and proves unsafe names fail with no created profile directory and no added config entry.
- A reviewer runs the progress-detected failover fixture and verifies the second account is not selected.
- Grep proof must show all profile path construction flows use the validated helper, not ad hoc `join(name)` or string interpolation.
- Closeout records the Kimi/PI prompt-delivery decision and cites the command/help output or primary documentation used for that decision.

## Open Questions

- Should the account slug type preserve a separate display label for human-friendly names?
- Should retry-after-progress write a durable recovery marker under `.auto/`?
- Which provider error fields are safe to display verbatim after sanitizer redaction?
