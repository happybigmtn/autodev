use console::Style;
use serde_json::Value;

use crate::codex_stream::format::{
    compact_json, extract_content_text, json_string, push_plain_line, push_styled_line,
    write_block, UsageSummary,
};

#[derive(Default)]
pub(super) struct PiRenderState {
    pub(super) tool_count: usize,
    pub(super) usage: UsageSummary,
    pub(super) last_agent_message: Option<String>,
}

pub(super) fn render_pi_stream_line(line: &str, state: &mut PiRenderState) -> String {
    let mut out = String::new();
    let trimmed = line.trim();
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        if !trimmed.is_empty() {
            push_plain_line(&mut out, trimmed);
        }
        return out;
    };

    let green = Style::new().green();
    let yellow = Style::new().yellow();
    let red = Style::new().red();
    let cyan = Style::new().cyan();
    let dim = Style::new().dim();

    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "tool_execution_start" => {
            state.tool_count += 1;
            let tool_name = value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let args = value.get("args").unwrap_or(&Value::Null);
            push_plain_line(&mut out, "");
            if tool_name == "bash" {
                push_styled_line(&mut out, &cyan, "[command]");
                write_block(
                    &mut out,
                    "   ",
                    Some(display_pi_bash_command(args)),
                    &dim,
                    2,
                );
            } else {
                push_styled_line(&mut out, &yellow, format!("[tool] {tool_name}"));
                write_block(&mut out, "   args: ", compact_json(args), &dim, 4);
            }
        }
        "tool_execution_end" => {
            let tool_name = value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let is_error = value
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let style = if is_error { &red } else { &green };
            let summary = summarize_pi_tool_result(value.get("result").unwrap_or(&Value::Null))
                .unwrap_or_else(|| {
                    if is_error {
                        format!("{tool_name} failed")
                    } else {
                        format!("{tool_name} completed")
                    }
                });
            write_block(&mut out, "   -> result: ", Some(summary), style, 6);
        }
        "message_end" => {
            let message = value.get("message").unwrap_or(&Value::Null);
            if message.get("role").and_then(Value::as_str) == Some("assistant") {
                update_usage_from_pi_message(message, state);
                if let Some(text) = extract_pi_assistant_text(message) {
                    if state.last_agent_message.as_deref() != Some(text.as_str()) {
                        write_block(&mut out, "", Some(text.clone()), &Style::new(), 8);
                        state.last_agent_message = Some(text);
                    }
                }
            }
        }
        "turn_end" => {
            if let Some(message) = value.get("message") {
                update_usage_from_pi_message(message, state);
            }
        }
        "agent_end" => {
            push_plain_line(&mut out, "");
            push_plain_line(&mut out, "========================================");
            push_styled_line(
                &mut out,
                &green,
                format!(
                    "done | Tokens: in {} out {} | Cached: {} | Tools: {}",
                    state.usage.input_tokens,
                    state.usage.output_tokens,
                    state.usage.cached_input_tokens,
                    state.tool_count
                ),
            );
        }
        "error" => {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .or_else(|| compact_json(&value))
                .unwrap_or_else(|| "unknown PI error".to_string());
            push_styled_line(&mut out, &red, format!("error: {message}"));
        }
        _ => {}
    }

    out
}

