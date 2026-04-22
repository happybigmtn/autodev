//! `kimi-cli` backend for `auto bug` and `auto nemesis`.
//!
//! `kimi-cli` is Moonshot's first-party CLI for Kimi k2.6. It replaces the
//! `pi` (fabro) binary path we previously used for Kimi phases. We invoke it
//! as `kimi-cli --yolo --print --output-format stream-json` so the pipeline
//! gets one structured JSON line per turn (easy to parse) and sandbox
//! approvals are auto-granted (`--yolo` = YES-OverLock-You — auto-approve all
//! actions, matches the `pi` behaviour we used to rely on).
//!
//! The module intentionally stays tiny: it resolves the binary, assembles
//! the command, and extracts the final assistant text from the stream. The
//! surrounding pipeline still owns streaming + timeout + stderr capture.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Default Kimi model id passed to `kimi-cli -m`. `kimi-cli` 1.22 expects
/// the full provider-qualified name from its `~/.kimi/config.toml` — the
/// short ids like `k2.6` that we use in CLI flags and logs must be mapped
/// back to this form or kimi-cli will bail with `LLM not set`.
pub(crate) const KIMI_CLI_DEFAULT_MODEL: &str = "kimi-code/kimi-for-coding";

/// Env override for the kimi-cli model id. Operators who pin a non-default
/// `~/.kimi/config.toml` model can set `FABRO_KIMI_CLI_MODEL=<full id>` and
/// both `auto bug` and `auto nemesis` pick it up.
const KIMI_CLI_MODEL_ENV: &str = "FABRO_KIMI_CLI_MODEL";

/// Map a user-facing short id (`k2.6`, `kimi`, `kimi-coding/k2p6`, …) onto
/// the provider-qualified id kimi-cli expects. Falls back to the configured
/// default when the input doesn't match a known alias.
pub(crate) fn resolve_kimi_cli_model(short_id: &str) -> String {
    if let Ok(explicit) = std::env::var(KIMI_CLI_MODEL_ENV) {
        if !explicit.trim().is_empty() {
            return explicit.trim().to_string();
        }
    }
    let lower = short_id.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return KIMI_CLI_DEFAULT_MODEL.to_string();
    }
    // If the caller already passed a provider-qualified id (contains `/`),
    // trust it — that's the shape kimi-cli reads from `~/.kimi/config.toml`.
    if short_id.contains('/') {
        return short_id.trim().to_string();
    }
    match lower.as_str() {
        "k2.6"
        | "kimi"
        | "kimi-2.6"
        | "kimi-k2.6"
        | "kimi-k2.6-code-preview"
        | "kimi-2.6-code-preview"
        | "k2p6"
        | "kimi-for-coding" => KIMI_CLI_DEFAULT_MODEL.to_string(),
        "k2.5" | "kimi-2.5" | "kimi-k2.5" | "k2p5" => "kimi-code/kimi-for-coding-k2p5".to_string(),
        _ => KIMI_CLI_DEFAULT_MODEL.to_string(),
    }
}

/// Resolve the `kimi-cli` binary. If the caller passed an absolute path or a
/// non-default `kimi-bin` override, honour it; otherwise discover via
/// `$FABRO_KIMI_CLI_BIN` → `~/.npm-global/bin/kimi-cli` → `~/.local/bin/kimi-cli`
/// → fallback to bare `kimi-cli` (lets PATH resolution decide).
pub(crate) fn resolve_kimi_bin(configured: &Path) -> PathBuf {
    if configured != Path::new("kimi-cli") {
        return configured.to_path_buf();
    }
    if let Some(path) = std::env::var_os("FABRO_KIMI_CLI_BIN").map(PathBuf::from) {
        return path;
    }
    if let Some(home) = std::env::var_os("HOME") {
        for bundled in [
            PathBuf::from(&home)
                .join(".npm-global")
                .join("bin")
                .join("kimi-cli"),
            PathBuf::from(&home)
                .join(".local")
                .join("bin")
                .join("kimi-cli"),
        ] {
            if bundled.exists() {
                return bundled;
            }
        }
    }
    PathBuf::from("kimi-cli")
}

