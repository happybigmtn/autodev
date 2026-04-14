use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PiProvider {
    Kimi,
    Minimax,
}

impl PiProvider {
    pub(crate) fn provider_label(self) -> &'static str {
        match self {
            Self::Kimi => "pi-kimi",
            Self::Minimax => "pi-minimax",
        }
    }

    pub(crate) fn default_model(self) -> &'static str {
        match self {
            Self::Kimi => "kimi-coding/k2p6",
            Self::Minimax => "minimax/MiniMax-M2.7-highspeed",
        }
    }

    pub(crate) fn detect(model: &str) -> Option<Self> {
        let normalized = model.trim().to_ascii_lowercase();
        if normalized.contains("kimi") {
            return Some(Self::Kimi);
        }
        if normalized.contains("minimax") {
            return Some(Self::Minimax);
        }
        None
    }

    pub(crate) fn resolve_model(self, requested_model: &str, codex_default_model: &str) -> String {
        let normalized = requested_model.trim();
        if normalized.is_empty() || normalized == codex_default_model {
            return self.default_model().to_string();
        }
        if normalized.contains('/') {
            return normalized.to_string();
        }
        match self {
            Self::Kimi => {
                let model = match normalized {
                    "kimi" | "kimi-k2.6" | "kimi-2.6" | "kimi-for-coding" => "k2p6",
                    "kimi-k2.6-code-preview" | "kimi-2.6-code-preview" | "k2.6-code-preview" => {
                        "k2p6"
                    }
                    "kimi-k2.5" | "kimi-2.5" => "k2p5",
                    "kimi-k2-thinking" => "kimi-k2-thinking",
                    other => other,
                };
                format!("kimi-coding/{model}")
            }
            Self::Minimax => format!("minimax/{}", map_minimax_model_name(normalized)),
        }
    }
}

pub(crate) fn resolve_pi_bin(configured: &Path) -> PathBuf {
    if configured != Path::new("pi") {
        return configured.to_path_buf();
    }
    if let Some(path) = std::env::var_os("FABRO_PI_BIN").map(PathBuf::from) {
        return path;
    }
    if let Some(home) = std::env::var_os("HOME") {
        for bundled in [
            PathBuf::from(&home)
                .join(".npm-global")
                .join("bin")
                .join("pi"),
            PathBuf::from(&home).join(".local").join("bin").join("pi"),
        ] {
            if bundled.exists() {
                return bundled;
            }
        }
    }
    PathBuf::from("pi")
}

pub(crate) fn parse_pi_error(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(detail) = parse_pi_error_event(&event) {
            return Some(detail);
        }
    }
    None
}

fn parse_pi_error_event(event: &Value) -> Option<String> {
    if let Some(message) = json_string_field(event, "errorMessage") {
        return Some(message);
    }

    if event.get("type").and_then(Value::as_str) == Some("error") {
        if let Some(message) = json_string_field(event, "message").or_else(|| {
            event
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(str::to_string)
        }) {
            return Some(message);
        }
        return serde_json::to_string(event)
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
    }

    if event.get("stopReason").and_then(Value::as_str) == Some("error") {
        if let Some(message) = event
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(str::to_string)
        {
            return Some(message);
        }
        return serde_json::to_string(event)
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
    }

    for key in ["message", "assistantMessageEvent", "partial"] {
        if let Some(detail) = event.get(key).and_then(parse_pi_error_event) {
            return Some(detail);
        }
    }

    None
}

fn json_string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
}

fn map_minimax_model_name(model: &str) -> String {
    match model {
        "minimax" => "MiniMax-M2.7-highspeed".to_string(),
        "minimax-m2" => "MiniMax-M2".to_string(),
        "minimax-m2.1" => "MiniMax-M2.1".to_string(),
        "minimax-m2.1-highspeed" => "MiniMax-M2.1-highspeed".to_string(),
        "minimax-m2.5" => "MiniMax-M2.5".to_string(),
        "minimax-m2.5-highspeed" => "MiniMax-M2.5-highspeed".to_string(),
        "minimax-m2.7" => "MiniMax-M2.7".to_string(),
        "minimax-m2.7-highspeed" => "MiniMax-M2.7-highspeed".to_string(),
        other if other.starts_with("MiniMax-") => other.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_pi_error, PiProvider};

    #[test]
    fn minimax_alias_defaults_to_m27_highspeed() {
        assert_eq!(
            PiProvider::Minimax.resolve_model("minimax", "gpt-5.4"),
            "minimax/MiniMax-M2.7-highspeed"
        );
    }

    #[test]
    fn kimi_alias_defaults_to_k2p6() {
        assert_eq!(
            PiProvider::Kimi.resolve_model("kimi", "gpt-5.4"),
            "kimi-coding/k2p6"
        );
    }

    #[test]
    fn kimi_k25_aliases_still_resolve_to_legacy_k2p5() {
        assert_eq!(
            PiProvider::Kimi.resolve_model("kimi-k2.5", "gpt-5.4"),
            "kimi-coding/k2p5"
        );
    }

    #[test]
    fn kimi_k26_preview_aliases_resolve_to_k2p6() {
        assert_eq!(
            PiProvider::Kimi.resolve_model("kimi-k2.6-code-preview", "gpt-5.4"),
            "kimi-coding/k2p6"
        );
    }

    #[test]
    fn parse_pi_error_reads_top_level_error_message() {
        let stdout = r#"{"role":"assistant","stopReason":"error","errorMessage":"context window exceeds limit"}"#;
        assert_eq!(
            parse_pi_error(stdout).as_deref(),
            Some("context window exceeds limit")
        );
    }

    #[test]
    fn parse_pi_error_reads_nested_message_error() {
        let stdout = r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"provider rejected request"}}"#;
        assert_eq!(
            parse_pi_error(stdout).as_deref(),
            Some("provider rejected request")
        );
    }

    #[test]
    fn parse_pi_error_reads_api_error_payload() {
        let stdout = r#"{"type":"error","error":{"type":"invalid_request_error","message":"invalid params"}}"#;
        assert_eq!(parse_pi_error(stdout).as_deref(), Some("invalid params"));
    }
}
