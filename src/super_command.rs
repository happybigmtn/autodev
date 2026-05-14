use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::audit_everything::{AuditFinding, AuditFindingsSummary};
use crate::backend_policy::PipelineStage;
use crate::codex_exec::run_codex_exec_max_context;
use crate::design_command;
use crate::generation;
use crate::parallel_command;
use crate::prompt_builder::{EthosPosture, PromptSpec};
use crate::state::load_state;
use crate::task_parser::{parse_tasks, validate_execution_row, PLAN_TASK_PROCESS_FIELDS};
use crate::util::{
    atomic_write, binary_provenance_line, ensure_repo_layout, git_repo_root, timestamp_slug,
};
use crate::{
    AuditHarvestArgs, CorpusArgs, GenerationArgs, ParallelAction, ParallelArgs,
    ParallelCargoTarget, SuperArgs,
};

// `harvest_cluster` is a sibling source file but kept as a submodule of
// `super_command` so the canonical `src/harvest_cluster.rs` layout works
// without an extra `mod` declaration at the crate root.
#[path = "harvest_cluster.rs"]
mod harvest_cluster;

use harvest_cluster::{
    classify_complexity, cluster_findings, ClusterGroup, ComplexityClass,
};

/// Canonical machine-readable findings emitted by the corpus-review phase.
/// Replaces the historical seven-file markdown bundle
/// (`CEO-14-DAY-PLAN.md`, `FUNCTIONAL-REVIEWS.md`, `PRODUCTION-READINESS.md`,
/// `RISK-REGISTER.md`, `QUALITY-GATES.md`, `SYSTEM-MAP.md`, `SUPER-REPORT.md`),
/// the `CROSS-REPO-MANIFEST.json` stub, and the `CODEBASE-BOOK/` dump --
/// all of which had no downstream consumers and only existence-checked.
const SUPER_FINDINGS_FILE: &str = "super-findings.json";
/// Human-readable narrative rendered from the JSON. Operator-facing.
const SUPER_REPORT_FILE: &str = "SUPER-REPORT.md";
const EXECUTION_GATE_FILE: &str = "EXECUTION-GATE.md";
const IMPLEMENTATION_PLAN: &str = "IMPLEMENTATION_PLAN.md";

// Typed `super-findings.json` schema + deterministic operator-queue
// resolvers (`auto_resolve_deterministic_entries`, baseline-walk,
// env-default, gdd-composition).
include!("super/findings.rs");

#[derive(Clone, Deserialize, Serialize)]
struct SuperManifest {
    run_id: String,
    repo_root: String,
    planning_root: String,
    output_dir: Option<String>,
    super_root: String,
    prompt: Option<String>,
    focus: Option<String>,
    model: String,
    reasoning_effort: String,
    worker_model: String,
    worker_reasoning_effort: String,
    max_concurrent_workers: usize,
    max_iterations: Option<usize>,
    execute: bool,
    design_enabled: bool,
    #[serde(default)]
    super_review_skipped: bool,
    design_resolve_passes: usize,
    #[serde(default)]
    with_audit: bool,
    #[serde(default)]
    audit_threads: usize,
    #[serde(default)]
    audit_first_pass_retries: usize,
    #[serde(default)]
    audit_run_id: Option<String>,
    branch: Option<String>,
    reference_repos: Vec<String>,
    binary: String,
    stages: Vec<SuperStage>,
}

#[derive(Clone, Deserialize, Serialize)]
struct SuperStage {
    name: String,
    status: String,
    artifact: Option<String>,
}

