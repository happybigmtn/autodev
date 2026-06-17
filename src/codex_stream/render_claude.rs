use console::Style;
use serde_json::Value;

use crate::codex_stream::format::{
    compact_json, extract_content_text, json_string, push_plain_line, push_styled_line, write_block,
};

pub(crate) const CLAUDE_FUTILITY_THRESHOLD: usize = 8;
/// Futility threshold used for passes that are read-heavy by design (code
/// review). The standard threshold is tuned for implementation loops where
/// each tool call produces a file edit; review spends more calls inspecting
/// source before emitting anything, so the 8-count trigger frequently
/// false-fires on an otherwise-healthy review run.
pub(crate) const CLAUDE_FUTILITY_THRESHOLD_REVIEW: usize = 16;
const CLAUDE_SEARCH_MISS_HINT_THRESHOLD: usize = 3;

pub(super) struct ClaudeRenderState {
    pub(super) tool_count: usize,
    pub(super) current_tool_name: Option<String>,
    pub(super) last_agent_message: Option<String>,
    pub(super) consecutive_empty_results: usize,
    pub(super) consecutive_search_misses: usize,
    pub(super) futility_detected: bool,
    /// Threshold after which consecutive empty tool results are treated as
    /// futility. Defaults to `CLAUDE_FUTILITY_THRESHOLD`; review mode raises
    /// this because reviewer runs are read-heavy by design.
    pub(super) futility_threshold: usize,
}

impl Default for ClaudeRenderState {
    fn default() -> Self {
        Self {
            tool_count: 0,
            current_tool_name: None,
            last_agent_message: None,
            consecutive_empty_results: 0,
            consecutive_search_misses: 0,
            futility_detected: false,
            futility_threshold: CLAUDE_FUTILITY_THRESHOLD,
        }
    }
}

pub(super) fn render_claude_stream_line(line: &str, state: &mut ClaudeRenderState) -> String {
    let mut out = String::new();
    let trimmed = line.trim();
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        if !trimmed.is_empty() {
            push_plain_line(&mut out, trimmed);
        }
        return out;
    };

    let green = Style::new().green();
    let red = Style::new().red();
    let yellow = Style::new().yellow();
    let dim = Style::new().dim();

    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match event_type {
        "assistant" => {
            render_claude_assistant_message(&value, state, &mut out);
        }
        "user" => {
            render_claude_tool_results(&value, &mut out, &green, &red);
            if let Some(note) = track_claude_tool_futility(&value, state) {
                push_styled_line(&mut out, &yellow, format!("note: {note}"));
            }
        }
        "result" => {
            let cost = value.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
            let duration_ms = value
                .get("duration_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let turns = value.get("num_turns").and_then(Value::as_u64).unwrap_or(0);
            let input_tokens = value
                .get("total_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output_tokens = value
                .get("total_output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            push_plain_line(&mut out, "");
            push_plain_line(&mut out, "========================================");
            push_styled_line(
                &mut out,
                &green,
                format!(
                    "done | ${cost:.2} | {turns} turns | Tokens: in {input_tokens} out {output_tokens} | Tools: {} | {:.0}s",
                    state.tool_count,
                    duration_ms as f64 / 1000.0,
                ),
            );
        }
        "error" => {
            let message = json_string(&value, "error")
                .or_else(|| json_string(&value, "message"))
                .unwrap_or_else(|| value.to_string());
            push_styled_line(&mut out, &red, format!("error: {message}"));
        }
        "system" => {
            if let Some(msg) = json_string(&value, "message") {
                push_styled_line(&mut out, &dim, format!("system: {msg}"));
            }
        }
        _ => {}
    }

    out
}

fn render_claude_assistant_message(value: &Value, state: &mut ClaudeRenderState, out: &mut String) {
    let yellow = Style::new().yellow();
    let dim = Style::new().dim();

    let content = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    let Some(blocks) = content else {
        return;
    };
    for block in blocks {
        match block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "text" => {
                if let Some(text) = block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    if state.last_agent_message.as_deref() != Some(text) {
                        write_block(out, "", Some(text.to_string()), &Style::new(), 8);
                        state.last_agent_message = Some(text.to_string());
                    }
                }
            }
            "tool_use" => {
                state.tool_count += 1;
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                state.current_tool_name = Some(name.to_string());
                push_styled_line(out, &yellow, format!("[tool] {name}"));
                write_block(
                    out,
                    "args: ",
                    block.get("input").and_then(compact_json),
                    &dim,
                    4,
                );
            }
            _ => {}
        }
    }
}

