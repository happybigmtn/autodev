//! LLM backend layer for `auto nemesis`: backend selection, process spawning,
//! stream capture, and the Codex fallback wrapper.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::codex_exec::MAX_CODEX_MODEL_CONTEXT_WINDOW;
use crate::codex_stream::{capture_codex_output, capture_pi_output};
use crate::kimi_backend::{
    extract_final_text as kimi_extract_final_text, kimi_exec_args, parse_kimi_error,
    resolve_kimi_bin, resolve_kimi_cli_model,
};
use crate::nemesis::DEFAULT_CODEX_NEMESIS_MODEL;
use crate::pi_backend::{parse_pi_error, resolve_pi_bin, PiProvider};
use crate::prompt_ethos::with_autodev_prompt_ethos;
use crate::util::opencode_agent_dir;

pub(crate) enum NemesisBackend {
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

impl NemesisBackend {
    pub(crate) fn label(&self) -> &'static str {
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

    pub(crate) fn variant(&self) -> &str {
        match self {
            Self::Codex {
                reasoning_effort, ..
            } => reasoning_effort,
            Self::Pi { thinking, .. } => thinking,
            Self::KimiCli { thinking, .. } => thinking,
        }
    }

    #[allow(dead_code)]
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
) -> NemesisBackend {
    let is_kimi = is_kimi_model(model);
    if is_kimi && use_kimi_cli {
        return NemesisBackend::KimiCli {
            model: resolve_kimi_cli_model(model),
            thinking: effort.to_string(),
            kimi_bin: resolve_kimi_bin(kimi_bin),
        };
    }

    if let Some(provider) = PiProvider::detect(model) {
        return NemesisBackend::Pi {
            provider_label: provider.provider_label(),
            model: provider.resolve_model(model, DEFAULT_CODEX_NEMESIS_MODEL),
            thinking: effort.to_string(),
            pi_bin: resolve_pi_bin(pi_bin),
        };
    }

    NemesisBackend::Codex {
        model: model.to_string(),
        reasoning_effort: effort.to_string(),
        codex_bin: codex_bin.to_path_buf(),
    }
}

pub(crate) fn is_kimi_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower.contains("kimi") || lower.starts_with("k2.") || lower.starts_with("k2p")
}

pub(crate) fn print_phase_header(phase: &str, backend: &NemesisBackend) {
    println!();
    println!("phase:       {phase}");
    println!("backend:     {}", backend.label());
    println!("model:       {}", backend.model());
    println!("variant:     {}", backend.variant());
}

pub(crate) async fn run_nemesis_backend(
    repo_root: &Path,
    prompt: &str,
    backend: &NemesisBackend,
    codex_bin: &Path,
) -> Result<String> {
    let prompt = with_autodev_prompt_ethos(prompt);
    match backend {
        NemesisBackend::Codex {
            model,
            reasoning_effort,
            codex_bin,
        } => run_codex(repo_root, &prompt, model, reasoning_effort, codex_bin).await,
        NemesisBackend::Pi {
            model,
            thinking,
            pi_bin,
            ..
        } => match run_pi(repo_root, &prompt, model, thinking, pi_bin).await {
            Ok(output) => Ok(output),
            Err(e) => {
                eprintln!("[auto-nemesis] Kimi (pi) backend failed: {e:#}");
                eprintln!("[auto-nemesis] falling back to Codex");
                let fallback = NemesisBackend::Codex {
                    model: DEFAULT_CODEX_NEMESIS_MODEL.to_string(),
                    reasoning_effort: "high".to_string(),
                    codex_bin: codex_bin.to_path_buf(),
                };
                print_phase_header("fallback", &fallback);
                run_codex(
                    repo_root,
                    &prompt,
                    DEFAULT_CODEX_NEMESIS_MODEL,
                    "high",
                    codex_bin,
                )
                .await
            }
        },
        NemesisBackend::KimiCli {
            model,
            thinking,
            kimi_bin,
        } => match run_kimi_cli(repo_root, &prompt, model, thinking, kimi_bin).await {
            Ok(output) => Ok(output),
            Err(e) => {
                eprintln!("[auto-nemesis] kimi-cli backend failed: {e:#}");
                eprintln!("[auto-nemesis] falling back to Codex");
                let fallback = NemesisBackend::Codex {
                    model: DEFAULT_CODEX_NEMESIS_MODEL.to_string(),
                    reasoning_effort: "high".to_string(),
                    codex_bin: codex_bin.to_path_buf(),
                };
                print_phase_header("fallback", &fallback);
                run_codex(
                    repo_root,
                    &prompt,
                    DEFAULT_CODEX_NEMESIS_MODEL,
                    "high",
                    codex_bin,
                )
                .await
            }
        },
    }
}

