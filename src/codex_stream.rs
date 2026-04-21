use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};
use console::Style;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::oneshot;
use tokio::time::{self, Duration, MissedTickBehavior};

use crate::util::clip_line_for_display;

pub(crate) const CLAUDE_FUTILITY_THRESHOLD: usize = 8;
/// Futility threshold used for passes that are read-heavy by design (code
/// review). The standard threshold is tuned for implementation loops where
/// each tool call produces a file edit; review spends more calls inspecting
/// source before emitting anything, so the 8-count trigger frequently
/// false-fires on an otherwise-healthy review run.
pub(crate) const CLAUDE_FUTILITY_THRESHOLD_REVIEW: usize = 16;
const CLAUDE_SEARCH_MISS_HINT_THRESHOLD: usize = 3;

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

#[derive(Default)]
struct PiRenderState {
    tool_count: usize,
    usage: UsageSummary,
    last_agent_message: Option<String>,
}

struct ClaudeRenderState {
    tool_count: usize,
    current_tool_name: Option<String>,
    last_agent_message: Option<String>,
    consecutive_empty_results: usize,
    consecutive_search_misses: usize,
    futility_detected: bool,
    /// Threshold after which consecutive empty tool results are treated as
    /// futility. Defaults to `CLAUDE_FUTILITY_THRESHOLD`; review mode raises
    /// this because reviewer runs are read-heavy by design.
    futility_threshold: usize,
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

pub(crate) async fn capture_codex_output_with_heartbeat<R>(
    stream: R,
    heartbeat_label: &str,
    heartbeat_secs: u64,
) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream).lines();
    let mut raw = String::new();
    let mut state = CodexRenderState::default();
    let mut interval = time::interval(Duration::from_secs(heartbeat_secs.max(1)));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval.tick().await;
    let mut saw_streamed_output = false;
    let mut elapsed = 0u64;

    loop {
        tokio::select! {
            line = reader.next_line() => {
                let Some(line) = line.context("failed reading Codex JSON stream")? else {
                    break;
                };
                raw.push_str(&line);
                raw.push('\n');
                let rendered = render_codex_stream_line(&line, &mut state);
                if !rendered.is_empty() {
                    saw_streamed_output = true;
                    print!("{rendered}");
                    let _ = io::stdout().flush();
                }
            }
            _ = interval.tick() => {
                elapsed += heartbeat_secs.max(1);
                let message = if saw_streamed_output {
                    format!("status: {heartbeat_label} still running ({elapsed}s elapsed)")
                } else {
                    format!(
                        "status: {heartbeat_label} still running ({elapsed}s elapsed, waiting for streamed output)"
                    )
                };
                let mut rendered = String::new();
                push_styled_line(&mut rendered, &Style::new().dim(), message);
                print!("{rendered}");
                let _ = io::stdout().flush();
            }
        }
    }

    Ok(raw)
}

#[allow(dead_code)]
pub(crate) async fn stream_codex_output<R>(stream: R) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    capture_codex_output_prefixed(stream, None, None).await?;
    Ok(())
}

pub(crate) async fn capture_codex_output_prefixed<R>(
    stream: R,
    prefix: Option<&str>,
    rendered_log_path: Option<&Path>,
) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream).lines();
    let mut raw = String::new();
    let mut state = CodexRenderState::default();
    let mut rendered_log = open_rendered_log(rendered_log_path)?;
    while let Some(line) = reader
        .next_line()
        .await
        .context("failed reading Codex JSON stream")?
    {
        raw.push_str(&line);
        raw.push('\n');
        let rendered = render_codex_stream_line(&line, &mut state);
        if !rendered.is_empty() {
            print!("{}", render_with_prefix(&rendered, prefix));
            let _ = io::stdout().flush();
            if let Some(file) = rendered_log.as_mut() {
                file.write_all(rendered.as_bytes())
                    .context("failed writing Codex rendered output log")?;
                let _ = file.flush();
            }
        }
    }
    Ok(raw)
}

pub(crate) async fn stream_claude_output_with_threshold<R>(
    stream: R,
    futility_tx: Option<oneshot::Sender<()>>,
    prefix: Option<&str>,
    rendered_log_path: Option<&Path>,
    futility_threshold: usize,
) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream).lines();
    let mut state = ClaudeRenderState {
        futility_threshold,
        ..ClaudeRenderState::default()
    };
    let mut futility_tx = futility_tx;
    let mut rendered_log = open_rendered_log(rendered_log_path)?;
    while let Some(line) = reader
        .next_line()
        .await
        .context("failed reading Claude JSON stream")?
    {
        let rendered = render_claude_stream_line(&line, &mut state);
        if !rendered.is_empty() {
            print!("{}", render_with_prefix(&rendered, prefix));
            let _ = io::stdout().flush();
            if let Some(file) = rendered_log.as_mut() {
                file.write_all(rendered.as_bytes())
                    .context("failed writing Claude rendered output log")?;
                let _ = file.flush();
            }
        }
        if state.futility_detected {
            if let Some(tx) = futility_tx.take() {
                let _ = tx.send(());
            }
        }
    }
    Ok(())
}

