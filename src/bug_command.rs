use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::codex_stream::{capture_codex_output, capture_pi_output};
use crate::pi_backend::{parse_pi_error, resolve_pi_bin, PiProvider};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, copy_tree, ensure_repo_layout, git_repo_root,
    git_stdout, opencode_agent_dir, prune_pi_runtime_state, push_branch_with_remote_sync,
    sync_branch_with_remote, timestamp_slug, truncate_file_to_max_bytes,
};
use crate::BugArgs;

const DEFAULT_CODEX_MODEL: &str = "gpt-5.4";
const DEFAULT_CODEX_REASONING_EFFORT: &str = "high";
const BUG_STDERR_LOG_MAX_BYTES: usize = 1024 * 1024;
const JSON_REPAIR_MAX_BYTES: usize = 256 * 1024;

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
    verified: Vec<AcceptedFinding>,
    reviews: Vec<ReviewResult>,
}

#[derive(Clone, Debug)]
struct PhaseConfig {
    model: String,
    effort: String,
}

enum LlmBackend {
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
}

impl LlmBackend {
    fn label(&self) -> &str {
        match self {
            Self::Codex { .. } => "codex",
            Self::Pi { provider_label, .. } => provider_label,
        }
    }

    fn model(&self) -> &str {
        match self {
            Self::Codex { model, .. } => model,
            Self::Pi { model, .. } => model,
        }
    }

    fn effort(&self) -> &str {
        match self {
            Self::Codex {
                reasoning_effort, ..
            } => reasoning_effort,
            Self::Pi { thinking, .. } => thinking,
        }
    }
}

