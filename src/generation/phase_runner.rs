//! Codex and Claude process spawning for the generation pipeline's authoring
//! and independent-review phases.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::claude_exec::looks_like_claude_model;
use crate::codex_exec::{run_codex_exec_max_context, run_codex_exec_max_context_with_env};
use crate::generation::format_duration;
use crate::process_group::{AbortOnDropTask, ContainedChild};
use crate::util::{atomic_write, timestamp_slug};

pub(crate) struct PhaseRunSummary {
    pub(crate) prompt_path: PathBuf,
    pub(crate) response_path: Option<PathBuf>,
}

pub(crate) struct CodexReviewRunSummary {
    pub(crate) prompt_path: PathBuf,
    pub(crate) stderr_log_path: PathBuf,
    pub(crate) report_path: PathBuf,
}

struct ReviewProcess<'a> {
    executable: &'a Path,
    extra_env: &'a [(String, String)],
}

pub(crate) fn codex_review_report_path(repo_root: &Path, phase_slug: &str) -> PathBuf {
    repo_root
        .join(".auto")
        .join("logs")
        .join(format!("{phase_slug}-{}-report.md", timestamp_slug()))
}

pub(crate) async fn run_logged_codex_review(
    repo_root: &Path,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    report_path: &Path,
) -> Result<CodexReviewRunSummary> {
    run_logged_codex_review_with_env(
        repo_root,
        phase_slug,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
        report_path,
        &[],
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_logged_codex_review_with_env(
    repo_root: &Path,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    report_path: &Path,
    extra_env: &[(String, String)],
) -> Result<CodexReviewRunSummary> {
    // The function name is historical: it now routes to Claude when the
    // operator picks an opus/sonnet/claude alias for `--review-model`. The
    // codex path still applies for gpt-5.6-sol and explicit codex models.
    if author_phase_uses_claude_model(model) {
        let claude_bin = review_claude_bin(codex_bin);
        let process = ReviewProcess {
            executable: claude_bin,
            extra_env,
        };
        return run_logged_claude_review(
            repo_root,
            &process,
            phase_slug,
            prompt,
            model,
            reasoning_effort,
            report_path,
        )
        .await;
    }
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("{phase_slug}-{}-prompt.md", timestamp_slug()));
    let stderr_log_path = prompt_path.with_file_name(
        prompt_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("codex-review-prompt.md")
            .replace("-prompt.md", "-stderr.log"),
    );
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("phase:       {phase_slug}");
    println!("model:       {model}");
    println!("effort:      {reasoning_effort}");
    println!("context:     max");
    println!("codex bin:   {}", codex_bin.display());
    println!("prompt log:  {}", prompt_path.display());
    println!("stderr log:  {}", stderr_log_path.display());
    println!("report path: {}", report_path.display());

    let status = run_codex_exec_max_context_with_env(
        repo_root,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
        &stderr_log_path,
        None,
        phase_slug,
        extra_env,
    )
    .await?;
    if !status.success() {
        bail!(
            "independent review phase `{phase_slug}` failed with status {status}; see {}",
            stderr_log_path.display()
        );
    }
    verify_codex_review_report(report_path)?;
    Ok(CodexReviewRunSummary {
        prompt_path,
        stderr_log_path,
        report_path: report_path.to_path_buf(),
    })
}

async fn run_logged_claude_review(
    repo_root: &Path,
    process: &ReviewProcess<'_>,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    report_path: &Path,
) -> Result<CodexReviewRunSummary> {
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("{phase_slug}-{}-prompt.md", timestamp_slug()));
    let stderr_log_path = prompt_path.with_file_name(
        prompt_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("claude-review-prompt.md")
            .replace("-prompt.md", "-stderr.log"),
    );
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("phase:       {phase_slug}");
    println!("model:       {model} (claude)");
    println!("effort:      {reasoning_effort}");
    println!("context:     max");
    println!("prompt log:  {}", prompt_path.display());
    println!("stderr log:  {}", stderr_log_path.display());
    println!("report path: {}", report_path.display());

    // Claude reviews can be longer than authoring; give them generous turns.
    let response = run_claude_prompt(
        repo_root,
        process.executable,
        prompt,
        model,
        reasoning_effort,
        500,
        phase_slug,
        &prompt_path,
        process.extra_env,
    )
    .await?;
    if !response.trim().is_empty() {
        let response_path = prompt_path.with_file_name(
            prompt_path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("claude-review-prompt.md")
                .replace("-prompt.md", "-response.txt"),
        );
        atomic_write(&response_path, response.as_bytes())
            .with_context(|| format!("failed to write {}", response_path.display()))?;
    }
    verify_codex_review_report(report_path)?;
    Ok(CodexReviewRunSummary {
        prompt_path,
        stderr_log_path,
        report_path: report_path.to_path_buf(),
    })
}

