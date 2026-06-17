//! LLM backend layer for `auto bug`: backend selection, process spawning,
//! stream capture, timeout handling, and the Codex fallback wrapper.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::{self, Duration};

use crate::bug_command::{DEFAULT_CODEX_MODEL, DEFAULT_CODEX_REASONING_EFFORT};
use crate::codex_exec::MAX_CODEX_MODEL_CONTEXT_WINDOW;
use crate::codex_stream::{capture_codex_output, capture_pi_output};
use crate::kimi_backend::{
    extract_final_text as kimi_extract_final_text, kimi_exec_args, parse_kimi_error,
    resolve_kimi_bin, resolve_kimi_cli_model,
};
use crate::pi_backend::{parse_pi_error, resolve_pi_bin, PiProvider};
use crate::prompt_ethos::with_autodev_prompt_ethos;
use crate::util::{
    opencode_agent_dir, prune_pi_runtime_state, timestamp_slug, truncate_file_to_max_bytes,
};

const BUG_STDERR_LOG_MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub(crate) enum LlmBackend {
    Codex {
        model: String,
        reasoning_effort: String,
        codex_bin: PathBuf,
    },
    Pi {
        provider_label: &'static str,
        model: String,
        thinking: String,
        pi_bin: PathBuf,
    },
    KimiCli {
        model: String,
        thinking: String,
        kimi_bin: PathBuf,
    },
}

impl LlmBackend {
    pub(crate) fn label(&self) -> &str {
        match self {
            Self::Codex { .. } => "codex",
            Self::Pi { provider_label, .. } => provider_label,
            Self::KimiCli { .. } => "kimi-cli",
        }
    }

    pub(crate) fn model(&self) -> &str {
        match self {
            Self::Codex { model, .. } => model,
            Self::Pi { model, .. } => model,
            Self::KimiCli { model, .. } => model,
        }
    }

    pub(crate) fn effort(&self) -> &str {
        match self {
            Self::Codex {
                reasoning_effort, ..
            } => reasoning_effort,
            Self::Pi { thinking, .. } => thinking,
            Self::KimiCli { thinking, .. } => thinking,
        }
    }

    pub(crate) fn is_kimi_family(&self) -> bool {
        matches!(self, Self::KimiCli { .. })
            || matches!(self, Self::Pi { provider_label, .. } if *provider_label == "pi-kimi")
    }
}

pub(crate) fn select_backend(
    model: &str,
    effort: &str,
    codex_bin: &Path,
    pi_bin: &Path,
    kimi_bin: &Path,
    use_kimi_cli: bool,
) -> LlmBackend {
    if is_kimi_model(model) {
        if use_kimi_cli {
            return LlmBackend::KimiCli {
                model: resolve_kimi_cli_model(model),
                thinking: effort.to_string(),
                kimi_bin: resolve_kimi_bin(kimi_bin),
            };
        }
        if let Some(provider) = PiProvider::detect(model) {
            return LlmBackend::Pi {
                provider_label: provider.provider_label(),
                model: provider.resolve_model(model, DEFAULT_CODEX_MODEL),
                thinking: effort.to_string(),
                pi_bin: resolve_pi_bin(pi_bin),
            };
        }
    }

    if let Some(provider) = PiProvider::detect(model) {
        return LlmBackend::Pi {
            provider_label: provider.provider_label(),
            model: provider.resolve_model(model, DEFAULT_CODEX_MODEL),
            thinking: effort.to_string(),
            pi_bin: resolve_pi_bin(pi_bin),
        };
    }

    LlmBackend::Codex {
        model: model.to_string(),
        reasoning_effort: effort.to_string(),
        codex_bin: codex_bin.to_path_buf(),
    }
}

/// Does this model string refer to a Kimi coding model in any of its recognised
/// spellings? Covers `k2.6`, `kimi`, `kimi-coding/k2p6`, `k2.5`, etc.
pub(crate) fn is_kimi_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower.contains("kimi") || lower.starts_with("k2.") || lower.starts_with("k2p")
}

