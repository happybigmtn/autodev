use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Write};

use anyhow::{Context, Result};
use console::Style;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::util::clip_line_for_display;

#[derive(Default)]
struct CodexRenderState {
    tool_count: usize,
    exec_calls: HashMap<String, ExecCallState>,
    usage: UsageSummary,
    last_agent_message: Option<String>,
}

#[derive(Default)]
struct ExecCallState {
    output: String,
}

#[derive(Default)]
struct UsageSummary {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
}

pub(crate) async fn capture_codex_output<R>(stream: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream).lines();
    let mut raw = String::new();
    let mut state = CodexRenderState::default();
    while let Some(line) = reader
        .next_line()
        .await
        .context("failed reading Codex JSON stream")?
    {
        raw.push_str(&line);
        raw.push('\n');
        let rendered = render_codex_stream_line(&line, &mut state);
        if !rendered.is_empty() {
            print!("{rendered}");
            let _ = io::stdout().flush();
        }
    }
    Ok(raw)
}

pub(crate) async fn stream_codex_output<R>(stream: R) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    capture_codex_output(stream).await?;
    Ok(())
}

pub(crate) async fn capture_opencode_output<R>(stream: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream).lines();
    let mut raw = String::new();
    while let Some(line) = reader
        .next_line()
        .await
        .context("failed reading OpenCode JSON stream")?
    {
        raw.push_str(&line);
        raw.push('\n');
        let rendered = render_opencode_stream_line(&line);
        if !rendered.is_empty() {
            print!("{rendered}");
            let _ = io::stdout().flush();
        }
    }
    Ok(raw)
}