fn review_claude_bin(codex_bin: &Path) -> &Path {
    if cfg!(test) && codex_bin != Path::new("codex") {
        // The review-gate tests inject their fake reviewer through the existing
        // binary argument. Keep that seam direct and offline for Claude aliases.
        return codex_bin;
    }
    Path::new("claude")
}

fn verify_codex_review_report(report_path: &Path) -> Result<()> {
    if !report_path.exists() {
        bail!(
            "independent review completed but did not write required report {}",
            report_path.display()
        );
    }
    let report = fs::read_to_string(report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    if report.trim().is_empty() {
        bail!(
            "independent review report {} must not be empty",
            report_path.display()
        );
    }
    Ok(())
}

pub(crate) async fn run_logged_author_phase(
    repo_root: &Path,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    max_turns: usize,
    codex_bin: &Path,
) -> Result<PhaseRunSummary> {
    if author_phase_uses_claude_model(model) {
        return run_logged_claude_phase(
            repo_root,
            phase_slug,
            prompt,
            model,
            reasoning_effort,
            max_turns,
        )
        .await;
    }
    run_logged_codex_author_phase(
        repo_root,
        phase_slug,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
    )
    .await
}

async fn run_logged_codex_author_phase(
    repo_root: &Path,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<PhaseRunSummary> {
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("{phase_slug}-{}-prompt.md", timestamp_slug()));
    let stdout_log_path = prompt_path.with_file_name(
        prompt_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("author-prompt.md")
            .replace("-prompt.md", "-stdout.log"),
    );
    let stderr_log_path = prompt_path.with_file_name(
        prompt_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("author-prompt.md")
            .replace("-prompt.md", "-stderr.log"),
    );
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("phase:       {phase_slug}");
    println!("model:       {model}");
    println!("effort:      {reasoning_effort}");
    println!("context:     max");
    println!("codex bin:   {}", codex_bin.display());
    println!("prompt log:  {}", prompt_path.display());
    println!("stdout log:  {}", stdout_log_path.display());
    println!("stderr log:  {}", stderr_log_path.display());

    let status = run_codex_exec_max_context(
        repo_root,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
        &stderr_log_path,
        Some(&stdout_log_path),
        phase_slug,
    )
    .await?;
    if !status.success() {
        bail!(
            "Codex authoring phase `{phase_slug}` failed with status {status}; see {}",
            stderr_log_path.display()
        );
    }
    Ok(PhaseRunSummary {
        prompt_path,
        response_path: Some(stdout_log_path),
    })
}

pub(crate) fn author_phase_uses_claude_model(model: &str) -> bool {
    model.trim().is_empty() || looks_like_claude_model(model)
}

async fn run_logged_claude_phase(
    repo_root: &Path,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    max_turns: usize,
) -> Result<PhaseRunSummary> {
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("{phase_slug}-{}-prompt.md", timestamp_slug()));
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("phase:       {phase_slug}");
    println!("prompt log:  {}", prompt_path.display());
    let response = run_claude_prompt(
        repo_root,
        Path::new("claude"),
        prompt,
        model,
        reasoning_effort,
        max_turns,
        phase_slug,
        &prompt_path,
        &[],
    )
    .await?;
    let response_path = if response.trim().is_empty() {
        None
    } else {
        let path = prompt_path.with_file_name(
            prompt_path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("phase-response.txt")
                .replace("-prompt.md", "-response.txt"),
        );
        atomic_write(&path, response.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        Some(path)
    };
    Ok(PhaseRunSummary {
        prompt_path,
        response_path,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_claude_prompt(
    repo_root: &Path,
    claude_bin: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    max_turns: usize,
    phase_label: &str,
    prompt_path: &Path,
    extra_env: &[(String, String)],
) -> Result<String> {
    let phase_started_at = Instant::now();
    let mut command = Command::new(claude_bin);
    command
        .arg("-p")
        .arg("--verbose")
        .arg("--dangerously-skip-permissions")
        .arg("--model")
        .arg(model)
        .arg("--effort")
        .arg(reasoning_effort)
        .arg("--max-turns")
        .arg(max_turns.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = ContainedChild::spawn(&mut command)
        .with_context(|| format!("failed to launch Claude for {phase_label}"))?;
    let pid = child.id();
    println!("model:       {model}");
    println!("effort:      {reasoning_effort}");
    println!("max turns:   {max_turns}");
    println!("phase start: {phase_label}");
    println!(
        "claude pid:  {}",
        pid.map_or_else(|| "unknown".to_string(), |pid| pid.to_string())
    );
    println!("cwd:         {}", repo_root.display());
    println!("prompt file: {}", prompt_path.display());

    let mut stdin = child.take_stdin().context("Claude stdin missing")?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .with_context(|| format!("failed to write prompt for {phase_label}"))?;
    drop(stdin);

    let stdout = child.take_stdout().context("Claude stdout missing")?;
    let stderr = child.take_stderr().context("Claude stderr missing")?;
    let stdout_task = AbortOnDropTask::spawn(crate::backend_process::read_stream_bytes(stdout));
    let stderr_task = AbortOnDropTask::spawn(crate::backend_process::read_stream_bytes(stderr));
    let status = child
        .wait()
        .await
        .with_context(|| format!("failed waiting for Claude during {phase_label}"))?;
    let stdout = stdout_task
        .join()
        .await
        .context("Claude stdout task panicked")??;
    let stderr = stderr_task
        .join()
        .await
        .context("Claude stderr task panicked")??;
    let stdout = String::from_utf8(stdout).context("Claude stdout was not valid UTF-8")?;
    let stderr = String::from_utf8(stderr).context("Claude stderr was not valid UTF-8")?;
    if status.success() {
        println!(
            "phase done:  {phase_label} (+{})",
            format_duration(phase_started_at.elapsed())
        );
        return Ok(stdout.trim().to_string());
    }
    println!(
        "phase fail:  {phase_label} (+{})",
        format_duration(phase_started_at.elapsed())
    );
    let detail = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        "no stderr/stdout".to_string()
    };
    bail!("{phase_label} failed: {detail}");
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::path::{Path, PathBuf};
    #[cfg(unix)]
    use std::process::Stdio;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    use super::{
        author_phase_uses_claude_model, run_logged_codex_review, run_logged_codex_review_with_env,
    };

    #[cfg(unix)]
    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-phase-runner-{label}-{}-{}",
            std::process::id(),
            crate::util::timestamp_slug()
        ));
        fs::create_dir_all(path.join(".auto/logs")).expect("create review log dir");
        path
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write fake executable");
        let mut permissions = fs::metadata(path).expect("fake metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod fake executable");
    }

    #[cfg(unix)]
    async fn assert_recorded_pid_gone(pid_path: &Path) {
        let pid = fs::read_to_string(pid_path)
            .expect("read descendant pid")
            .trim()
            .to_string();
        for _ in 0..50 {
            let alive = std::process::Command::new("kill")
                .arg("-0")
                .arg(&pid)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !alive {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("descendant process {pid} was still alive");
    }

    #[test]
    fn generation_author_backend_uses_codex_for_non_claude_models() {
        assert!(author_phase_uses_claude_model("claude-sonnet-4-6"));
        assert!(author_phase_uses_claude_model("sonnet"));
        assert!(author_phase_uses_claude_model("fable 5"));
        assert!(author_phase_uses_claude_model("fable-5"));
        assert!(!author_phase_uses_claude_model("gpt-5.6-sol"));
        assert!(!author_phase_uses_claude_model("o3"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn review_extra_environment_reaches_codex_and_claude_backends() {
        let root = temp_dir("review-extra-env");
        let extra_env = vec![("REVIEW_ENV_PROBE".to_string(), "present".to_string())];

        for (label, model) in [("codex", "gpt-test"), ("claude", "opus")] {
            let fake = root.join(format!("fake-{label}"));
            let report = root.join(format!("{label}-report.md"));
            write_executable(
                &fake,
                &format!(
                    "#!/bin/sh\n\
                     [ \"${{REVIEW_ENV_PROBE:-}}\" = present ] || exit 45\n\
                     cat >/dev/null\n\
                     printf 'VERDICT: CLEAN\\n' > '{}'\n",
                    report.display()
                ),
            );

            run_logged_codex_review_with_env(
                &root,
                &format!("{label}-guarded-review"),
                "review",
                model,
                "high",
                &fake,
                &report,
                &extra_env,
            )
            .await
            .unwrap_or_else(|err| panic!("{label} review did not inherit extra env: {err:#}"));
        }

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn successful_codex_review_kills_delayed_pipe_holding_descendant() {
        let root = temp_dir("codex-success-contained");
        let fake = root.join("fake-codex");
        let sentinel = root.join("delayed-sentinel");
        let source = root.join("reviewed-source.rs");
        let direct_pid_path = root.join("direct.pid");
        let pid_path = root.join("descendant.pid");
        fs::write(&source, "original\n").expect("write reviewed source");
        let report = root.join("report.md");
        write_executable(
            &fake,
            &format!(
                "#!/usr/bin/env bash\necho $$ > '{}'\ncat >/dev/null\nprintf 'CLEAN\\n' > '{}'\n(sleep 2; touch '{}'; printf 'mutated\\n' > '{}') &\necho $! > '{}'\nexit 0\n",
                direct_pid_path.display(),
                report.display(),
                sentinel.display(),
                source.display(),
                pid_path.display()
            ),
        );
        let started = Instant::now();

        run_logged_codex_review(
            &root,
            "contained-review",
            "review",
            "gpt-test",
            "high",
            &fake,
            &report,
        )
        .await
        .expect("successful contained review");

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "review must not await a descendant-held output pipe"
        );
        assert_recorded_pid_gone(&direct_pid_path).await;
        assert_recorded_pid_gone(&pid_path).await;
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(!sentinel.exists());
        assert_eq!(
            fs::read_to_string(&source).expect("read reviewed source"),
            "original\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_codex_review_kills_child_and_delayed_descendant() {
        let root = temp_dir("codex-timeout-contained");
        let fake = root.join("fake-codex");
        let sentinel = root.join("delayed-sentinel");
        let direct_pid_path = root.join("direct.pid");
        let pid_path = root.join("descendant.pid");
        write_executable(
            &fake,
            &format!(
                "#!/usr/bin/env bash\necho $$ > '{}'\ncat >/dev/null\n(sleep 2; touch '{}') &\necho $! > '{}'\nsleep 30\n",
                direct_pid_path.display(),
                sentinel.display(),
                pid_path.display()
            ),
        );
        let report = root.join("report.md");
        fs::write(&report, "CLEAN\n").expect("write report");

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_logged_codex_review(
                &root,
                "timed-review",
                "review",
                "gpt-test",
                "high",
                &fake,
                &report,
            ),
        )
        .await;
        assert!(result.is_err(), "review should hit the one-second bound");
        assert_recorded_pid_gone(&direct_pid_path).await;
        assert_recorded_pid_gone(&pid_path).await;
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(!sentinel.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_claude_alias_review_is_async_and_kills_its_tree() {
        let root = temp_dir("claude-timeout-contained");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("create fake bin");
        let sentinel = root.join("delayed-sentinel");
        let direct_pid_path = root.join("direct.pid");
        let pid_path = root.join("descendant.pid");
        write_executable(
            &bin.join("claude"),
            &format!(
                "#!/usr/bin/env bash\necho $$ > '{}'\ncat >/dev/null\n(sleep 2; touch '{}') &\necho $! > '{}'\nsleep 30\n",
                direct_pid_path.display(),
                sentinel.display(),
                pid_path.display()
            ),
        );
        let fake_claude = bin.join("claude");
        let report = root.join("report.md");
        fs::write(&report, "CLEAN\n").expect("write report");

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_logged_codex_review(
                &root,
                "timed-claude-review",
                "review",
                "opus",
                "high",
                &fake_claude,
                &report,
            ),
        )
        .await;
        assert!(
            result.is_err(),
            "the Claude branch must yield to Tokio timeout"
        );
        assert_recorded_pid_gone(&direct_pid_path).await;
        assert_recorded_pid_gone(&pid_path).await;
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(!sentinel.exists());
        let _ = fs::remove_dir_all(root);
    }
}