pub(crate) async fn run_backend_prompt(
    repo_root: &Path,
    prompt: &str,
    backend: &LlmBackend,
    stderr_log_path: &Path,
    stream_label: &str,
    timeout: Duration,
) -> Result<String> {
    let prompt = with_autodev_prompt_ethos(prompt);
    match backend {
        LlmBackend::Codex {
            model,
            reasoning_effort,
            codex_bin,
        } => {
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
                .arg("-c")
                .arg(format!(
                    "model_context_window={MAX_CODEX_MODEL_CONTEXT_WINDOW}"
                ))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .current_dir(repo_root);

            let mut child = command.spawn().with_context(|| {
                format!(
                    "failed to launch Codex at {} from {}",
                    codex_bin.display(),
                    repo_root.display()
                )
            })?;

            let mut stdin = child
                .stdin
                .take()
                .context("Codex stdin should be piped for auto bug")?;
            stdin
                .write_all(prompt.as_bytes())
                .await
                .context("failed to write auto bug prompt to Codex")?;
            drop(stdin);

            let stdout = child
                .stdout
                .take()
                .context("Codex stdout should be piped for auto bug")?;
            let stderr = child
                .stderr
                .take()
                .context("Codex stderr should be piped for auto bug")?;

            let stdout_task = tokio::spawn(async move { capture_codex_output(stdout).await });
            let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

            let wait_result = time::timeout(timeout, child.wait()).await;
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
            append_stderr_log(stderr_log_path, &stderr_text)?;
            if timed_out {
                bail!(
                    "Codex bug phase timed out after {}s while running {stream_label}",
                    timeout.as_secs()
                );
            }
            let status = wait_result
                .expect("timeout already handled")
                .context("failed waiting for Codex")?;

            if !status.success() {
                bail!(
                    "Codex bug phase failed: {}",
                    stderr_text.trim().if_empty_then(stdout.trim())
                );
            }
            Ok(stdout)
        }
        LlmBackend::Pi {
            model,
            thinking,
            pi_bin,
            ..
        } => {
            let mut command = TokioCommand::new(pi_bin);
            command
                .arg("--model")
                .arg(model)
                .arg("--thinking")
                .arg(thinking)
                .arg("--mode")
                .arg("json")
                .arg("-p")
                .arg("--no-session")
                .arg("--tools")
                .arg("read,bash,edit,write,grep,find,ls")
                .arg(&prompt)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .current_dir(repo_root);
            configure_pi_env(&mut command, repo_root)?;

            let mut child = command.spawn().with_context(|| {
                format!(
                    "failed to launch PI at {} from {}",
                    pi_bin.display(),
                    repo_root.display()
                )
            })?;

            let stdout = child
                .stdout
                .take()
                .context("PI stdout should be piped for auto bug")?;
            let stderr = child
                .stderr
                .take()
                .context("PI stderr should be piped for auto bug")?;

            let stream_label = stream_label.to_string();
            let heartbeat_label = stream_label.clone();
            let stdout_task =
                tokio::spawn(async move { capture_pi_output(stdout, &heartbeat_label, 15).await });
            let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

            let wait_result = time::timeout(timeout, child.wait()).await;
            let timed_out = wait_result.is_err();
            if timed_out {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            let stdout = stdout_task
                .await
                .context("PI stdout capture task panicked")??;
            let stderr_text = stderr_task
                .await
                .context("PI stderr capture task panicked")??;
            append_stderr_log(stderr_log_path, &stderr_text)?;
            if timed_out {
                bail!(
                    "PI bug phase timed out after {}s while running {stream_label}",
                    timeout.as_secs()
                );
            }
            let status = wait_result
                .expect("timeout already handled")
                .context("failed waiting for PI")?;

            if !status.success() {
                bail!(
                    "PI bug phase failed: {}",
                    stderr_text
                        .trim()
                        .if_empty_then(parse_pi_error(&stdout).as_deref().unwrap_or(stdout.trim()))
                );
            }
            if let Some(detail) = parse_pi_error(&stdout) {
                bail!("PI bug phase failed: {detail}");
            }
            Ok(stdout)
        }
        LlmBackend::KimiCli {
            model,
            thinking,
            kimi_bin,
        } => {
            let args = kimi_exec_args(model, thinking, &prompt);
            let mut command = TokioCommand::new(kimi_bin);
            command
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .current_dir(repo_root);

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
                .context("kimi-cli stdout should be piped for auto bug")?;
            let stderr = child
                .stderr
                .take()
                .context("kimi-cli stderr should be piped for auto bug")?;

            let stream_label_owned = stream_label.to_string();
            let heartbeat_label = stream_label_owned.clone();
            // Reuse the PI output helper: kimi-cli stream-json frames are
            // JSON lines, same shape family; capture_pi_output preserves raw
            // output + drives the heartbeat.
            let stdout_task =
                tokio::spawn(async move { capture_pi_output(stdout, &heartbeat_label, 15).await });
            let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

            let wait_result = time::timeout(timeout, child.wait()).await;
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
            append_stderr_log(stderr_log_path, &stderr_text)?;
            if timed_out {
                bail!(
                    "kimi-cli bug phase timed out after {}s while running {stream_label_owned}",
                    timeout.as_secs()
                );
            }
            let status = wait_result
                .expect("timeout already handled")
                .context("failed waiting for kimi-cli")?;
            if !status.success() {
                bail!(
                    "kimi-cli bug phase failed: {}",
                    stderr_text.trim().if_empty_then(
                        parse_kimi_error(&stdout)
                            .as_deref()
                            .unwrap_or(stdout.trim())
                    )
                );
            }
            if let Some(detail) = parse_kimi_error(&stdout) {
                bail!("kimi-cli bug phase failed: {detail}");
            }
            // kimi-cli's stream-json puts the final text inside one
            // `{"role":"assistant","content":[{"type":"text",...}]}` frame.
            // Stitch those frames together so downstream JSON extractors see
            // the model's actual answer rather than the trace.
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
                // Fall back to the raw stream so callers still have something
                // to parse; kimi-cli must be reporting text in a non-standard
                // frame shape.
                return Ok(stdout);
            }
            Ok(final_text)
        }
    }
}

