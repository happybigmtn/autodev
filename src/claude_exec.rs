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

const DEFAULT_CLAUDE_MODEL_ALIAS: &str = "opus";

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_claude_exec(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    effort: &str,
    max_turns: Option<usize>,
    stderr_log_path: &Path,
    stdout_log_path: Option<&Path>,
    context_label: &str,
) -> Result<std::process::ExitStatus> {
    run_claude_exec_with_env(
        repo_root,
        full_prompt,
        model,
        effort,
        max_turns,
        stderr_log_path,
        stdout_log_path,
        context_label,
        &[],
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_claude_with_futility(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    effort: &str,
    max_turns: Option<usize>,
    stderr_log_path: &Path,
    stdout_log_path: Option<&Path>,
    context_label: &str,
    futility_threshold: Option<usize>,
) -> Result<std::process::ExitStatus> {
    run_claude_exec_with_env(
        repo_root,
        full_prompt,
        model,
        effort,
        max_turns,
        stderr_log_path,
        stdout_log_path,
        context_label,
        &[],
        None,
        futility_threshold,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_claude_exec_with_env(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    effort: &str,
    max_turns: Option<usize>,
    stderr_log_path: &Path,
    stdout_log_path: Option<&Path>,
    context_label: &str,
    extra_env: &[(String, String)],
    worker_pid_path: Option<&Path>,
    futility_threshold: Option<usize>,
) -> Result<std::process::ExitStatus> {
    let resolved_model = resolve_claude_model(model);
    let resolved_effort = resolve_claude_effort(effort);
    let (status, stderr_text) = if quota_exec::is_quota_available(Provider::Claude) {
        let repo_root = repo_root.to_owned();
        let full_prompt = full_prompt.to_owned();
        let resolved_model = resolved_model.clone();
        let resolved_effort = resolved_effort.clone();
        let context_label = context_label.to_owned();
        let extra_env = extra_env.to_vec();
        let stdout_log_path = stdout_log_path.map(Path::to_path_buf);
        let result = quota_exec::run_with_quota(Provider::Claude, move || {
            let repo_root = repo_root.clone();
            let full_prompt = full_prompt.clone();
            let resolved_model = resolved_model.clone();
            let resolved_effort = resolved_effort.clone();
            let context_label = context_label.clone();
            let extra_env = extra_env.clone();
            let stdout_log_path = stdout_log_path.clone();
            async move {
                spawn_claude(
                    &repo_root,
                    &full_prompt,
                    &resolved_model,
                    &resolved_effort,
                    max_turns,
                    stdout_log_path.as_deref(),
                    &context_label,
                    &extra_env,
                    worker_pid_path,
                    futility_threshold,
                )
                .await
            }
        })
        .await?;
        (result.exit_status, result.stderr_text)
    } else {
        spawn_claude(
            repo_root,
            full_prompt,
            &resolved_model,
            &resolved_effort,
            max_turns,
            stdout_log_path,
            context_label,
            extra_env,
            worker_pid_path,
            futility_threshold,
        )
        .await?
    };
    log_stderr(&stderr_text, stderr_log_path)?;
    Ok(status)
}

pub(crate) const FUTILITY_EXIT_MARKER: i32 = 137;

#[allow(clippy::too_many_arguments)]
async fn spawn_claude(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    effort: &str,
    max_turns: Option<usize>,
    stdout_log_path: Option<&Path>,
    context_label: &str,
    extra_env: &[(String, String)],
    worker_pid_path: Option<&Path>,
    futility_threshold: Option<usize>,
) -> Result<(std::process::ExitStatus, String)> {
    let mut command = TokioCommand::new("claude");
    command
        .arg("-p")
        .arg("--verbose")
        .arg("--dangerously-skip-permissions")
        .arg("--model")
        .arg(model)
        .arg("--effort")
        .arg(effort)
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
    for (key, value) in extra_env {
        command.env(key, value);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch Claude from {}", repo_root.display()))?;
    write_worker_pid(worker_pid_path, child.id())?;

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
    let stream_label = context_label.to_string();
    let stdout_log_path = stdout_log_path.map(Path::to_path_buf);
    let resolved_threshold = futility_threshold.unwrap_or(codex_stream::CLAUDE_FUTILITY_THRESHOLD);
    let stdout_task = tokio::spawn(async move {
        codex_stream::stream_claude_output_with_threshold(
            stdout,
            Some(futility_tx),
            Some(stream_label.as_str()),
            stdout_log_path.as_deref(),
            resolved_threshold,
        )
        .await
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
    clear_worker_pid(worker_pid_path)?;

    stdout_task
        .await
        .context("Claude stdout streaming task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Claude stderr capture task panicked")??;

    Ok((status, stderr_text))
}

fn write_worker_pid(worker_pid_path: Option<&Path>, pid: Option<u32>) -> Result<()> {
    let Some(path) = worker_pid_path else {
        return Ok(());
    };
    let Some(pid) = pid else {
        return Ok(());
    };
    atomic_write(path, pid.to_string().as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

fn clear_worker_pid(worker_pid_path: Option<&Path>) -> Result<()> {
    let Some(path) = worker_pid_path else {
        return Ok(());
    };
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
}

pub(crate) fn resolve_claude_model(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return DEFAULT_CLAUDE_MODEL_ALIAS.to_string();
    }
    if looks_like_claude_model(trimmed) {
        return trimmed.to_string();
    }
    DEFAULT_CLAUDE_MODEL_ALIAS.to_string()
}

pub(crate) fn resolve_claude_effort(effort: &str) -> String {
    let trimmed = effort.trim();
    if trimmed.is_empty() {
        "high".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn describe_claude_harness(model: &str, effort: &str) -> String {
    format!(
        "Claude ({})",
        [resolve_claude_model(model), resolve_claude_effort(effort)].join(" ")
    )
}

fn looks_like_claude_model(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    normalized.starts_with("claude") || matches!(normalized.as_str(), "opus" | "sonnet" | "haiku")
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use std::fs;

    use super::{describe_claude_harness, log_stderr, resolve_claude_effort, resolve_claude_model};
    use crate::util::timestamp_slug;

    #[test]
    fn non_claude_model_defaults_to_opus_alias() {
        assert_eq!(resolve_claude_model("gpt-5.5"), "opus");
        assert_eq!(resolve_claude_model(""), "opus");
    }

    #[test]
    fn explicit_claude_settings_are_preserved() {
        assert_eq!(resolve_claude_model("opus"), "opus");
        assert_eq!(
            resolve_claude_model("claude-sonnet-4-6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(resolve_claude_effort("xhigh"), "xhigh");
        assert_eq!(resolve_claude_effort(""), "high");
    }

    #[test]
    fn harness_description_uses_resolved_settings() {
        assert_eq!(
            describe_claude_harness("gpt-5.5", "high"),
            "Claude (opus high)"
        );
    }

    #[test]
    fn empty_stderr_still_writes_artifact() {
        let path = std::env::temp_dir().join(format!("claude-stderr-{}.log", timestamp_slug()));
        log_stderr("", &path).expect("write stderr log");
        let written = fs::read_to_string(&path).expect("read stderr log");
        assert!(written.contains("[no stderr captured]"));
        let _ = fs::remove_file(path);
    }
}

fn log_stderr(stderr_text: &str, stderr_log_path: &Path) -> Result<()> {
    let rendered = if stderr_text.trim().is_empty() {
        "[no stderr captured]"
    } else {
        stderr_text
    };
    let entry = format!("\n===== {} =====\n{rendered}\n", timestamp_slug());
    let mut existing = if stderr_log_path.exists() {
        fs::read(stderr_log_path)
            .with_context(|| format!("failed to read {}", stderr_log_path.display()))?
    } else {
        Vec::new()
    };
    existing.extend_from_slice(entry.as_bytes());
    atomic_write(stderr_log_path, &existing)?;
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