pub(crate) async fn run_bug(args: BugArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    if !args.dry_run && !args.report_only && current_branch.is_empty() {
        bail!(
            "auto bug requires a checked-out branch so implementation commits can push to origin"
        );
    }

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join("bug"));
    let (previous_snapshot, resumed_existing_output) = if args.dry_run {
        (None, args.resume && output_dir.exists())
    } else {
        prepare_bug_output_dir(&repo_root, &output_dir, args.resume)?
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
    ensure_code_writer_config("auto bug implementation pass", &fixer)?;

    println!("auto bug");
    println!("repo root:   {}", repo_root.display());
    println!("output dir:  {}", output_dir.display());
    println!("chunks:      {}", chunks.len());
    println!("finder:      {} ({})", finder.model, finder.effort);
    println!("skeptic:     {} ({})", skeptic.model, skeptic.effort);
    println!("reviewer:    {} ({})", reviewer.model, reviewer.effort);
    println!("implementer: {} ({})", fixer.model, fixer.effort);
    if !current_branch.is_empty() {
        println!("branch:      {}", current_branch);
    }
    if let Some(previous) = &previous_snapshot {
        println!("prior input: {}", previous.display());
    }
    if args.resume {
        println!(
            "resume:      {}",
            if resumed_existing_output {
                "reusing existing bug artifacts"
            } else {
                "no existing bug artifacts found; starting fresh in-place"
            }
        );
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
    if !args.report_only
        && !current_branch.is_empty()
        && sync_branch_with_remote(&repo_root, current_branch.as_str())?
    {
        println!("remote sync: rebased onto origin/{}", current_branch);
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

        let findings = load_or_run_finder_phase(
            &repo_root,
            &chunk,
            &chunk_dir,
            &finder,
            &args,
            &stderr_log_path,
        )
        .await?;

        let (disproved_count, accepted) = if findings.is_empty() {
            let accepted_path = chunk_dir.join("accepted-findings.json");
            if !accepted_path.exists() {
                atomic_write(&accepted_path, b"[]")?;
            }
            (0, Vec::new())
        } else {
            load_or_run_skeptic_phase(
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

        let reviews = if accepted.is_empty() {
            Vec::new()
        } else {
            load_or_run_review_phase(
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
        let verified = derive_verified_findings(&accepted, &reviews)?;

        println!(
            "summary:     {} reported | {} accepted | {} verified | {} disproved",
            findings.len(),
            accepted.len(),
            verified.len(),
            disproved_count
        );
        if !reviews.is_empty() {
            println!("review:      {} item(s)", reviews.len());
        }

        outcomes.push(ChunkOutcome {
            chunk,
            findings,
            disproved_count,
            accepted,
            verified,
            reviews,
        });
    }

    let all_verified = outcomes
        .iter()
        .flat_map(|outcome| outcome.verified.clone())
        .collect::<Vec<_>>();
    atomic_write(
        &output_dir.join("verified-findings.json"),
        serde_json::to_string_pretty(&all_verified)?.as_bytes(),
    )?;

    let resumed_fix_results = if args.report_only || all_verified.is_empty() {
        None
    } else {
        try_resume_fix_results(&output_dir, &all_verified, args.resume)?
    };
    let fix_commit_before =
        if args.report_only || all_verified.is_empty() || resumed_fix_results.is_some() {
            None
        } else {
            Some(git_stdout(&repo_root, ["rev-parse", "HEAD"])?)
        };
    let fixes = if args.report_only || all_verified.is_empty() {
        Vec::new()
    } else if let Some(results) = resumed_fix_results {
        results
    } else {
        run_fix_phase(
            &repo_root,
            &output_dir,
            &fixer,
            &current_branch,
            &args,
            &stderr_log_path,
        )
        .await?
    };
    if let Some(commit_before) = fix_commit_before {
        let commit_after = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        if commit_before.trim() != commit_after.trim() {
            if push_branch_with_remote_sync(&repo_root, current_branch.as_str())? {
                println!("remote sync: rebased onto origin/{}", current_branch);
            }
            if !args.allow_dirty {
                if let Some(commit) = auto_checkpoint_if_needed(
                    &repo_root,
                    current_branch.as_str(),
                    "auto bug implementation checkpoint",
                )? {
                    println!("checkpoint:  committed trailing implementation changes at {commit}");
                }
            }
        } else if !args.allow_dirty {
            if let Some(commit) = auto_checkpoint_if_needed(
                &repo_root,
                current_branch.as_str(),
                "auto bug implementation checkpoint",
            )? {
                println!("checkpoint:  committed implementation changes at {commit}");
            }
        }
    }

    if !fixes.is_empty() {
        println!();
        println!("implementation: {} item(s)", fixes.len());
    }

    write_bug_summary(&output_dir, &outcomes, &fixes, args.report_only)?;
    let should_prune_bug_output = !args.report_only && !all_verified.is_empty();
    if should_prune_bug_output {
        fs::remove_dir_all(&output_dir)
            .with_context(|| format!("failed to prune {}", output_dir.display()))?;
    }
    println!();
    println!("bug run complete");
    if should_prune_bug_output {
        println!("cleanup:     pruned {}", output_dir.display());
    } else {
        println!(
            "summary:     {}",
            output_dir.join("BUG_REPORT.md").display()
        );
        println!(
            "verified:    {}",
            output_dir.join("verified-findings.json").display()
        );
        if !fixes.is_empty() {
            println!(
                "implemented: {}",
                output_dir.join("implementation-results.json").display()
            );
        }
        println!("stderr log:  {}", stderr_log_path.display());
    }

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

    let backend = select_backend(&config.model, &config.effort, &args.codex_bin, &args.pi_bin);
    print_phase_header("finder", chunk, &backend);
    let raw_response = run_backend_prompt(
        repo_root,
        &prompt,
        &backend,
        stderr_log_path,
        &format!("finder {} {}", chunk.id, backend.label()),
    )
    .await?;
    prune_bug_phase_pi_state(repo_root, &backend);
    atomic_write(&response_path, raw_response.as_bytes())?;

    let findings: Vec<BugFinding> = load_json_file(&findings_json_path)?;
    validate_findings(chunk, &findings)?;
    Ok(findings)
}

async fn load_or_run_finder_phase(
    repo_root: &Path,
    chunk: &RepoChunk,
    chunk_dir: &Path,
    config: &PhaseConfig,
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<Vec<BugFinding>> {
    if args.resume {
        let findings_json_path = chunk_dir.join("finder-findings.json");
        if let Some(findings) = try_resume_finder_findings(chunk, &findings_json_path)? {
            return Ok(findings);
        }
    }

    run_finder_phase(repo_root, chunk, chunk_dir, config, args, stderr_log_path).await
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

    let backend = select_backend(&config.model, &config.effort, &args.codex_bin, &args.pi_bin);
    print_phase_header("skeptic", chunk, &backend);
    let raw_response = run_backend_prompt(
        repo_root,
        &prompt,
        &backend,
        stderr_log_path,
        &format!("skeptic {} {}", chunk.id, backend.label()),
    )
    .await?;
    prune_bug_phase_pi_state(repo_root, &backend);
    atomic_write(&response_path, raw_response.as_bytes())?;

    let verdicts: Vec<SkepticVerdict> = load_json_file(&verdicts_json_path)?;
    let (disproved_count, accepted) = derive_accepted_findings(chunk, findings, &verdicts)?;
    atomic_write(
        &chunk_dir.join("accepted-findings.json"),
        serde_json::to_string_pretty(&accepted)?.as_bytes(),
    )?;
    Ok((disproved_count, accepted))
}

async fn load_or_run_skeptic_phase(
    repo_root: &Path,
    chunk: &RepoChunk,
    chunk_dir: &Path,
    config: &PhaseConfig,
    findings: &[BugFinding],
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<(usize, Vec<AcceptedFinding>)> {
    if args.resume {
        let accepted_json_path = chunk_dir.join("accepted-findings.json");
        if let Some(outcome) = try_resume_skeptic_outcome(chunk, findings, &accepted_json_path)? {
            return Ok(outcome);
        }
    }

    run_skeptic_phase(
        repo_root,
        chunk,
        chunk_dir,
        config,
        findings,
        args,
        stderr_log_path,
    )
    .await
}

async fn run_fix_phase(
    repo_root: &Path,
    output_dir: &Path,
    config: &PhaseConfig,
    branch: &str,
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<Vec<FixResult>> {
    let prompt_path = output_dir.join("implementation-prompt.md");
    let response_path = output_dir.join("implementation-response.jsonl");
    let results_json_path = output_dir.join("implementation-results.json");
    let results_md_path = output_dir.join("implementation-results.md");
    let verified_json_path = output_dir.join("verified-findings.json");
    let prompt = build_fix_prompt(
        &verified_json_path,
        &results_json_path,
        &results_md_path,
        branch,
    );
    atomic_write(&prompt_path, prompt.as_bytes())?;

    let backend = select_backend(&config.model, &config.effort, &args.codex_bin, &args.pi_bin);
    print_global_phase_header("implementer", &backend);
    let raw_response = run_backend_prompt(
        repo_root,
        &prompt,
        &backend,
        stderr_log_path,
        &format!("implementer {}", backend.label()),
    )
    .await?;
    prune_bug_phase_pi_state(repo_root, &backend);
    atomic_write(&response_path, raw_response.as_bytes())?;

    let results: Vec<FixResult> = load_json_file(&results_json_path)?;
    let verified: Vec<AcceptedFinding> = load_json_file(&verified_json_path)?;
    validate_fix_results(&verified, &results)?;
    Ok(results)
}

fn try_resume_fix_results(
    output_dir: &Path,
    verified: &[AcceptedFinding],
    resume: bool,
) -> Result<Option<Vec<FixResult>>> {
    if !resume {
        return Ok(None);
    }

    let results_json_path = output_dir.join("implementation-results.json");
    let Some(results) =
        try_load_existing_json::<Vec<FixResult>>(&results_json_path, "implementation results")?
    else {
        return Ok(None);
    };

    match validate_fix_results(verified, &results) {
        Ok(()) => {
            println!("resume:      reusing implementation results");
            Ok(Some(results))
        }
        Err(err) => {
            println!(
                "warning: ignoring invalid implementation results in {}: {err}",
                results_json_path.display()
            );
            Ok(None)
        }
    }
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
    let prompt = build_review_prompt(
        chunk,
        &accepted_json_path,
        &results_json_path,
        &results_md_path,
    );
    atomic_write(&prompt_path, prompt.as_bytes())?;

    let backend = select_backend(&config.model, &config.effort, &args.codex_bin, &args.pi_bin);
    print_phase_header("reviewer", chunk, &backend);
    let raw_response = run_backend_prompt(
        repo_root,
        &prompt,
        &backend,
        stderr_log_path,
        &format!("reviewer {} {}", chunk.id, backend.label()),
    )
    .await?;
    prune_bug_phase_pi_state(repo_root, &backend);
    atomic_write(&response_path, raw_response.as_bytes())?;

    let results: Vec<ReviewResult> = load_json_file(&results_json_path)?;
    validate_review_results(accepted, &results)?;
    Ok(results)
}

async fn load_or_run_review_phase(
    repo_root: &Path,
    chunk: &RepoChunk,
    chunk_dir: &Path,
    config: &PhaseConfig,
    accepted: &[AcceptedFinding],
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<Vec<ReviewResult>> {
    if args.resume {
        let results_json_path = chunk_dir.join("review-results.json");
        if let Some(results) = try_resume_review_results(chunk, accepted, &results_json_path)? {
            return Ok(results);
        }
    }

    run_review_phase(
        repo_root,
        chunk,
        chunk_dir,
        config,
        accepted,
        args,
        stderr_log_path,
    )
    .await
}

fn try_resume_finder_findings(
    chunk: &RepoChunk,
    findings_json_path: &Path,
) -> Result<Option<Vec<BugFinding>>> {
    let Some(findings) =
        try_load_existing_json::<Vec<BugFinding>>(findings_json_path, "finder findings")?
    else {
        return Ok(None);
    };

    match validate_findings(chunk, &findings) {
        Ok(()) => {
            println!("resume:      {} finder findings", chunk.id);
            Ok(Some(findings))
        }
        Err(err) => {
            println!(
                "warning: ignoring invalid finder findings in {}: {err}",
                findings_json_path.display()
            );
            Ok(None)
        }
    }
}

fn try_resume_skeptic_outcome(
    chunk: &RepoChunk,
    findings: &[BugFinding],
    accepted_json_path: &Path,
) -> Result<Option<(usize, Vec<AcceptedFinding>)>> {
    let Some(accepted) =
        try_load_existing_json::<Vec<AcceptedFinding>>(accepted_json_path, "accepted findings")?
    else {
        return Ok(None);
    };

    match validate_accepted_findings(findings, &accepted) {
        Ok(()) => {
            let disproved_count = findings.len().saturating_sub(accepted.len());
            println!("resume:      {} skeptic output", chunk.id);
            Ok(Some((disproved_count, accepted)))
        }
        Err(err) => {
            println!(
                "warning: ignoring invalid accepted findings in {}: {err}",
                accepted_json_path.display()
            );
            Ok(None)
        }
    }
}

fn try_resume_review_results(
    chunk: &RepoChunk,
    accepted: &[AcceptedFinding],
    results_json_path: &Path,
) -> Result<Option<Vec<ReviewResult>>> {
    let Some(results) =
        try_load_existing_json::<Vec<ReviewResult>>(results_json_path, "review results")?
    else {
        return Ok(None);
    };

    match validate_review_results(accepted, &results) {
        Ok(()) => {
            println!("resume:      {} review results", chunk.id);
            Ok(Some(results))
        }
        Err(err) => {
            println!(
                "warning: ignoring invalid review results in {}: {err}",
                results_json_path.display()
            );
            Ok(None)
        }
    }
}

fn try_load_existing_json<T>(path: &Path, label: &str) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(None);
    }

    match load_json_file(path) {
        Ok(parsed) => Ok(Some(parsed)),
        Err(err) => {
            println!(
                "warning: ignoring invalid existing {label} in {}: {err}",
                path.display()
            );
            Ok(None)
        }
    }
}

fn select_backend(model: &str, effort: &str, codex_bin: &Path, pi_bin: &Path) -> LlmBackend {
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

fn ensure_code_writer_config(label: &str, config: &PhaseConfig) -> Result<()> {
    if config.model.trim() != DEFAULT_CODEX_MODEL {
        bail!(
            "{label} must use `{}`; got `{}`",
            DEFAULT_CODEX_MODEL,
            config.model
        );
    }
    if config.effort.trim().to_ascii_lowercase() != DEFAULT_CODEX_REASONING_EFFORT {
        bail!(
            "{label} must use `{}` reasoning; got `{}`",
            DEFAULT_CODEX_REASONING_EFFORT,
            config.effort
        );
    }
    Ok(())
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

fn print_global_phase_header(phase: &str, backend: &LlmBackend) {
    println!();
    println!("phase:       {phase}");
    println!("scope:       verified findings");
    println!("backend:     {}", backend.label());
    println!("model:       {}", backend.model());
    println!("variant:     {}", backend.effort());
}

async fn run_backend_prompt(
    repo_root: &Path,
    prompt: &str,
    backend: &LlmBackend,
    stderr_log_path: &Path,
    stream_label: &str,
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
                .context("PI stdout should be piped for auto bug")?;
            let stderr = child
                .stderr
                .take()
                .context("PI stderr should be piped for auto bug")?;

            let stream_label = stream_label.to_string();
            let stdout_task =
                tokio::spawn(async move { capture_pi_output(stdout, &stream_label, 15).await });
            let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

            let status = child.wait().await.context("failed waiting for PI")?;
            let stdout = stdout_task
                .await
                .context("PI stdout capture task panicked")??;
            let stderr_text = stderr_task
                .await
                .context("PI stderr capture task panicked")??;
            append_stderr_log(stderr_log_path, &stderr_text)?;

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
    }
}

fn prune_bug_phase_pi_state(repo_root: &Path, backend: &LlmBackend) {
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
    atomic_write(stderr_log_path, &existing)?;
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
- Prefer findings with a believable reproduction path, violated invariant, and plausible root-cause region over vague smell reports.
- Cover correctness, state consistency, security, performance, and runtime behavior when the code supports them.
- Use bug IDs with prefix `BUG-{ordinal:03}-`.
- Match `points` to `impact` exactly.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
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
- Challenge whether the claim identifies a real root-cause bug instead of a symptom, style issue, or speculative concern.
- Prefer discarding findings that cannot be grounded in a runnable falsification path or direct code evidence.
- Only `accepted` findings should survive to verification.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
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
    verified_json: &Path,
    results_json: &Path,
    results_md: &Path,
    branch: &str,
) -> String {
    format!(
        r#"You are the final implementation pass in a multi-pass bug pipeline.

Implement every verified bug in the repository-wide findings set.

Input verified findings file:
- `{verified_json}`

Rules:
- Modify code only as needed to address the verified findings plus the minimum adjacent integration surfaces.
- Reproduce each bug with a failing test, failing command, or other executable proof first when practical. If that is truly not practical, document the best direct evidence you used instead of pretending.
- Fix root causes, not cosmetic symptoms.
- Add or update regression coverage for every `fixed` result when the repo has a real test surface for that behavior.
- Run validation commands that honestly support your changes.
- Stay on the currently checked-out branch `{branch}`.
- Commit only truthful fix increments with a message like `repo-name: bug fixes`.
- Push to `origin/{branch}` after each successful commit.
- Do not create or switch branches.
- Do not stage or commit unrelated pre-existing changes already present in the worktree.
- Do not stage or commit generated workflow artifacts under `bug/`, `.auto/`, `nemesis/`, or `gen-*`.
- Only write these files:
  - `{results_json}`
  - `{results_md}`
- `{results_json}` must be a JSON array with one entry per verified bug. If there are no verified bugs, write `[]`.

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-001-01",
  "status": "fixed|deferred|not_reproduced",
  "summary": "What changed and why",
  "validation_commands": ["Command actually run"],
  "touched_files": ["path/to/file"],
  "residual_risks": ["Anything still not fully closed"]
}}

Requirements:
- Treat verified findings as the contract; do not widen scope into unrelated cleanup.
- For browser-facing or runtime-sensitive bugs, use runtime/browser verification when available.
- `{results_md}` should summarize proof-before-fix, root cause, fix, validation, and any deferred items.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
"#,
        verified_json = verified_json.display(),
        results_json = results_json.display(),
        results_md = results_md.display(),
        branch = branch,
    )
}

fn build_review_prompt(
    chunk: &RepoChunk,
    accepted_json: &Path,
    results_json: &Path,
    results_md: &Path,
) -> String {
    format!(
        r#"You are the verification review pass in a multi-pass bug pipeline.

Review the skeptic-approved bugs for chunk `{chunk_id}` with primary scope `{scope}`.

Input accepted findings file:
- `{accepted_json}`

Rules:
- Treat the codebase as truth.
- Verify that each accepted bug is strong enough to survive to the final implementation pass.
- Do not modify code.
- Only write these files:
  - `{results_json}`
  - `{results_md}`
- `{results_json}` must be a JSON array with one entry per accepted bug. If there are no accepted bugs, write `[]`.

Each JSON item must use exactly this schema:
{{
  "bug_id": "BUG-{ordinal:03}-01",
  "verdict": "verified|discarded",
  "confidence": "high|medium|low",
  "notes": "Why this finding should or should not survive to implementation",
  "follow_up": ["Concrete follow-up validation or scoping note"]
}}

Requirements:
- `verified` means the finding should be implemented in the final GPT-5.4 implementation pass.
- `discarded` means the finding is too weak, duplicated, or insufficiently supported to implement.
- Prefer `verified` only when the bug is concrete enough to justify a reproduce-first/root-cause fix workflow.
- Call out missing regression coverage, missing runtime proof, or suspiciously broad scope in `follow_up`.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
- `{results_md}` should summarize what survived to implementation and what was discarded.
"#,
        chunk_id = chunk.id,
        scope = chunk.scope_label,
        accepted_json = accepted_json.display(),
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

fn validate_accepted_findings(findings: &[BugFinding], accepted: &[AcceptedFinding]) -> Result<()> {
    let finding_ids = findings
        .iter()
        .map(|finding| finding.bug_id.as_str())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    for finding in accepted {
        if !finding_ids.contains(finding.bug_id.as_str()) {
            bail!(
                "accepted findings contains unknown bug id `{}`",
                finding.bug_id
            );
        }
        if !seen.insert(finding.bug_id.as_str()) {
            bail!(
                "accepted findings contains duplicate bug id `{}`",
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

fn derive_verified_findings(
    accepted: &[AcceptedFinding],
    reviews: &[ReviewResult],
) -> Result<Vec<AcceptedFinding>> {
    let mut reviews_by_id = HashMap::<&str, &ReviewResult>::new();
    for review in reviews {
        reviews_by_id.insert(review.bug_id.as_str(), review);
    }

    let mut verified = Vec::new();
    for finding in accepted {
        let review = reviews_by_id
            .get(finding.bug_id.as_str())
            .with_context(|| format!("review output missing verdict for {}", finding.bug_id))?;
        match review.verdict.trim().to_ascii_lowercase().as_str() {
            "verified" => verified.push(finding.clone()),
            "discarded" => {}
            other => bail!("invalid review verdict `{other}` for {}", finding.bug_id),
        }
    }

    Ok(verified)
}

fn validate_fix_results(verified: &[AcceptedFinding], results: &[FixResult]) -> Result<()> {
    validate_bug_id_coverage(
        verified.iter().map(|finding| finding.bug_id.as_str()),
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
            "verified" | "discarded" => {}
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JsonRepairContext {
    Object(ObjectParseState),
    Array(ArrayParseState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectParseState {
    KeyOrEnd,
    Colon,
    Value,
    CommaOrEnd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayParseState {
    ValueOrEnd,
    CommaOrEnd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JsonStringRole {
    Key,
    Value,
}

fn load_json_file<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    match serde_json::from_str(&content) {
        Ok(parsed) => Ok(parsed),
        Err(original_error) => {
            let repair_candidate = json_repair_candidate(&content);
            if repair_candidate.len() > JSON_REPAIR_MAX_BYTES {
                bail!(
                    "failed to parse JSON from {}: {}; automatic repair skipped because the \
candidate is {} bytes and exceeds the {}-byte limit",
                    path.display(),
                    original_error,
                    repair_candidate.len(),
                    JSON_REPAIR_MAX_BYTES
                );
            }

            if let Some(repaired) = repair_llm_json_candidate(&repair_candidate, &content) {
                match serde_json::from_str(&repaired) {
                    Ok(parsed) => {
                        println!(
                            "warning: repaired invalid or incomplete JSON in {}",
                            path.display()
                        );
                        if repaired != content {
                            atomic_write(path, repaired.as_bytes())?;
                        }
                        Ok(parsed)
                    }
                    Err(repair_error) => bail!(
                        "failed to parse JSON from {}: {}; automatic repair also failed: {}",
                        path.display(),
                        original_error,
                        repair_error
                    ),
                }
            } else {
                bail!(
                    "failed to parse JSON from {}: {}",
                    path.display(),
                    original_error
                )
            }
        }
    }
}

fn repair_llm_json(content: &str) -> Option<String> {
    let candidate = json_repair_candidate(content);
    if candidate.len() > JSON_REPAIR_MAX_BYTES {
        return None;
    }
    repair_llm_json_candidate(&candidate, content)
}

fn json_repair_candidate(content: &str) -> String {
    extract_fenced_json_block(content).unwrap_or_else(|| content.to_string())
}

fn repair_llm_json_candidate(candidate: &str, original: &str) -> Option<String> {
    let escaped = escape_unescaped_quotes_in_json_strings(&candidate);
    let repaired = normalize_bug_pipeline_json_shapes(&escaped).unwrap_or(escaped);
    (repaired != original).then_some(repaired)
}

fn normalize_bug_pipeline_json_shapes(content: &str) -> Option<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let repaired = normalize_bug_pipeline_value(&mut value);
    repaired
        .then(|| serde_json::to_string_pretty(&value).ok())
        .flatten()
}

fn normalize_bug_pipeline_value(value: &mut serde_json::Value) -> bool {
    let serde_json::Value::Array(entries) = value else {
        return false;
    };

    let mut repaired = false;
    for entry in entries {
        let serde_json::Value::Object(object) = entry else {
            continue;
        };

        if object.contains_key("bug_id")
            && object.contains_key("title")
            && object.contains_key("impact")
            && object.contains_key("why_plausible")
        {
            repaired |= ensure_array_field(object, "falsification_checks");
            repaired |= ensure_array_field(object, "evidence");
            continue;
        }

        if object.contains_key("bug_id")
            && object.contains_key("decision")
            && object.contains_key("confidence_percent")
        {
            repaired |= ensure_array_field(object, "follow_up_checks");
            continue;
        }

        if object.contains_key("bug_id")
            && object.contains_key("status")
            && object.contains_key("summary")
        {
            repaired |= ensure_array_field(object, "validation_commands");
            repaired |= ensure_array_field(object, "touched_files");
            repaired |= ensure_array_field(object, "residual_risks");
            continue;
        }

        if object.contains_key("bug_id")
            && object.contains_key("verdict")
            && object.contains_key("confidence")
        {
            repaired |= ensure_array_field(object, "follow_up");
            continue;
        }

        if object.contains_key("bug_id")
            && object.contains_key("chunk_id")
            && object.contains_key("skeptic_confidence_percent")
        {
            repaired |= ensure_array_field(object, "falsification_checks");
            repaired |= ensure_array_field(object, "evidence");
            repaired |= ensure_array_field(object, "skeptic_follow_up_checks");
        }
    }

    repaired
}

fn ensure_array_field(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> bool {
    match object.get_mut(field) {
        None => {
            object.insert(field.to_string(), serde_json::Value::Array(Vec::new()));
            true
        }
        Some(serde_json::Value::Array(_)) => false,
        Some(serde_json::Value::Null) => {
            object.insert(field.to_string(), serde_json::Value::Array(Vec::new()));
            true
        }
        Some(serde_json::Value::String(existing)) => {
            let trimmed = existing.trim();
            let value = if trimmed.is_empty() {
                serde_json::Value::Array(Vec::new())
            } else {
                serde_json::Value::Array(vec![serde_json::Value::String(existing.clone())])
            };
            object.insert(field.to_string(), value);
            true
        }
        Some(_) => false,
    }
}

fn extract_fenced_json_block(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if !trimmed.starts_with("```") {
        return None;
    }

    let mut lines = trimmed.lines();
    let opening = lines.next()?.trim();
    if !opening.starts_with("```") {
        return None;
    }

    let mut extracted = String::new();
    let mut saw_closing = false;
    for line in lines {
        if line.trim_start().starts_with("```") {
            saw_closing = true;
            break;
        }
        extracted.push_str(line);
        extracted.push('\n');
    }

    saw_closing.then(|| extracted.trim().to_string())
}

fn escape_unescaped_quotes_in_json_strings(content: &str) -> String {
    let chars = content.chars().collect::<Vec<_>>();
    let mut repaired = String::with_capacity(content.len() + 32);
    let mut contexts = Vec::<JsonRepairContext>::new();
    let mut string_role = None::<JsonStringRole>;
    let mut escaped = false;
    let mut primitive_value = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if primitive_value {
            if matches!(ch, ',' | '}' | ']') {
                finish_json_value(&mut contexts);
                primitive_value = false;
                continue;
            }
            repaired.push(ch);
            index += 1;
            continue;
        }

        if let Some(role) = string_role {
            if escaped {
                repaired.push(ch);
                escaped = false;
                index += 1;
                continue;
            }

            match ch {
                '\\' => {
                    repaired.push(ch);
                    escaped = true;
                }
                '"' => {
                    if is_likely_string_terminator(&chars, index, role, &contexts) {
                        repaired.push(ch);
                        string_role = None;
                        finish_string_token(&mut contexts, role);
                    } else {
                        repaired.push('\\');
                        repaired.push('"');
                    }
                }
                _ => repaired.push(ch),
            }
            index += 1;
            continue;
        }

        match ch {
            '"' => {
                string_role = Some(current_string_role(&contexts));
                repaired.push(ch);
            }
            '{' => {
                contexts.push(JsonRepairContext::Object(ObjectParseState::KeyOrEnd));
                repaired.push(ch);
            }
            '[' => {
                contexts.push(JsonRepairContext::Array(ArrayParseState::ValueOrEnd));
                repaired.push(ch);
            }
            '}' => {
                repaired.push(ch);
                if matches!(contexts.last(), Some(JsonRepairContext::Object(_))) {
                    contexts.pop();
                    finish_json_value(&mut contexts);
                }
            }
            ']' => {
                repaired.push(ch);
                if matches!(contexts.last(), Some(JsonRepairContext::Array(_))) {
                    contexts.pop();
                    finish_json_value(&mut contexts);
                }
            }
            ':' => {
                repaired.push(ch);
                if let Some(JsonRepairContext::Object(state)) = contexts.last_mut() {
                    if *state == ObjectParseState::Colon {
                        *state = ObjectParseState::Value;
                    }
                }
            }
            ',' => {
                repaired.push(ch);
                advance_json_context_after_comma(&mut contexts);
            }
            ch if ch.is_whitespace() => repaired.push(ch),
            _ => {
                repaired.push(ch);
                primitive_value = context_expects_value(&contexts);
            }
        }

        index += 1;
    }

    if primitive_value {
        finish_json_value(&mut contexts);
    }

    repaired
}

fn current_string_role(contexts: &[JsonRepairContext]) -> JsonStringRole {
    match contexts.last() {
        Some(JsonRepairContext::Object(ObjectParseState::KeyOrEnd)) => JsonStringRole::Key,
        _ => JsonStringRole::Value,
    }
}

fn finish_string_token(contexts: &mut [JsonRepairContext], role: JsonStringRole) {
    match role {
        JsonStringRole::Key => {
            if let Some(JsonRepairContext::Object(state)) = contexts.last_mut() {
                *state = ObjectParseState::Colon;
            }
        }
        JsonStringRole::Value => finish_json_value(contexts),
    }
}

fn finish_json_value(contexts: &mut [JsonRepairContext]) {
    if let Some(context) = contexts.last_mut() {
        match context {
            JsonRepairContext::Object(state) if *state == ObjectParseState::Value => {
                *state = ObjectParseState::CommaOrEnd;
            }
            JsonRepairContext::Array(state) if *state == ArrayParseState::ValueOrEnd => {
                *state = ArrayParseState::CommaOrEnd;
            }
            _ => {}
        }
    }
}

fn advance_json_context_after_comma(contexts: &mut [JsonRepairContext]) {
    if let Some(context) = contexts.last_mut() {
        match context {
            JsonRepairContext::Object(state) if *state == ObjectParseState::CommaOrEnd => {
                *state = ObjectParseState::KeyOrEnd;
            }
            JsonRepairContext::Array(state) if *state == ArrayParseState::CommaOrEnd => {
                *state = ArrayParseState::ValueOrEnd;
            }
            _ => {}
        }
    }
}

fn context_expects_value(contexts: &[JsonRepairContext]) -> bool {
    matches!(
        contexts.last(),
        Some(JsonRepairContext::Object(ObjectParseState::Value))
            | Some(JsonRepairContext::Array(ArrayParseState::ValueOrEnd))
            | None
    )
}

fn is_likely_string_terminator(
    chars: &[char],
    quote_index: usize,
    role: JsonStringRole,
    contexts: &[JsonRepairContext],
) -> bool {
    let Some((delimiter_index, delimiter)) = next_significant_char(chars, quote_index + 1) else {
        return role == JsonStringRole::Value;
    };

    match role {
        JsonStringRole::Key => delimiter == ':',
        JsonStringRole::Value => match delimiter {
            '}' | ']' => true,
            ',' => {
                let Some((_, next_token)) = next_significant_char(chars, delimiter_index + 1)
                else {
                    return false;
                };
                match contexts.last() {
                    Some(JsonRepairContext::Object(ObjectParseState::Value)) => next_token == '"',
                    Some(JsonRepairContext::Array(ArrayParseState::ValueOrEnd)) => {
                        is_valid_array_value_start(next_token)
                    }
                    None => false,
                    _ => false,
                }
            }
            _ => false,
        },
    }
}

fn next_significant_char(chars: &[char], mut index: usize) -> Option<(usize, char)> {
    while index < chars.len() {
        let ch = chars[index];
        if !ch.is_whitespace() {
            return Some((index, ch));
        }
        index += 1;
    }
    None
}

fn is_valid_array_value_start(ch: char) -> bool {
    matches!(ch, '"' | '{' | '[' | '-' | 't' | 'f' | 'n') || ch.is_ascii_digit()
}

fn write_bug_summary(
    output_dir: &Path,
    outcomes: &[ChunkOutcome],
    fixes: &[FixResult],
    report_only: bool,
) -> Result<()> {
    let all_accepted = outcomes
        .iter()
        .flat_map(|outcome| outcome.accepted.clone())
        .collect::<Vec<_>>();
    let all_verified = outcomes
        .iter()
        .flat_map(|outcome| outcome.verified.clone())
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
    markdown.push_str(&format!("- Findings verified: `{}`\n", all_verified.len()));
    markdown.push_str(&format!(
        "- Findings disproved: `{}`\n",
        outcomes
            .iter()
            .map(|outcome| outcome.disproved_count)
            .sum::<usize>()
    ));
    markdown.push_str(&format!("- Implementation results: `{}`\n", fixes.len()));
    markdown.push_str(&format!("- Review verdicts: `{}`\n", all_reviews.len()));
    markdown.push_str(&format!(
        "- Mode: `{}`\n\n",
        if report_only {
            "report-only"
        } else {
            "verify-and-implement"
        }
    ));

    markdown.push_str("## Chunk Summary\n\n");
    for outcome in outcomes {
        markdown.push_str(&format!(
            "- `{}` (`{}`): {} reported, {} accepted, {} verified, {} disproved\n",
            outcome.chunk.id,
            outcome.chunk.scope_label,
            outcome.findings.len(),
            outcome.accepted.len(),
            outcome.verified.len(),
            outcome.disproved_count
        ));
    }

    markdown.push_str("\n## Verified Findings\n\n");
    if all_verified.is_empty() {
        markdown.push_str("No verified findings survived the review pass.\n");
    } else {
        for finding in &all_verified {
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

    markdown.push_str("## Verification Review\n\n");
    if all_reviews.is_empty() {
        markdown.push_str("No verification review output captured.\n");
    } else {
        for review in &all_reviews {
            markdown.push_str(&format!(
                "- `{}`: `{}` ({})\n",
                review.bug_id, review.verdict, review.confidence
            ));
        }
    }

    if !report_only {
        markdown.push_str("\n## Implementation Results\n\n");
        if fixes.is_empty() {
            markdown.push_str("No implementation output captured.\n");
        } else {
            for fix in fixes {
                markdown.push_str(&format!("- `{}`: `{}`\n", fix.bug_id, fix.status));
            }
        }
    }

    atomic_write(&output_dir.join("BUG_REPORT.md"), markdown.as_bytes())?;
    atomic_write(
        &output_dir.join("verified-findings.json"),
        serde_json::to_string_pretty(&all_verified)?.as_bytes(),
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

fn prepare_bug_output_dir(
    repo_root: &Path,
    output_dir: &Path,
    resume: bool,
) -> Result<(Option<PathBuf>, bool)> {
    if !resume {
        return Ok((prepare_output_dir(repo_root, output_dir)?, false));
    }

    if !output_dir.exists() {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        return Ok((None, false));
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
    Ok((None, has_contents))
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
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        build_fix_prompt, collect_repo_chunks, escape_unescaped_quotes_in_json_strings,
        load_json_file, repair_llm_json, run_backend_prompt, should_audit_path, slugify,
        validate_accepted_findings, AcceptedFinding, BugFinding, LlmBackend, SkepticVerdict,
    };
    use crate::pi_backend::PiProvider;

    fn temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-bug-{name}-{}-{nonce}", std::process::id()))
    }

    fn write_fake_pi_script(path: &Path) {
        let script = "#!/bin/sh\nprintf '[]\\n'\n";
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
            PiProvider::Minimax.resolve_model("minimax", "gpt-5.4"),
            "minimax/MiniMax-M2.7-highspeed"
        );
    }

    #[test]
    fn fix_prompt_requires_commit_and_push_to_current_branch() {
        let prompt = build_fix_prompt(
            Path::new("bug/verified-findings.json"),
            Path::new("bug/implementation-results.json"),
            Path::new("bug/implementation-results.md"),
            "main",
        );

        assert!(prompt.contains("Commit only truthful fix increments"));
        assert!(prompt.contains("Push to `origin/main` after each successful commit."));
        assert!(prompt.contains(
            "Do not stage or commit unrelated pre-existing changes already present in the worktree."
        ));
        assert!(prompt.contains(
            "Do not stage or commit generated workflow artifacts under `bug/`, `.auto/`, `nemesis/`, or `gen-*`."
        ));
    }

    #[test]
    fn accepted_findings_must_reference_known_bug_ids() {
        let findings = vec![BugFinding {
            bug_id: "BUG-001-01".to_string(),
            title: "title".to_string(),
            location: "path:1".to_string(),
            impact: "medium".to_string(),
            points: 5,
            description: "desc".to_string(),
            why_plausible: "why".to_string(),
            falsification_checks: vec!["check".to_string()],
            evidence: vec!["evidence".to_string()],
        }];
        let accepted = vec![AcceptedFinding {
            bug_id: "BUG-999-01".to_string(),
            chunk_id: "chunk-001-root".to_string(),
            title: "title".to_string(),
            location: "path:1".to_string(),
            impact: "medium".to_string(),
            points: 5,
            description: "desc".to_string(),
            why_plausible: "why".to_string(),
            falsification_checks: vec!["check".to_string()],
            evidence: vec!["evidence".to_string()],
            skeptic_confidence_percent: 90,
            skeptic_counter_argument: "counter".to_string(),
            skeptic_follow_up_checks: vec!["follow-up".to_string()],
        }];

        assert!(validate_accepted_findings(&findings, &accepted).is_err());
    }

    #[test]
    fn repairs_unescaped_quotes_inside_json_strings() {
        let invalid = r#"[
  {
    "bug_id": "BUG-003-02",
    "decision": "disproved",
    "confidence_percent": 95,
    "counter_argument": "The telemetry scraper matches '"message":"bitino-house live funding*'' lines and keeps the txid.",
    "risk_calculation": "Very low risk.",
    "follow_up_checks": ["Check the live logs"]
  }
]"#;

        assert!(serde_json::from_str::<Vec<SkepticVerdict>>(invalid).is_err());

        let repaired = escape_unescaped_quotes_in_json_strings(invalid);
        let parsed = serde_json::from_str::<Vec<SkepticVerdict>>(&repaired)
            .expect("repaired JSON should parse");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0]
            .counter_argument
            .contains("\"message\":\"bitino-house live funding*"));
    }

    #[test]
    fn repairs_missing_bug_finding_evidence_field() {
        let invalid = r#"[
  {
    "bug_id": "BUG-008-02",
    "title": "Missing evidence field should be repaired",
    "location": "services/home-miner-daemon/tests/test_launch_wallets.py",
    "impact": "medium",
    "points": 5,
    "description": "One finding omitted the evidence array entirely.",
    "why_plausible": "The JSON is otherwise valid and should not abort the run.",
    "falsification_checks": ["Inspect the generated finder output"]
  }
]"#;

        assert!(serde_json::from_str::<Vec<BugFinding>>(invalid).is_err());

        let repaired = repair_llm_json(invalid).expect("repair should add missing evidence");
        let parsed =
            serde_json::from_str::<Vec<BugFinding>>(&repaired).expect("repaired JSON should parse");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].evidence.is_empty());
        assert_eq!(parsed[0].falsification_checks.len(), 1);
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
            model: "kimi-coding/k2p5".to_string(),
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
        )
        .await;

        let stdout = result.expect("successful PI output should survive cleanup failures");
        assert_eq!(stdout, "[]\n");
    }

    #[test]
    fn oversized_invalid_json_skips_automatic_repair() {
        let path = temp_path("oversized-json").join("finder-response.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        let repeated_quotes = "\"broken\" ".repeat(40_000);
        let invalid = format!(
            "[{{\"bug_id\":\"BUG-001-01\",\"decision\":\"disproved\",\"confidence_percent\":95,\
\"counter_argument\":\"{repeated_quotes}\",\"risk_calculation\":\"low\",\"follow_up_checks\":[\"check\"]}}]"
        );
        fs::write(&path, invalid).expect("failed to write oversized invalid json");

        let error = load_json_file::<Vec<SkepticVerdict>>(&path)
            .expect_err("oversized invalid JSON should not attempt automatic repair");
        let message = error.to_string();
        assert!(message.contains("automatic repair skipped"));
        assert!(message.contains("exceeds"));
    }
}