async fn run_kimi_cli(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    thinking: &str,
    kimi_bin: &Path,
) -> Result<String> {
    let args = kimi_exec_args(model, thinking, prompt);
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
        .context("kimi-cli stdout should be piped for nemesis")?;
    let stderr = child
        .stderr
        .take()
        .context("kimi-cli stderr should be piped for nemesis")?;

    let stdout_task =
        tokio::spawn(async move { capture_pi_output(stdout, "auto nemesis kimi-cli", 15).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child
        .wait()
        .await
        .context("failed waiting for kimi-cli nemesis run")?;
    let stdout = stdout_task
        .await
        .context("kimi-cli stdout capture task panicked")??;
    let stderr = stderr_task
        .await
        .context("kimi-cli stderr capture task panicked")??;
    if !status.success() {
        bail!(
            "kimi-cli nemesis run failed: {}",
            stderr.trim().if_empty_then(
                parse_kimi_error(&stdout)
                    .as_deref()
                    .unwrap_or(stdout.trim())
            )
        );
    }
    if let Some(detail) = parse_kimi_error(&stdout) {
        bail!("kimi-cli nemesis run failed: {detail}");
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
        return Ok(stdout);
    }
    Ok(final_text)
}

async fn run_codex(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<String> {
    let mut child = TokioCommand::new(codex_bin)
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
        .current_dir(repo_root)
        .spawn()
        .with_context(|| {
            format!(
                "failed to launch Codex at {} from {}",
                codex_bin.display(),
                repo_root.display()
            )
        })?;

    let mut stdin = child
        .stdin
        .take()
        .context("Codex stdin missing for Nemesis run")?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .context("failed to write Nemesis prompt to Codex")?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .context("Codex stdout missing for Nemesis run")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex stderr missing for Nemesis run")?;

    let stdout_task = tokio::spawn(async move { capture_codex_output(stdout).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child
        .wait()
        .await
        .context("failed waiting for Codex Nemesis run")?;
    let stdout = stdout_task
        .await
        .context("Codex stdout capture task panicked")??;
    let stderr = stderr_task
        .await
        .context("Codex stderr capture task panicked")??;
    if status.success() {
        return Ok(stdout);
    }
    bail!(
        "Codex Nemesis run failed: {}",
        stderr.trim().if_empty_then(stdout.trim())
    );
}

async fn run_pi(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    thinking: &str,
    pi_bin: &Path,
) -> Result<String> {
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
        .arg(prompt)
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
        .context("PI stdout missing for Nemesis run")?;
    let stderr = child
        .stderr
        .take()
        .context("PI stderr missing for Nemesis run")?;

    let stream_label = "nemesis".to_string();
    let stdout_task =
        tokio::spawn(async move { capture_pi_output(stdout, &stream_label, 15).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child
        .wait()
        .await
        .context("failed waiting for PI Nemesis run")?;
    let stdout = stdout_task
        .await
        .context("PI stdout capture task panicked")??;
    let stderr = stderr_task
        .await
        .context("PI stderr capture task panicked")??;
    if status.success() {
        if let Some(detail) = parse_pi_error(&stdout) {
            bail!("PI Nemesis run failed: {detail}");
        }
        return Ok(stdout);
    }
    bail!(
        "PI Nemesis run failed: {}",
        stderr
            .trim()
            .if_empty_then(parse_pi_error(&stdout).as_deref().unwrap_or(stdout.trim()))
    );
}

fn configure_pi_env(command: &mut TokioCommand, repo_root: &Path) -> Result<()> {
    let agent_dir = opencode_agent_dir(repo_root);
    fs::create_dir_all(&agent_dir)
        .with_context(|| format!("failed to create {}", agent_dir.display()))?;
    command.env("PI_CODING_AGENT_DIR", &agent_dir);
    command.env("OPENCODE_CODING_AGENT_DIR", &agent_dir);
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

trait EmptyFallback {
    fn if_empty_then<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl EmptyFallback for str {
    fn if_empty_then<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.trim().is_empty() {
            fallback
        } else {
            self
        }
    }
}