fn render_codex_stream_line(line: &str, state: &mut CodexRenderState) -> String {
    let mut out = String::new();
    let trimmed = line.trim();
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        if !trimmed.is_empty() {
            push_plain_line(&mut out, trimmed);
        }
        return out;
    };

    let green = Style::new().green();
    let blue = Style::new().blue();
    let yellow = Style::new().yellow();
    let red = Style::new().red();
    let cyan = Style::new().cyan();
    let dim = Style::new().dim();

    let event = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event {
        "task_started" | "turn.started" => {}
        "task_complete" | "completed" | "turn.completed" => {
            if let Some(message) = value
                .get("last_agent_message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(str::to_string)
            {
                if state.last_agent_message.as_deref() != Some(message.as_str()) {
                    write_block(&mut out, "", Some(message.clone()), &Style::new(), 8);
                    state.last_agent_message = Some(message);
                }
            }
            if let Some(usage) = value.get("usage") {
                update_usage_from_value(usage, state);
            }
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
        "item.started" | "item_started" => render_legacy_item_started(&value, state, &mut out),
        "item.completed" | "item_completed" => {
            render_legacy_item_completed(&value, state, &mut out)
        }
        "agent_reasoning" | "reasoning" => {
            write_block(&mut out, "thinking: ", json_string(&value, "text"), &dim, 3);
        }
        "agent_message" | "message" | "assistant" => {
            let text = agent_message_text(&value);
            if let Some(message) = text.clone() {
                state.last_agent_message = Some(message);
            }
            write_block(&mut out, "", text, &Style::new(), 8);
        }
        "tool.call" | "tool_use" => {
            state.tool_count += 1;
            let name = value.get("name").and_then(Value::as_str).unwrap_or("tool");
            push_styled_line(&mut out, &yellow, format!("[tool] {name}"));
            write_block(
                &mut out,
                "args: ",
                value
                    .get("input")
                    .or_else(|| value.get("arguments"))
                    .and_then(compact_json),
                &dim,
                4,
            );
            write_block(
                &mut out,
                "command: ",
                json_string(&value, "command"),
                &cyan,
                2,
            );
        }
        "exec_command_begin" => {
            state.tool_count += 1;
            let call_id = json_string(&value, "call_id").unwrap_or_default();
            state.exec_calls.entry(call_id).or_default();
            push_plain_line(&mut out, "");
            push_styled_line(&mut out, &cyan, "[command]");
            write_block(&mut out, "   ", Some(display_exec_command(&value)), &dim, 2);
            if let Some(cwd) = json_string(&value, "cwd") {
                write_block(&mut out, "   cwd: ", Some(cwd), &dim, 1);
            }
        }
        "exec_command_output_delta" => {
            if let Some(call_id) = value.get("call_id").and_then(Value::as_str) {
                let chunk = value
                    .get("chunk")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                state
                    .exec_calls
                    .entry(call_id.to_string())
                    .or_default()
                    .output
                    .push_str(chunk);
            }
        }
        "exec_command_end" => {
            let call_id = value
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let buffered_output = state
                .exec_calls
                .remove(&call_id)
                .map(|call| call.output)
                .unwrap_or_default();
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let is_success = status == "completed"
                && value
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .unwrap_or_default()
                    == 0;
            let output = json_string(&value, "formatted_output")
                .or_else(|| json_string(&value, "aggregated_output"))
                .or_else(|| {
                    (!buffered_output.trim().is_empty())
                        .then_some(buffered_output.trim().to_string())
                })
                .unwrap_or_else(|| "(no output)".to_string());
            let prefix = if is_success {
                "   -> result: "
            } else {
                "   -> exit: "
            };
            let style = if is_success { &green } else { &red };
            let annotated = if is_success {
                output
            } else {
                let exit_code = value
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .map(|code| format!("code {code}\n{output}"))
                    .unwrap_or(output);
                exit_code
            };
            write_block(&mut out, prefix, Some(annotated), style, 8);
        }
        "mcp_tool_call_begin" => {
            state.tool_count += 1;
            let label = display_mcp_invocation(&value);
            push_plain_line(&mut out, "");
            push_styled_line(&mut out, &yellow, format!("[tool] {label}"));
            write_block(
                &mut out,
                "   args: ",
                value
                    .get("invocation")
                    .and_then(|invocation| invocation.get("arguments"))
                    .and_then(compact_json),
                &dim,
                4,
            );
        }
        "mcp_tool_call_end" => {
            let (summary, is_error) = summarize_mcp_result(&value);
            let style = if is_error { &red } else { &green };
            write_block(&mut out, "   -> result: ", summary, style, 8);
        }
        "patch_apply_begin" => {
            state.tool_count += 1;
            let files = summarize_patch_targets(&value);
            push_plain_line(&mut out, "");
            push_styled_line(&mut out, &yellow, "[patch]");
            write_block(&mut out, "   ", Some(files), &dim, 2);
        }
        "patch_apply_end" => {
            let is_success = value
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let style = if is_success { &green } else { &red };
            let text = json_string(&value, "stdout")
                .or_else(|| json_string(&value, "stderr"))
                .unwrap_or_else(|| {
                    if is_success {
                        "patch applied".to_string()
                    } else {
                        "patch failed".to_string()
                    }
                });
            write_block(&mut out, "   -> result: ", Some(text), style, 6);
        }
        "web_search_end" => {
            state.tool_count += 1;
            let label = display_web_search(&value);
            push_plain_line(&mut out, "");
            push_styled_line(&mut out, &yellow, "[web]");
            write_block(&mut out, "   ", Some(label), &dim, 2);
        }
        "plan_update" => {
            state.tool_count += 1;
            push_styled_line(&mut out, &blue, "plan:");
            if let Some(explanation) = value
                .get("explanation")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                write_block(&mut out, "  note: ", Some(explanation.to_string()), &dim, 2);
            }
            if let Some(plan) = value.get("plan").and_then(Value::as_array) {
                for item in plan.iter().take(6) {
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("pending");
                    let step = item.get("step").and_then(Value::as_str).unwrap_or_default();
                    if !step.trim().is_empty() {
                        push_styled_line(&mut out, &dim, format!("  {status}: {step}"));
                    }
                }
                if plan.len() > 6 {
                    push_styled_line(
                        &mut out,
                        &dim,
                        format!("  ... +{} more steps", plan.len() - 6),
                    );
                }
            }
        }
        "token_count" => {
            if let Some(usage) = value
                .get("info")
                .and_then(|info| info.get("total_token_usage"))
                .or_else(|| {
                    value
                        .get("info")
                        .and_then(|info| info.get("last_token_usage"))
                })
            {
                update_usage_from_value(usage, state);
            }
        }
        "warning" => {
            let message = json_string(&value, "message").unwrap_or_else(|| value.to_string());
            push_styled_line(&mut out, &yellow, format!("warning: {message}"));
        }
        "model_reroute" => {
            let from = value
                .get("from")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let to = value.get("to").and_then(Value::as_str).unwrap_or("unknown");
            push_styled_line(&mut out, &yellow, format!("reroute: {from} -> {to}"));
        }
        "error" | "turn_aborted" | "stream_error" => {
            let message = json_string(&value, "message").unwrap_or_else(|| value.to_string());
            push_styled_line(&mut out, &red, format!("error: {message}"));
        }
        _ => {}
    }

    out
}