pub(super) fn render_opencode_stream_line(line: &str) -> String {
    let mut out = String::new();
    let trimmed = line.trim();
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        if !trimmed.is_empty() {
            push_plain_line(&mut out, trimmed);
        }
        return out;
    };

    let blue = Style::new().blue();
    let red = Style::new().red();
    let dim = Style::new().dim();

    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "text" => {
            let text = value
                .get("part")
                .and_then(|part| part.get("text"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string);
            write_block(&mut out, "", text, &Style::new(), 8);
        }
        "step_start" => {
            let label = json_string(&value, "message")
                .or_else(|| {
                    value
                        .get("part")
                        .and_then(|part| part.get("title"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(str::to_string)
                })
                .or_else(|| {
                    value
                        .get("part")
                        .and_then(|part| part.get("type"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty() && *text != "step-start")
                        .map(str::to_string)
                });
            if let Some(label) = label {
                push_styled_line(&mut out, &blue, format!("[step] {label}"));
            }
        }
        "step_finish" => {
            if let Some(message) = json_string(&value, "message") {
                push_styled_line(&mut out, &dim, format!("done: {message}"));
            }
        }
        "error" => {
            let detail = value
                .get("error")
                .and_then(|error| error.get("data"))
                .and_then(|data| data.get("message"))
                .and_then(Value::as_str)
                .or_else(|| {
                    value
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                })
                .or_else(|| value.get("message").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .or_else(|| compact_json(&value))
                .unwrap_or_else(|| "unknown OpenCode error".to_string());
            push_styled_line(&mut out, &red, format!("error: {detail}"));
        }
        _ => {}
    }

    out
}

fn display_pi_bash_command(args: &Value) -> String {
    args.get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "unknown command".to_string())
}

fn summarize_pi_tool_result(result: &Value) -> Option<String> {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| extract_content_text(items))
        .filter(|text| !text.trim().is_empty());
    content.or_else(|| compact_json(result))
}

fn extract_pi_assistant_text(message: &Value) -> Option<String> {
    let content = message.get("content").and_then(Value::as_array)?;
    let mut parts = Vec::new();
    for item in content {
        if item.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(text) = item
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            parts.push(text.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn update_usage_from_pi_message(message: &Value, state: &mut PiRenderState) {
    let Some(usage) = message.get("usage") else {
        return;
    };
    state.usage.input_tokens = usage
        .get("input")
        .and_then(Value::as_u64)
        .unwrap_or(state.usage.input_tokens);
    state.usage.cached_input_tokens = usage
        .get("cacheRead")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0));
    state.usage.output_tokens = usage
        .get("output")
        .and_then(Value::as_u64)
        .unwrap_or(state.usage.output_tokens);
}

#[cfg(test)]
mod tests {
    use super::{render_opencode_stream_line, render_pi_stream_line, PiRenderState};

    #[test]
    fn renders_opencode_text_events() {
        console::set_colors_enabled(false);
        let rendered = render_opencode_stream_line(
            r#"{"type":"text","part":{"text":"\n\nChunk audit complete"}}"#,
        );
        assert!(rendered.contains("Chunk audit complete"));
    }

    #[test]
    fn suppresses_unlabeled_opencode_step_start_json_noise() {
        console::set_colors_enabled(false);
        let rendered = render_opencode_stream_line(
            r#"{"type":"step_start","part":{"id":"abc","type":"step-start"},"timestamp":1}"#,
        );
        assert!(rendered.is_empty());
    }

    #[test]
    fn renders_pi_bash_tool_execution() {
        console::set_colors_enabled(false);
        let mut state = PiRenderState::default();
        let start = r#"{"type":"tool_execution_start","toolName":"bash","args":{"command":"pwd"}}"#;
        let end = r#"{"type":"tool_execution_end","toolName":"bash","result":{"content":[{"type":"text","text":"/tmp/repo\n"}]},"isError":false}"#;

        let rendered = format!(
            "{}{}",
            render_pi_stream_line(start, &mut state),
            render_pi_stream_line(end, &mut state)
        );

        assert!(rendered.contains("[command]"));
        assert!(rendered.contains("   pwd"));
        assert!(rendered.contains("   -> result: /tmp/repo"));
    }

    #[test]
    fn renders_pi_assistant_message_and_done_summary() {
        console::set_colors_enabled(false);
        let mut state = PiRenderState::default();
        let message_end = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Chunk audit complete"}],"usage":{"input":10,"output":5,"cacheRead":2,"cacheWrite":3}}}"#;
        let agent_end = r#"{"type":"agent_end"}"#;

        let rendered = format!(
            "{}{}",
            render_pi_stream_line(message_end, &mut state),
            render_pi_stream_line(agent_end, &mut state)
        );

        assert!(rendered.contains("Chunk audit complete"));
        assert!(rendered.contains("done | Tokens: in 10 out 5 | Cached: 5 | Tools: 0"));
    }
}
