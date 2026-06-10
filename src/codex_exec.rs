use std::fs;
use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;

use crate::backend_process::{clear_worker_pid, log_stderr, read_stream, write_worker_pid};
use crate::codex_stream;
use crate::codex_stream::capture_pi_output;
use crate::kimi_backend::{kimi_exec_args, parse_kimi_error, resolve_kimi_bin};
use crate::pi_backend::{parse_pi_error, resolve_pi_bin, PiProvider};
use crate::prompt_ethos::with_autodev_prompt_ethos;
use crate::quota_config::Provider;
use crate::quota_exec;
use crate::util::{atomic_write, opencode_agent_dir};

pub(crate) const MAX_CODEX_MODEL_CONTEXT_WINDOW: i64 = 1_000_000;

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
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_codex_exec_max_context(
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
        Some(MAX_CODEX_MODEL_CONTEXT_WINDOW),
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
    model_context_window: Option<i64>,
) -> Result<std::process::ExitStatus> {
    let full_prompt = with_autodev_prompt_ethos(full_prompt);
    let backend = select_shared_exec_backend(model, codex_bin);
    let (status, stderr_text) = match backend {
        SharedExecBackend::Codex { model, codex_bin } => {
            if quota_exec::is_quota_available(Provider::Codex) {
                let repo_root = repo_root.to_owned();
                let full_prompt = full_prompt.to_owned();
                let model = model.to_owned();
                let reasoning_effort = reasoning_effort.to_owned();
                let codex_bin = codex_bin.to_owned();
                let context_label = context_label.to_owned();
                let extra_env = extra_env.to_vec();
                let stdout_log_path = stdout_log_path.map(Path::to_path_buf);
                // Keep owned copies for the all-accounts-dead fallback:
                // the originals below are moved into the quota closure.
                let fb_repo_root = repo_root.clone();
                let fb_full_prompt = full_prompt.clone();
                let fb_model = model.clone();
                let fb_reasoning_effort = reasoning_effort.clone();
                let fb_codex_bin = codex_bin.clone();
                let fb_context_label = context_label.clone();
                let fb_extra_env = extra_env.clone();
                let fb_stdout_log_path = stdout_log_path.clone();
                let result = quota_exec::run_with_quota(Provider::Codex, move |account| {
                    let repo_root = repo_root.clone();
                    let full_prompt = full_prompt.clone();
                    let model = model.clone();
                    let reasoning_effort = reasoning_effort.clone();
                    let codex_bin = codex_bin.clone();
                    let context_label = context_label.clone();
                    let mut extra_env = extra_env.clone();
                    extra_env.extend(account.extra_env());
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
                            model_context_window,
                        )
                        .await
                    }
                })
                .await;
                match result {
                    Ok(result) => (result.exit_status, result.stderr_text),
                    Err(err) if quota_exec::error_is_all_accounts_invalid(&err) => {
                        eprintln!("[quota-router] {err:#}");
                        eprintln!(
                            "[quota-router] falling back to the default codex login (~/.codex)"
                        );
                        spawn_codex(
                            &fb_repo_root,
                            &fb_full_prompt,
                            &fb_model,
                            &fb_reasoning_effort,
                            &fb_codex_bin,
                            fb_stdout_log_path.as_deref(),
                            &fb_context_label,
                            &fb_extra_env,
                            worker_pid_path,
                            model_context_window,
                        )
                        .await?
                    }
                    Err(err) => return Err(err),
                }
            } else {
                spawn_codex(
                    repo_root,
                    &full_prompt,
                    &model,
                    reasoning_effort,
                    &codex_bin,
                    stdout_log_path,
                    context_label,
                    extra_env,
                    worker_pid_path,
                    model_context_window,
                )
                .await?
            }
        }
        SharedExecBackend::KimiCli { model, kimi_bin } => {
            spawn_kimi_cli(
                repo_root,
                &full_prompt,
                &model,
                reasoning_effort,
                &kimi_bin,
                stdout_log_path,
                context_label,
                extra_env,
                worker_pid_path,
            )
            .await?
        }
        SharedExecBackend::Pi {
            provider_label,
            model,
            pi_bin,
        } => {
            spawn_pi(
                repo_root,
                &full_prompt,
                &provider_label,
                &model,
                reasoning_effort,
                &pi_bin,
                stdout_log_path,
                context_label,
                extra_env,
                worker_pid_path,
            )
            .await?
        }
    };
    log_stderr(&stderr_text, stderr_log_path)?;
    Ok(status)
}

#[derive(Debug, Eq, PartialEq)]
enum SharedExecBackend {
    Codex {
        model: String,
        codex_bin: std::path::PathBuf,
    },
    KimiCli {
        model: String,
        kimi_bin: std::path::PathBuf,
    },
    Pi {
        provider_label: String,
        model: String,
        pi_bin: std::path::PathBuf,
    },
}

fn select_shared_exec_backend(model: &str, codex_bin: &Path) -> SharedExecBackend {
    if is_kimi_model(model) {
        return SharedExecBackend::KimiCli {
            model: model.to_string(),
            kimi_bin: resolve_kimi_bin(Path::new("kimi-cli")),
        };
    }
    if let Some(provider) = PiProvider::detect(model) {
        return SharedExecBackend::Pi {
            provider_label: provider.provider_label().to_string(),
            model: provider.resolve_model(model, "gpt-5.5"),
            pi_bin: resolve_pi_bin(Path::new("pi")),
        };
    }
    SharedExecBackend::Codex {
        model: model.to_string(),
        codex_bin: codex_bin.to_path_buf(),
    }
}

