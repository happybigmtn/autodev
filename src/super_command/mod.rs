//! `auto super`: the staged corpus -> design -> review -> gen -> gate -> parallel orchestrator.

mod audit_harvest;
mod gate;
mod manifest;
mod stages;

use std::fs;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::design_command;
use crate::generation;
use crate::parallel_command;
use crate::state::load_state;
use crate::util::{atomic_write, binary_provenance_line, ensure_repo_layout, git_repo_root};
use crate::{CorpusArgs, ParallelAction, ParallelArgs, ParallelCargoTarget, SuperArgs};

use crate::super_command::audit_harvest::{run_super_audit_harvest, run_super_audit_phase};
use crate::super_command::gate::verify_super_snapshot_ready_plan;
use crate::super_command::manifest::{
    build_super_generation_args, prepare_super_run, push_skipped_stage_if_needed, push_stage,
    read_deterministic_gate, super_snapshot_parallel_decision, super_snapshot_promotion_command,
    super_stage_artifact, super_stage_terminal, super_stage_terminal_any, write_manifest,
    write_super_branch_reconciliation_plan, write_super_cross_repo_manifest,
    write_super_final_sanity,
};
use crate::super_command::stages::{
    audit_generated_plan_against_operator_bans, build_super_focus, run_super_corpus_review,
    run_super_execution_gate,
};

pub(crate) use audit_harvest::run_audit_harvest_standalone;

pub(crate) const SUPER_REPORT_FILES: [&str; 7] = [
    "CEO-14-DAY-PLAN.md",
    "FUNCTIONAL-REVIEWS.md",
    "PRODUCTION-READINESS.md",
    "RISK-REGISTER.md",
    "QUALITY-GATES.md",
    "SYSTEM-MAP.md",
    "SUPER-REPORT.md",
];
pub(crate) const EXECUTION_GATE_FILE: &str = "EXECUTION-GATE.md";
pub(crate) const IMPLEMENTATION_PLAN: &str = "IMPLEMENTATION_PLAN.md";
pub(crate) const SUPER_GENERATION_MODE_SNAPSHOT_ONLY: &str = "snapshot-only";
pub(crate) const SUPER_PLAN_SOURCE_GENERATED_SNAPSHOT: &str = "generated snapshot";
pub(crate) const SUPER_PLAN_SOURCE_ROOT_LEDGER: &str = "root ledger";
pub(crate) const SUPER_ROOT_PLAN_STATUS_UNCHANGED: &str = "unchanged";
pub(crate) const SUPER_EXECUTION_GATE_VERDICTS: [&str; 2] = ["Verdict: GO", "Verdict: NO-GO"];