/// Wrapper that falls back to Codex when a PI backend terminates.
pub(crate) async fn run_backend_prompt_with_fallback(
    repo_root: &Path,
    prompt: &str,
    backend: &LlmBackend,
    codex_bin: &Path,
    stderr_log_path: &Path,
    stream_label: &str,
    timeout: Duration,
) -> Result<(String, LlmBackend)> {
    match run_backend_prompt(
        repo_root,
        prompt,
        backend,
        stderr_log_path,
        stream_label,
        timeout,
    )
    .await
    {
        Ok(r) => Ok((r, backend.clone())),
        Err(e) if backend.is_kimi_family() => {
            eprintln!("[auto-bug] Kimi backend failed: {e:#}");
            eprintln!("[auto-bug] falling back to Codex");
            let fallback = LlmBackend::Codex {
                model: DEFAULT_CODEX_MODEL.to_string(),
                reasoning_effort: DEFAULT_CODEX_REASONING_EFFORT.to_string(),
                codex_bin: codex_bin.to_path_buf(),
            };
            print_global_phase_header("fallback", &fallback);
            let r = run_backend_prompt(
                repo_root,
                prompt,
                &fallback,
                stderr_log_path,
                &format!("{stream_label} (codex-fallback)"),
                timeout,
            )
            .await?;
            Ok((r, fallback))
        }
        Err(e) => Err(e),
    }
}