fn is_kimi_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower.contains("kimi") || lower.starts_with("k2.") || lower.starts_with("k2p")
}

#[allow(clippy::too_many_arguments)]
async fn spawn_kimi_cli(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    reasoning_effort: &str,
    kimi_bin: &Path,
    stdout_log_path: Option<&Path>,
    context_label: &str,
    extra_env: &[(String, String)],
    worker_pid_path: Option<&Path>,
) -> Result<(std::process::ExitStatus, String)> {
    let args = kimi_exec_args(model, reasoning_effort, full_prompt);
    let mut command = TokioCommand::new(kimi_bin);
    command
        .args(&args)
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
    write_worker_pid(worker_pid_path, child.id())?;

    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("kimi-cli stdout should be piped for {context_label}"))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("kimi-cli stderr should be piped for {context_label}"))?;

    let heartbeat_label = context_label.to_string();
    let stdout_task =
        tokio::spawn(async move { capture_pi_output(stdout, &heartbeat_label, 15).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child.wait().await.context("failed waiting for kimi-cli")?;
    clear_worker_pid(worker_pid_path)?;
    let stdout = stdout_task
        .await
        .context("kimi-cli stdout capture task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("kimi-cli stderr capture task panicked")??;
    write_stdout_log(stdout_log_path, &stdout)?;
    if let Some(detail) = parse_kimi_error(&stdout) {
        bail!("kimi-cli {context_label} failed: {detail}");
    }
    Ok((status, stderr_text))
}

#[allow(clippy::too_many_arguments)]
async fn spawn_pi(
    repo_root: &Path,
    full_prompt: &str,
    provider_label: &str,
    model: &str,
    reasoning_effort: &str,
    pi_bin: &Path,
    stdout_log_path: Option<&Path>,
    context_label: &str,
    extra_env: &[(String, String)],
    worker_pid_path: Option<&Path>,
) -> Result<(std::process::ExitStatus, String)> {
    let mut command = TokioCommand::new(pi_bin);
    command
        .arg("--model")
        .arg(model)
        .arg("--thinking")
        .arg(reasoning_effort)
        .arg("--mode")
        .arg("json")
        .arg("-p")
        .arg("--no-session")
        .arg("--tools")
        .arg("read,bash,edit,write,grep,find,ls")
        .arg(full_prompt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);
    configure_pi_env(&mut command, repo_root)?;
    for (key, value) in extra_env {
        command.env(key, value);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch {provider_label} at {} from {}",
            pi_bin.display(),
            repo_root.display()
        )
    })?;
    write_worker_pid(worker_pid_path, child.id())?;

    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("{provider_label} stdout should be piped for {context_label}"))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("{provider_label} stderr should be piped for {context_label}"))?;

    let heartbeat_label = context_label.to_string();
    let stdout_task =
        tokio::spawn(async move { capture_pi_output(stdout, &heartbeat_label, 15).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child
        .wait()
        .await
        .with_context(|| format!("failed waiting for {provider_label}"))?;
    clear_worker_pid(worker_pid_path)?;
    let stdout = stdout_task
        .await
        .with_context(|| format!("{provider_label} stdout capture task panicked"))??;
    let stderr_text = stderr_task
        .await
        .with_context(|| format!("{provider_label} stderr capture task panicked"))??;
    write_stdout_log(stdout_log_path, &stdout)?;
    if let Some(detail) = parse_pi_error(&stdout) {
        bail!("{provider_label} {context_label} failed: {detail}");
    }
    Ok((status, stderr_text))
}

fn configure_pi_env(command: &mut TokioCommand, repo_root: &Path) -> Result<()> {
    let agent_dir = opencode_agent_dir(repo_root);
    fs::create_dir_all(&agent_dir)
        .with_context(|| format!("failed to create {}", agent_dir.display()))?;
    command.env("PI_CODING_AGENT_DIR", &agent_dir);
    command.env("OPENCODE_CODING_AGENT_DIR", &agent_dir);
    Ok(())
}

fn write_stdout_log(stdout_log_path: Option<&Path>, stdout: &str) -> Result<()> {
    let Some(path) = stdout_log_path else {
        return Ok(());
    };
    atomic_write(path, stdout.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
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
    model_context_window: Option<i64>,
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
        .arg(format!("model_reasoning_effort=\"{reasoning_effort}\""));
    if let Some(model_context_window) = model_context_window {
        command
            .arg("-c")
            .arg(format!("model_context_window={model_context_window}"));
    }
    command
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{select_shared_exec_backend, SharedExecBackend};

    #[test]
    fn shared_exec_routes_minimax_alias_to_pi() {
        let backend = select_shared_exec_backend("minimax", Path::new("codex"));
        let SharedExecBackend::Pi {
            provider_label,
            model,
            ..
        } = backend
        else {
            panic!("expected PI backend");
        };
        assert_eq!(provider_label, "pi-minimax");
        assert_eq!(model, "minimax/MiniMax-M2.7-highspeed");
    }

    #[test]
    fn shared_exec_routes_kimi_alias_to_kimi_cli() {
        let backend = select_shared_exec_backend("kimi", Path::new("codex"));
        let SharedExecBackend::KimiCli { model, .. } = backend else {
            panic!("expected kimi-cli backend");
        };
        assert_eq!(model, "kimi");
    }

    #[test]
    fn shared_exec_leaves_codex_models_on_codex() {
        let backend = select_shared_exec_backend("gpt-5.5", Path::new("/tmp/codex-test"));
        assert_eq!(
            backend,
            SharedExecBackend::Codex {
                model: "gpt-5.5".to_string(),
                codex_bin: PathBuf::from("/tmp/codex-test"),
            }
        );
    }
}
