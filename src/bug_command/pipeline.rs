//! The read-only chunk pipeline plus the five `auto bug` phase runners and
//! their resumable wrappers.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::time::Duration;

use crate::bug_command::backend::{
    print_global_phase_header, print_phase_header, prune_bug_phase_pi_state,
    run_backend_prompt_with_fallback, select_backend,
};
use crate::bug_command::chunker::write_chunk_manifest;
use crate::bug_command::prompts::{
    build_final_review_prompt, build_finder_prompt, build_fix_prompt, build_review_prompt,
    build_skeptic_prompt, final_review_result_json_schema, finder_json_schema,
    fix_result_json_schema, review_result_json_schema, skeptic_verdict_json_schema,
};
use crate::bug_command::types::{
    AcceptedFinding, BugFinding, ChunkOutcome, FinalReviewResult, FixResult, PhaseConfig, RepoChunk,
    ReviewResult, SkepticVerdict,
};
use crate::bug_command::validate::{
    derive_accepted_findings, derive_verified_findings, load_json_file,
    load_json_file_with_backend_repair, normalize_and_validate_finder_findings,
    validate_accepted_findings, validate_final_review_results, validate_fix_results,
    validate_review_results,
};
use crate::bug_command::{
    BUG_CHUNK_PHASE_TIMEOUT_SECS, BUG_FINAL_REVIEW_PHASE_TIMEOUT_SECS,
    BUG_IMPLEMENTATION_PHASE_TIMEOUT_SECS, BUG_PHASE_MAX_ATTEMPTS,
};
use crate::util::atomic_write;
use crate::BugArgs;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_read_only_chunk_pipelines(
    repo_root: &Path,
    output_dir: &Path,
    chunks: Vec<RepoChunk>,
    finder: &PhaseConfig,
    skeptic: &PhaseConfig,
    reviewer: &PhaseConfig,
    args: &BugArgs,
) -> Result<Vec<ChunkOutcome>> {
    let lanes = args.read_parallelism.max(1);
    let semaphore = Arc::new(Semaphore::new(lanes));
    let mut handles = Vec::new();
    for chunk in chunks {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .context("bug chunk semaphore closed")?;
        let repo_root = repo_root.to_path_buf();
        let output_dir = output_dir.to_path_buf();
        let finder = finder.clone();
        let skeptic = skeptic.clone();
        let reviewer = reviewer.clone();
        let args = args.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            run_read_only_chunk_pipeline(
                &repo_root,
                &output_dir,
                chunk,
                &finder,
                &skeptic,
                &reviewer,
                &args,
            )
            .await
        }));
    }

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(
            handle
                .await
                .context("bug read-only chunk task panicked")??,
        );
    }
    outcomes.sort_by_key(|outcome| outcome.chunk.ordinal);
    Ok(outcomes)
}

