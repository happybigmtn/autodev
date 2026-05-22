//! `auto bug` — multi-pass LLM bug-hardening pipeline.
//!
//! `run_bug` wires arguments into per-phase configuration, drives the
//! read-only chunk pipeline, fixes verified findings per chunk, runs the final
//! review, and writes the run report. Submodules own the cohesive concerns:
//! `chunker` builds repo chunks, `pipeline` runs the five phases, `prompts`
//! and `validate` are pure logic, `backend` is the process layer, `report`
//! writes output, and `llm_json` holds the shared JSON-repair engine.

mod backend;
mod chunker;
pub(crate) mod llm_json;
mod pipeline;
mod prompts;
mod report;
mod types;
mod validate;

use std::fs;

use anyhow::{bail, Context, Result};

use crate::bug_command::backend::is_kimi_model;
use crate::bug_command::chunker::{collect_repo_chunks, write_bug_pre_index};
use crate::bug_command::pipeline::{
    load_or_run_fix_phase_for_chunk, run_final_review_phase, run_read_only_chunk_pipelines,
    try_resume_final_review_results,
};
use crate::bug_command::report::{prepare_bug_output_dir, write_bug_summary};
use crate::bug_command::types::{FixResult, PhaseConfig};
use crate::kimi_backend::{preflight_kimi_cli, resolve_kimi_bin};
use crate::pi_backend::PiProvider;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, sync_branch_with_remote,
};
use crate::{BugArgs, HardeningProfile};

const DEFAULT_CODEX_MODEL: &str = "gpt-5.5";
const DEFAULT_CODEX_DISCOVERY_REASONING_EFFORT: &str = "low";
const DEFAULT_CODEX_REASONING_EFFORT: &str = "high";
const BUG_CHUNK_PHASE_TIMEOUT_SECS: u64 = 30 * 60;
const BUG_IMPLEMENTATION_PHASE_TIMEOUT_SECS: u64 = 90 * 60;
const BUG_FINAL_REVIEW_PHASE_TIMEOUT_SECS: u64 = 90 * 60;
const BUG_PHASE_MAX_ATTEMPTS: usize = 2;

fn apply_bug_profile(
    profile: HardeningProfile,
    finder: &mut PhaseConfig,
    skeptic: &mut PhaseConfig,
    reviewer: &mut PhaseConfig,
    fixer: &mut PhaseConfig,
    finalizer: &mut PhaseConfig,
) {
    match profile {
        HardeningProfile::Balanced => {}
        HardeningProfile::Fast => {
            set_default_effort(finder, DEFAULT_CODEX_DISCOVERY_REASONING_EFFORT, "low");
            set_default_effort(skeptic, DEFAULT_CODEX_DISCOVERY_REASONING_EFFORT, "low");
            set_default_effort(reviewer, DEFAULT_CODEX_REASONING_EFFORT, "medium");
            set_default_effort(fixer, DEFAULT_CODEX_REASONING_EFFORT, "high");
            set_default_effort(finalizer, DEFAULT_CODEX_REASONING_EFFORT, "high");
        }
        HardeningProfile::MaxQuality => {
            set_default_effort(finder, DEFAULT_CODEX_DISCOVERY_REASONING_EFFORT, "xhigh");
            set_default_effort(skeptic, DEFAULT_CODEX_DISCOVERY_REASONING_EFFORT, "xhigh");
            set_default_effort(reviewer, DEFAULT_CODEX_REASONING_EFFORT, "xhigh");
            set_default_effort(fixer, DEFAULT_CODEX_REASONING_EFFORT, "xhigh");
            set_default_effort(finalizer, DEFAULT_CODEX_REASONING_EFFORT, "xhigh");
        }
    }
}

fn set_default_effort(config: &mut PhaseConfig, default_effort: &str, effort: &str) {
    if config.model == DEFAULT_CODEX_MODEL && config.effort == default_effort {
        config.effort = effort.to_string();
    }
}

