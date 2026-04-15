use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::sleep;

use crate::codex_stream;
use crate::quota_config::Provider;
use crate::quota_exec;
use crate::util::{atomic_write, timestamp_slug};

#[derive(Clone, Debug)]
pub(crate) struct TmuxCodexRunConfig {
    pub(crate) session_name: String,
    pub(crate) window_name: String,
    pub(crate) run_dir: PathBuf,
    pub(crate) lane_label: String,
}

pub(crate) async fn run_codex_exec(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    stderr_log_path: &Path,
    context_label: &str,
) -> Result<std::process::ExitStatus> {
    run_codex_exec_with_env(
        repo_root,
        full_prompt,
        model,
        reasoning_effort,
        codex_bin,
        stderr_log_path,
        context_label,
        &[],
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
    context_label: &str,
    extra_env: &[(String, String)],
) -> Result<std::process::ExitStatus> {
    let (status, stderr_text) = if quota_exec::is_quota_available(Provider::Codex) {
        let repo_root = repo_root.to_owned();
        let full_prompt = full_prompt.to_owned();
        let model = model.to_owned();
        let reasoning_effort = reasoning_effort.to_owned();
        let codex_bin = codex_bin.to_owned();
        let context_label = context_label.to_owned();
        let extra_env = extra_env.to_vec();
        let result = quota_exec::run_with_quota(Provider::Codex, move || {
            let repo_root = repo_root.clone();
            let full_prompt = full_prompt.clone();
            let model = model.clone();
            let reasoning_effort = reasoning_effort.clone();
            let codex_bin = codex_bin.clone();
            let context_label = context_label.clone();
            let extra_env = extra_env.clone();
            async move {
                spawn_codex(
                    &repo_root,
                    &full_prompt,
                    &model,
                    &reasoning_effort,
                    &codex_bin,
                    &context_label,
                    &extra_env,
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
            context_label,
            extra_env,
        )
        .await?
    };
    log_stderr(&stderr_text, stderr_log_path)?;
    Ok(status)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_codex_exec_in_tmux_with_env(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    stderr_log_path: &Path,
    context_label: &str,
    extra_env: &[(String, String)],
    tmux: &TmuxCodexRunConfig,
) -> Result<std::process::ExitStatus> {
    if let Some((status, stderr_text)) = read_completed_success(&tmux.run_dir)? {
        log_stderr(&stderr_text, stderr_log_path)?;
        return Ok(status);
    }
    if tmux_worker_is_alive(&tmux.run_dir)? {
        println!(
            "tmux lane already running for {context_label}: tmux attach -t {}",
            tmux.session_name
        );
        let (status, stderr_text) = wait_for_tmux_completion(&tmux.run_dir).await?;
        log_stderr(&stderr_text, stderr_log_path)?;
        return Ok(status);
    }

    let (status, stderr_text) = if quota_exec::is_quota_available(Provider::Codex) {
        let repo_root = repo_root.to_owned();
        let full_prompt = full_prompt.to_owned();
        let model = model.to_owned();
        let reasoning_effort = reasoning_effort.to_owned();
        let codex_bin = codex_bin.to_owned();
        let context_label = context_label.to_owned();
        let extra_env = extra_env.to_vec();
        let tmux = tmux.clone();
        let result = quota_exec::run_with_quota(Provider::Codex, move || {
            let repo_root = repo_root.clone();
            let full_prompt = full_prompt.clone();
            let model = model.clone();
            let reasoning_effort = reasoning_effort.clone();
            let codex_bin = codex_bin.clone();
            let context_label = context_label.clone();
            let extra_env = extra_env.clone();
            let tmux = tmux.clone();
            async move {
                spawn_codex_in_tmux(
                    &repo_root,
                    &full_prompt,
                    &model,
                    &reasoning_effort,
                    &codex_bin,
                    &context_label,
                    &extra_env,
                    &tmux,
                )
                .await
            }
        })
        .await?;
        (result.exit_status, result.stderr_text)
    } else {
        spawn_codex_in_tmux(
            repo_root,
            full_prompt,
            model,
            reasoning_effort,
            codex_bin,
            context_label,
            extra_env,
            tmux,
        )
        .await?
    };
    log_stderr(&stderr_text, stderr_log_path)?;
    Ok(status)
}

pub(crate) fn ensure_tmux_lanes(session_name: &str, lanes: usize, cwd: &Path) -> Result<()> {
    for lane in 1..=lanes.max(1) {
        ensure_tmux_lane(session_name, &format!("lane-{lane}"), cwd)?;
    }
    Ok(())
}

async fn spawn_codex(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    context_label: &str,
    extra_env: &[(String, String)],
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

    let stdout_task = tokio::spawn(async move { codex_stream::stream_codex_output(stdout).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child.wait().await.context("failed waiting for Codex")?;
    stdout_task
        .await
        .context("Codex stdout streaming task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Codex stderr capture task panicked")??;

    Ok((status, stderr_text))
}

#[allow(clippy::too_many_arguments)]
async fn spawn_codex_in_tmux(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    context_label: &str,
    extra_env: &[(String, String)],
    tmux: &TmuxCodexRunConfig,
) -> Result<(std::process::ExitStatus, String)> {
    fs::create_dir_all(&tmux.run_dir)
        .with_context(|| format!("failed to create {}", tmux.run_dir.display()))?;
    let prompt_path = tmux.run_dir.join("prompt.md");
    let script_path = tmux.run_dir.join("run.sh");
    let stdout_path = tmux.run_dir.join("stdout.log");
    let stderr_path = tmux.run_dir.join("stderr.log");
    let status_path = tmux.run_dir.join("status");
    let done_path = tmux.run_dir.join("done");
    let pid_path = tmux.run_dir.join("pid");

    if done_path.exists() {
        remove_if_exists(&done_path)?;
    }
    if status_path.exists() {
        remove_if_exists(&status_path)?;
    }
    if tmux_worker_is_alive(&tmux.run_dir)? {
        println!(
            "tmux lane already running for {context_label}: tmux attach -t {}",
            tmux.session_name
        );
        return wait_for_tmux_completion(&tmux.run_dir).await;
    }
    remove_if_exists(&stdout_path)?;
    remove_if_exists(&stderr_path)?;

    atomic_write(&prompt_path, full_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    atomic_write(
        &script_path,
        render_tmux_codex_script(
            repo_root,
            codex_bin,
            model,
            reasoning_effort,
            extra_env,
            &prompt_path,
            &stdout_path,
            &stderr_path,
            &status_path,
            &done_path,
            &pid_path,
            &tmux.lane_label,
        )
        .as_bytes(),
    )
    .with_context(|| format!("failed to write {}", script_path.display()))?;

    ensure_tmux_lane(&tmux.session_name, &tmux.window_name, repo_root)?;
    let target = format!("{}:{}", tmux.session_name, tmux.window_name);
    run_tmux_owned(vec![
        "send-keys".to_string(),
        "-t".to_string(),
        target.clone(),
        "C-c".to_string(),
    ])?;
    run_tmux_owned(vec![
        "send-keys".to_string(),
        "-t".to_string(),
        target.clone(),
        "clear".to_string(),
        "C-m".to_string(),
    ])?;
    run_tmux_owned(vec![
        "send-keys".to_string(),
        "-t".to_string(),
        target.clone(),
        format!("bash {}", shell_quote_path(&script_path)),
        "C-m".to_string(),
    ])?;
    println!(
        "tmux lane: {} -> {} ({})",
        tmux.lane_label, target, context_label
    );
    println!("attach:    tmux attach -t {}", tmux.session_name);

    wait_for_tmux_completion(&tmux.run_dir).await
}

#[allow(clippy::too_many_arguments)]
fn render_tmux_codex_script(
    repo_root: &Path,
    codex_bin: &Path,
    model: &str,
    reasoning_effort: &str,
    extra_env: &[(String, String)],
    prompt_path: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
    status_path: &Path,
    done_path: &Path,
    pid_path: &Path,
    lane_label: &str,
) -> String {
    let mut exports = String::new();
    for (key, value) in extra_env {
        exports.push_str(&format!("export {}={}\n", key, shell_quote(value)));
    }
    format!(
        r#"#!/usr/bin/env bash
set +e
cd {repo_root}
{exports}printf '%s\n' "$$" > {pid_path}
echo "[auto-loop] {lane_label} starting"
echo "[auto-loop] repo: {repo_root}"
echo "[auto-loop] prompt: {prompt_path}"
echo "[auto-loop] stdout log: {stdout_path}"
echo "[auto-loop] stderr log: {stderr_path}"
{codex_bin} exec \
  --dangerously-bypass-approvals-and-sandbox \
  --skip-git-repo-check \
  --cd {repo_root} \
  -m {model} \
  -c {reasoning_effort_arg} \
  < {prompt_path} \
  > >(tee -a {stdout_path}) \
  2> >(tee -a {stderr_path} >&2)
status=$?
printf '%s\n' "$status" > {status_path}
rm -f {pid_path}
touch {done_path}
echo "[auto-loop] {lane_label} finished with status $status"
echo "[auto-loop] leaving shell open for inspection"
exec "${{SHELL:-bash}}"
"#,
        repo_root = shell_quote_path(repo_root),
        exports = exports,
        pid_path = shell_quote_path(pid_path),
        lane_label = lane_label,
        prompt_path = shell_quote_path(prompt_path),
        stdout_path = shell_quote_path(stdout_path),
        stderr_path = shell_quote_path(stderr_path),
        codex_bin = shell_quote_path(codex_bin),
        model = shell_quote(model),
        reasoning_effort_arg =
            shell_quote(&format!("model_reasoning_effort=\"{reasoning_effort}\"")),
        status_path = shell_quote_path(status_path),
        done_path = shell_quote_path(done_path),
    )
}

async fn wait_for_tmux_completion(run_dir: &Path) -> Result<(std::process::ExitStatus, String)> {
    let done_path = run_dir.join("done");
    let status_path = run_dir.join("status");
    let stdout_path = run_dir.join("stdout.log");
    let stderr_path = run_dir.join("stderr.log");
    loop {
        if done_path.exists() {
            let status = read_status(&status_path)?;
            let stdout_text = fs::read_to_string(&stdout_path).unwrap_or_default();
            let mut stderr_text = fs::read_to_string(&stderr_path).unwrap_or_default();
            if codex_stdout_has_agent_progress(&stdout_text) {
                stderr_text.push_str("\n[auto-loop] agent-progress-detected=true\n");
            }
            return Ok((status, stderr_text));
        }
        sleep(Duration::from_secs(2)).await;
    }
}

fn read_completed_success(run_dir: &Path) -> Result<Option<(std::process::ExitStatus, String)>> {
    let done_path = run_dir.join("done");
    if !done_path.exists() {
        return Ok(None);
    }
    let status = read_status(&run_dir.join("status"))?;
    if !status.success() {
        return Ok(None);
    }
    let stderr_text = fs::read_to_string(run_dir.join("stderr.log")).unwrap_or_default();
    Ok(Some((status, stderr_text)))
}

fn codex_stdout_has_agent_progress(stdout: &str) -> bool {
    let lower = stdout.to_ascii_lowercase();
    lower.contains("tokens used")
        || lower.contains("\nexec\n")
        || lower.contains("\napply_patch")
        || lower.contains("patch applied")
        || lower.contains("files changed")
}

fn read_status(path: &Path) -> Result<std::process::ExitStatus> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let code = text
        .trim()
        .parse::<i32>()
        .with_context(|| format!("invalid status in {}", path.display()))?;
    Ok(std::process::ExitStatus::from_raw(code << 8))
}

fn read_pid(path: &Path) -> Result<Option<u32>> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(text.trim().parse::<u32>().ok())
}

fn tmux_worker_is_alive(run_dir: &Path) -> Result<bool> {
    let pid_path = run_dir.join("pid");
    if let Some(pid) = read_pid(&pid_path)? {
        if process_alive(pid) {
            return Ok(true);
        }
        remove_if_exists(&pid_path)?;
    }
    Ok(false)
}

fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn ensure_tmux_lane(session_name: &str, window_name: &str, cwd: &Path) -> Result<()> {
    if !tmux_session_exists(session_name)? {
        run_tmux_owned(vec![
            "new-session".to_string(),
            "-d".to_string(),
            "-s".to_string(),
            session_name.to_string(),
            "-n".to_string(),
            window_name.to_string(),
            "-c".to_string(),
            cwd.display().to_string(),
        ])?;
        return Ok(());
    }
    if !tmux_window_exists(session_name, window_name)? {
        run_tmux_owned(vec![
            "new-window".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "-n".to_string(),
            window_name.to_string(),
            "-c".to_string(),
            cwd.display().to_string(),
        ])?;
    }
    Ok(())
}

fn tmux_session_exists(session_name: &str) -> Result<bool> {
    let output = std::process::Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .output()
        .context("failed to launch tmux")?;
    Ok(output.status.success())
}

fn tmux_window_exists(session_name: &str, window_name: &str) -> Result<bool> {
    let output = std::process::Command::new("tmux")
        .args(["list-windows", "-t", session_name, "-F", "#{window_name}"])
        .output()
        .context("failed to launch tmux")?;
    if !output.status.success() {
        bail!(
            "tmux list-windows failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|name| name == window_name))
}

fn run_tmux_owned(args: Vec<String>) -> Result<()> {
    let output = std::process::Command::new("tmux")
        .args(&args)
        .output()
        .context("failed to launch tmux")?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "tmux command failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn shell_quote_path(path: &Path) -> String {
    shell_quote(&path.display().to_string())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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

#[cfg(test)]
mod tests {
    use super::codex_stdout_has_agent_progress;

    #[test]
    fn detects_tmux_stdout_progress_for_quota_guard() {
        assert!(codex_stdout_has_agent_progress(
            "analysis\nexec\n{\"cmd\":\"cargo test\"}\ntokens used: 1200"
        ));
    }

    #[test]
    fn immediate_startup_output_is_not_progress() {
        assert!(!codex_stdout_has_agent_progress(
            "[auto-loop] lane-1 P-015 starting"
        ));
    }
}