fn render_opencode_stream_line(line: &str) -> String {
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

fn render_legacy_item_started(value: &Value, state: &mut CodexRenderState, out: &mut String) {
    let item = value.get("item").unwrap_or(&Value::Null);
    if item.get("type").and_then(Value::as_str) == Some("command_execution") {
        state.tool_count += 1;
        push_styled_line(out, &Style::new().cyan(), "command:");
        write_block(
            out,
            "  ",
            json_string(item, "command"),
            &Style::new().dim(),
            2,
        );
    }
}

fn render_legacy_item_completed(value: &Value, state: &mut CodexRenderState, out: &mut String) {
    let item = value.get("item").unwrap_or(&Value::Null);
    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
        "reasoning" => {
            write_block(
                out,
                "thinking: ",
                json_string(item, "text"),
                &Style::new().dim(),
                3,
            );
        }
        "command_execution" => {
            let exit_code = item.get("exit_code").and_then(Value::as_i64).unwrap_or(0);
            let style = if exit_code == 0 {
                Style::new().green()
            } else {
                Style::new().red()
            };
            let prefix = if exit_code == 0 { "result: " } else { "exit: " };
            let text = json_string(item, "aggregated_output")
                .unwrap_or_else(|| format!("code {exit_code}"));
            write_block(out, prefix, Some(text), &style, 6);
        }
        "agent_message" => {
            let text = json_string(item, "text");
            if let Some(message) = text.clone() {
                state.last_agent_message = Some(message);
            }
            write_block(out, "", text, &Style::new(), 8);
        }
        other => {
            let text = json_string(item, "text")
                .or_else(|| compact_json(item))
                .unwrap_or_default();
            if !other.is_empty() && !text.trim().is_empty() {
                write_block(
                    out,
                    &format!("{other}: "),
                    Some(text),
                    &Style::new().dim(),
                    3,
                );
            }
        }
    }
}

fn display_exec_command(value: &Value) -> String {
    if let Some(command) = value.get("command").and_then(Value::as_array) {
        let rendered = command
            .iter()
            .filter_map(Value::as_str)
            .map(display_shell_arg)
            .collect::<Vec<_>>()
            .join(" ");
        if !rendered.is_empty() {
            return rendered;
        }
    }
    json_string(value, "command").unwrap_or_else(|| "unknown command".to_string())
}

fn display_shell_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    if arg.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '/' | '.' | ':' | '=' | '+')
    }) {
        return arg.to_string();
    }
    let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn display_mcp_invocation(value: &Value) -> String {
    let invocation = value.get("invocation").unwrap_or(&Value::Null);
    let server = invocation
        .get("server")
        .and_then(Value::as_str)
        .unwrap_or("mcp");
    let tool = invocation
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    format!("{server}.{tool}")
}

fn summarize_mcp_result(value: &Value) -> (Option<String>, bool) {
    let Some(result) = value.get("result") else {
        return (None, false);
    };
    if let Some(err) = result.get("Err").and_then(Value::as_str) {
        return (Some(err.to_string()), true);
    }

    let ok = result.get("Ok").unwrap_or(&Value::Null);
    let is_error = ok.get("isError").and_then(Value::as_bool).unwrap_or(false);
    if let Some(structured) = ok
        .get("structuredContent")
        .filter(|content| !content.is_null())
    {
        return (compact_json(structured), is_error);
    }
    if let Some(text) = ok
        .get("content")
        .and_then(Value::as_array)
        .map(|content| extract_content_text(content))
        .filter(|text| !text.trim().is_empty())
    {
        return (Some(text), is_error);
    }
    (compact_json(ok), is_error)
}

fn extract_content_text(content: &[Value]) -> String {
    let mut parts = Vec::new();
    for item in content {
        if let Some(text) = item
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            parts.push(text.to_string());
            continue;
        }
        if let Some(text) = item.as_str().map(str::trim).filter(|text| !text.is_empty()) {
            parts.push(text.to_string());
            continue;
        }
        if let Some(summary) = compact_json(item) {
            parts.push(summary);
        }
    }
    parts.join("\n")
}

