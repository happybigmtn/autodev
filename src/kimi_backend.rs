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

use anyhow::{bail, Result};
use serde_json::Value;

/// Default Kimi model id passed to `kimi-cli -m`. `kimi-cli` reads an
/// overridable config at `~/.kimi/config.toml`; we pass a value that both
/// cloud and self-hosted setups recognise.
pub(crate) const KIMI_CLI_DEFAULT_MODEL: &str = "k2.6";

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
    let mut args = vec![
        "--yolo".to_string(),
        "--print".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "-m".to_string(),
        model.to_string(),
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
    if text.is_empty() { None } else { Some(text) }
}

/// Decode a kimi-cli-reported error from a stream-json payload. kimi-cli
/// emits `{"type":"error", ...}` frames on auth / quota / context-window
/// failures; map them to a terse operator message.
pub(crate) fn parse_kimi_error(stdout: &str) -> Option<String> {
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
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "k2.6"));
        assert!(args.last().map(String::as_str) == Some("audit this"));
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
        assert_eq!(
            parse_kimi_error(stdout).as_deref(),
            Some("invalid api key")
        );
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