fn render_claude_tool_results(value: &Value, out: &mut String, green: &Style, red: &Style) {
    let content = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    let Some(blocks) = content else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let is_error = block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let style = if is_error { red } else { green };
        let prefix = if is_error {
            "   -> error: "
        } else {
            "   -> result: "
        };
        let text = block
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .or_else(|| {
                block
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|items| extract_content_text(items))
                    .filter(|t| !t.trim().is_empty())
            })
            .or_else(|| compact_json(block.get("content").unwrap_or(&Value::Null)));
        write_block(out, prefix, text, style, 8);
    }
}

fn track_claude_tool_futility(
    value: &Value,
    state: &mut ClaudeRenderState,
) -> Option<&'static str> {
    let blocks = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    let blocks = blocks?;
    let mut emit_search_hint = false;
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        if is_benign_search_miss(block, state.current_tool_name.as_deref()) {
            state.consecutive_search_misses += 1;
            state.consecutive_empty_results = 0;
            if state.consecutive_search_misses == CLAUDE_SEARCH_MISS_HINT_THRESHOLD {
                emit_search_hint = true;
            }
            continue;
        }
        state.consecutive_search_misses = 0;
        if is_empty_tool_result(block) {
            state.consecutive_empty_results += 1;
        } else {
            state.consecutive_empty_results = 0;
        }
    }
    if state.consecutive_empty_results >= state.futility_threshold {
        state.futility_detected = true;
    }
    if emit_search_hint {
        Some(
            "repeated empty search results: inspect the containing enum/struct/module, nearby tests, or a focused compiler error before retrying the same search",
        )
    } else {
        None
    }
}

fn is_empty_tool_result(block: &Value) -> bool {
    if block
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    match block.get("content") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => {
            let t = s.trim();
            t.is_empty() || t.starts_with("No matches found") || t.starts_with("No files found")
        }
        Some(Value::Array(arr)) => arr.iter().all(|item| {
            item.get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .is_none_or(|t| {
                    t.is_empty()
                        || t.starts_with("No matches found")
                        || t.starts_with("No files found")
                })
        }),
        _ => false,
    }
}

fn is_benign_search_miss(block: &Value, current_tool_name: Option<&str>) -> bool {
    if !current_tool_name.is_some_and(is_search_tool_name) {
        return false;
    }
    match block.get("content") {
        Some(Value::String(s)) => is_search_miss_text(s),
        Some(Value::Array(arr)) => arr.iter().all(|item| {
            item.get("text")
                .and_then(Value::as_str)
                .is_some_and(is_search_miss_text)
        }),
        _ => false,
    }
}

fn is_search_tool_name(name: &str) -> bool {
    matches!(
        name,
        "Grep" | "Glob" | "LS" | "Find" | "Search" | "search_code"
    )
}

fn is_search_miss_text(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("No matches found") || trimmed.starts_with("No files found")
}

#[cfg(test)]
mod tests {
    use super::{render_claude_stream_line, ClaudeRenderState, CLAUDE_FUTILITY_THRESHOLD};

    #[test]
    fn renders_claude_assistant_text_and_tool_use() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState::default();
        let event = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Reading the file now."},{"type":"tool_use","id":"tu_1","name":"Read","input":{"path":"/tmp/foo.rs"}}]}}"#;

        let rendered = render_claude_stream_line(event, &mut state);

