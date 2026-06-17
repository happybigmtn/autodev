use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};
use console::Style;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::oneshot;
use tokio::time::{self, Duration, MissedTickBehavior};

use crate::codex_stream::format::push_styled_line;
use crate::codex_stream::render_claude::{render_claude_stream_line, ClaudeRenderState};
use crate::codex_stream::render_codex::{render_codex_stream_line, CodexRenderState};
use crate::codex_stream::render_pi::{
    render_opencode_stream_line, render_pi_stream_line, PiRenderState,
};

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