fn summarize_patch_targets(value: &Value) -> String {
    let Some(changes) = value.get("changes").and_then(Value::as_object) else {
        return "unknown files".to_string();
    };
    let mut files = changes.keys().cloned().collect::<Vec<_>>();
    files.sort();
    if files.is_empty() {
        return "unknown files".to_string();
    }
    let preview = files.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if files.len() > 3 {
        format!("{preview} +{} more", files.len() - 3)
    } else {
        preview
    }
}

fn display_web_search(value: &Value) -> String {
    if let Some(action_type) = value
        .get("action")
        .and_then(|action| action.get("type"))
        .and_then(Value::as_str)
    {
        let detail = value
            .get("query")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .get("action")
                    .and_then(|action| action.get("query"))
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                value
                    .get("action")
                    .and_then(|action| action.get("url"))
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                value
                    .get("action")
                    .and_then(|action| action.get("pattern"))
                    .and_then(Value::as_str)
            })
            .unwrap_or_default();
        if detail.is_empty() {
            return action_type.to_string();
        }
        return format!("{action_type}: {detail}");
    }
    "search".to_string()
}

fn agent_message_text(value: &Value) -> Option<String> {
    json_string(value, "message")
        .or_else(|| json_string(value, "text"))
        .or_else(|| {
            value
                .get("content")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        })
}

fn update_usage_from_value(value: &Value, state: &mut CodexRenderState) {
    state.usage.input_tokens = value
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(state.usage.input_tokens);
    state.usage.cached_input_tokens = value
        .get("cached_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(state.usage.cached_input_tokens);
    state.usage.output_tokens = value
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(state.usage.output_tokens);
}

fn push_plain_line(out: &mut String, line: &str) {
    let sanitized = sanitize_terminal_text(line);
    let _ = writeln!(out, "{sanitized}");
}

fn push_styled_line(out: &mut String, style: &Style, line: impl AsRef<str>) {
    let sanitized = sanitize_terminal_text(line.as_ref());
    let _ = writeln!(out, "{}", style.apply_to(sanitized));
}

fn write_block(out: &mut String, prefix: &str, text: Option<String>, style: &Style, limit: usize) {
    let Some(text) = text else {
        return;
    };
    let sanitized = sanitize_terminal_text(&text);
    let lines = sanitized
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    for line in lines.iter().take(limit) {
        let clipped = if line.chars().count() > 140 {
            format!("{}...", clip_line_for_display(line, 137))
        } else {
            (*line).to_string()
        };
        push_styled_line(out, style, format!("{prefix}{clipped}"));
    }
    if lines.len() > limit {
        push_styled_line(
            out,
            &Style::new().dim(),
            format!("{prefix}... +{} more lines", lines.len() - limit),
        );
    }
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn compact_json(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    serde_json::to_string(value)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty() && text != "null")
}

fn sanitize_terminal_text(input: &str) -> String {
    let mut chars = input.chars().peekable();
    let mut out = String::with_capacity(input.len());
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => skip_escape_sequence(&mut chars),
            '\u{009b}' => skip_csi_sequence(&mut chars),
            '\u{009d}' => skip_osc_sequence(&mut chars),
            '\u{08}' => pop_last_inline_char(&mut out),
            '\r' => {
                if chars.peek() != Some(&'\n') && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            '\n' | '\t' => out.push(ch),
            '\u{00}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => {}
            _ => out.push(ch),
        }
    }
    out
}

fn skip_escape_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            skip_csi_sequence(chars);
        }
        Some(']') => {
            chars.next();
            skip_osc_sequence(chars);
        }
        Some('P' | 'X' | '^' | '_') => {
            chars.next();
            skip_st_sequence(chars);
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

fn skip_csi_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(ch) = chars.next() {
        if ('@'..='~').contains(&ch) {
            break;
        }
    }
}

fn skip_osc_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(ch) = chars.next() {
        match ch {
            '\u{07}' => break,
            '\u{1b}' if chars.peek() == Some(&'\\') => {
                chars.next();
                break;
            }
            _ => {}
        }
    }
}

fn skip_st_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'\\') {
            chars.next();
            break;
        }
    }
}