pub(crate) async fn run_super(args: SuperArgs) -> Result<()> {
    let started_at = Instant::now();
    let repo_root = git_repo_root()?;
    let repo_slug = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    crate::session_survival::reexec_if_reapable(&format!("super-{repo_slug}"))?;
    crate::gc_command::warn_if_auto_dir_oversized(&repo_root);
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
            skip_codex_review: false,
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
        // Auto-resolve deterministic operator-queue entries (baseline-walk,
        // env-default, gdd-composition) before the execution gate reads
        // super-findings.json. Entries with `policy = external` or `manual`
        // stay parked for human review.
        let auto_resolve_count = auto_resolve_super_findings_in_place(&repo_root, &super_root)?;
        if auto_resolve_count > 0 {
            println!(
                "auto-resolved: {auto_resolve_count} deterministic operator-queue entr{}",
                if auto_resolve_count == 1 { "y" } else { "ies" },
            );
        }
        // Deterministic gate pass over super-findings.json. Informational for
        // v1 -- the execution-gate LLM run is still the final say. Emits a
        // severity-count verdict and (when a prior super run exists) a
        // blocker-bitrot delta so the operator can spot stuck IDs.
        emit_super_gate_signals(&repo_root, &super_root);
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
        generation::run_gen(GenerationArgs {
            planning_root: Some(planning_root.clone()),
            output_dir: args.output_dir.clone(),
            model: args.model.clone(),
            reasoning_effort: args.reasoning_effort.clone(),
            codex_review_model: args.model.clone(),
            codex_review_effort: args.reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            skip_codex_review: false,
            max_turns: args.max_turns,
            parallelism: args.planning_parallelism,
            plan_only: false,
            snapshot_only: false,
            sync_only: false,
        })
        .await?;
        let state = load_state(&repo_root)?;
        let output_dir = state
            .latest_output_dir
            .clone()
            .or_else(|| args.output_dir.clone());
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
                Some(&repo_root.join(".auto").join("audit-everything").join(&audit_run_id)),
            )?;
        }

        if super_stage_terminal(&manifest, "audit harvest") {
            println!("stage:       audit harvest (resume skip)");
        } else if let Some(audit_run_id) = manifest.audit_run_id.clone() {
            println!("stage:       audit harvest");
            let harvest_artifact = run_super_audit_harvest(
                &args,
                &repo_root,
                &super_root,
                &audit_run_id,
            )
            .await?;
            push_stage(
                &super_root,
                &mut manifest,
                "audit harvest",
                "complete",
                Some(&harvest_artifact),
            )?;
            // Build a coverage row from the audit run's skipped.json so the
            // operator can spot allowlist drift without re-running the audit.
            emit_super_coverage_report(
                &repo_root,
                &super_root,
                manifest.audit_run_id.as_deref(),
            );
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
        let gate = verify_parallel_ready_plan(&repo_root.join(IMPLEMENTATION_PLAN))?;
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
        crate::gc_command::archive_after_super_run(&repo_root);
        return Ok(());
    }

    if super_stage_terminal(&manifest, "parallel") {
        println!("stage:       parallel (resume skip)");
    } else {
        println!("stage:       parallel");
        run_super_parallel_stage(&args, &repo_root, &super_root, &gate).await?;
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
    crate::gc_command::archive_after_super_run(&repo_root);
    Ok(())
}

fn prepare_super_run(repo_root: &Path, args: &mut SuperArgs) -> Result<(PathBuf, SuperManifest)> {
    if let Some(resume_root) = args.resume.clone() {
        let super_root = absolutize_super_path(repo_root, &resume_root);
        let manifest = load_super_manifest(&super_root)?;
        if manifest.repo_root != repo_root.display().to_string() {
            bail!(
                "refusing to resume auto super run rooted at `{}` from repo `{}`; manifest belongs to `{}`",
                super_root.display(),
                repo_root.display(),
                manifest.repo_root
            );
        }
        hydrate_super_args_from_manifest(args, &manifest);
        return Ok((super_root, manifest));
    }

    let run_id = timestamp_slug();
    let super_root = repo_root.join(".auto").join("super").join(&run_id);
    let planning_root = args
        .planning_root
        .clone()
        .unwrap_or_else(|| repo_root.join("genesis"));
    let manifest = SuperManifest {
        run_id,
        repo_root: repo_root.display().to_string(),
        planning_root: planning_root.display().to_string(),
        output_dir: args
            .output_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        super_root: super_root.display().to_string(),
        prompt: args.prompt.clone(),
        focus: args.focus.clone(),
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        worker_model: args.worker_model.clone(),
        worker_reasoning_effort: args.worker_reasoning_effort.clone(),
        max_concurrent_workers: args.max_concurrent_workers.max(1),
        max_iterations: args.max_iterations,
        execute: !args.no_execute,
        design_enabled: !args.skip_design,
        super_review_skipped: args.skip_super_review,
        design_resolve_passes: if args.no_execute || args.skip_design {
            0
        } else {
            args.design_resolve_passes.max(1)
        },
        with_audit: args.with_audit,
        audit_threads: args.audit_threads.max(1),
        audit_first_pass_retries: args.audit_first_pass_retries,
        audit_run_id: args.audit_run_id.clone(),
        branch: args.branch.clone(),
        reference_repos: args
            .reference_repos
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        binary: binary_provenance_line(),
        stages: Vec::new(),
    };
    Ok((super_root, manifest))
}