async fn run_read_only_chunk_pipeline(
    repo_root: &Path,
    output_dir: &Path,
    chunk: RepoChunk,
    finder: &PhaseConfig,
    skeptic: &PhaseConfig,
    reviewer: &PhaseConfig,
    args: &BugArgs,
) -> Result<ChunkOutcome> {
    let chunk_dir = output_dir.join("chunks").join(&chunk.id);
    fs::create_dir_all(&chunk_dir)
        .with_context(|| format!("failed to create {}", chunk_dir.display()))?;
    write_chunk_manifest(&chunk_dir, &chunk)?;
    let stderr_log_path = chunk_dir.join("read-only.stderr.log");

    let findings = load_or_run_finder_phase(
        repo_root,
        &chunk,
        &chunk_dir,
        finder,
        args,
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
            repo_root,
            &chunk,
            &chunk_dir,
            skeptic,
            &findings,
            args,
            &stderr_log_path,
        )
        .await?
    };
    let reviews = if accepted.is_empty() {
        Vec::new()
    } else {
        load_or_run_review_phase(
            repo_root,
            &chunk,
            &chunk_dir,
            reviewer,
            &accepted,
            args,
            &stderr_log_path,
        )
        .await?
    };
    let verified = derive_verified_findings(&accepted, &reviews)?;

    println!(
        "summary:     {} | {} reported | {} accepted | {} verified | {} disproved",
        chunk.id,
        findings.len(),
        accepted.len(),
        verified.len(),
        disproved_count
    );
    Ok(ChunkOutcome {
        chunk,
        findings,
        disproved_count,
        accepted,
        verified,
        reviews,
    })
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
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    print_phase_header("finder", chunk, &backend);
    let (raw_response, backend) = run_backend_prompt_with_fallback(
        repo_root,
        &prompt,
        &backend,
        &args.codex_bin,
        stderr_log_path,
        &format!("finder {} {}", chunk.id, backend.label()),
        Duration::from_secs(BUG_CHUNK_PHASE_TIMEOUT_SECS),
    )
    .await?;
    prune_bug_phase_pi_state(repo_root, &backend);
    atomic_write(&response_path, raw_response.as_bytes())?;

    let findings: Vec<BugFinding> = load_json_file_with_backend_repair(
        repo_root,
        &findings_json_path,
        &backend,
        stderr_log_path,
        "finder findings",
        finder_json_schema(),
        &response_path,
    )
    .await?;
    let findings = normalize_and_validate_finder_findings(chunk, &findings_json_path, findings)?;
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
    let mut previous_errors = Vec::new();
    for attempt in 1..=BUG_PHASE_MAX_ATTEMPTS {
        if attempt > 1 {
            clear_skeptic_phase_outputs(chunk_dir)?;
        }

        match run_skeptic_phase_once(
            repo_root,
            chunk,
            chunk_dir,
            config,
            findings,
            args,
            stderr_log_path,
        )
        .await
        {
            Ok(outcome) => return Ok(outcome),
            Err(err) if attempt < BUG_PHASE_MAX_ATTEMPTS => {
                println!(
                    "warning: skeptic {} attempt {attempt} produced unusable output: {err}; retrying",
                    chunk.id
                );
                previous_errors.push(format!("attempt {attempt}: {err}"));
            }
            Err(err) => {
                if previous_errors.is_empty() {
                    return Err(err);
                }
                bail!(
                    "skeptic {} failed after {BUG_PHASE_MAX_ATTEMPTS} attempts; final error: {err}; previous errors: {}",
                    chunk.id,
                    previous_errors.join("; ")
                );
            }
        }
    }

    unreachable!("skeptic retry loop always returns")
}

async fn run_skeptic_phase_once(
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
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    print_phase_header("skeptic", chunk, &backend);
    let (raw_response, backend) = run_backend_prompt_with_fallback(
        repo_root,
        &prompt,
        &backend,
        &args.codex_bin,
        stderr_log_path,
        &format!("skeptic {} {}", chunk.id, backend.label()),
        Duration::from_secs(BUG_CHUNK_PHASE_TIMEOUT_SECS),
    )
    .await?;
    prune_bug_phase_pi_state(repo_root, &backend);
    atomic_write(&response_path, raw_response.as_bytes())?;

    let verdicts: Vec<SkepticVerdict> = load_json_file_with_backend_repair(
        repo_root,
        &verdicts_json_path,
        &backend,
        stderr_log_path,
        "skeptic verdicts",
        skeptic_verdict_json_schema(),
        &response_path,
    )
    .await?;
    let (disproved_count, accepted) = derive_accepted_findings(chunk, findings, &verdicts)?;
    atomic_write(
        &chunk_dir.join("accepted-findings.json"),
        serde_json::to_string_pretty(&accepted)?.as_bytes(),
    )?;
    Ok((disproved_count, accepted))
}

fn clear_skeptic_phase_outputs(chunk_dir: &Path) -> Result<()> {
    for file in [
        "skeptic-response.jsonl",
        "skeptic-verdicts.json",
        "skeptic-verdicts.md",
        "accepted-findings.json",
    ] {
        let path = chunk_dir.join(file);
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove stale {}", path.display()))?;
        }
    }
    Ok(())
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