fn open_rendered_log(path: Option<&Path>) -> Result<Option<std::fs::File>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open rendered output log {}", path.display()))?;
    Ok(Some(file))
}

fn render_with_prefix(rendered: &str, prefix: Option<&str>) -> String {
    let Some(prefix) = prefix.filter(|value| !value.is_empty()) else {
        return rendered.to_string();
    };
    let prefix_text = format!("[{prefix}] ");
    let mut out = String::with_capacity(rendered.len() + prefix_text.len() * 4);
    for segment in rendered.split_inclusive('\n') {
        let has_newline = segment.ends_with('\n');
        let body = segment.strip_suffix('\n').unwrap_or(segment);
        if body.is_empty() {
            if has_newline {
                out.push('\n');
            }
            continue;
        }
        out.push_str(&prefix_text);
        out.push_str(body);
        if has_newline {
            out.push('\n');
        }
    }
    out
}

#[allow(dead_code)]
pub(crate) async fn capture_opencode_output<R>(
    stream: R,
    heartbeat_label: &str,
    heartbeat_secs: u64,
) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream).lines();
    let mut raw = String::new();
    let mut interval = time::interval(Duration::from_secs(heartbeat_secs.max(1)));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval.tick().await;
    let mut saw_streamed_output = false;
    let mut elapsed = 0u64;

    loop {
        tokio::select! {
            line = reader
                .next_line() => {
                let Some(line) = line.context("failed reading OpenCode JSON stream")? else {
                    break;
                };
                raw.push_str(&line);
                raw.push('\n');
                let rendered = render_opencode_stream_line(&line);
                if !rendered.is_empty() {
                    saw_streamed_output = true;
                    print!("{rendered}");
                    let _ = io::stdout().flush();
                }
            }
            _ = interval.tick() => {
                elapsed += heartbeat_secs.max(1);
                let message = if saw_streamed_output {
                    format!(
                        "status: {heartbeat_label} still running ({elapsed}s elapsed)"
                    )
                } else {
                    format!(
                        "status: {heartbeat_label} still running ({elapsed}s elapsed, waiting for streamed output)"
                    )
                };
                let mut rendered = String::new();
                push_styled_line(&mut rendered, &Style::new().dim(), message);
                print!("{rendered}");
                let _ = io::stdout().flush();
            }
        }
    }
    Ok(raw)
}

pub(crate) async fn capture_pi_output<R>(
    stream: R,
    heartbeat_label: &str,
    heartbeat_secs: u64,
) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream).lines();
    let mut raw = String::new();
    let mut state = PiRenderState::default();
    let mut interval = time::interval(Duration::from_secs(heartbeat_secs.max(1)));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval.tick().await;
    let mut saw_streamed_output = false;
    let mut elapsed = 0u64;

    loop {
        tokio::select! {
            line = reader.next_line() => {
                let Some(line) = line.context("failed reading PI JSON stream")? else {
                    break;
                };
                raw.push_str(&line);
                raw.push('\n');
                let rendered = render_pi_stream_line(&line, &mut state);
                if !rendered.is_empty() {
                    saw_streamed_output = true;
                    print!("{rendered}");
                    let _ = io::stdout().flush();
                }
            }
            _ = interval.tick() => {
                elapsed += heartbeat_secs.max(1);
                let message = if saw_streamed_output {
                    format!("status: {heartbeat_label} still running ({elapsed}s elapsed)")
                } else {
                    format!(
                        "status: {heartbeat_label} still running ({elapsed}s elapsed, waiting for streamed output)"
                    )
                };
                let mut rendered = String::new();
                push_styled_line(&mut rendered, &Style::new().dim(), message);
                print!("{rendered}");
                let _ = io::stdout().flush();
            }
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

fn render_pi_stream_line(line: &str, state: &mut PiRenderState) -> String {
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

fn render_claude_stream_line(line: &str, state: &mut ClaudeRenderState) -> String {
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

fn display_pi_bash_command(args: &Value) -> String {
    args.get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "unknown command".to_string())
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

fn summarize_pi_tool_result(result: &Value) -> Option<String> {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| extract_content_text(items))
        .filter(|text| !text.trim().is_empty());
    content.or_else(|| compact_json(result))
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
    for ch in chars.by_ref() {
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
        render_claude_stream_line, render_codex_stream_line, render_opencode_stream_line,
        render_pi_stream_line, sanitize_terminal_text, ClaudeRenderState, CodexRenderState,
        PiRenderState, CLAUDE_FUTILITY_THRESHOLD,
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