fn absolutize_super_path(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

fn load_super_manifest(super_root: &Path) -> Result<SuperManifest> {
    let path = super_root.join("manifest.json");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn hydrate_super_args_from_manifest(args: &mut SuperArgs, manifest: &SuperManifest) {
    if args.prompt.is_none() {
        args.prompt = manifest.prompt.clone();
    }
    if args.focus.is_none() {
        args.focus = manifest.focus.clone();
    }
    if args.planning_root.is_none() {
        args.planning_root = Some(PathBuf::from(&manifest.planning_root));
    }
    if args.output_dir.is_none() {
        args.output_dir = manifest.output_dir.as_ref().map(PathBuf::from);
    }
    if args.branch.is_none() {
        args.branch = manifest.branch.clone();
    }
    if args.reference_repos.is_empty() {
        args.reference_repos = manifest.reference_repos.iter().map(PathBuf::from).collect();
    }
    args.model = manifest.model.clone();
    args.reasoning_effort = manifest.reasoning_effort.clone();
    args.worker_model = manifest.worker_model.clone();
    args.worker_reasoning_effort = manifest.worker_reasoning_effort.clone();
    args.max_concurrent_workers = manifest.max_concurrent_workers.max(1);
    args.max_iterations = manifest.max_iterations;
    args.no_execute = !manifest.execute;
    args.skip_design = !manifest.design_enabled;
    args.skip_super_review = manifest.super_review_skipped;
    if manifest.design_resolve_passes > 0 {
        args.design_resolve_passes = manifest.design_resolve_passes;
    }
    args.with_audit = manifest.with_audit;
    if manifest.audit_threads > 0 {
        args.audit_threads = manifest.audit_threads;
    }
    if manifest.audit_first_pass_retries > 0 {
        args.audit_first_pass_retries = manifest.audit_first_pass_retries;
    }
    if args.audit_run_id.is_none() && manifest.audit_run_id.is_some() {
        args.audit_run_id = manifest.audit_run_id.clone();
    }
}

fn super_stage_terminal(manifest: &SuperManifest, name: &str) -> bool {
    manifest
        .stages
        .iter()
        .rev()
        .find(|stage| stage.name == name)
        .is_some_and(|stage| matches!(stage.status.as_str(), "complete" | "skipped" | "launched"))
}

fn super_stage_terminal_any(manifest: &SuperManifest, names: &[&str]) -> bool {
    names
        .iter()
        .any(|name| super_stage_terminal(manifest, name))
}

fn super_stage_artifact(manifest: &SuperManifest, name: &str) -> Option<PathBuf> {
    manifest
        .stages
        .iter()
        .rev()
        .find(|stage| stage.name == name)
        .and_then(|stage| stage.artifact.as_ref())
        .map(PathBuf::from)
}

fn push_skipped_stage_if_needed(
    super_root: &Path,
    manifest: &mut SuperManifest,
    name: &str,
) -> Result<()> {
    if super_stage_terminal(manifest, name) {
        println!("stage:       {name} (resume skip)");
        return Ok(());
    }
    push_stage(super_root, manifest, name, "skipped", None)
}

fn read_deterministic_gate(super_root: &Path) -> Result<DeterministicGateSummary> {
    let path = super_root.join("DETERMINISTIC-GATE.json");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn write_super_branch_reconciliation_plan(
    super_root: &Path,
    repo_root: &Path,
    args: &SuperArgs,
    phase: &str,
) -> Result<()> {
    let branch =
        git_text(repo_root, ["branch", "--show-current"]).unwrap_or_else(|| "unknown".to_string());
    let head = git_text(repo_root, ["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let status = git_text(repo_root, ["status", "--short", "--branch"])
        .unwrap_or_else(|| "git status unavailable".to_string());
    let target = args.branch.as_deref().unwrap_or(branch.as_str());
    let content = format!(
        "# Auto Super Branch Reconciliation\n\n\
Phase: `{phase}`\n\
Primary repo: `{}`\n\
Active branch: `{branch}`\n\
Parallel target branch: `{target}`\n\
HEAD: `{head}`\n\n\
## Current Status\n\n```text\n{}\n```\n\n\
## Reconciliation Doctrine\n\n\
1. Do not merge this branch into trunk while auto super or auto parallel is still mutating it.\n\
2. Preserve dirty operator/audit artifacts on trunk before updating trunk from origin.\n\
3. After the run is complete, merge or intentionally cherry-pick this branch into trunk, then run the gate commands named in `FINAL-SANITY.md`.\n\
4. Push trunk only after queue truth, receipts, branch head, and remote head agree.\n",
        repo_root.display(),
        status.trim()
    );
    let path = super_root.join("BRANCH-RECONCILIATION.md");
    atomic_write(&path, content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

fn write_super_final_sanity(
    super_root: &Path,
    repo_root: &Path,
    gate: &DeterministicGateSummary,
    args: &SuperArgs,
    phase: &str,
) -> Result<()> {
    let branch =
        git_text(repo_root, ["branch", "--show-current"]).unwrap_or_else(|| "unknown".to_string());
    let head = git_text(repo_root, ["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let remote =
        git_text(repo_root, ["ls-remote", "--heads", "origin", &branch]).unwrap_or_default();
    let remote_head = remote
        .split_whitespace()
        .next()
        .unwrap_or("unavailable")
        .to_string();
    let content = format!(
        "# Auto Super Final Sanity\n\n\
Phase: `{phase}`\n\
Branch: `{branch}`\n\
HEAD: `{head}`\n\
Remote HEAD: `{remote_head}`\n\
Execute: `{}`\n\
Ready tasks at deterministic gate: `{}`\n\
Priority tasks: `{}`\n\
Follow-on tasks: `{}`\n\
Worker model: `{}`\n\
Worker reasoning effort: `{}`\n\n\
## Required Closeout Checks\n\n\
- Root queue has no accidental empty or malformed executable rows.\n\
- Every landed implementation item has a `REVIEW.md` handoff or repo-native completion artifact.\n\
- Verification receipts exist for executable `Verification:` commands where the repo requires the wrapper.\n\
- No lane repo remains in cherry-pick, rebase, or stale `rebase-merge` recovery.\n\
- Branch reconciliation is recorded in `BRANCH-RECONCILIATION.md` before trunk is pushed.\n",
        !args.no_execute,
        gate.unchecked_tasks,
        gate.priority_tasks,
        gate.follow_on_tasks,
        args.worker_model,
        args.worker_reasoning_effort,
    );
    let path = super_root.join("FINAL-SANITY.md");
    atomic_write(&path, content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

fn git_text<const N: usize>(repo_root: &Path, args: [&str; N]) -> Option<String> {
    crate::util::git_stdout(repo_root, args)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn build_super_focus(prompt: Option<&str>, focus: Option<&str>) -> String {
    let mut parts = Vec::new();
    parts.push(
        "You are the new CEO inheriting this codebase. Over the next 14 days, race it to production with unlimited compute and resources. Do not capacity-trim the ambition: prioritize the deliverables that maximize production readiness, then assume max parallel execution can attack them. Perfect design/runtime integrity first, then run equally rigorous functional reviews across product, engineering, security, reliability, QA, data/contracts, operations, release, DX, and performance. Keep auto corpus and auto gen as the control primitives, but shape the corpus toward release blockers, operator trust, verification evidence, first-run DX, and maintainable execution contracts.",
    );
    if let Some(prompt) = prompt.filter(|value| !value.trim().is_empty()) {
        parts.push(prompt.trim());
    }
    if let Some(focus) = focus.filter(|value| !value.trim().is_empty()) {
        parts.push(focus.trim());
    }
    parts.join("\n\n")
}

// CEO functional review codex phase + deterministic super-gate signal
// emission + the corpus-review prompt builder.
include!("super/corpus_review.rs");

async fn run_super_codex_phase(
    repo_root: &Path,
    super_root: &Path,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<()> {
    let prompt_path = super_root.join(format!("{phase_slug}-prompt.md"));
    let stderr_path = super_root.join(format!("{phase_slug}-stderr.log"));
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("phase:       {phase_slug}");
    println!("model:       {model}");
    println!("effort:      {reasoning_effort}");
    println!("context:     max");
    println!("prompt log:  {}", prompt_path.display());
    println!("stderr log:  {}", stderr_path.display());
    let claude_route = crate::claude_exec::looks_like_claude_model(model);
    println!(
        "backend:     {}",
        if claude_route { "claude" } else { "codex" }
    );
    let status = if claude_route {
        // Honor Claude model aliases (opus/sonnet/haiku/claude-*) for the
        // super orchestrator phases historically pinned to Codex. The Claude
        // backend's stream futility detector kicks in on hung tool-result
        // loops; effort maps through resolve_claude_effort.
        crate::claude_exec::run_claude_exec(
            repo_root,
            prompt,
            model,
            reasoning_effort,
            None,
            &stderr_path,
            None,
            phase_slug,
        )
        .await?
    } else {
        run_codex_exec_max_context(
            repo_root,
            prompt,
            model,
            reasoning_effort,
            codex_bin,
            &stderr_path,
            None,
            phase_slug,
        )
        .await?
    };
    if !status.success() {
        bail!(
            "super phase `{phase_slug}` failed with status {status}; see {}",
            stderr_path.display()
        );
    }
    Ok(())
}

/// Run `auto audit --everything` as a subprocess so the audit's clap defaults,
/// quota router, and per-file checkpointing all apply identically to a manual
/// invocation. Returns the run-id (either the supplied `--audit-run-id` or the
/// freshly generated one).
async fn run_super_audit_phase(args: &SuperArgs, repo_root: &Path) -> Result<String> {
    let audit_root = repo_root.join(".auto").join("audit-everything");
    fs::create_dir_all(&audit_root)
        .with_context(|| format!("failed to create {}", audit_root.display()))?;

    let auto_bin = std::env::current_exe()
        .context("failed to resolve current `auto` binary path")?;
    let mut cmd = Command::new(&auto_bin);
    cmd.current_dir(repo_root)
        .arg("audit")
        .arg("--everything")
        .arg("--everything-threads")
        .arg(args.audit_threads.max(1).to_string())
        .arg("--remediation-threads")
        .arg(args.audit_threads.max(1).saturating_div(2).max(1).to_string())
        .arg("--first-pass-retries")
        .arg(args.audit_first_pass_retries.to_string())
        .arg("--first-pass-model")
        .arg(&args.model)
        .arg("--first-pass-effort")
        .arg("low")
        .arg("--synthesis-model")
        .arg(&args.model)
        .arg("--synthesis-effort")
        .arg(&args.reasoning_effort)
        .arg("--codex-bin")
        .arg(&args.codex_bin);
    if let Some(run_id) = args.audit_run_id.as_deref() {
        cmd.arg("--everything-run-id").arg(run_id);
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    println!("audit:       {} threads, {} retry round(s)",
        args.audit_threads.max(1), args.audit_first_pass_retries);
    let status = cmd
        .spawn()
        .with_context(|| format!("failed to spawn `{}` audit subprocess", auto_bin.display()))?
        .wait()
        .await
        .context("audit subprocess wait failed")?;
    if !status.success() {
        bail!("audit phase exited with status {status}");
    }

    if let Some(run_id) = args.audit_run_id.clone() {
        return Ok(run_id);
    }
    let latest_link = audit_root.join("latest-run");
    if latest_link.exists() {
        let target = fs::read_link(&latest_link)
            .or_else(|_| fs::read_to_string(&latest_link).map(PathBuf::from))
            .with_context(|| format!("failed to read {}", latest_link.display()))?;
        if let Some(name) = target.file_name().and_then(|s| s.to_str()) {
            return Ok(name.to_string());
        }
    }
    let mut latest: Option<(String, std::time::SystemTime)> = None;
    for entry in fs::read_dir(&audit_root)
        .with_context(|| format!("failed to read {}", audit_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "latest-run" {
            continue;
        }
        let mtime = entry.metadata()?.modified().unwrap_or(std::time::UNIX_EPOCH);
        match &latest {
            None => latest = Some((name, mtime)),
            Some((_, t)) if mtime > *t => latest = Some((name, mtime)),
            _ => {}
        }
    }
    latest
        .map(|(name, _)| name)
        .context("audit completed but no run-id directory was found under .auto/audit-everything")
}

// Audit-harvest stage: cluster typed findings, route non-single-row clusters
// to dedicated queues, and prompt codex to append plan rows for the residue.
include!("super/audit_harvest.rs");

// Execution-gate stage: LLM Verdict: GO/NO-GO prompt + the deterministic
// plan-readiness verifier and its task-block schema checks.
include!("super/execution_gate.rs");

// Parallel-dispatch stage: launch `auto parallel` inside the super run and
// write the post-parallel reconciliation + final-sanity notes.
include!("super/parallel_dispatch.rs");

fn require_nonempty_file(path: &Path) -> Result<()> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if text.trim().is_empty() {
        bail!("{} must not be empty", path.display());
    }
    Ok(())
}

fn push_stage(
    super_root: &Path,
    manifest: &mut SuperManifest,
    name: &str,
    status: &str,
    artifact: Option<&Path>,
) -> Result<()> {
    manifest.stages.push(SuperStage {
        name: name.to_string(),
        status: status.to_string(),
        artifact: artifact.map(|path| path.display().to_string()),
    });
    write_manifest(super_root, manifest)
}

fn write_manifest(super_root: &Path, manifest: &SuperManifest) -> Result<()> {
    let path = super_root.join("manifest.json");
    atomic_write(&path, &serde_json::to_vec_pretty(manifest)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

// Tests are spliced in via `include!` so the wrapping `mod tests { ... }`
// in the file is the only declaration; this keeps the test block's raw-string
// indentation byte-identical to the pre-split source.
include!("super/tests.rs");