/// Per-chunk fix-on-verify. Writes the chunk's verified findings to a local
/// JSON file, runs the configured implementer against them, and records the results
/// inside the chunk directory so `--resume` can skip already-landed chunks.
#[allow(clippy::too_many_arguments)]
async fn run_fix_phase_for_chunk(
    repo_root: &Path,
    chunk: &RepoChunk,
    chunk_dir: &Path,
    verified: &[AcceptedFinding],
    config: &PhaseConfig,
    branch: &str,
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<Vec<FixResult>> {
    let verified_json_path = chunk_dir.join("verified-findings.json");
    atomic_write(
        &verified_json_path,
        serde_json::to_string_pretty(&verified)?.as_bytes(),
    )?;
    let label = format!("implementer[{}]", chunk.id);
    run_fix_phase_at(
        repo_root,
        chunk_dir,
        &verified_json_path,
        config,
        branch,
        args,
        stderr_log_path,
        &label,
    )
    .await
}

/// Core fixer pass, parameterised by the output directory (top-level or
/// chunk-local) and the path the implementer should read verified findings
/// from.
#[allow(clippy::too_many_arguments)]
async fn run_fix_phase_at(
    repo_root: &Path,
    scope_dir: &Path,
    verified_json_path: &Path,
    config: &PhaseConfig,
    branch: &str,
    args: &BugArgs,
    stderr_log_path: &Path,
    phase_label: &str,
) -> Result<Vec<FixResult>> {
    let prompt_path = scope_dir.join("implementation-prompt.md");
    let response_path = scope_dir.join("implementation-response.jsonl");
    let results_json_path = scope_dir.join("implementation-results.json");
    let results_md_path = scope_dir.join("implementation-results.md");
    let prompt = build_fix_prompt(
        verified_json_path,
        &results_json_path,
        &results_md_path,
        branch,
    );
    atomic_write(&prompt_path, prompt.as_bytes())?;

    let backend = select_backend(
        &config.model,
        &config.effort,
        &args.codex_bin,
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    print_global_phase_header(phase_label, &backend);
    let (raw_response, backend) = run_backend_prompt_with_fallback(
        repo_root,
        &prompt,
        &backend,
        &args.codex_bin,
        stderr_log_path,
        &format!("{} {}", phase_label, backend.label()),
        Duration::from_secs(BUG_IMPLEMENTATION_PHASE_TIMEOUT_SECS),
    )
    .await?;
    prune_bug_phase_pi_state(repo_root, &backend);
    atomic_write(&response_path, raw_response.as_bytes())?;

    let results: Vec<FixResult> = load_json_file_with_backend_repair(
        repo_root,
        &results_json_path,
        &backend,
        stderr_log_path,
        "implementation results",
        fix_result_json_schema(),
        &response_path,
    )
    .await?;
    let verified: Vec<AcceptedFinding> = load_json_file(verified_json_path)?;
    validate_fix_results(&verified, &results)?;
    Ok(results)
}

/// Resumable wrapper around `run_fix_phase_for_chunk`. When the chunk already
/// has a valid `implementation-results.json` that covers every verified
/// finding, reuse it; otherwise invoke the fixer fresh.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn load_or_run_fix_phase_for_chunk(
    repo_root: &Path,
    chunk: &RepoChunk,
    chunk_dir: &Path,
    verified: &[AcceptedFinding],
    config: &PhaseConfig,
    branch: &str,
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<Vec<FixResult>> {
    if args.resume {
        let results_json_path = chunk_dir.join("implementation-results.json");
        if let Some(results) =
            try_load_existing_json::<Vec<FixResult>>(&results_json_path, "implementation results")?
        {
            match validate_fix_results(verified, &results) {
                Ok(()) => {
                    println!(
                        "resume:      {} implementation results ({} item(s))",
                        chunk.id,
                        results.len()
                    );
                    return Ok(results);
                }
                Err(err) => {
                    println!(
                        "warning: ignoring invalid implementation results in {}: {err}",
                        results_json_path.display()
                    );
                }
            }
        }
    }
    run_fix_phase_for_chunk(
        repo_root,
        chunk,
        chunk_dir,
        verified,
        config,
        branch,
        args,
        stderr_log_path,
    )
    .await
}

pub(crate) async fn run_final_review_phase(
    repo_root: &Path,
    output_dir: &Path,
    config: &PhaseConfig,
    branch: &str,
    args: &BugArgs,
    stderr_log_path: &Path,
) -> Result<Vec<FinalReviewResult>> {
    let prompt_path = output_dir.join("final-review-prompt.md");
    let response_path = output_dir.join("final-review-response.jsonl");
    let results_json_path = output_dir.join("final-review-results.json");
    let results_md_path = output_dir.join("final-review-results.md");
    let verified_json_path = output_dir.join("verified-findings.json");
    let implementation_json_path = output_dir.join("implementation-results.json");
    let prompt = build_final_review_prompt(
        &verified_json_path,
        &implementation_json_path,
        &results_json_path,
        &results_md_path,
        branch,
    );
    atomic_write(&prompt_path, prompt.as_bytes())?;

    let backend = select_backend(
        &config.model,
        &config.effort,
        &args.codex_bin,
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    print_global_phase_header("finalizer", &backend);
    let (raw_response, backend) = run_backend_prompt_with_fallback(
        repo_root,
        &prompt,
        &backend,
        &args.codex_bin,
        stderr_log_path,
        &format!("finalizer {}", backend.label()),
        Duration::from_secs(BUG_FINAL_REVIEW_PHASE_TIMEOUT_SECS),
    )
    .await?;
    prune_bug_phase_pi_state(repo_root, &backend);
    atomic_write(&response_path, raw_response.as_bytes())?;

    let results: Vec<FinalReviewResult> = load_json_file_with_backend_repair(
        repo_root,
        &results_json_path,
        &backend,
        stderr_log_path,
        "final review results",
        final_review_result_json_schema(),
        &response_path,
    )
    .await?;
    let verified: Vec<AcceptedFinding> = load_json_file(&verified_json_path)?;
    validate_final_review_results(&verified, &results)?;
    Ok(results)
}

pub(crate) fn try_resume_final_review_results(
    output_dir: &Path,
    verified: &[AcceptedFinding],
    resume: bool,
) -> Result<Option<Vec<FinalReviewResult>>> {
    if !resume {
        return Ok(None);
    }

    let results_json_path = output_dir.join("final-review-results.json");
    let Some(results) = try_load_existing_json::<Vec<FinalReviewResult>>(
        &results_json_path,
        "final review results",
    )?
    else {
        return Ok(None);
    };

    match validate_final_review_results(verified, &results) {
        Ok(()) => {
            println!("resume:      reusing final review results");
            Ok(Some(results))
        }
        Err(err) => {
            println!(
                "warning: ignoring invalid final review results in {}: {err}",
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

    let backend = select_backend(
        &config.model,
        &config.effort,
        &args.codex_bin,
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    print_phase_header("reviewer", chunk, &backend);
    let (raw_response, backend) = run_backend_prompt_with_fallback(
        repo_root,
        &prompt,
        &backend,
        &args.codex_bin,
        stderr_log_path,
        &format!("reviewer {} {}", chunk.id, backend.label()),
        Duration::from_secs(BUG_CHUNK_PHASE_TIMEOUT_SECS),
    )
    .await?;
    prune_bug_phase_pi_state(repo_root, &backend);
    atomic_write(&response_path, raw_response.as_bytes())?;

    let results: Vec<ReviewResult> = load_json_file_with_backend_repair(
        repo_root,
        &results_json_path,
        &backend,
        stderr_log_path,
        "review results",
        review_result_json_schema(),
        &response_path,
    )
    .await?;
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

    match normalize_and_validate_finder_findings(chunk, findings_json_path, findings) {
        Ok(findings) => {
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
