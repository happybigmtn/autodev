//! Auditor process layer for `auto audit`: Codex/kimi-cli spawning, stream
//! capture, and timeout handling.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::process::Command as TokioCommand;
use tokio::time;

use crate::audit_command::AUDITOR_TIMEOUT_SECS;
use crate::codex_exec::MAX_CODEX_MODEL_CONTEXT_WINDOW;
use crate::codex_stream::{capture_codex_output_prefixed, capture_pi_output};
use crate::kimi_backend::{
    extract_final_text as kimi_extract_final_text, kimi_exec_args, parse_kimi_error,
    resolve_kimi_bin, resolve_kimi_cli_model,
};
use crate::prompt_ethos::with_autodev_prompt_ethos;
use crate::AuditArgs;

#[cfg(test)]
pub(crate) async fn run_auditor(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
) -> Result<String> {
    run_auditor_labeled(repo_root, prompt, args, None).await
}

pub(crate) async fn run_auditor_labeled(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    label: Option<&str>,
) -> Result<String> {
    run_auditor_labeled_with_env(repo_root, prompt, args, label, &[]).await
}

async fn run_auditor_labeled_with_env(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    label: Option<&str>,
    extra_env: &[(String, String)],
) -> Result<String> {
    run_auditor_labeled_with_env_and_timeout(
        repo_root,
        prompt,
        args,
        label,
        extra_env,
        AUDITOR_TIMEOUT_SECS,
    )
    .await
}

pub(crate) async fn run_auditor_labeled_with_env_and_timeout(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    label: Option<&str>,
    extra_env: &[(String, String)],
    timeout_secs: u64,
) -> Result<String> {
    let prompt = with_autodev_prompt_ethos(prompt);
    if args.use_kimi_cli && is_kimi_model(&args.model) {
        run_auditor_kimi(repo_root, &prompt, args, extra_env, timeout_secs).await
    } else if is_kimi_model(&args.model) {
        bail!("auto audit Kimi models currently require --use-kimi-cli");
    } else {
        run_auditor_codex(repo_root, &prompt, args, label, extra_env, timeout_secs).await
    }
}

pub(crate) fn is_kimi_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower.contains("kimi") || lower.starts_with("k2.") || lower.starts_with("k2p")
}

async fn run_auditor_codex(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    label: Option<&str>,
    extra_env: &[(String, String)],
    timeout_secs: u64,
) -> Result<String> {
    let mut command = TokioCommand::new(&args.codex_bin);
    command
        .arg("exec")
        .arg("--json")
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .arg("--skip-git-repo-check")
        .arg("--cd")
        .arg(repo_root)
        .arg("-m")
        .arg(&args.model)
        .arg("-c")
        .arg(format!(
            "model_reasoning_effort=\"{}\"",
            args.reasoning_effort
        ))
        .arg("-c")
        .arg(format!(
            "model_context_window={MAX_CODEX_MODEL_CONTEXT_WINDOW}"
        ))
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
            args.codex_bin.display(),
            repo_root.display()
        )
    })?;
    let mut stdin = child
        .stdin
        .take()
        .context("Codex stdin should be piped for auto audit")?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .context("failed to write auto audit prompt to Codex")?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .context("Codex stdout should be piped for auto audit")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex stderr should be piped for auto audit")?;
    let label = label.map(str::to_string);
    let stdout_task =
        tokio::spawn(
            async move { capture_codex_output_prefixed(stdout, label.as_deref(), None).await },
        );
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });
    let wait_result = time::timeout(Duration::from_secs(timeout_secs), child.wait()).await;
    let timed_out = wait_result.is_err();
    if timed_out {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let stdout = stdout_task
        .await
        .context("Codex stdout capture task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Codex stderr capture task panicked")??;
    if timed_out {
        bail!("Codex audit pass timed out after {}s", timeout_secs);
    }
    let status = wait_result
        .expect("timeout already handled")
        .context("failed waiting for Codex")?;
    if !status.success() {
        bail!(
            "Codex audit failed: {}",
            if !stderr_text.trim().is_empty() {
                stderr_text.trim().to_string()
            } else {
                stdout.trim().to_string()
            }
        );
    }
    Ok(stdout)
}

async fn run_auditor_kimi(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    extra_env: &[(String, String)],
    timeout_secs: u64,
) -> Result<String> {
    let kimi_bin = resolve_kimi_bin(&args.kimi_bin);
    let model = resolve_kimi_cli_model(&args.model);
    let exec_args = kimi_exec_args(&model, &args.reasoning_effort, prompt);
    let mut command = TokioCommand::new(&kimi_bin);
    command
        .args(&exec_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch kimi-cli at {} from {}",
            kimi_bin.display(),
            repo_root.display()
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .context("kimi-cli stdout should be piped for auto audit")?;
    let stderr = child
        .stderr
        .take()
        .context("kimi-cli stderr should be piped for auto audit")?;
    let stdout_task =
        tokio::spawn(async move { capture_pi_output(stdout, "auto audit kimi-cli", 30).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });
    let wait_result = time::timeout(Duration::from_secs(timeout_secs), child.wait()).await;
    let timed_out = wait_result.is_err();
    if timed_out {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let stdout = stdout_task
        .await
        .context("kimi-cli stdout capture task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("kimi-cli stderr capture task panicked")??;
    if timed_out {
        bail!("kimi-cli audit pass timed out after {}s", timeout_secs);
    }
    let status = wait_result
        .expect("timeout already handled")
        .context("failed waiting for kimi-cli")?;
    if !status.success() {
        bail!(
            "kimi-cli audit failed: {}",
            if !stderr_text.trim().is_empty() {
                stderr_text.trim().to_string()
            } else {
                parse_kimi_error(&stdout).unwrap_or_else(|| stdout.trim().to_string())
            }
        );
    }
    if let Some(detail) = parse_kimi_error(&stdout) {
        bail!("kimi-cli audit failed: {detail}");
    }
    let mut final_text = String::new();
    for line in stdout.lines() {
        if let Some(chunk) = kimi_extract_final_text(line) {
            if !final_text.is_empty() {
                final_text.push('\n');
            }
            final_text.push_str(&chunk);
        }
    }
    if final_text.trim().is_empty() {
        Ok(stdout)
    } else {
        Ok(final_text)
    }
}

async fn read_stream<R>(stream: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = tokio::io::BufReader::new(stream);
    let mut text = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut reader, &mut text)
        .await
        .context("failed to read child stream")?;
    Ok(text)
}