pub(crate) fn prune_bug_phase_pi_state(repo_root: &Path, backend: &LlmBackend) {
    if !matches!(backend, LlmBackend::Pi { .. }) {
        return;
    }
    if let Err(err) = prune_pi_runtime_state(repo_root) {
        eprintln!(
            "warning: failed to prune PI runtime state in {}: {err}",
            opencode_agent_dir(repo_root).display()
        );
    }
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

fn append_stderr_log(stderr_log_path: &Path, stderr_text: &str) -> Result<()> {
    if stderr_text.trim().is_empty() {
        return Ok(());
    }
    let entry = format!("\n===== {} =====\n{stderr_text}\n", timestamp_slug());
    let mut existing = if stderr_log_path.exists() {
        fs::read(stderr_log_path)
            .with_context(|| format!("failed to read {}", stderr_log_path.display()))?
    } else {
        Vec::new()
    };
    existing.extend_from_slice(entry.as_bytes());
    crate::util::atomic_write(stderr_log_path, &existing)?;
    truncate_file_to_max_bytes(stderr_log_path, BUG_STDERR_LOG_MAX_BYTES)?;
    Ok(())
}

fn configure_pi_env(command: &mut TokioCommand, repo_root: &Path) -> Result<()> {
    let agent_dir = opencode_agent_dir(repo_root);
    fs::create_dir_all(&agent_dir)
        .with_context(|| format!("failed to create {}", agent_dir.display()))?;
    command.env("PI_CODING_AGENT_DIR", &agent_dir);
    command.env("OPENCODE_CODING_AGENT_DIR", &agent_dir);
    Ok(())
}

pub(crate) fn print_phase_header(
    phase: &str,
    chunk: &crate::bug_command::types::RepoChunk,
    backend: &LlmBackend,
) {
    println!();
    println!("phase:       {phase}");
    println!("chunk:       {}", chunk.id);
    println!("scope:       {}", chunk.scope_label);
    println!("backend:     {}", backend.label());
    println!("model:       {}", backend.model());
    println!("variant:     {}", backend.effort());
}

pub(crate) fn print_global_phase_header(phase: &str, backend: &LlmBackend) {
    println!();
    println!("phase:       {phase}");
    println!("scope:       verified findings");
    println!("backend:     {}", backend.label());
    println!("model:       {}", backend.model());
    println!("variant:     {}", backend.effort());
}

pub(crate) trait EmptyFallback {
    fn if_empty_then<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl EmptyFallback for str {
    fn if_empty_then<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.is_empty() {
            fallback
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{run_backend_prompt, run_backend_prompt_with_fallback, LlmBackend};

    fn temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-bug-{name}-{}-{nonce}", std::process::id()))
    }

    fn write_fake_script(path: &Path, script: &str) {
        fs::write(path, script).expect("failed to write fake pi script");
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(path)
                .expect("failed to stat fake pi script")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("failed to chmod fake pi script");
        }
    }

    fn write_fake_pi_script(path: &Path) {
        write_fake_script(path, "#!/bin/sh\nprintf '[]\\n'\n");
    }

    #[tokio::test]
    async fn pi_cleanup_failure_is_best_effort_after_successful_run() {
        let repo_root = temp_path("pi-cleanup-best-effort");
        fs::create_dir_all(&repo_root).expect("failed to create repo root");
        let agent_dir = repo_root
            .join(".auto")
            .join("opencode-data")
            .join("opencode");
        fs::create_dir_all(&agent_dir).expect("failed to create agent dir");
        fs::write(agent_dir.join("snapshot"), "not a directory")
            .expect("failed to create invalid snapshot path");

        let fake_pi = repo_root.join("fake-pi.sh");
        write_fake_pi_script(&fake_pi);

        let backend = LlmBackend::Pi {
            provider_label: "pi-kimi",
            model: "kimi-coding/k2p6".to_string(),
            thinking: "high".to_string(),
            pi_bin: fake_pi,
        };
        let stderr_log_path = repo_root.join("bug.stderr.log");

        let result = run_backend_prompt(
            &repo_root,
            "prompt",
            &backend,
            &stderr_log_path,
            "test pi cleanup",
            Duration::from_secs(5),
        )
        .await;

        let stdout = result.expect("successful PI output should survive cleanup failures");
        assert_eq!(stdout, "[]\n");
    }

    #[tokio::test]
    async fn pi_timeout_falls_back_to_codex() {
        let repo_root = temp_path("pi-timeout-fallback");
        fs::create_dir_all(&repo_root).expect("failed to create repo root");

        let fake_pi = repo_root.join("fake-pi-sleep.sh");
        write_fake_script(&fake_pi, "#!/bin/sh\nsleep 2\nprintf '[]\\n'\n");

        let fake_codex = repo_root.join("fake-codex.sh");
        write_fake_script(&fake_codex, "#!/bin/sh\ncat >/dev/null\nprintf '[]\\n'\n");

        let backend = LlmBackend::Pi {
            provider_label: "pi-kimi",
            model: "kimi-coding/k2p6".to_string(),
            thinking: "high".to_string(),
            pi_bin: fake_pi,
        };
        let stderr_log_path = repo_root.join("bug.stderr.log");

        let (stdout, used_backend) = run_backend_prompt_with_fallback(
            &repo_root,
            "prompt",
            &backend,
            &fake_codex,
            &stderr_log_path,
            "timeout fallback",
            Duration::from_millis(250),
        )
        .await
        .expect("timeout should fall back to codex");

        assert_eq!(stdout, "[]\n");
        assert!(matches!(used_backend, LlmBackend::Codex { .. }));
    }
}
