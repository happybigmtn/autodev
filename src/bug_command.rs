use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::codex_stream::{capture_codex_output, capture_opencode_output};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, copy_tree, ensure_repo_layout, git_repo_root,
    git_stdout, timestamp_slug,
};
use crate::BugArgs;

const DEFAULT_CODEX_MODEL: &str = "gpt-5.4";

#[derive(Clone, Debug)]
struct RepoChunk {
    ordinal: usize,
    id: String,
    scope_label: String,
    files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BugFinding {
    bug_id: String,
    title: String,
    location: String,
    impact: String,
    points: u8,
    description: String,
    why_plausible: String,
    falsification_checks: Vec<String>,
    evidence: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SkepticVerdict {
    bug_id: String,
    decision: String,
    confidence_percent: u8,
    counter_argument: String,
    risk_calculation: String,
    follow_up_checks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AcceptedFinding {
    bug_id: String,
    chunk_id: String,
    title: String,
    location: String,
    impact: String,
    points: u8,
    description: String,
    why_plausible: String,
    falsification_checks: Vec<String>,
    evidence: Vec<String>,
    skeptic_confidence_percent: u8,
    skeptic_counter_argument: String,
    skeptic_follow_up_checks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FixResult {
    bug_id: String,
    status: String,
    summary: String,
    validation_commands: Vec<String>,
    touched_files: Vec<String>,
    residual_risks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ReviewResult {
    bug_id: String,
    verdict: String,
    confidence: String,
    notes: String,
    follow_up: Vec<String>,
}

#[derive(Clone, Debug)]
struct ChunkOutcome {
    chunk: RepoChunk,
    findings: Vec<BugFinding>,
    disproved_count: usize,
    accepted: Vec<AcceptedFinding>,
    fixes: Vec<FixResult>,
    reviews: Vec<ReviewResult>,
}

#[derive(Clone, Debug)]
struct PhaseConfig {
    model: String,
    effort: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpencodeProvider {
    Kimi,
    Minimax,
}

impl OpencodeProvider {
    fn provider_label(self) -> &'static str {
        match self {
            Self::Kimi => "opencode-kimi",
            Self::Minimax => "opencode-minimax",
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Self::Kimi => "kimi-for-coding/k2p5",
            Self::Minimax => "minimax/MiniMax-M2.7-highspeed",
        }
    }

    fn detect(model: &str) -> Option<Self> {
        let normalized = model.trim().to_ascii_lowercase();
        if normalized.contains("kimi") {
            return Some(Self::Kimi);
        }
        if normalized.contains("minimax") {
            return Some(Self::Minimax);
        }
        None
    }

    fn resolve_model(self, requested_model: &str) -> String {
        let normalized = requested_model.trim();
        if normalized.is_empty() || normalized == DEFAULT_CODEX_MODEL {
            return self.default_model().to_string();
        }
        if normalized.contains('/') {
            return normalized.to_string();
        }
        match self {
            Self::Kimi => {
                let model = match normalized {
                    "kimi" | "kimi-k2.5" | "kimi-2.5" | "kimi-for-coding" => "k2p5",
                    "kimi-k2-thinking" => "kimi-k2-thinking",
                    other => other,
                };
                format!("kimi-for-coding/{model}")
            }
            Self::Minimax => format!("minimax/{}", map_minimax_model_name(normalized)),
        }
    }
}

enum LlmBackend {
    Codex {
        model: String,
        reasoning_effort: String,
        codex_bin: PathBuf,
    },
    Opencode {
        provider_label: &'static str,
        model: String,
        variant: String,
        opencode_bin: PathBuf,
    },
}

impl LlmBackend {
    fn label(&self) -> &str {
        match self {
            Self::Codex { .. } => "codex",
            Self::Opencode { provider_label, .. } => provider_label,
        }
    }

    fn model(&self) -> &str {
        match self {
            Self::Codex { model, .. } => model,
            Self::Opencode { model, .. } => model,
        }
    }

    fn effort(&self) -> &str {
        match self {
            Self::Codex {
                reasoning_effort, ..
            } => reasoning_effort,
            Self::Opencode { variant, .. } => variant,
        }
    }
}

pub(crate) async fn run_bug(args: BugArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    if !args.dry_run && !args.report_only && !args.allow_dirty {
        if current_branch.is_empty() {
            bail!("auto bug could not determine the current branch for its startup checkpoint");
        }
    }

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join("bug"));
    let previous_snapshot = if args.dry_run {
        None
    } else {
        prepare_output_dir(&repo_root, &output_dir)?
    };

    let chunks = collect_repo_chunks(&repo_root, args.chunk_size, args.max_chunks)?;
    if chunks.is_empty() {
        bail!("auto bug found no tracked repo files eligible for audit");
    }

    let stderr_log_path = output_dir.join("bug.stderr.log");
    let finder = PhaseConfig {
        model: args.finder_model.clone(),
        effort: args.finder_effort.clone(),
    };
    let skeptic = PhaseConfig {
        model: args.skeptic_model.clone(),
        effort: args.skeptic_effort.clone(),
    };
    let fixer = PhaseConfig {
        model: args.fixer_model.clone(),
        effort: args.fixer_effort.clone(),
    };
    let reviewer = PhaseConfig {
        model: args.reviewer_model.clone(),
        effort: args.reviewer_effort.clone(),
    };

    println!("auto bug");
    println!("repo root:   {}", repo_root.display());
    println!("output dir:  {}", output_dir.display());
    println!("chunks:      {}", chunks.len());
    println!("finder:      {} ({})", finder.model, finder.effort);
    println!("skeptic:     {} ({})", skeptic.model, skeptic.effort);
    println!("fixer:       {} ({})", fixer.model, fixer.effort);
    println!("reviewer:    {} ({})", reviewer.model, reviewer.effort);
    if !current_branch.is_empty() {
        println!("branch:      {}", current_branch);
    }
    if let Some(previous) = &previous_snapshot {
        println!("prior input: {}", previous.display());
    }
    if args.report_only {
        println!("mode:        report-only");
    }
    if args.dry_run {
        println!("mode:        dry-run");
        for chunk in chunks.iter().take(8) {
            println!(
                "chunk:       {} | {} file(s) | {}",
                chunk.id,
                chunk.files.len(),
                chunk.scope_label
            );
        }
        if chunks.len() > 8 {
            println!("chunk:       ... +{} more", chunks.len() - 8);
        }
        return Ok(());
    }
    if !args.report_only && !args.allow_dirty {
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, current_branch.as_str(), "auto bug checkpoint")?
        {
            println!("checkpoint:  committed pre-existing changes at {commit}");
        }
    }

    let mut outcomes = Vec::new();
    for chunk in chunks {
        let chunk_dir = output_dir.join("chunks").join(&chunk.id);
        fs::create_dir_all(&chunk_dir)
            .with_context(|| format!("failed to create {}", chunk_dir.display()))?;
        write_chunk_manifest(&chunk_dir, &chunk)?;

        let findings = run_finder_phase(
            &repo_root,
            &chunk,
            &chunk_dir,
            &finder,
            &args,
            &stderr_log_path,
        )
        .await?;

        let (disproved_count, accepted) = if findings.is_empty() {
            atomic_write(&chunk_dir.join("accepted-findings.json"), b"[]")?;
            (0, Vec::new())
        } else {
            run_skeptic_phase(
                &repo_root,
                &chunk,
                &chunk_dir,
                &skeptic,
                &findings,
                &args,
                &stderr_log_path,
            )
            .await?
        };

        let fixes = if args.report_only || accepted.is_empty() {
            Vec::new()
        } else {
            run_fix_phase(
                &repo_root,
                &chunk,
                &chunk_dir,
                &fixer,
                &accepted,
                &args,
                &stderr_log_path,
            )
            .await?
        };

        let reviews = if args.report_only || accepted.is_empty() {
            Vec::new()
        } else {
            run_review_phase(
                &repo_root,
                &chunk,
                &chunk_dir,
                &reviewer,
                &accepted,
                &args,
                &stderr_log_path,
            )
            .await?
        };

        println!(
            "summary:     {} reported | {} accepted | {} disproved",
            findings.len(),
            accepted.len(),
            disproved_count
        );
        if !fixes.is_empty() {
            println!(
                "remediation: {} item(s) | review: {} item(s)",
                fixes.len(),
                reviews.len()
            );
        }

        outcomes.push(ChunkOutcome {
            chunk,
            findings,
            disproved_count,
            accepted,
            fixes,
            reviews,
        });
    }

    write_bug_summary(&output_dir, &outcomes, args.report_only)?;
    println!();
    println!("bug run complete");
    println!(
        "summary:     {}",
        output_dir.join("BUG_REPORT.md").display()
    );
    println!(
        "verified:    {}",
        output_dir.join("verified-findings.json").display()
    );
    println!("stderr log:  {}", stderr_log_path.display());

    Ok(())
}

async fn run_finder_phase(
    repo_root: &Path,
    chunk: &RepoChunk,
    chunk_dir: &Path,
    config: &PhaseConfig,
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<Vec<BugFinding>> {
    let prompt_path = chunk_dir.join("finder-prompt.md");
    let response_path = chunk_dir.join("finder-response.jsonl");
    let findings_json_path = chunk_dir.join("finder-findings.json");
    let findings_md_path = chunk_dir.join("finder-findings.md");
    let prompt = build_finder_prompt(chunk, &findings_json_path, &findings_md_path);
    atomic_write(&prompt_path, prompt.as_bytes())?;

    let backend = select_backend(
        &config.model,
        &config.effort,
        &args.codex_bin,
        &args.opencode_bin,
    );
    print_phase_header("finder", chunk, &backend);
    let raw_response = run_backend_prompt(repo_root, &prompt, &backend, stderr_log_path).await?;
    atomic_write(&response_path, raw_response.as_bytes())?;

    let findings: Vec<BugFinding> = load_json_file(&findings_json_path)?;
    validate_findings(chunk, &findings)?;
    Ok(findings)
}

async fn run_skeptic_phase(
    repo_root: &Path,
    chunk: &RepoChunk,
    chunk_dir: &Path,
    config: &PhaseConfig,
    findings: &[BugFinding],
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<(usize, Vec<AcceptedFinding>)> {
    let prompt_path = chunk_dir.join("skeptic-prompt.md");
    let response_path = chunk_dir.join("skeptic-response.jsonl");
    let verdicts_json_path = chunk_dir.join("skeptic-verdicts.json");
    let verdicts_md_path = chunk_dir.join("skeptic-verdicts.md");
    let finder_json_path = chunk_dir.join("finder-findings.json");
    let prompt = build_skeptic_prompt(
        chunk,
        &finder_json_path,
        &verdicts_json_path,
        &verdicts_md_path,
    );
    atomic_write(&prompt_path, prompt.as_bytes())?;

    let backend = select_backend(
        &config.model,
        &config.effort,
        &args.codex_bin,
        &args.opencode_bin,
    );
    print_phase_header("skeptic", chunk, &backend);
    let raw_response = run_backend_prompt(repo_root, &prompt, &backend, stderr_log_path).await?;
    atomic_write(&response_path, raw_response.as_bytes())?;

    let verdicts: Vec<SkepticVerdict> = load_json_file(&verdicts_json_path)?;
    let (disproved_count, accepted) = derive_accepted_findings(chunk, findings, &verdicts)?;
    atomic_write(
        &chunk_dir.join("accepted-findings.json"),
        serde_json::to_string_pretty(&accepted)?.as_bytes(),
    )?;
    Ok((disproved_count, accepted))
}

async fn run_fix_phase(
    repo_root: &Path,
    chunk: &RepoChunk,
    chunk_dir: &Path,
    config: &PhaseConfig,
    accepted: &[AcceptedFinding],
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<Vec<FixResult>> {
    let prompt_path = chunk_dir.join("fix-prompt.md");
    let response_path = chunk_dir.join("fix-response.jsonl");
    let results_json_path = chunk_dir.join("fix-results.json");
    let results_md_path = chunk_dir.join("fix-results.md");
    let accepted_json_path = chunk_dir.join("accepted-findings.json");
    let prompt = build_fix_prompt(
        chunk,
        &accepted_json_path,
        &results_json_path,
        &results_md_path,
    );
    atomic_write(&prompt_path, prompt.as_bytes())?;

    let backend = select_backend(
        &config.model,
        &config.effort,
        &args.codex_bin,
        &args.opencode_bin,
    );
    print_phase_header("fixer", chunk, &backend);
    let raw_response = run_backend_prompt(repo_root, &prompt, &backend, stderr_log_path).await?;
    atomic_write(&response_path, raw_response.as_bytes())?;

    let results: Vec<FixResult> = load_json_file(&results_json_path)?;
    validate_fix_results(accepted, &results)?;
    Ok(results)
}

async fn run_review_phase(
    repo_root: &Path,
    chunk: &RepoChunk,
    chunk_dir: &Path,
    config: &PhaseConfig,
    accepted: &[AcceptedFinding],
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<Vec<ReviewResult>> {
    let prompt_path = chunk_dir.join("review-prompt.md");
    let response_path = chunk_dir.join("review-response.jsonl");
    let results_json_path = chunk_dir.join("review-results.json");
    let results_md_path = chunk_dir.join("review-results.md");
    let accepted_json_path = chunk_dir.join("accepted-findings.json");
    let fix_json_path = chunk_dir.join("fix-results.json");
    let prompt = build_review_prompt(
        chunk,
        &accepted_json_path,
        &fix_json_path,
        &results_json_path,
        &results_md_path,
    );
    atomic_write(&prompt_path, prompt.as_bytes())?;

    let backend = select_backend(
        &config.model,
        &config.effort,
        &args.codex_bin,
        &args.opencode_bin,
    );
    print_phase_header("reviewer", chunk, &backend);
    let raw_response = run_backend_prompt(repo_root, &prompt, &backend, stderr_log_path).await?;
    atomic_write(&response_path, raw_response.as_bytes())?;

    let results: Vec<ReviewResult> = load_json_file(&results_json_path)?;
    validate_review_results(accepted, &results)?;
    Ok(results)
}

fn select_backend(model: &str, effort: &str, codex_bin: &Path, opencode_bin: &Path) -> LlmBackend {
    if let Some(provider) = OpencodeProvider::detect(model) {
        return LlmBackend::Opencode {
            provider_label: provider.provider_label(),
            model: provider.resolve_model(model),
            variant: effort.to_string(),
            opencode_bin: resolve_opencode_bin(opencode_bin),
        };
    }

    LlmBackend::Codex {
        model: model.to_string(),
        reasoning_effort: effort.to_string(),
        codex_bin: codex_bin.to_path_buf(),
    }
}

fn print_phase_header(phase: &str, chunk: &RepoChunk, backend: &LlmBackend) {
    println!();
    println!("phase:       {phase}");
    println!("chunk:       {}", chunk.id);
    println!("scope:       {}", chunk.scope_label);
    println!("backend:     {}", backend.label());
    println!("model:       {}", backend.model());
    println!("variant:     {}", backend.effort());
}

async fn run_backend_prompt(
    repo_root: &Path,
    prompt: &str,
    backend: &LlmBackend,
    stderr_log_path: &Path,
) -> Result<String> {
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

            let status = child.wait().await.context("failed waiting for Codex")?;
            let stdout = stdout_task
                .await
                .context("Codex stdout capture task panicked")??;
            let stderr_text = stderr_task
                .await
                .context("Codex stderr capture task panicked")??;
            append_stderr_log(stderr_log_path, &stderr_text)?;

            if !status.success() {
                bail!(
                    "Codex bug phase failed: {}",
                    stderr_text.trim().if_empty_then(stdout.trim())
                );
            }
            Ok(stdout)
        }
        LlmBackend::Opencode {
            model,
            variant,
            opencode_bin,
            ..
        } => {
            let opencode_data_home = repo_root.join(".auto").join("opencode-data");
            fs::create_dir_all(&opencode_data_home)
                .with_context(|| format!("failed to create {}", opencode_data_home.display()))?;

            let mut command = TokioCommand::new(opencode_bin);
            command
                .arg("run")
                .arg("--format")
                .arg("json")
                .arg("--dir")
                .arg(repo_root)
                .arg("--model")
                .arg(model)
                .arg("--variant")
                .arg(variant)
                .arg(prompt)
                .env("XDG_DATA_HOME", &opencode_data_home)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .current_dir(repo_root);

            let mut child = command.spawn().with_context(|| {
                format!(
                    "failed to launch OpenCode at {} from {}",
                    opencode_bin.display(),
                    repo_root.display()
                )
            })?;

            let stdout = child
                .stdout
                .take()
                .context("OpenCode stdout should be piped for auto bug")?;
            let stderr = child
                .stderr
                .take()
                .context("OpenCode stderr should be piped for auto bug")?;

            let stdout_task = tokio::spawn(async move { capture_opencode_output(stdout).await });
            let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

            let status = child.wait().await.context("failed waiting for OpenCode")?;
            let stdout = stdout_task
                .await
                .context("OpenCode stdout capture task panicked")??;
            let stderr_text = stderr_task
                .await
                .context("OpenCode stderr capture task panicked")??;
            append_stderr_log(stderr_log_path, &stderr_text)?;

            if !status.success() {
                bail!(
                    "OpenCode bug phase failed: {}",
                    stderr_text.trim().if_empty_then(
                        parse_opencode_error(&stdout)
                            .as_deref()
                            .unwrap_or(stdout.trim())
                    )
                );
            }
            if let Some(detail) = parse_opencode_error(&stdout) {
                bail!("OpenCode bug phase failed: {detail}");
            }
            Ok(stdout)
        }
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
    atomic_write(stderr_log_path, &existing)?;
    Ok(())
}

fn collect_repo_chunks(
    repo_root: &Path,
    chunk_size: usize,
    max_chunks: Option<usize>,
) -> Result<Vec<RepoChunk>> {
    if chunk_size == 0 {
        bail!("chunk size must be greater than zero");
    }

    let tracked = git_stdout(repo_root, ["ls-files"])?;
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for line in tracked.lines() {
        let path = line.trim();
        if path.is_empty() || !should_audit_path(path) {
            continue;
        }
        let scope = top_level_scope(path);
        grouped.entry(scope).or_default().push(path.to_string());
    }

    let mut chunks = Vec::new();
    let mut ordinal = 1usize;
    for (scope, mut files) in grouped {
        files.sort();
        for slice in files.chunks(chunk_size) {
            let id = format!("chunk-{:03}-{}", ordinal, slugify(&scope));
            chunks.push(RepoChunk {
                ordinal,
                id,
                scope_label: scope.clone(),
                files: slice.to_vec(),
            });
            ordinal += 1;
            if max_chunks.is_some_and(|limit| chunks.len() >= limit) {
                return Ok(chunks);
            }
        }
    }
    Ok(chunks)
}

fn top_level_scope(path: &str) -> String {
    if path.contains('/') {
        path.split('/').next().unwrap_or("root").to_string()
    } else {
        "root".to_string()
    }
}

fn should_audit_path(path: &str) -> bool {
    if path.starts_with(".auto/")
        || path.starts_with("bug/")
        || path.starts_with("nemesis/")
        || path.starts_with("genesis/")
        || path.starts_with("target/")
    {
        return false;
    }
    if path
        .split('/')
        .next()
        .is_some_and(|component| component.starts_with("gen-"))
    {
        return false;
    }

    let lower = path.to_ascii_lowercase();
    let excluded_exts = [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".ico", ".pdf", ".mp4", ".mov", ".zip",
        ".gz", ".tar", ".woff", ".woff2", ".ttf", ".otf", ".mp3", ".wav",
    ];
    !excluded_exts.iter().any(|ext| lower.ends_with(ext))
}

fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn write_chunk_manifest(chunk_dir: &Path, chunk: &RepoChunk) -> Result<()> {
    let mut manifest = String::new();
    manifest.push_str("# Bug Audit Chunk\n\n");
    manifest.push_str(&format!("- Chunk: `{}`\n", chunk.id));
    manifest.push_str(&format!("- Scope: `{}`\n", chunk.scope_label));
    manifest.push_str(&format!("- Files: `{}`\n\n", chunk.files.len()));
    manifest.push_str("## Files\n\n");
    for file in &chunk.files {
        manifest.push_str(&format!("- `{file}`\n"));
    }
    atomic_write(&chunk_dir.join("manifest.md"), manifest.as_bytes())
}

fn build_finder_prompt(chunk: &RepoChunk, findings_json: &Path, findings_md: &Path) -> String {
    format!(
        r#"You are the finder pass in a multi-pass bug pipeline.

Audit repo chunk `{chunk_id}` with primary scope `{scope}`.

Primary files in this chunk:
{files}

Rules:
- Treat the live codebase as truth.
- You may inspect adjacent files outside this chunk only when they are required to validate an integration path.
- Do not modify code.
- Only write these files:
  - `{findings_json}`
  - `{findings_md}`
- `{findings_json}` must be a JSON array. If nothing survives your audit, write `[]`.

Scoring:
- Low impact bug: 1 point
- Medium impact bug: 5 points
- Critical impact bug: 10 points

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-{ordinal:03}-01",
  "title": "Short bug title",
  "location": "path:line or subsystem identifier",
  "impact": "low|medium|critical",
  "points": 1,
  "description": "Concrete failure mode",
  "why_plausible": "Why this is plausibly real in this repo",
  "falsification_checks": ["Specific repro or validation step"],
  "evidence": ["Code reference or observed invariant"]
}}

Requirements:
- Maximize recall, but every finding must name a concrete failure mode and at least one falsification check.
- Use bug IDs with prefix `BUG-{ordinal:03}-`.
- Match `points` to `impact` exactly.
- `{findings_md}` should summarize the same findings, grouped by impact, and end with a total score.
"#,
        chunk_id = chunk.id,
        scope = chunk.scope_label,
        files = render_prompt_files(&chunk.files),
        findings_json = findings_json.display(),
        findings_md = findings_md.display(),
        ordinal = chunk.ordinal,
    )
}

fn build_skeptic_prompt(
    chunk: &RepoChunk,
    findings_json: &Path,
    verdicts_json: &Path,
    verdicts_md: &Path,
) -> String {
    format!(
        r#"You are the skeptic pass in a multi-pass bug pipeline.

Review chunk `{chunk_id}` with primary scope `{scope}`.

Input findings file:
- `{findings_json}`

Rules:
- Treat the codebase as truth.
- Challenge every reported bug.
- Do not modify code.
- Only write these files:
  - `{verdicts_json}`
  - `{verdicts_md}`
- `{verdicts_json}` must be a JSON array with one verdict per input bug. If the input file is empty, write `[]`.

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-{ordinal:03}-01",
  "decision": "accepted|disproved",
  "confidence_percent": 0,
  "counter_argument": "Why it is not a bug, or why the claim still survives challenge",
  "risk_calculation": "Reasoning about the downside of dismissing it incorrectly",
  "follow_up_checks": ["Extra validation that would tighten confidence"]
}}

Requirements:
- Be aggressive about disproving weak claims.
- Only `accepted` findings should survive to remediation.
- `{verdicts_md}` should summarize disproved vs accepted findings and call out the hardest borderline decisions.
"#,
        chunk_id = chunk.id,
        scope = chunk.scope_label,
        findings_json = findings_json.display(),
        verdicts_json = verdicts_json.display(),
        verdicts_md = verdicts_md.display(),
        ordinal = chunk.ordinal,
    )
}

fn build_fix_prompt(
    chunk: &RepoChunk,
    accepted_json: &Path,
    results_json: &Path,
    results_md: &Path,
) -> String {
    format!(
        r#"You are the remediation pass in a multi-pass bug pipeline.

Fix the accepted bugs for chunk `{chunk_id}` with primary scope `{scope}`.

Input accepted findings file:
- `{accepted_json}`

Rules:
- Modify code only as needed to address the accepted findings plus the minimum adjacent integration surfaces.
- Run validation commands that honestly support your changes.
- Do not commit, push, or create branches.
- Only write these files:
  - `{results_json}`
  - `{results_md}`
- `{results_json}` must be a JSON array with one entry per accepted bug. If there are no accepted bugs, write `[]`.

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-{ordinal:03}-01",
  "status": "fixed|deferred|not_reproduced",
  "summary": "What changed and why",
  "validation_commands": ["Command actually run"],
  "touched_files": ["path/to/file"],
  "residual_risks": ["Anything still not fully closed"]
}}

Requirements:
- Treat accepted findings as the contract; do not widen scope into unrelated cleanup.
- `{results_md}` should summarize the fixes, validation, and any deferred items.
"#,
        chunk_id = chunk.id,
        scope = chunk.scope_label,
        accepted_json = accepted_json.display(),
        results_json = results_json.display(),
        results_md = results_md.display(),
        ordinal = chunk.ordinal,
    )
}