        assert!(rendered.contains("Reading the file now."));
        assert!(rendered.contains("[tool] Read"));
        assert!(rendered.contains("/tmp/foo.rs"));
        assert_eq!(state.tool_count, 1);
    }

    #[test]
    fn renders_claude_tool_result() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState::default();
        let event = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"fn main() {}","is_error":false}]}}"#;

        let rendered = render_claude_stream_line(event, &mut state);

        assert!(rendered.contains("-> result: fn main() {}"));
    }

    #[test]
    fn renders_claude_tool_error() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState::default();
        let event = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"file not found","is_error":true}]}}"#;

        let rendered = render_claude_stream_line(event, &mut state);

        assert!(rendered.contains("-> error: file not found"));
    }

    #[test]
    fn renders_claude_result_summary() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState {
            tool_count: 5,
            ..Default::default()
        };
        let event = r#"{"type":"result","cost_usd":0.42,"duration_ms":30000,"num_turns":3,"total_input_tokens":10000,"total_output_tokens":2000}"#;

        let rendered = render_claude_stream_line(event, &mut state);

        assert!(rendered.contains("done"));
        assert!(rendered.contains("$0.42"));
        assert!(rendered.contains("3 turns"));
        assert!(rendered.contains("in 10000 out 2000"));
        assert!(rendered.contains("Tools: 5"));
        assert!(rendered.contains("30s"));
    }

    #[test]
    fn renders_claude_error_event() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState::default();
        let event = r#"{"type":"error","error":"rate limit exceeded"}"#;

        let rendered = render_claude_stream_line(event, &mut state);

        assert!(rendered.contains("error: rate limit exceeded"));
    }

    #[test]
    fn futility_detected_after_consecutive_empty_results() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState::default();

        let empty_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"","is_error":false}]}}"#;

        for _ in 0..CLAUDE_FUTILITY_THRESHOLD - 1 {
            render_claude_stream_line(empty_result, &mut state);
            assert!(!state.futility_detected);
        }
        render_claude_stream_line(empty_result, &mut state);
        assert!(state.futility_detected);
        assert_eq!(state.consecutive_empty_results, CLAUDE_FUTILITY_THRESHOLD);
    }

    #[test]
    fn substantive_result_resets_futility_counter() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState::default();

        let empty_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"","is_error":false}]}}"#;
        let good_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_2","content":"fn main() { println!(\"hello\"); }","is_error":false}]}}"#;

        for _ in 0..5 {
            render_claude_stream_line(empty_result, &mut state);
        }
        assert_eq!(state.consecutive_empty_results, 5);

        render_claude_stream_line(good_result, &mut state);
        assert_eq!(state.consecutive_empty_results, 0);
        assert!(!state.futility_detected);
    }

    #[test]
    fn error_tool_results_count_toward_futility() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState::default();

        let error_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"Argument list too long","is_error":true}]}}"#;

        for _ in 0..CLAUDE_FUTILITY_THRESHOLD {
            render_claude_stream_line(error_result, &mut state);
        }
        assert!(state.futility_detected);
    }

    #[test]
    fn benign_search_misses_do_not_count_toward_futility() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState {
            current_tool_name: Some("Grep".to_string()),
            ..Default::default()
        };

        let empty_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"No matches found","is_error":false}]}}"#;

        for _ in 0..CLAUDE_FUTILITY_THRESHOLD + 2 {
            render_claude_stream_line(empty_result, &mut state);
        }

        assert!(!state.futility_detected);
        assert_eq!(state.consecutive_empty_results, 0);
        assert_eq!(
            state.consecutive_search_misses,
            CLAUDE_FUTILITY_THRESHOLD + 2
        );
    }

    #[test]
    fn repeated_search_misses_emit_recovery_hint() {
        console::set_colors_enabled(false);
        let mut state = ClaudeRenderState {
            current_tool_name: Some("Grep".to_string()),
            ..Default::default()
        };

        let empty_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"No matches found","is_error":false}]}}"#;

        let first = render_claude_stream_line(empty_result, &mut state);
        let second = render_claude_stream_line(empty_result, &mut state);
        let third = render_claude_stream_line(empty_result, &mut state);

        assert!(!first.contains("repeated empty search results"));
        assert!(!second.contains("repeated empty search results"));
        assert!(third.contains("repeated empty search results"));
    }
}