pub(crate) async fn run_super(args: SuperArgs) -> Result<()> {
    let started_at = Instant::now();
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let mut args = args;
    let resume_requested = args.resume.is_some();
    let (super_root, mut manifest) = prepare_super_run(&repo_root, &mut args)?;
    let planning_root = args
        .planning_root
        .clone()
        .unwrap_or_else(|| repo_root.join("genesis"));
    let focus = build_super_focus(args.prompt.as_deref(), args.focus.as_deref());

    println!("auto super");
    println!("binary:      {}", binary_provenance_line());
    if resume_requested {
        println!("mode:        resume");
    }
    println!("repo root:   {}", repo_root.display());
    println!("planning:    {}", planning_root.display());
    if let Some(output_dir) = &args.output_dir {
        println!("output dir:  {}", output_dir.display());
    }
    println!("super root:  {}", super_root.display());
    println!("model:       {}", args.model);
    println!("effort:      {}", args.reasoning_effort);
    println!("workers:     {}", args.max_concurrent_workers.max(1));
    println!("gen mode:    {SUPER_GENERATION_MODE_SNAPSHOT_ONLY}");
    println!("root plan:   unchanged until explicit promotion");
    println!(
        "execute:     {}",
        if args.no_execute { "no" } else { "yes" }
    );

    if args.dry_run {
        println!("mode:        dry-run");
        println!(
            "stages:      corpus -> design perfection gate{} -> CEO functional review -> gen -> execution gate -> parallel",
            if args.skip_design { " (skipped)" } else { "" }
        );
        println!("snapshot:    generated gen-* output is staged for review");
        println!("promote:     auto gen --sync-only --output-dir <gen-dir>");
        if !args.skip_design && !args.no_execute {
            println!(
                "design fix:  up to {} resolve pass(es)",
                args.design_resolve_passes.max(1)
            );
        }
        return Ok(());
    }

    fs::create_dir_all(&super_root)
        .with_context(|| format!("failed to create {}", super_root.display()))?;
    write_manifest(&super_root, &manifest)?;
    write_super_cross_repo_manifest(&super_root, &repo_root, &planning_root, &args)?;

    if super_stage_terminal(&manifest, "corpus") {
        println!("stage:       corpus (resume skip)");
    } else {
        println!("stage:       corpus");
        generation::run_corpus(CorpusArgs {
            planning_root: Some(planning_root.clone()),
            idea: args.idea.clone(),
            focus: Some(focus.clone()),
            reference_repos: args.reference_repos.clone(),
            model: args.model.clone(),
            reasoning_effort: args.reasoning_effort.clone(),
            codex_review_model: args.model.clone(),
            codex_review_effort: args.reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            gbrain_bin: std::path::PathBuf::from("gbrain"),
            no_gbrain_context: false,
            skip_codex_review: false,
            resume_staging: None,
            verify_only: false,
            max_turns: args.max_turns,
            parallelism: args.planning_parallelism,
            dry_run: false,
        })
        .await?;
        push_stage(
            &super_root,
            &mut manifest,
            "corpus",
            "complete",
            Some(&planning_root),
        )?;
    }

    if args.skip_design {
        push_skipped_stage_if_needed(&super_root, &mut manifest, "design perfection gate")?;
    } else if super_stage_terminal(&manifest, "design perfection gate") {
        println!("stage:       design perfection gate (resume skip)");
    } else {
        println!("stage:       design perfection gate");
        design_command::run_super_design_module(&args, &repo_root, &planning_root, &super_root)
            .await?;
        push_stage(
            &super_root,
            &mut manifest,
            "design perfection gate",
            "complete",
            Some(&super_root.join("design")),
        )?;
    }

    if args.skip_super_review {
        push_skipped_stage_if_needed(&super_root, &mut manifest, "super corpus review")?;
    } else if super_stage_terminal_any(&manifest, &["CEO functional review", "super corpus review"])
    {
        println!("stage:       CEO functional review (resume skip)");
    } else {
        println!("stage:       CEO functional review");
        run_super_corpus_review(&args, &repo_root, &planning_root, &super_root).await?;
        push_stage(
            &super_root,
            &mut manifest,
            "CEO functional review",
            "complete",
            Some(&super_root),
        )?;
    }

    let output_dir = if super_stage_terminal(&manifest, "gen") {
        println!("stage:       gen (resume skip)");
        super_stage_artifact(&manifest, "gen").or_else(|| args.output_dir.clone())
    } else {
        println!("stage:       gen");
        generation::run_gen(build_super_generation_args(&args, &planning_root)).await?;
        let state = load_state(&repo_root)?;
        let output_dir = state
            .latest_output_dir
            .clone()
            .or_else(|| args.output_dir.clone());
        if let Some(output_dir) = output_dir.as_deref() {
            let command = super_snapshot_promotion_command(output_dir);
            manifest.output_dir = Some(output_dir.display().to_string());
            manifest.promotion_command = Some(command.clone());
            write_manifest(&super_root, &manifest)?;
            println!("snapshot:    {}", output_dir.display());
            println!("root plan:   unchanged");
            println!("promote:     {command}");
            audit_generated_plan_against_operator_bans(
                &output_dir.join(IMPLEMENTATION_PLAN),
                args.prompt.as_deref().or(args.focus.as_deref()),
            );
        }
        push_stage(
            &super_root,
            &mut manifest,
            "gen",
            "complete",
            output_dir.as_deref(),
        )?;
        output_dir
    };

    if args.with_audit {
        if super_stage_terminal(&manifest, "audit") {
            println!("stage:       audit (resume skip)");
        } else {
            println!("stage:       audit");
            let audit_run_id = run_super_audit_phase(&args, &repo_root).await?;
            manifest.audit_run_id = Some(audit_run_id.clone());
            write_manifest(&super_root, &manifest)?;
            push_stage(
                &super_root,
                &mut manifest,
                "audit",
                "complete",
                Some(
                    &repo_root
                        .join(".auto")
                        .join("audit-everything")
                        .join(&audit_run_id),
                ),
            )?;
        }

        if super_stage_terminal(&manifest, "audit harvest") {
            println!("stage:       audit harvest (resume skip)");
        } else if let Some(audit_run_id) = manifest.audit_run_id.clone() {
            println!("stage:       audit harvest");
            let harvest_artifact =
                run_super_audit_harvest(&args, &repo_root, &super_root, &audit_run_id).await?;
            push_stage(
                &super_root,
                &mut manifest,
                "audit harvest",
                "complete",
                Some(&harvest_artifact),
            )?;
        }
    } else {
        push_skipped_stage_if_needed(&super_root, &mut manifest, "audit")?;
        push_skipped_stage_if_needed(&super_root, &mut manifest, "audit harvest")?;
    }

    if args.skip_super_review {
        push_skipped_stage_if_needed(&super_root, &mut manifest, "execution gate review")?;
    } else if super_stage_terminal(&manifest, "execution gate review") {
        println!("stage:       execution gate review (resume skip)");
    } else {
        println!("stage:       execution gate review");
        run_super_execution_gate(
            &args,
            &repo_root,
            &planning_root,
            output_dir.as_deref(),
            &super_root,
        )
        .await?;
        push_stage(
            &super_root,
            &mut manifest,
            "execution gate review",
            "complete",
            Some(&super_root.join(EXECUTION_GATE_FILE)),
        )?;
    }

    let gate = if super_stage_terminal(&manifest, "deterministic execution gate") {
        println!("stage:       deterministic execution gate (resume skip)");
        read_deterministic_gate(&super_root)?
    } else {
        println!("stage:       deterministic execution gate");
        let gate = verify_super_snapshot_ready_plan(output_dir.as_deref())?;
        let gate_artifact = super_root.join("DETERMINISTIC-GATE.json");
        atomic_write(&gate_artifact, &serde_json::to_vec_pretty(&gate)?)
            .with_context(|| format!("failed to write {}", gate_artifact.display()))?;
        write_super_branch_reconciliation_plan(&super_root, &repo_root, &args, "pre-parallel")?;
        write_super_final_sanity(&super_root, &repo_root, &gate, &args, "pre-parallel")?;
        push_stage(
            &super_root,
            &mut manifest,
            "deterministic execution gate",
            "complete",
            Some(&gate_artifact),
        )?;
        gate
    };
    println!("ready tasks: {}", gate.unchecked_tasks);

    if args.no_execute {
        println!("auto super complete");
        println!("parallel:    skipped (--no-execute)");
        println!("super root:  {}", super_root.display());
        println!("elapsed:     {:?}", started_at.elapsed());
        return Ok(());
    }

    let parallel_decision = super_snapshot_parallel_decision(output_dir.as_deref())?;
    if !parallel_decision.launch {
        push_stage(
            &super_root,
            &mut manifest,
            "parallel",
            "skipped",
            output_dir.as_deref(),
        )?;
        println!("auto super complete");
        println!(
            "parallel:    skipped ({})",
            parallel_decision
                .skip_reason
                .as_deref()
                .unwrap_or("snapshot requires promotion")
        );
        if let Some(command) = parallel_decision.promotion_command {
            println!("promote:     {command}");
        }
        println!("super root:  {}", super_root.display());
        println!("elapsed:     {:?}", started_at.elapsed());
        return Ok(());
    }

    if super_stage_terminal(&manifest, "parallel") {
        println!("stage:       parallel (resume skip)");
    } else {
        println!("stage:       parallel");
        parallel_command::run_parallel(ParallelArgs {
            action: None::<ParallelAction>,
            apply_receipt_backfill_handoffs: false,
            json: false,
            apply: false,
            max_iterations: args.max_iterations,
            max_concurrent_workers: args.max_concurrent_workers.max(1),
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: args.worker_model.clone(),
            reasoning_effort: args.worker_reasoning_effort.clone(),
            branch: args.branch.clone(),
            reference_repos: args.reference_repos.clone(),
            include_siblings: false,
            run_root: Some(super_root.join("parallel")),
            codex_bin: args.codex_bin.clone(),
            claude: false,
            max_turns: None,
            max_retries: 2,
        })
        .await?;
        write_super_branch_reconciliation_plan(&super_root, &repo_root, &args, "post-parallel")?;
        write_super_final_sanity(&super_root, &repo_root, &gate, &args, "post-parallel")?;
        push_stage(
            &super_root,
            &mut manifest,
            "parallel",
            "launched",
            Some(&super_root.join("parallel")),
        )?;
    }

    println!("auto super complete");
    println!("super root:  {}", super_root.display());
    println!("elapsed:     {:?}", started_at.elapsed());
    Ok(())
}