/// Build the argv for `kimi-cli --yolo --print --output-format stream-json -m <model> -p <prompt>`.
/// Caller spawns the command and wires stdout through `parse_kimi_stream_line`
/// for progress rendering + final-text extraction.
#[must_use]
pub(crate) fn kimi_exec_args(model: &str, thinking: &str, prompt: &str) -> Vec<String> {
    let resolved_model = resolve_kimi_cli_model(model);
    let mut args = vec![
        "--yolo".to_string(),
        "--print".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "-m".to_string(),
        resolved_model,
    ];
    // `thinking` maps onto kimi-cli's `--thinking` toggle. Kimi k2.6 currently
    // accepts "on" (default) or "off"; we map our effort strings conservatively.
    let thinking_flag = match thinking.trim().to_ascii_lowercase().as_str() {
        "none" | "off" | "minimal" | "low" => "--no-thinking",
        _ => "--thinking",
    };
    args.push(thinking_flag.to_string());
    args.push("-p".to_string());
    args.push(prompt.to_string());
    args
}

/// Extract the final assistant TextPart from a single `kimi-cli --output-format
/// stream-json` line. Returns None for `think` / tool-call frames so callers
/// can concatenate text across frames.
pub(crate) fn extract_final_text(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    let content = value.get("content")?.as_array()?;
    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(part) = block.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(part);
            }
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Decode a kimi-cli-reported error from a stream-json payload. kimi-cli
/// emits `{"type":"error", ...}` frames on auth / quota / context-window
/// failures; map them to a terse operator message.
///
/// Also detects the plain-text "LLM not set" failure that kimi-cli 1.22
/// emits as a bare string on stdout (exit 0!) when the requested model id
/// isn't present in `~/.kimi/config.toml`. That failure mode is silent from
/// the caller's perspective, so we surface it explicitly.
pub(crate) fn parse_kimi_error(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed == "LLM not set" || trimmed.ends_with("\nLLM not set") {
        return Some(
            "kimi-cli reported `LLM not set` — the configured model id is not in \
             ~/.kimi/config.toml. Run `kimi-cli login`, check `default_model`, or \
             set FABRO_KIMI_CLI_MODEL=<full model id>."
                .to_string(),
        );
    }
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) == Some("error") {
            if let Some(message) = event.get("message").and_then(Value::as_str) {
                if !message.trim().is_empty() {
                    return Some(message.trim().to_string());
                }
            }
            if let Some(message) = event.pointer("/error/message").and_then(Value::as_str) {
                if !message.trim().is_empty() {
                    return Some(message.trim().to_string());
                }
            }
        }
    }
    None
}