fn pop_last_inline_char(out: &mut String) {
    if out.ends_with('\n') {
        return;
    }
    out.pop();
}

#[cfg(test)]
mod tests {
    use super::{
        render_codex_stream_line, render_opencode_stream_line, sanitize_terminal_text,
        CodexRenderState,
    };

    #[test]
    fn renders_exec_commands_with_result_lines() {
        console::set_colors_enabled(false);
        let mut state = CodexRenderState::default();
        let begin = r#"{"type":"exec_command_begin","call_id":"1","command":["/usr/bin/bash","-lc","sed -n '1,5p' README.md"],"cwd":"/tmp"}"#;
        let end = r#"{"type":"exec_command_end","call_id":"1","status":"completed","exit_code":0,"formatted_output":"line one\nline two"}"#;

        let rendered = format!(
            "{}{}",
            render_codex_stream_line(begin, &mut state),
            render_codex_stream_line(end, &mut state)
        );

        assert!(rendered.contains("[command]"));
        assert!(rendered.contains("   /usr/bin/bash -lc \"sed -n '1,5p' README.md\""));
        assert!(rendered.contains("   cwd: /tmp"));
        assert!(rendered.contains("   -> result: line one"));
        assert!(rendered.contains("   -> result: line two"));
    }

    #[test]
    fn renders_mcp_tool_results() {
        console::set_colors_enabled(false);
        let mut state = CodexRenderState::default();
        let begin = r#"{"type":"mcp_tool_call_begin","call_id":"1","invocation":{"server":"github","tool":"fetch_pr","arguments":{"pr":7}}}"#;
        let end = r#"{"type":"mcp_tool_call_end","call_id":"1","invocation":{"server":"github","tool":"fetch_pr","arguments":{"pr":7}},"result":{"Ok":{"content":[{"type":"text","text":"PR title"}]}}}"#;

        let rendered = format!(
            "{}{}",
            render_codex_stream_line(begin, &mut state),
            render_codex_stream_line(end, &mut state)
        );

        assert!(rendered.contains("[tool] github.fetch_pr"));
        assert!(rendered.contains(r#"   args: {"pr":7}"#));
        assert!(rendered.contains("   -> result: PR title"));
    }

    #[test]
    fn renders_plan_updates() {
        console::set_colors_enabled(false);
        let mut state = CodexRenderState::default();
        let event = r#"{"type":"plan_update","explanation":"Keep the user informed","plan":[{"step":"Inspect renderer","status":"completed"},{"step":"Add shared module","status":"in_progress"},{"step":"Run tests","status":"pending"}]}"#;

        let rendered = render_codex_stream_line(event, &mut state);

        assert!(rendered.contains("plan:"));
        assert!(rendered.contains("note: Keep the user informed"));
        assert!(rendered.contains("completed: Inspect renderer"));
        assert!(rendered.contains("in_progress: Add shared module"));
        assert!(rendered.contains("pending: Run tests"));
    }

    #[test]
    fn sanitizes_escape_sequences_for_plain_selection() {
        let text = "alpha\u{1b}[31m red\u{1b}[0m\u{1b}]8;;https://example.com\u{07} link\u{1b}]8;;\u{07}\rbravo";
        assert_eq!(sanitize_terminal_text(text), "alpha red link\nbravo");
    }

    #[test]
    fn sanitizes_backspaces_from_command_output() {
        let text = "buildin\u{08}g ok";
        assert_eq!(sanitize_terminal_text(text), "buildig ok");
    }

    #[test]
    fn renders_exec_results_without_terminal_control_sequences() {
        console::set_colors_enabled(false);
        let mut state = CodexRenderState::default();
        let begin = r#"{"type":"exec_command_begin","call_id":"1","command":["printf","demo"],"cwd":"/tmp"}"#;
        let end = "{\"type\":\"exec_command_end\",\"call_id\":\"1\",\"status\":\"completed\",\"exit_code\":0,\"formatted_output\":\"ok\\u001b[32m green\\u001b[0m\\rnext\"}";

        let rendered = format!(
            "{}{}",
            render_codex_stream_line(begin, &mut state),
            render_codex_stream_line(end, &mut state)
        );

        assert!(rendered.contains("   -> result: ok green"));
        assert!(rendered.contains("   -> result: next"));
        assert!(!rendered.contains('\u{1b}'));
    }

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
}
