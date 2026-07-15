//! Codex and Claude process spawning for the generation pipeline's authoring
//! and independent-review phases.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::claude_exec::looks_like_claude_model;
use crate::codex_exec::run_codex_exec_max_context;
use crate::generation::format_duration;
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
    // The function name is historical: it now routes to Claude when the
    // operator picks an opus/sonnet/claude alias for `--review-model`. The
    // codex path still applies for gpt-5.6-sol and explicit codex models.
    if author_phase_uses_claude_model(model) {
        return run_logged_claude_review(
            repo_root,
            phase_slug,
            prompt,
            model,
            reasoning_effort,
            report_path,
        );
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

    let status = run_codex_exec_max_context(
        repo_root,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
        &stderr_log_path,
        None,
        phase_slug,
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

fn run_logged_claude_review(
    repo_root: &Path,
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
        prompt,
        model,
        reasoning_effort,
        500,
        phase_slug,
        &prompt_path,
    )?;
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
        );
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

fn run_logged_claude_phase(
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
        prompt,
        model,
        reasoning_effort,
        max_turns,
        phase_slug,
        &prompt_path,
    )?;
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

fn run_claude_prompt(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    max_turns: usize,
    phase_label: &str,
    prompt_path: &Path,
) -> Result<String> {
    let phase_started_at = Instant::now();
    let mut child = Command::new("claude")
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
        .current_dir(repo_root)
        .spawn()
        .with_context(|| format!("failed to launch Claude for {phase_label}"))?;
    let pid = child.id();
    println!("model:       {model}");
    println!("effort:      {reasoning_effort}");
    println!("max turns:   {max_turns}");
    println!("phase start: {phase_label}");
    println!("claude pid:  {pid}");
    println!("cwd:         {}", repo_root.display());
    println!("prompt file: {}", prompt_path.display());

    use std::io::Write;
    child
        .stdin
        .as_mut()
        .context("Claude stdin missing")?
        .write_all(prompt.as_bytes())
        .with_context(|| format!("failed to write prompt for {phase_label}"))?;
    child.stdin.take();

    let output = child
        .wait_with_output()
        .with_context(|| format!("failed waiting for Claude during {phase_label}"))?;
    let stdout = String::from_utf8(output.stdout).context("Claude stdout was not valid UTF-8")?;
    let stderr = String::from_utf8(output.stderr).context("Claude stderr was not valid UTF-8")?;
    if output.status.success() {
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
    use super::author_phase_uses_claude_model;

    #[test]
    fn generation_author_backend_uses_codex_for_non_claude_models() {
        assert!(author_phase_uses_claude_model("claude-sonnet-4-6"));
        assert!(author_phase_uses_claude_model("sonnet"));
        assert!(author_phase_uses_claude_model("fable 5"));
        assert!(author_phase_uses_claude_model("fable-5"));
        assert!(!author_phase_uses_claude_model("gpt-5.6-sol"));
        assert!(!author_phase_uses_claude_model("o3"));
    }
}
