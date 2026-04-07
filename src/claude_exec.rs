use std::fs;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::codex_stream;
use crate::util::{atomic_write, timestamp_slug};

pub(crate) async fn run_claude_exec(
    repo_root: &Path,
    full_prompt: &str,
    max_turns: usize,
    stderr_log_path: &Path,
    context_label: &str,
) -> Result<std::process::ExitStatus> {
    let mut command = TokioCommand::new("claude");
    command
        .arg("-p")
        .arg("--verbose")
        .arg("--dangerously-skip-permissions")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--max-turns")
        .arg(max_turns.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch Claude from {}", repo_root.display()))?;

    let mut stdin = child
        .stdin
        .take()
        .with_context(|| format!("Claude stdin should be piped for {context_label}"))?;
    stdin
        .write_all(full_prompt.as_bytes())
        .await
        .with_context(|| format!("failed to write Claude {context_label} prompt"))?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("Claude stdout should be piped for {context_label}"))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("Claude stderr should be piped for {context_label}"))?;

    let stdout_task = tokio::spawn(async move { codex_stream::stream_claude_output(stdout).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child.wait().await.context("failed waiting for Claude")?;
    stdout_task
        .await
        .context("Claude stdout streaming task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Claude stderr capture task panicked")??;
    if !stderr_text.trim().is_empty() {
        let entry = format!("\n===== {} =====\n{stderr_text}\n", timestamp_slug());
        let mut existing = if stderr_log_path.exists() {
            fs::read(stderr_log_path)
                .with_context(|| format!("failed to read {}", stderr_log_path.display()))?
        } else {
            Vec::new()
        };
        existing.extend_from_slice(entry.as_bytes());
        atomic_write(stderr_log_path, &existing)?;
    }

    Ok(status)
}

async fn read_stream<R>(stream: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .await
        .context("failed to read child stream")?;
    Ok(text)
}