fn display_phase_model(config: &PhaseConfig) -> String {
    PiProvider::detect(&config.model)
        .map(|provider| provider.resolve_model(&config.model, DEFAULT_CODEX_MODEL))
        .unwrap_or_else(|| config.model.clone())
}

fn ensure_code_writer_config(label: &str, config: &PhaseConfig) -> Result<()> {
    if config.model.trim() != DEFAULT_CODEX_MODEL {
        bail!(
            "{label} must use `{}`; got `{}`",
            DEFAULT_CODEX_MODEL,
            config.model
        );
    }
    let effort = config.effort.trim().to_ascii_lowercase();
    if effort != DEFAULT_CODEX_REASONING_EFFORT && effort != "xhigh" {
        bail!(
            "{label} must use `{}` or `xhigh` reasoning; got `{}`",
            DEFAULT_CODEX_REASONING_EFFORT,
            config.effort
        );
    }
    Ok(())
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
    if !args.dry_run {
        write_bug_pre_index(&output_dir, &chunks)?;
    }

    let stderr_log_path = output_dir.join("bug.stderr.log");
    let mut finder = PhaseConfig {
        model: args.finder_model.clone(),
        effort: args.finder_effort.clone(),
    };
    let mut skeptic = PhaseConfig {
        model: args.skeptic_model.clone(),
        effort: args.skeptic_effort.clone(),
    };
    let mut fixer = PhaseConfig {
        model: args.fixer_model.clone(),
        effort: args.fixer_effort.clone(),
    };
    let mut reviewer = PhaseConfig {
        model: args.reviewer_model.clone(),
        effort: args.reviewer_effort.clone(),
    };
    let mut finalizer = PhaseConfig {
        model: args.finalizer_model.clone(),
        effort: args.finalizer_effort.clone(),
    };
    apply_bug_profile(
        args.profile,
        &mut finder,
        &mut skeptic,
        &mut reviewer,
        &mut fixer,
        &mut finalizer,
    );
    ensure_code_writer_config("auto bug final review pass", &finalizer)?;
    let kimi_preflight_model = [&finder, &skeptic, &reviewer, &fixer]
        .iter()
        .find(|config| is_kimi_model(&config.model))
        .map(|config| config.model.as_str());
    if args.use_kimi_cli {
        if let Some(model) = kimi_preflight_model {
            let kimi_bin = resolve_kimi_bin(&args.kimi_bin);
            preflight_kimi_cli(&kimi_bin, model).with_context(|| {
                format!(
                    "kimi-cli preflight failed. Pipeline aborted before touching {} \
                     chunks; no work was wasted.",
                    chunks.len()
                )
            })?;
        }
    }

    println!("auto bug");
    println!("repo root:   {}", repo_root.display());
    println!("output dir:  {}", output_dir.display());
    println!("chunks:      {}", chunks.len());
    println!("profile:     {:?}", args.profile);
    println!("read lanes:  {}", args.read_parallelism.max(1));
    println!(
        "finder:      {} ({})",
        display_phase_model(&finder),
        finder.effort
    );
    println!(
        "skeptic:     {} ({})",
        display_phase_model(&skeptic),
        skeptic.effort
    );
    println!(
        "reviewer:    {} ({})",
        display_phase_model(&reviewer),
        reviewer.effort
    );
    println!(
        "implementer: {} ({})",
        display_phase_model(&fixer),
        fixer.effort
    );
    println!(
        "finalizer:   {} ({})",
        display_phase_model(&finalizer),
        finalizer.effort
    );
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
    if !args.report_only && !args.allow_dirty {
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, current_branch.as_str(), "auto bug checkpoint")?
        {
            println!("checkpoint:  committed pre-existing changes at {commit}");
        } else if !current_branch.is_empty()
            && sync_branch_with_remote(&repo_root, current_branch.as_str())?
        {
            println!("remote sync: rebased onto origin/{}", current_branch);
        }
    } else if !args.report_only
        && !current_branch.is_empty()
        && sync_branch_with_remote(&repo_root, current_branch.as_str())?
    {
        println!("remote sync: rebased onto origin/{}", current_branch);
    }

    let outcomes = run_read_only_chunk_pipelines(
        &repo_root,
        &output_dir,
        chunks,
        &finder,
        &skeptic,
        &reviewer,
        &args,
    )
    .await?;
    let mut per_chunk_fixes: Vec<FixResult> = Vec::new();
    for outcome in &outcomes {
        let chunk = &outcome.chunk;
        let chunk_dir = output_dir.join("chunks").join(&chunk.id);
        // Fix-on-verify: as soon as this chunk's findings survive the skeptic
        // and reviewer, run the configured implementer against them and commit the
        // diff. Keeps remediation atomic per chunk so resume can skip over
        // landed work without re-prompting the fixer.
        let chunk_fixes = if args.report_only || outcome.verified.is_empty() {
            Vec::new()
        } else {
            let results = load_or_run_fix_phase_for_chunk(
                &repo_root,
                chunk,
                &chunk_dir,
                &outcome.verified,
                &fixer,
                current_branch.as_str(),
                &args,
                &stderr_log_path,
            )
            .await?;
            if !current_branch.is_empty() && !args.allow_dirty {
                if let Some(commit) = auto_checkpoint_if_needed(
                    &repo_root,
                    current_branch.as_str(),
                    &format!("auto bug fix {}", chunk.id),
                )? {
                    println!(
                        "checkpoint:  committed chunk {} fixes at {}",
                        chunk.id, commit
                    );
                }
            }
            results
        };
        per_chunk_fixes.extend(chunk_fixes.clone());
    }

    let all_verified = outcomes
        .iter()
        .flat_map(|outcome| outcome.verified.clone())
        .collect::<Vec<_>>();
    atomic_write(
        &output_dir.join("verified-findings.json"),
        serde_json::to_string_pretty(&all_verified)?.as_bytes(),
    )?;

    // Aggregate per-chunk fix results as the canonical
    // `implementation-results.json`. The old `run_fix_phase` aggregate call is
    // gone now that fixes land per-chunk; the aggregate file just mirrors the
    // union so the downstream finalizer phase + existing resume tooling still
    // have the shape they expect.
    let aggregate_fixes_path = output_dir.join("implementation-results.json");
    if !args.report_only && !per_chunk_fixes.is_empty() {
        atomic_write(
            &aggregate_fixes_path,
            serde_json::to_string_pretty(&per_chunk_fixes)?.as_bytes(),
        )?;
    }
    let resumed_final_review_results = if args.report_only || all_verified.is_empty() {
        None
    } else {
        try_resume_final_review_results(&output_dir, &all_verified, args.resume)?
    };
    let code_phase_commit_before =
        if args.report_only || all_verified.is_empty() || resumed_final_review_results.is_some() {
            None
        } else {
            Some(git_stdout(&repo_root, ["rev-parse", "HEAD"])?)
        };
    let fixes = per_chunk_fixes;
    let final_reviews = if args.report_only || all_verified.is_empty() {
        Vec::new()
    } else if let Some(results) = resumed_final_review_results {
        results
    } else {
        run_final_review_phase(
            &repo_root,
            &output_dir,
            &finalizer,
            &current_branch,
            &args,
            &stderr_log_path,
        )
        .await?
    };
    if let Some(commit_before) = code_phase_commit_before {
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
    if !final_reviews.is_empty() {
        println!("final review:  {} item(s)", final_reviews.len());
    }

    write_bug_summary(
        &output_dir,
        &outcomes,
        &fixes,
        &final_reviews,
        args.report_only,
    )?;
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
        if !final_reviews.is_empty() {
            println!(
                "finalized:   {}",
                output_dir.join("final-review-results.json").display()
            );
        }
        println!("stderr log:  {}", stderr_log_path.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::pi_backend::PiProvider;

    #[test]
    fn bug_pipeline_minimax_alias_defaults_to_m27_highspeed() {
        assert_eq!(
            PiProvider::Minimax.resolve_model("minimax", "gpt-5.5"),
            "minimax/MiniMax-M2.7-highspeed"
        );
    }
}
