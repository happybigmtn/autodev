use std::fs;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::codex_stream;
use crate::quota_config::Provider;
use crate::quota_exec;
use crate::util::{atomic_write, timestamp_slug};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_codex_exec(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    stderr_log_path: &Path,
    stdout_log_path: Option<&Path>,
    context_label: &str,
) -> Result<std::process::ExitStatus> {
    run_codex_exec_with_env(
        repo_root,
        full_prompt,
        model,
        reasoning_effort,
        codex_bin,
        stderr_log_path,
        stdout_log_path,
        context_label,
        &[],
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_codex_exec_with_env(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    stderr_log_path: &Path,
    stdout_log_path: Option<&Path>,
    context_label: &str,
    extra_env: &[(String, String)],
    worker_pid_path: Option<&Path>,
) -> Result<std::process::ExitStatus> {
    let (status, stderr_text) = if quota_exec::is_quota_available(Provider::Codex) {
        let repo_root = repo_root.to_owned();
        let full_prompt = full_prompt.to_owned();
        let model = model.to_owned();
        let reasoning_effort = reasoning_effort.to_owned();
        let codex_bin = codex_bin.to_owned();
        let context_label = context_label.to_owned();
        let extra_env = extra_env.to_vec();
        let stdout_log_path = stdout_log_path.map(Path::to_path_buf);
        let result = quota_exec::run_with_quota(Provider::Codex, move || {
            let repo_root = repo_root.clone();
            let full_prompt = full_prompt.clone();
            let model = model.clone();
            let reasoning_effort = reasoning_effort.clone();
            let codex_bin = codex_bin.clone();
            let context_label = context_label.clone();
            let extra_env = extra_env.clone();
            let stdout_log_path = stdout_log_path.clone();
            async move {
                spawn_codex(
                    &repo_root,
                    &full_prompt,
                    &model,
                    &reasoning_effort,
                    &codex_bin,
                    stdout_log_path.as_deref(),
                    &context_label,
                    &extra_env,
                    worker_pid_path,
                )
                .await
            }
        })
        .await?;
        (result.exit_status, result.stderr_text)
    } else {
        spawn_codex(
            repo_root,
            full_prompt,
            model,
            reasoning_effort,
            codex_bin,
            stdout_log_path,
            context_label,
            extra_env,
            worker_pid_path,
        )
        .await?
    };
    log_stderr(&stderr_text, stderr_log_path)?;
    Ok(status)
}

#[allow(clippy::too_many_arguments)]
async fn spawn_codex(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    stdout_log_path: Option<&Path>,
    context_label: &str,
    extra_env: &[(String, String)],
    worker_pid_path: Option<&Path>,
) -> Result<(std::process::ExitStatus, String)> {
    let mut command = TokioCommand::new(codex_bin);
    command
        .arg("exec")
        .arg("--json")
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .arg("--skip-git-repo-check")
        .arg("--cd")
        .arg(repo_root)
        .arg("-m")
        .arg(model)
        .arg("-c")
        .arg(format!("model_reasoning_effort=\"{reasoning_effort}\""))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);
    for (key, value) in extra_env {
        command.env(key, value);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch Codex at {} from {}",
            codex_bin.display(),
            repo_root.display()
        )
    })?;
    write_worker_pid(worker_pid_path, child.id())?;

    let mut stdin = child
        .stdin
        .take()
        .with_context(|| format!("Codex stdin should be piped for {context_label}"))?;
    stdin
        .write_all(full_prompt.as_bytes())
        .await
        .with_context(|| format!("failed to write Codex {context_label} prompt"))?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("Codex stdout should be piped for {context_label}"))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("Codex stderr should be piped for {context_label}"))?;

    let stream_label = context_label.to_string();
    let stdout_log_path = stdout_log_path.map(Path::to_path_buf);
    let stdout_task = tokio::spawn(async move {
        codex_stream::capture_codex_output_prefixed(
            stdout,
            Some(stream_label.as_str()),
            stdout_log_path.as_deref(),
        )
        .await
    });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child.wait().await.context("failed waiting for Codex")?;
    clear_worker_pid(worker_pid_path)?;
    stdout_task
        .await
        .context("Codex stdout streaming task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Codex stderr capture task panicked")??;

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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::log_stderr;
    use crate::util::timestamp_slug;

    #[test]
    fn empty_stderr_still_writes_artifact() {
        let path = std::env::temp_dir().join(format!("codex-stderr-{}.log", timestamp_slug()));
        log_stderr("", &path).expect("write stderr log");
        let written = fs::read_to_string(&path).expect("read stderr log");
        assert!(written.contains("[no stderr captured]"));
        let _ = fs::remove_file(path);
    }
}