fn build_review_prompt(
    chunk: &RepoChunk,
    accepted_json: &Path,
    fix_json: &Path,
    results_json: &Path,
    results_md: &Path,
) -> String {
    format!(
        r#"You are the remediation review pass in a multi-pass bug pipeline.

Review the remediation for chunk `{chunk_id}` with primary scope `{scope}`.

Inputs:
- Accepted findings: `{accepted_json}`
- Fix results: `{fix_json}`

Rules:
- Treat the codebase as truth.
- Verify that the accepted bugs were actually addressed and look for nearby regressions.
- Do not modify code.
- Only write these files:
  - `{results_json}`
  - `{results_md}`
- `{results_json}` must be a JSON array with one entry per accepted bug. If there are no accepted bugs, write `[]`.

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-{ordinal:03}-01",
  "verdict": "accepted_fix|fix_incomplete|regression_found",
  "confidence": "high|medium|low",
  "notes": "What review concluded",
  "follow_up": ["Concrete follow-up action or check"]
}}

Requirements:
- `accepted_fix` means the remediation looks correct and supported.
- `fix_incomplete` means the bug still survives or the validation is not strong enough.
- `regression_found` means the fix created a new problem nearby.
- `{results_md}` should summarize what is safe to trust and what still needs work.
"#,
        chunk_id = chunk.id,
        scope = chunk.scope_label,
        accepted_json = accepted_json.display(),
        fix_json = fix_json.display(),
        results_json = results_json.display(),
        results_md = results_md.display(),
        ordinal = chunk.ordinal,
    )
}

