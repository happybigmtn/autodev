use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::oneshot;

use crate::codex_stream;
use crate::quota_config::Provider;
use crate::quota_exec;
use crate::util::{atomic_write, timestamp_slug};

pub(crate) async fn run_claude_exec(
    repo_root: &Path,
    full_prompt: &str,
    max_turns: Option<usize>,
    stderr_log_path: &Path,
    context_label: &str,
) -> Result<std::process::ExitStatus> {
    let (status, stderr_text) = if quota_exec::is_quota_available(Provider::Claude) {
        let repo_root = repo_root.to_owned();
        let full_prompt = full_prompt.to_owned();
        let context_label = context_label.to_owned();
        let result = quota_exec::run_with_quota(Provider::Claude, move || {
            let repo_root = repo_root.clone();
            let full_prompt = full_prompt.clone();
            let context_label = context_label.clone();
            async move {
                spawn_claude(&repo_root, &full_prompt, max_turns, &context_label).await
            }
        })
        .await?;
        (result.exit_status, result.stderr_text)
    } else {
        spawn_claude(repo_root, full_prompt, max_turns, context_label).await?
    };
    log_stderr(&stderr_text, stderr_log_path)?;
    Ok(status)
}

pub(crate) const FUTILITY_EXIT_MARKER: i32 = 137;

async fn spawn_claude(
    repo_root: &Path,
    full_prompt: &str,
    max_turns: Option<usize>,
    context_label: &str,
) -> Result<(std::process::ExitStatus, String)> {
    let mut command = TokioCommand::new("claude");
    command
        .arg("-p")
        .arg("--verbose")
        .arg("--dangerously-skip-permissions")
        .arg("--output-format")
        .arg("stream-json");
    if let Some(turns) = max_turns {
        command.arg("--max-turns").arg(turns.to_string());
    }
    command
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

    let (futility_tx, futility_rx) = oneshot::channel::<()>();
    let stdout_task = tokio::spawn(async move {
        codex_stream::stream_claude_output(stdout, Some(futility_tx)).await
    });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = tokio::select! {
        result = child.wait() => {
            result.context("failed waiting for Claude")?
        }
        Ok(()) = futility_rx => {
            println!(
                "\nfutility spiral detected: killing Claude after {} consecutive empty tool results",
                codex_stream::CLAUDE_FUTILITY_THRESHOLD,
            );
            let _ = child.start_kill();
            // Return a synthetic non-zero exit status so the loop can retry
            let _ = child.wait().await;
            // Raw wait status: exit code in upper byte, lower byte is signal.
            // Shift left by 8 so .code() returns FUTILITY_EXIT_MARKER.
            std::process::ExitStatus::from_raw(FUTILITY_EXIT_MARKER << 8)
        }
    };

    stdout_task
        .await
        .context("Claude stdout streaming task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Claude stderr capture task panicked")??;

    Ok((status, stderr_text))
}

fn log_stderr(stderr_text: &str, stderr_log_path: &Path) -> Result<()> {
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
    Ok(())
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