/// Preflight that the kimi-cli binary + model combination actually produces
/// usable output. Run once at the top of `auto bug` / `auto nemesis` so we
/// fail in under a second if the operator hasn't run `kimi-cli login` or
/// has the wrong model id, instead of burning 119 chunks worth of fallback
/// attempts.
pub(crate) fn preflight_kimi_cli(kimi_bin: &std::path::Path, model: &str) -> Result<()> {
    let resolved = resolve_kimi_cli_model(model);
    let output = std::process::Command::new(kimi_bin)
        .args([
            "--yolo",
            "--print",
            "--output-format",
            "text",
            "--final-message-only",
            "-m",
            &resolved,
            "--no-thinking",
            "-p",
            "reply with exactly one word: ok",
        ])
        .output()
        .with_context(|| {
            format!(
                "failed to invoke `{} --print -p` for kimi-cli preflight",
                kimi_bin.display()
            )
        })?;
    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|c: i32| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        bail!(
            "kimi-cli preflight failed with status {}: {}",
            code,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(detail) = parse_kimi_error(&stdout) {
        bail!("kimi-cli preflight failed: {detail}");
    }
    if stdout.trim().is_empty() {
        bail!(
            "kimi-cli preflight produced empty output; aborting before running the pipeline. \
             Model resolved to `{resolved}`; check `kimi-cli info` and `~/.kimi/config.toml`."
        );
    }
    Ok(())
}

/// Sanity-check a user-supplied model string. Rejects patterns that look like
/// `pi`/MiniMax aliases so an accidental leftover config doesn't silently
/// resolve to the legacy path.
#[allow(dead_code)]
pub(crate) fn validate_kimi_model(model: &str) -> Result<()> {
    let lower = model.trim().to_ascii_lowercase();
    if lower.contains("minimax") {
        bail!(
            "model `{model}` looks like a MiniMax alias; auto bug + auto nemesis no longer \
             route through MiniMax. Use a Kimi model (e.g. `k2.6`) or pass `--no-use-kimi-cli` \
             with a Codex model."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_args_contain_yolo_and_print_and_stream_json() {
        let args = kimi_exec_args("k2.6", "high", "audit this");
        assert!(args.contains(&"--yolo".to_string()));
        assert!(args.contains(&"--print".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"--thinking".to_string()));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-m" && w[1] == KIMI_CLI_DEFAULT_MODEL),
            "short id `k2.6` must be resolved to the provider-qualified default model"
        );
        assert!(args.last().map(String::as_str) == Some("audit this"));
    }

    #[test]
    fn resolve_kimi_cli_model_maps_short_ids_to_full_provider_qualified_name() {
        assert_eq!(resolve_kimi_cli_model("k2.6"), KIMI_CLI_DEFAULT_MODEL);
        assert_eq!(resolve_kimi_cli_model("kimi"), KIMI_CLI_DEFAULT_MODEL);
        assert_eq!(
            resolve_kimi_cli_model("kimi-code/kimi-for-coding"),
            "kimi-code/kimi-for-coding",
            "caller-supplied provider-qualified names pass through unchanged"
        );
    }

    #[test]
    fn parse_kimi_error_detects_llm_not_set() {
        let stdout = "LLM not set\n";
        let err = parse_kimi_error(stdout).expect("LLM not set must surface");
        assert!(err.contains("LLM not set"));
        assert!(err.contains("FABRO_KIMI_CLI_MODEL"));
    }

    #[test]
    fn exec_args_disable_thinking_for_low_effort() {
        let args = kimi_exec_args("k2.6", "minimal", "prompt");
        assert!(args.contains(&"--no-thinking".to_string()));
        assert!(!args.contains(&"--thinking".to_string()));
    }

    #[test]
    fn extract_final_text_concatenates_text_parts_skipping_think() {
        let line = r#"{"role":"assistant","content":[{"type":"think","think":"thinking..."},{"type":"text","text":"Hello"},{"type":"text","text":"World"}]}"#;
        assert_eq!(extract_final_text(line), Some("Hello\nWorld".to_string()));
    }

    #[test]
    fn extract_final_text_returns_none_for_think_only_frames() {
        let line = r#"{"role":"assistant","content":[{"type":"think","think":"..."}]}"#;
        assert_eq!(extract_final_text(line), None);
    }

    #[test]
    fn parse_kimi_error_reads_top_level_error_message() {
        let stdout = r#"{"type":"error","message":"quota exceeded"}"#;
        assert_eq!(parse_kimi_error(stdout).as_deref(), Some("quota exceeded"));
    }

    #[test]
    fn parse_kimi_error_reads_nested_api_error() {
        let stdout = r#"{"type":"error","error":{"message":"invalid api key"}}"#;
        assert_eq!(parse_kimi_error(stdout).as_deref(), Some("invalid api key"));
    }

    #[test]
    fn validate_kimi_model_refuses_minimax_aliases() {
        assert!(validate_kimi_model("minimax/MiniMax-M2.7-highspeed").is_err());
        assert!(validate_kimi_model("MINIMAX-M2.5").is_err());
    }

    #[test]
    fn validate_kimi_model_accepts_kimi_aliases_and_bare_models() {
        assert!(validate_kimi_model("k2.6").is_ok());
        assert!(validate_kimi_model("kimi-coding/k2p6").is_ok());
        assert!(validate_kimi_model("kimi").is_ok());
    }
}