fn render_prompt_files(files: &[String]) -> String {
    files
        .iter()
        .map(|file| format!("- `{file}`"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_findings(chunk: &RepoChunk, findings: &[BugFinding]) -> Result<()> {
    for finding in findings {
        if !finding
            .bug_id
            .starts_with(&format!("BUG-{:03}-", chunk.ordinal))
        {
            bail!(
                "finder bug id `{}` does not match chunk ordinal {:03}",
                finding.bug_id,
                chunk.ordinal
            );
        }
        let impact = finding.impact.to_ascii_lowercase();
        let expected_points = match impact.as_str() {
            "low" => 1,
            "medium" => 5,
            "critical" => 10,
            other => bail!("invalid impact `{other}` in {}", finding.bug_id),
        };
        if finding.points != expected_points {
            bail!(
                "finder points mismatch in {}: expected {} for impact `{}` but found {}",
                finding.bug_id,
                expected_points,
                finding.impact,
                finding.points
            );
        }
        if finding.title.trim().is_empty()
            || finding.location.trim().is_empty()
            || finding.description.trim().is_empty()
            || finding.why_plausible.trim().is_empty()
        {
            bail!(
                "finder output for {} is missing required fields",
                finding.bug_id
            );
        }
        if finding.falsification_checks.is_empty() {
            bail!(
                "finder output for {} must include falsification checks",
                finding.bug_id
            );
        }
    }
    Ok(())
}

fn derive_accepted_findings(
    chunk: &RepoChunk,
    findings: &[BugFinding],
    verdicts: &[SkepticVerdict],
) -> Result<(usize, Vec<AcceptedFinding>)> {
    let mut verdicts_by_id = HashMap::<&str, &SkepticVerdict>::new();
    for verdict in verdicts {
        verdicts_by_id.insert(verdict.bug_id.as_str(), verdict);
    }

    let mut accepted = Vec::new();
    let mut disproved = 0usize;
    for finding in findings {
        let verdict = verdicts_by_id
            .get(finding.bug_id.as_str())
            .with_context(|| format!("skeptic output missing verdict for {}", finding.bug_id))?;
        match verdict.decision.trim().to_ascii_lowercase().as_str() {
            "accepted" => accepted.push(AcceptedFinding {
                bug_id: finding.bug_id.clone(),
                chunk_id: chunk.id.clone(),
                title: finding.title.clone(),
                location: finding.location.clone(),
                impact: finding.impact.clone(),
                points: finding.points,
                description: finding.description.clone(),
                why_plausible: finding.why_plausible.clone(),
                falsification_checks: finding.falsification_checks.clone(),
                evidence: finding.evidence.clone(),
                skeptic_confidence_percent: verdict.confidence_percent,
                skeptic_counter_argument: verdict.counter_argument.clone(),
                skeptic_follow_up_checks: verdict.follow_up_checks.clone(),
            }),
            "disproved" => disproved += 1,
            other => bail!("invalid skeptic decision `{other}` for {}", finding.bug_id),
        }
    }

    Ok((disproved, accepted))
}

fn validate_fix_results(accepted: &[AcceptedFinding], results: &[FixResult]) -> Result<()> {
    validate_bug_id_coverage(
        accepted.iter().map(|finding| finding.bug_id.as_str()),
        results.iter().map(|result| result.bug_id.as_str()),
        "fix results",
    )?;
    for result in results {
        match result.status.trim().to_ascii_lowercase().as_str() {
            "fixed" | "deferred" | "not_reproduced" => {}
            other => bail!("invalid fix status `{other}` for {}", result.bug_id),
        }
        if result.summary.trim().is_empty() {
            bail!("fix result for {} is missing a summary", result.bug_id);
        }
    }
    Ok(())
}

fn validate_review_results(accepted: &[AcceptedFinding], results: &[ReviewResult]) -> Result<()> {
    validate_bug_id_coverage(
        accepted.iter().map(|finding| finding.bug_id.as_str()),
        results.iter().map(|result| result.bug_id.as_str()),
        "review results",
    )?;
    for result in results {
        match result.verdict.trim().to_ascii_lowercase().as_str() {
            "accepted_fix" | "fix_incomplete" | "regression_found" => {}
            other => bail!("invalid review verdict `{other}` for {}", result.bug_id),
        }
        match result.confidence.trim().to_ascii_lowercase().as_str() {
            "high" | "medium" | "low" => {}
            other => bail!("invalid review confidence `{other}` for {}", result.bug_id),
        }
    }
    Ok(())
}

fn validate_bug_id_coverage<'a>(
    expected: impl Iterator<Item = &'a str>,
    actual: impl Iterator<Item = &'a str>,
    label: &str,
) -> Result<()> {
    let expected = expected.collect::<Vec<_>>();
    let actual = actual.collect::<Vec<_>>();
    for bug_id in expected {
        if !actual.iter().any(|candidate| candidate == &bug_id) {
            bail!("{label} missing entry for {bug_id}");
        }
    }
    Ok(())
}

fn load_json_file<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse JSON from {}", path.display()))
}

fn write_bug_summary(
    output_dir: &Path,
    outcomes: &[ChunkOutcome],
    report_only: bool,
) -> Result<()> {
    let all_accepted = outcomes
        .iter()
        .flat_map(|outcome| outcome.accepted.clone())
        .collect::<Vec<_>>();
    let all_reviews = outcomes
        .iter()
        .flat_map(|outcome| outcome.reviews.clone())
        .collect::<Vec<_>>();

    let mut markdown = String::new();
    markdown.push_str("# BUG_REPORT\n\n");
    markdown.push_str(&format!(
        "- Generated: `{}`\n",
        Local::now().format("%Y-%m-%d %H:%M:%S %Z")
    ));
    markdown.push_str(&format!("- Chunks audited: `{}`\n", outcomes.len()));
    markdown.push_str(&format!(
        "- Findings reported: `{}`\n",
        outcomes
            .iter()
            .map(|outcome| outcome.findings.len())
            .sum::<usize>()
    ));
    markdown.push_str(&format!("- Findings accepted: `{}`\n", all_accepted.len()));
    markdown.push_str(&format!(
        "- Findings disproved: `{}`\n",
        outcomes
            .iter()
            .map(|outcome| outcome.disproved_count)
            .sum::<usize>()
    ));
    markdown.push_str(&format!(
        "- Remediation results: `{}`\n",
        outcomes
            .iter()
            .map(|outcome| outcome.fixes.len())
            .sum::<usize>()
    ));
    markdown.push_str(&format!("- Review verdicts: `{}`\n", all_reviews.len()));
    markdown.push_str(&format!(
        "- Mode: `{}`\n\n",
        if report_only {
            "report-only"
        } else {
            "fix-and-review"
        }
    ));

    markdown.push_str("## Chunk Summary\n\n");
    for outcome in outcomes {
        markdown.push_str(&format!(
            "- `{}` (`{}`): {} reported, {} accepted, {} disproved, {} remediated\n",
            outcome.chunk.id,
            outcome.chunk.scope_label,
            outcome.findings.len(),
            outcome.accepted.len(),
            outcome.disproved_count,
            outcome.fixes.len()
        ));
    }

    markdown.push_str("\n## Verified Findings\n\n");
    if all_accepted.is_empty() {
        markdown.push_str("No verified findings survived the skeptic pass.\n");
    } else {
        for finding in &all_accepted {
            markdown.push_str(&format!(
                "### `{}` {} (`{}` / {} points)\n\n",
                finding.bug_id, finding.title, finding.impact, finding.points
            ));
            markdown.push_str(&format!("- Chunk: `{}`\n", finding.chunk_id));
            markdown.push_str(&format!("- Location: `{}`\n", finding.location));
            markdown.push_str(&format!("- Description: {}\n", finding.description));
            markdown.push_str(&format!(
                "- Skeptic confidence: `{}`\n",
                finding.skeptic_confidence_percent
            ));
            markdown.push_str(&format!(
                "- Skeptic counter: {}\n\n",
                finding.skeptic_counter_argument
            ));
        }
    }

    if !report_only {
        markdown.push_str("## Remediation Review\n\n");
        if all_reviews.is_empty() {
            markdown.push_str("No remediation review output captured.\n");
        } else {
            for review in &all_reviews {
                markdown.push_str(&format!(
                    "- `{}`: `{}` ({})\n",
                    review.bug_id, review.verdict, review.confidence
                ));
            }
        }
    }

    atomic_write(&output_dir.join("BUG_REPORT.md"), markdown.as_bytes())?;
    atomic_write(
        &output_dir.join("verified-findings.json"),
        serde_json::to_string_pretty(&all_accepted)?.as_bytes(),
    )?;
    Ok(())
}

fn prepare_output_dir(repo_root: &Path, output_dir: &Path) -> Result<Option<PathBuf>> {
    if !output_dir.exists() {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        return Ok(None);
    }
    if !output_dir.is_dir() {
        bail!(
            "bug output path {} is not a directory",
            output_dir.display()
        );
    }

    let has_contents = fs::read_dir(output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
        .next()
        .transpose()?
        .is_some();
    let archived = if has_contents {
        let snapshot_root = repo_root.join(".auto").join("fresh-input").join(format!(
            "{}-previous-{}",
            output_dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("bug"),
            timestamp_slug()
        ));
        copy_tree(output_dir, &snapshot_root).with_context(|| {
            format!(
                "failed to archive existing bug output from {} into {}",
                output_dir.display(),
                snapshot_root.display()
            )
        })?;
        Some(snapshot_root)
    } else {
        None
    };

    fs::remove_dir_all(output_dir)
        .with_context(|| format!("failed to clear {}", output_dir.display()))?;
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to recreate {}", output_dir.display()))?;
    Ok(archived)
}

fn resolve_opencode_bin(configured: &Path) -> PathBuf {
    if configured != Path::new("opencode") {
        return configured.to_path_buf();
    }
    if let Some(path) = std::env::var_os("FABRO_OPENCODE_BIN").map(PathBuf::from) {
        return path;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let bundled = PathBuf::from(home)
            .join(".opencode")
            .join("bin")
            .join("opencode");
        if bundled.exists() {
            return bundled;
        }
    }
    PathBuf::from("opencode")
}

fn map_minimax_model_name(model: &str) -> String {
    match model {
        "minimax" => "MiniMax-M2.7-highspeed".to_string(),
        "minimax-m2.5" => "MiniMax-M2.5".to_string(),
        "minimax-m2" => "MiniMax-M2".to_string(),
        "minimax-m2.1" => "MiniMax-M2.1".to_string(),
        "minimax-m2.5-highspeed" => "MiniMax-M2.5-highspeed".to_string(),
        "minimax-m2.7" => "MiniMax-M2.7".to_string(),
        "minimax-m2.7-highspeed" => "MiniMax-M2.7-highspeed".to_string(),
        other if other.starts_with("MiniMax-") => other.to_string(),
        other => other.to_string(),
    }
}

fn parse_opencode_error(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event.get("type").and_then(serde_json::Value::as_str) != Some("error") {
            continue;
        }
        if let Some(message) = event
            .get("error")
            .and_then(|error| error.get("data"))
            .and_then(|data| data.get("message"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(serde_json::Value::as_str)
            })
            .or_else(|| event.get("message").and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|message| !message.is_empty())
        {
            return Some(message.to_string());
        }
    }
    None
}

trait EmptyFallback {
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
    use std::path::Path;

    use super::{collect_repo_chunks, should_audit_path, slugify, OpencodeProvider};

    #[test]
    fn excludes_generated_and_binary_paths() {
        assert!(!should_audit_path(".auto/log.txt"));
        assert!(!should_audit_path("gen-20260403/specs/foo.md"));
        assert!(!should_audit_path("bug/chunks/chunk-001/report.md"));
        assert!(!should_audit_path("assets/logo.png"));
        assert!(should_audit_path("src/main.rs"));
        assert!(should_audit_path("Cargo.toml"));
    }

    #[test]
    fn slugifies_scope_labels() {
        assert_eq!(slugify("src/lib"), "src-lib");
        assert_eq!(slugify("Cargo.toml"), "cargo-toml");
    }

    #[test]
    fn chunk_collection_requires_non_zero_size() {
        let repo_root = Path::new("/tmp");
        let result = collect_repo_chunks(repo_root, 0, None);
        assert!(result.is_err());
    }

    #[test]
    fn bug_pipeline_minimax_alias_defaults_to_m27_highspeed() {
        assert_eq!(
            OpencodeProvider::Minimax.resolve_model("minimax"),
            "minimax/MiniMax-M2.7-highspeed"
        );
    }
}
