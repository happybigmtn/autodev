use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::codex_exec::run_codex_exec_max_context;
use crate::design_command;
use crate::generation;
use crate::parallel_command;
use crate::state::load_state;
use crate::task_parser::{parse_tasks, validate_execution_row, PLAN_TASK_PROCESS_FIELDS};
use crate::util::{
    atomic_write, binary_provenance_line, ensure_repo_layout, git_repo_root, timestamp_slug,
};
use crate::{
    AuditHarvestArgs, CorpusArgs, GenerationArgs, ParallelAction, ParallelArgs,
    ParallelCargoTarget, SuperArgs,
};

const SUPER_REPORT_FILES: [&str; 7] = [
    "CEO-14-DAY-PLAN.md",
    "FUNCTIONAL-REVIEWS.md",
    "PRODUCTION-READINESS.md",
    "RISK-REGISTER.md",
    "QUALITY-GATES.md",
    "SYSTEM-MAP.md",
    "SUPER-REPORT.md",
];
const EXECUTION_GATE_FILE: &str = "EXECUTION-GATE.md";
const IMPLEMENTATION_PLAN: &str = "IMPLEMENTATION_PLAN.md";

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

#[derive(Serialize)]
struct SuperRepoRecord {
    role: String,
    path: String,
    branch: String,
    head: String,
    status: String,
}

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
        audit_generated_plan_against_operator_bans(
            &repo_root,
            args.prompt.as_deref().or(args.focus.as_deref()),
        );
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
        return Ok(());
    }

    if super_stage_terminal(&manifest, "parallel") {
        println!("stage:       parallel (resume skip)");
    } else {
        println!("stage:       parallel");
        parallel_command::run_parallel(ParallelArgs {
            action: None::<ParallelAction>,
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
    // "launched" was previously treated as terminal so the parallel stage would
    // be skipped on resume even when it exited with everything shelved. That
    // turned resume into a footgun: a ctrl-c or shelved-everything exit left
    // the run looking "done" and forced operators to bypass super entirely.
    // Only true completion ("complete") or explicit skip ("skipped") now count
    // as terminal. Resume re-enters any stage that didn't write "complete".
    manifest
        .stages
        .iter()
        .rev()
        .find(|stage| stage.name == name)
        .is_some_and(|stage| matches!(stage.status.as_str(), "complete" | "skipped"))
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

fn write_super_cross_repo_manifest(
    super_root: &Path,
    repo_root: &Path,
    planning_root: &Path,
    args: &SuperArgs,
) -> Result<()> {
    #[derive(Serialize)]
    struct CrossRepoManifest {
        primary: SuperRepoRecord,
        references: Vec<SuperRepoRecord>,
        autodev_binary: String,
        planning_root: String,
        worker_model: String,
        worker_reasoning_effort: String,
    }

    let manifest = CrossRepoManifest {
        primary: repo_record("primary", repo_root),
        references: args
            .reference_repos
            .iter()
            .map(|path| repo_record("reference", path))
            .collect(),
        autodev_binary: binary_provenance_line(),
        planning_root: planning_root.display().to_string(),
        worker_model: args.worker_model.clone(),
        worker_reasoning_effort: args.worker_reasoning_effort.clone(),
    };
    let path = super_root.join("CROSS-REPO-MANIFEST.json");
    atomic_write(&path, &serde_json::to_vec_pretty(&manifest)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn repo_record(role: &str, path: &Path) -> SuperRepoRecord {
    SuperRepoRecord {
        role: role.to_string(),
        path: path.display().to_string(),
        branch: git_text(path, ["branch", "--show-current"])
            .unwrap_or_else(|| "unknown".to_string()),
        head: git_text(path, ["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string()),
        status: git_text(path, ["status", "--short", "--branch"])
            .unwrap_or_else(|| "not a readable git repo".to_string()),
    }
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

async fn run_super_corpus_review(
    args: &SuperArgs,
    repo_root: &Path,
    planning_root: &Path,
    super_root: &Path,
) -> Result<()> {
    let prompt = build_super_corpus_review_prompt(repo_root, planning_root, super_root);
    run_super_codex_phase(
        repo_root,
        super_root,
        "super-corpus-review",
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
    )
    .await?;
    for file in SUPER_REPORT_FILES {
        require_nonempty_file(&super_root.join(file))?;
    }
    Ok(())
}

async fn run_super_execution_gate(
    args: &SuperArgs,
    repo_root: &Path,
    planning_root: &Path,
    output_dir: Option<&Path>,
    super_root: &Path,
) -> Result<()> {
    let prompt =
        build_super_execution_gate_prompt(repo_root, planning_root, output_dir, super_root);
    run_super_codex_phase(
        repo_root,
        super_root,
        "super-execution-gate",
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
    )
    .await?;
    let gate_path = super_root.join(EXECUTION_GATE_FILE);
    require_nonempty_file(&gate_path)?;
    let gate = fs::read_to_string(&gate_path)
        .with_context(|| format!("failed to read {}", gate_path.display()))?;
    if !gate.lines().any(|line| line.trim() == "Verdict: GO") {
        bail!(
            "super execution gate did not approve parallel execution; expected `Verdict: GO` in {}",
            gate_path.display()
        );
    }
    Ok(())
}

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
    let status = run_codex_exec_max_context(
        repo_root,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
        &stderr_path,
        None,
        phase_slug,
    )
    .await?;
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

    let auto_bin =
        std::env::current_exe().context("failed to resolve current `auto` binary path")?;
    let mut cmd = Command::new(&auto_bin);
    cmd.current_dir(repo_root)
        .arg("audit")
        .arg("--everything")
        .arg("--everything-threads")
        .arg(args.audit_threads.max(1).to_string())
        .arg("--remediation-threads")
        .arg(
            args.audit_threads
                .max(1)
                .saturating_div(2)
                .max(1)
                .to_string(),
        )
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

    println!(
        "audit:       {} threads, {} retry round(s)",
        args.audit_threads.max(1),
        args.audit_first_pass_retries
    );
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
        let mtime = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
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

/// Harvest audit findings into `IMPLEMENTATION_PLAN.md` so the parallel stage
/// has actionable rows that target real audited files. Reads every
/// `analysis.json` under the audit run, ranks by score, and asks codex to
/// emit IMPLEMENTATION_PLAN.md task rows that follow the existing schema.
async fn run_super_audit_harvest(
    args: &SuperArgs,
    repo_root: &Path,
    super_root: &Path,
    audit_run_id: &str,
) -> Result<PathBuf> {
    harvest_audit_findings(
        repo_root,
        super_root,
        audit_run_id,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        0,
        0,
        8,
    )
    .await
}

/// Standalone entrypoint for `auto audit-harvest --run-id <id>`. Resolves a
/// run-id (defaulting to the latest under `.auto/audit-everything/`) and
/// writes summary + IMPLEMENTATION_PLAN.md additions next to the audit run.
pub(crate) async fn run_audit_harvest_standalone(args: AuditHarvestArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    let audit_root = repo_root.join(".auto").join("audit-everything");
    let run_id = match args.run_id {
        Some(id) => id,
        None => resolve_latest_audit_run_id(&audit_root)?,
    };
    let harvest_root = audit_root.join(&run_id).join("harvest");
    fs::create_dir_all(&harvest_root)
        .with_context(|| format!("failed to create {}", harvest_root.display()))?;
    println!("audit-harvest");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    println!("run-id:      {run_id}");
    println!("output:      {}", harvest_root.display());
    let max_findings = if args.max_findings == 0 {
        usize::MAX
    } else {
        args.max_findings
    };
    let summary = harvest_audit_findings(
        &repo_root,
        &harvest_root,
        &run_id,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        max_findings,
        args.score_min,
        args.score_max,
    )
    .await?;
    println!("summary:     {}", summary.display());
    Ok(())
}

fn resolve_latest_audit_run_id(audit_root: &Path) -> Result<String> {
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
    for entry in fs::read_dir(audit_root)
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
        let mtime = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
        match &latest {
            None => latest = Some((name, mtime)),
            Some((_, t)) if mtime > *t => latest = Some((name, mtime)),
            _ => {}
        }
    }
    latest
        .map(|(name, _)| name)
        .context("no audit run-id directories found under .auto/audit-everything")
}

#[allow(clippy::too_many_arguments)]
async fn harvest_audit_findings(
    repo_root: &Path,
    output_root: &Path,
    audit_run_id: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    max_findings: usize,
    score_min: i64,
    score_max: i64,
) -> Result<PathBuf> {
    let files_dir = repo_root
        .join(".auto")
        .join("audit-everything")
        .join(audit_run_id)
        .join("worktree")
        .join("audit")
        .join("everything")
        .join(audit_run_id)
        .join("files");
    if !files_dir.exists() {
        bail!(
            "audit harvest expected files dir at {} (run-id {audit_run_id})",
            files_dir.display(),
        );
    }

    // Build a registry of paths already covered by existing AUDIT-* rows
    // in IMPLEMENTATION_PLAN.md. Without this, the codex prompt's "skip
    // duplicates" directive is too lenient: phrasing variations across
    // iterations cause the same file to get harvested multiple times,
    // which is what made the 2026-05-08 iteration loop fail to converge
    // (plan grew from 62 → 90 [ ] over two iters).
    let plan_path = repo_root.join(IMPLEMENTATION_PLAN);
    let plan_existing_full = fs::read_to_string(&plan_path).unwrap_or_default();
    let already_covered_paths = collect_paths_from_audit_rows(&plan_existing_full);
    println!(
        "audit harvest: {} path(s) already covered by existing AUDIT-* rows; will dedup",
        already_covered_paths.len(),
    );

    let mut findings = Vec::new();
    let mut filtered_dup = 0usize;
    for entry in fs::read_dir(&files_dir)
        .with_context(|| format!("failed to read {}", files_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let analysis_json = entry.path().join("analysis.json");
        let Ok(text) = fs::read_to_string(&analysis_json) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let score = value
            .get("score_out_of_10")
            .and_then(|v| v.as_i64())
            .unwrap_or(10);
        if score < score_min || score > score_max {
            continue;
        }
        let path = value.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if !path.is_empty() && already_covered_paths.contains(path) {
            filtered_dup += 1;
            continue;
        }
        findings.push((score, value));
    }
    if filtered_dup > 0 {
        println!(
            "audit harvest: filtered {} finding(s) whose path is already in an existing AUDIT-* row",
            filtered_dup,
        );
    }
    findings.sort_by_key(|(score, _)| *score);
    let take = findings.len().min(max_findings);
    let actionable_full: Vec<&serde_json::Value> =
        findings.iter().take(take).map(|(_, v)| v).collect();

    let actionable_compact: Vec<serde_json::Value> = actionable_full
        .iter()
        .map(|v| compress_finding_for_harvest(v))
        .collect();

    fs::create_dir_all(output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let summary_path = output_root.join("AUDIT-FINDINGS-SUMMARY.json");
    atomic_write(
        &summary_path,
        &serde_json::to_vec_pretty(&serde_json::json!({
            "audit_run_id": audit_run_id,
            "score_min": score_min,
            "score_max": score_max,
            "matched_in_range": findings.len(),
            "harvested": actionable_compact.len(),
            "findings": actionable_compact,
        }))?,
    )
    .with_context(|| format!("failed to write {}", summary_path.display()))?;

    if actionable_compact.is_empty() {
        println!(
            "audit harvest: no findings in score range [{}..{}]; IMPLEMENTATION_PLAN.md unchanged",
            score_min, score_max,
        );
        return Ok(summary_path);
    }

    println!(
        "audit harvest: harvesting {} finding(s) from score range [{}..{}]",
        actionable_compact.len(),
        score_min,
        score_max,
    );
    let plan_path = repo_root.join(IMPLEMENTATION_PLAN);
    let chunks = chunk_findings_for_codex(&actionable_compact);
    let chunk_count = chunks.len();
    println!(
        "audit harvest: dispatching {} codex chunk(s) (codex hard-caps prompts at ~1MB)",
        chunk_count,
    );
    for (idx, chunk) in chunks.into_iter().enumerate() {
        let plan_existing = fs::read_to_string(&plan_path).unwrap_or_default();
        let phase_slug = if chunk_count == 1 {
            "audit-harvest".to_string()
        } else {
            format!("audit-harvest-chunk-{:02}-of-{:02}", idx + 1, chunk_count)
        };
        println!(
            "audit harvest: chunk {}/{} ({} findings)",
            idx + 1,
            chunk_count,
            chunk.len(),
        );
        let prompt =
            build_audit_harvest_prompt(&plan_existing, &chunk, audit_run_id, score_min, score_max);
        run_super_codex_phase(
            repo_root,
            output_root,
            &phase_slug,
            &prompt,
            model,
            reasoning_effort,
            codex_bin,
        )
        .await?;
    }
    println!(
        "audit harvest: appended task rows to {}",
        plan_path.display()
    );
    Ok(summary_path)
}

/// Codex's API gateway hard-caps prompt input at ~1 MB of UTF-8 characters.
/// Reserve ~80 KB for prompt boilerplate (instructions + plan excerpt) and
/// split the findings into chunks whose serialized JSON stays under the
/// remaining budget. Each chunk runs as its own codex call; the harvest
/// prompt's "scan existing IMPLEMENTATION_PLAN.md and skip duplicates"
/// directive prevents duplicate rows across chunks.
fn chunk_findings_for_codex(compressed: &[serde_json::Value]) -> Vec<Vec<serde_json::Value>> {
    const HARD_CAP_CHARS: usize = 1_000_000; // codex limit
    const PROMPT_OVERHEAD_CHARS: usize = 80_000; // boilerplate + plan excerpt
    const PER_CHUNK_BUDGET_CHARS: usize = HARD_CAP_CHARS - PROMPT_OVERHEAD_CHARS;

    let mut chunks: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut current: Vec<serde_json::Value> = Vec::new();
    let mut current_chars: usize = 2; // for `[` `]`
    for finding in compressed {
        // serde_json::to_string never fails on owned Value; fall back to {} on
        // the impossible error path so we don't poison the chunk run.
        let serialized = serde_json::to_string(finding).unwrap_or_else(|_| "{}".to_string());
        let needed = serialized.chars().count() + 2; // entry + `,` separator
        if current_chars + needed > PER_CHUNK_BUDGET_CHARS && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_chars = 2;
        }
        current.push(finding.clone());
        current_chars += needed;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(Vec::new());
    }
    chunks
}

/// Compress an analysis.json down to the fields a harvest prompt actually
/// needs to write a task row. The verbose fields (`architecture_smells`,
/// Extract every file-like path token from existing AUDIT-* row blocks in
/// IMPLEMENTATION_PLAN.md. Paths in `Owns:`, `Source of truth:`, `Codebase
/// evidence:`, and `Spec:` lines all count. Used by harvest to dedup
/// findings whose target path is already covered by an existing row, even
/// if the new finding's wording differs from prior iterations' wording.
fn collect_paths_from_audit_rows(plan_text: &str) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let mut paths: HashSet<String> = HashSet::new();
    let mut in_audit_block = false;
    let path_token = regex::Regex::new(
        r"`?\b((?:[A-Za-z0-9_./-]+/)?[A-Za-z0-9_-]+\.(?:rs|md|toml|json|sh|py|ts|tsx|js|jsx|yaml|yml|css|html|svg|txt|sql|move))\b`?",
    )
    .ok();
    for line in plan_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- [")
            && (trimmed.contains("`AUDIT-") || trimmed.starts_with("- [ ] `AUDIT-"))
        {
            in_audit_block = true;
            continue;
        }
        if trimmed.starts_with("- [") {
            in_audit_block = false;
            continue;
        }
        if !in_audit_block {
            continue;
        }
        if let Some(re) = &path_token {
            for cap in re.captures_iter(line) {
                if let Some(m) = cap.get(1) {
                    paths.insert(m.as_str().to_string());
                }
            }
        }
    }
    paths
}

/// `behavior_preservation_needs`, `cross_file_questions`, etc.) get dropped
/// AND the surviving string values are truncated so thousands of findings
/// fit under the codex 1 MB prompt cap.
fn compress_finding_for_harvest(full: &serde_json::Value) -> serde_json::Value {
    fn truncate_string_value(v: &serde_json::Value, max: usize) -> serde_json::Value {
        match v {
            serde_json::Value::String(s) if s.chars().count() > max => {
                let mut shrunk: String = s.chars().take(max).collect();
                shrunk.push('…');
                serde_json::Value::String(shrunk)
            }
            other => other.clone(),
        }
    }
    let mut out = serde_json::Map::new();
    if let Some(v) = full.get("path") {
        out.insert("path".to_string(), v.clone());
    }
    if let Some(v) = full.get("group") {
        out.insert("group".to_string(), v.clone());
    }
    if let Some(v) = full.get("score_out_of_10") {
        out.insert("score_out_of_10".to_string(), v.clone());
    }
    if let Some(v) = full.get("summary") {
        out.insert("summary".to_string(), truncate_string_value(v, 240));
    }
    if let Some(arr) = full.get("recommended_actions").and_then(|v| v.as_array()) {
        let trimmed: Vec<serde_json::Value> = arr
            .iter()
            .take(2)
            .map(|v| truncate_string_value(v, 180))
            .collect();
        out.insert(
            "recommended_actions".to_string(),
            serde_json::Value::Array(trimmed),
        );
    }
    if let Some(arr) = full.get("ai_slop_signals").and_then(|v| v.as_array()) {
        let trimmed: Vec<serde_json::Value> = arr
            .iter()
            .take(2)
            .map(|v| truncate_string_value(v, 140))
            .collect();
        out.insert(
            "ai_slop_signals".to_string(),
            serde_json::Value::Array(trimmed),
        );
    }
    if let Some(arr) = full.get("deletion_candidates").and_then(|v| v.as_array()) {
        if let Some(first) = arr.first() {
            // Pull just `candidate` and `classification`; drop verbose evidence narratives.
            let mut compact = serde_json::Map::new();
            if let Some(c) = first.get("candidate") {
                compact.insert("candidate".to_string(), truncate_string_value(c, 160));
            }
            if let Some(c) = first.get("classification") {
                compact.insert("classification".to_string(), c.clone());
            }
            out.insert(
                "top_deletion_candidate".to_string(),
                serde_json::Value::Object(compact),
            );
        }
    }
    serde_json::Value::Object(out)
}

fn build_audit_harvest_prompt(
    plan_existing: &str,
    findings: &[serde_json::Value],
    audit_run_id: &str,
    score_min: i64,
    score_max: i64,
) -> String {
    let findings_json = serde_json::to_string_pretty(findings).unwrap_or_default();
    let plan_excerpt: String = plan_existing
        .lines()
        .take(60)
        .collect::<Vec<_>>()
        .join("\n");
    let cohort_label = if score_min == score_max {
        format!("score == {score_min}")
    } else {
        format!("scores {score_min}..={score_max}")
    };
    let consolidation_hint = if score_min >= 8 {
        "Many of these findings will share root causes (broad mild drift, repeated AI-slop patterns, schema gaps). Aggressively consolidate: one row per root cause, listing all affected paths in `Owns:` and `Integration touchpoints:`. A single thoughtful row that fixes 50 files is better than 50 thin rows."
    } else {
        "Each finding here is acute (low score). Prefer one task row per finding when the failure is file-specific, but still consolidate when several files share a clear root cause. Lean toward higher fidelity than for the score-8 cohort."
    };
    format!(
        "You are extending IMPLEMENTATION_PLAN.md with task rows that address findings from `auto audit --everything` run `{audit_run_id}`, restricted to {cohort_label}.

CONSTRAINTS:
- Append rows ONLY to the existing IMPLEMENTATION_PLAN.md. Do not edit other files. Do not create new files.
- Match the existing row schema exactly: every appended task block must use the `- [ ] `<ID>` <Title>` header followed by the indented field set seen in the existing rows (Spec / Why now / Codebase evidence / Source of truth / Runtime owner / UI consumers / Generated artifacts / Fixture boundary / Retired surfaces / Owns / Integration touchpoints / Scope boundary / Acceptance criteria / Verification / Required tests / Contract generation / Cross-surface tests / Review/closeout / Completion artifacts / Dependencies / Estimated scope / Completion signal).
- IDs must be unique across IMPLEMENTATION_PLAN.md. Use prefix `AUDIT-{audit_run_id}-NN`; scan the existing file first and start from the next free integer.
- {consolidation_hint}
- Skip duplicates of existing AUDIT-* rows. Skip findings whose `path` does not exist on disk.
- Acceptance criteria and Verification must reference real files and real cargo / pytest / shell commands the harness can run; no placeholders.
- Estimated scope must be XS, S, or M. Use M only when the row clearly spans multiple modules.

EXISTING IMPLEMENTATION_PLAN.md (first 60 lines for schema reference):
```
{plan_excerpt}
```

AUDIT FINDINGS (compressed JSON for {cohort_label}, ranked lowest-score first):
```json
{findings_json}
```

Now append the new task rows to IMPLEMENTATION_PLAN.md. Do not modify existing rows. Verify the file parses by re-reading it after the append. Report a one-line summary of how many rows you appended and the ID range used.
"
    )
}

fn build_super_corpus_review_prompt(
    repo_root: &Path,
    planning_root: &Path,
    super_root: &Path,
) -> String {
    format!(
        r#"You are the new CEO of this codebase running the `auto super` functional review war room.

The normal `auto corpus` authoring and review passes have already produced `{planning_root}` for the repository at `{repo_root}`. The design perfection gate may also have written design/runtime artifacts under `{super_root}/design`. Treat those design artifacts as the first production-readiness input, not as a subordinate style appendix.

Mission:
- You inherited this codebase today.
- You have 14 days to race it to production.
- Compute and implementation capacity are not constraints; prioritization is about production leverage, risk, and dependency order.
- Design/runtime integrity was perfected first. Now apply the same severity and precision across every functional lane.

Edit boundary:
- You may read the repository at `{repo_root}` and the planning corpus at `{planning_root}`.
- You may read `{super_root}/design` and should preserve its runtime-first design/UI findings when they exist.
- You may edit markdown files under `{planning_root}`.
- You must write these non-empty files under `{super_root}`:
  - `CEO-14-DAY-PLAN.md`
  - `FUNCTIONAL-REVIEWS.md`
  - `PRODUCTION-READINESS.md`
  - `RISK-REGISTER.md`
  - `QUALITY-GATES.md`
  - `SYSTEM-MAP.md`
  - `SUPER-REPORT.md`
- Do not edit source code, root specs, root implementation plans, generated `gen-*` dirs, or skill definition directories.

Run these functional reviews and synthesize their disagreements:
- CEO/Product: production definition, 10-star user outcome, non-goals, opportunity cost, scope discipline.
- Design/Frontend: design-system clarity, modern UI quality, accessibility, AI-slop risk, and runtime/UI drift; respect `{super_root}/design` as the opening gate.
- Principal Engineer/Architecture: architecture seams, data flow, state, dependency order, maintainability.
- Runtime/Engine: source-of-truth ownership, generated contracts, API/schema drift, state transitions, invariants.
- Security/Trust: credentials, shell/YAML injection, secrets, dangerous flags, logs, authz, trust boundaries.
- Reliability/Ops: idempotence, resume, partial failure, recovery, observability, receipts, operator handoff.
- QA/Test Architect: missing regression tests, integration proof, false-positive verification, browser/runtime evidence.
- Data/Contracts: migrations, compatibility, durable artifacts, schema ownership, backfill or rollback hazards.
- Performance/Scale: hot paths, large repos, concurrency, resource cleanup, timeout behavior.
- DX/Agent Workflow: first-run success, CLI help, errors, honest examples, setup friction, model/provider routing.
- Release Manager: CI, install proof, versioning, rollback, release blockers, ship/no-ship criteria.

Required output semantics:
- `CEO-14-DAY-PLAN.md` must define the 14-day production race, top outcomes, dependency waves, and prioritized deliverables without capacity trimming.
- `FUNCTIONAL-REVIEWS.md` must contain the lane-by-lane review board findings, severity, owner, needed artifact, and proof for each discipline above.
- `PRODUCTION-READINESS.md` must contain a matrix by major subsystem with grade, evidence, production blocker, required fix, and proof artifact/command.
- `RISK-REGISTER.md` must rank risks by severity, likelihood, blast radius, mitigation, and release-blocking status.
- `QUALITY-GATES.md` must define hard gates before parallel execution, before release candidate, and before ship.
- `SYSTEM-MAP.md` must map command surface, state files, external CLIs, credential flows, write paths, and generated artifacts.
- `SUPER-REPORT.md` must summarize top blockers, top non-blocking improvements, not-doing list, how design was handled first, functional-lane risks, and any amendments made to `{planning_root}`.

If the corpus under `{planning_root}` is missing production-readiness framing, amend it in place so the next `auto gen` pass produces release-oriented specs and executable plan tasks. Deliverables should be dependency-ordered for max-compute parallelism, not limited by a small team capacity assumption. Keep `genesis/` as corpus input, not a competing active control plane unless repository instructions explicitly say otherwise.
"#,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        super_root = super_root.display(),
    )
}

fn build_super_execution_gate_prompt(
    repo_root: &Path,
    planning_root: &Path,
    output_dir: Option<&Path>,
    super_root: &Path,
) -> String {
    let output_clause = output_dir
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the latest gen output recorded in .auto/state.json".to_string());
    format!(
        r#"You are the final `auto super` execution gate before `auto parallel` launches.

The repository is `{repo_root}`. The planning corpus is `{planning_root}`. The generated output is `{output_clause}`. The super artifacts are under `{super_root}`.

Edit boundary:
- You may read the repository, `{planning_root}`, generated output, root `specs/`, and root `IMPLEMENTATION_PLAN.md`.
- You may read `{super_root}/design`; design/runtime UI contract risks are execution-gate inputs, not decoration.
- You must read `{super_root}/CEO-14-DAY-PLAN.md`, `{super_root}/FUNCTIONAL-REVIEWS.md`, `{super_root}/PRODUCTION-READINESS.md`, `{super_root}/RISK-REGISTER.md`, `{super_root}/QUALITY-GATES.md`, and `{super_root}/SYSTEM-MAP.md` when present.
- You may edit only root `IMPLEMENTATION_PLAN.md`, root `specs/*.md`, and `{super_root}/EXECUTION-GATE.md`.
- Do not edit source code, `genesis/`, `gen-*`, skill definition directories, or worker artifacts.

Review the root execution queue as if max-compute tmux-backed implementation workers will start immediately.

Gate criteria:
- The queue must implement the CEO 14-day production race, not a generic cleanup backlog or capacity-trimmed wishlist.
- UI/design tasks must be tied to runtime/API source of truth, generated bindings, existing frontend helpers, and cross-surface readback proof. Reject fake mockups, manual frontend bindings, and fixture-data fallbacks as acceptance evidence.
- Security, reliability, QA, data/contracts, operations, release, DX, and performance lanes must receive the same severity and proof standard as design.
- Priority tasks must be dependency-ordered and small enough for one focused worker session.
- Every unfinished task must have concrete ownership, acceptance criteria, verification, required tests, completion artifacts, dependencies, estimated scope, and completion signal.
- Verification must be narrow and meaningful. Reject broad package-wide test commands, malformed shell snippets, zero-test filters, and directory greps as sole proof.
- Security, credentials, generated executable workflow text, destructive operations, and external-service tasks must carry explicit scope boundaries and proof expectations.
- Research or decision tasks must produce concrete artifacts and must not silently authorize implementation before the decision is made.
- If the plan is not ready for parallel execution, amend it until it is ready or write a NO-GO verdict explaining the blocker.

Write `{super_root}/EXECUTION-GATE.md` with:
- `# SUPER EXECUTION GATE`
- A line exactly `Verdict: GO` or `Verdict: NO-GO`
- Queue summary
- Changes made
- Remaining risks
- Parallel launch notes

Only write `Verdict: GO` if it is safe and useful for `auto parallel` to begin immediately after this gate.
"#,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        output_clause = output_clause,
        super_root = super_root.display(),
    )
}

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq)]
struct DeterministicGateSummary {
    unchecked_tasks: usize,
    priority_tasks: usize,
    follow_on_tasks: usize,
}

fn verify_parallel_ready_plan(plan_path: &Path) -> Result<DeterministicGateSummary> {
    let markdown = fs::read_to_string(plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    if !markdown.trim_start().starts_with("# IMPLEMENTATION_PLAN") {
        bail!(
            "{} must start with `# IMPLEMENTATION_PLAN`",
            plan_path.display()
        );
    }
    for section in [
        "## Priority Work",
        "## Follow-On Work",
        "## Completed / Already Satisfied",
    ] {
        if !markdown.contains(section) {
            bail!("{} is missing `{section}`", plan_path.display());
        }
    }

    let tasks = extract_super_task_blocks(&markdown);
    let unchecked = tasks
        .iter()
        .filter(|task| !task.checked && task.section != SuperPlanSection::Completed)
        .collect::<Vec<_>>();
    if unchecked.is_empty() {
        bail!("{} has no unchecked executable tasks", plan_path.display());
    }
    let shared_tasks = parse_tasks(&markdown);
    let all_task_ids = shared_tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let lenient = std::env::var("AUTO_LENIENT_GATE").ok().as_deref() == Some("1")
        || std::env::var("AUTO_LENIENT_DEPS").ok().as_deref() == Some("1");
    for task in &unchecked {
        if let Err(err) = verify_super_task(task, &all_task_ids) {
            if lenient {
                eprintln!("warning: {err:#} (continuing under AUTO_LENIENT_GATE=1)");
                continue;
            }
            return Err(err);
        }
    }

    Ok(DeterministicGateSummary {
        unchecked_tasks: unchecked.len(),
        priority_tasks: unchecked
            .iter()
            .filter(|task| task.section == SuperPlanSection::Priority)
            .count(),
        follow_on_tasks: unchecked
            .iter()
            .filter(|task| task.section == SuperPlanSection::FollowOn)
            .count(),
    })
}

fn verify_super_task(
    task: &SuperTaskBlock,
    all_task_ids: &std::collections::BTreeSet<&str>,
) -> Result<()> {
    let verification = first_super_task_field_line(task, "Verification:").unwrap_or("");
    if verification_looks_broad_or_malformed(verification) {
        bail!(
            "task `{}` uses package-wide cargo test verification; include a concrete test-name filter",
            task.task_id
        );
    }

    let parsed_task = parse_tasks(&task.markdown)
        .into_iter()
        .find(|candidate| candidate.id == task.task_id)
        .with_context(|| {
            format!(
                "task `{}` is not parseable by shared task parser",
                task.task_id
            )
        })?;
    validate_execution_row(&parsed_task, all_task_ids)
        .with_context(|| format!("task `{}` failed execution-row validation", task.task_id))?;

    for forbidden in [
        "TBD",
        "TODO",
        "decomposition required",
        "split before implementation",
    ] {
        if task.markdown.contains(forbidden) {
            bail!(
                "task `{}` contains forbidden placeholder `{forbidden}`",
                task.task_id
            );
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn verify_super_task_process_fields(task: &SuperTaskBlock) -> Result<()> {
    for &field in PLAN_TASK_PROCESS_FIELDS {
        let value = first_super_task_field_line(task, field)
            .with_context(|| format!("task `{}` is missing `{field}`", task.task_id))?;
        let lowercase = value.to_ascii_lowercase();
        for forbidden in ["tbd", "todo", "unspecified", "unknown"] {
            if lowercase.contains(forbidden) {
                bail!(
                    "task `{}` has vague `{field}` content `{forbidden}`",
                    task.task_id
                );
            }
        }
    }

    let ui_consumers = first_super_task_field_line(task, "UI consumers:").unwrap_or("none");
    let has_ui = !field_value_is_none(ui_consumers);
    let cross_surface = first_super_task_field_line(task, "Cross-surface tests:").unwrap_or("none");
    if has_ui && field_value_is_none(cross_surface) {
        bail!(
            "task `{}` names UI consumers but has no `Cross-surface tests:` proof",
            task.task_id
        );
    }

    let generated_artifacts =
        first_super_task_field_line(task, "Generated artifacts:").unwrap_or("none");
    let contract_generation =
        first_super_task_field_line(task, "Contract generation:").unwrap_or("none");
    if !field_value_is_none(generated_artifacts) && field_value_is_none(contract_generation) {
        bail!(
            "task `{}` names generated artifacts but has no `Contract generation:` command",
            task.task_id
        );
    }

    let review_closeout = first_super_task_field_line(task, "Review/closeout:").unwrap_or("");
    let review_lower = review_closeout.to_ascii_lowercase();
    if review_lower == "cargo check" || review_lower.contains("cargo check only") {
        bail!(
            "task `{}` cannot use only cargo check for `Review/closeout:`",
            task.task_id
        );
    }

    Ok(())
}

#[allow(dead_code)]
fn field_value_is_none(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower == "none" || lower.starts_with("none ") || lower.starts_with("none --")
}

fn verification_looks_broad_or_malformed(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("cargo test --all")
        || lower.contains("cargo test --workspace")
        || lower.lines().any(cargo_test_line_is_package_wide)
        || lower.lines().any(|line| line.trim() == "cargo --lib")
}

#[allow(dead_code)]
fn cargo_test_line_is_package_wide(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("cargo test") else {
        return false;
    };
    let tokens = rest.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return true;
    }
    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index];
        if token == "--" || token == "&&" || token == ";" || token == "||" {
            break;
        }
        if matches!(
            token,
            "-p" | "--package"
                | "--manifest-path"
                | "--target"
                | "--features"
                | "-F"
                | "--test"
                | "--bin"
                | "--example"
                | "--bench"
        ) {
            index += 2;
            continue;
        }
        if token.starts_with('-') || token.starts_with("--package=") || token.starts_with("-p") {
            index += 1;
            continue;
        }
        return false;
    }
    true
}

#[allow(dead_code)]
fn contains_path_like_token(body: &str) -> bool {
    body.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | '(' | ')'))
        .map(|token| token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | ':' | '.')))
        .any(|token| {
            token.contains('/')
                || token.starts_with("refs/")
                || [
                    "src",
                    "docs",
                    "specs",
                    "tests",
                    "scripts",
                    "README.md",
                    "IMPLEMENTATION_PLAN.md",
                ]
                .contains(&token)
                || [
                    ".rs", ".md", ".toml", ".json", ".yaml", ".yml", ".sh", ".ts", ".tsx", ".js",
                ]
                .iter()
                .any(|extension| token.ends_with(extension))
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SuperPlanSection {
    Priority,
    FollowOn,
    Completed,
}

struct SuperTaskBlock {
    section: SuperPlanSection,
    task_id: String,
    checked: bool,
    markdown: String,
}

fn extract_super_task_blocks(markdown: &str) -> Vec<SuperTaskBlock> {
    let mut section = SuperPlanSection::Priority;
    let mut blocks = Vec::new();
    let mut current = Vec::<String>::new();
    for line in markdown.lines() {
        match line.trim() {
            "## Priority Work" => {
                finish_super_task(section, &mut current, &mut blocks);
                section = SuperPlanSection::Priority;
                continue;
            }
            "## Follow-On Work" => {
                finish_super_task(section, &mut current, &mut blocks);
                section = SuperPlanSection::FollowOn;
                continue;
            }
            "## Completed / Already Satisfied" => {
                finish_super_task(section, &mut current, &mut blocks);
                section = SuperPlanSection::Completed;
                continue;
            }
            _ => {}
        }
        if parse_super_task_header(line).is_some() {
            finish_super_task(section, &mut current, &mut blocks);
            current.push(line.to_string());
        } else if !current.is_empty() {
            current.push(line.to_string());
        }
    }
    finish_super_task(section, &mut current, &mut blocks);
    blocks
}

fn finish_super_task(
    section: SuperPlanSection,
    current: &mut Vec<String>,
    blocks: &mut Vec<SuperTaskBlock>,
) {
    if current.is_empty() {
        return;
    }
    if let Some((checked, task_id)) = parse_super_task_header(&current[0]) {
        blocks.push(SuperTaskBlock {
            section,
            task_id,
            checked,
            markdown: current.join("\n"),
        });
    }
    current.clear();
}

fn parse_super_task_header(line: &str) -> Option<(bool, String)> {
    let trimmed = line.trim_start();
    let checked = if trimmed.starts_with("- [ ] ") || trimmed.starts_with("- [~] ") {
        false
    } else if trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ") {
        true
    } else {
        return None;
    };
    let rest = trimmed[6..].trim_start().strip_prefix('`')?;
    let tick = rest.find('`')?;
    Some((checked, rest[..tick].trim().to_string()))
}

#[allow(dead_code)]
fn task_field_value<'a>(task: &'a SuperTaskBlock, field: &str) -> Option<&'a str> {
    task.markdown
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(field).map(str::trim))
        .filter(|value| !value.is_empty())
}

fn first_super_task_field_line<'a>(task: &'a SuperTaskBlock, field: &str) -> Option<&'a str> {
    task.markdown
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(field).map(str::trim))
        .filter(|value| !value.is_empty())
}

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
    append_status_log(super_root, name, status);
    write_manifest(super_root, manifest)
}

fn audit_generated_plan_against_operator_bans(repo_root: &Path, operator_prompt: Option<&str>) {
    // Best-effort observability: when the operator's prompt enumerates banned
    // path prefixes (typical pattern: "No new docs/ops/...", "No new
    // genesis/checkpoints/0XX-*.md"), count how often the generated plan
    // mentions those prefixes. If many tasks reference banned paths, the
    // operator is likely about to burn cycles producing doc-spam that the
    // AUTO_REJECT_DOCS_ONLY_COMMITS=1 filter will reject downstream. Surface
    // this loudly so the operator can intervene before parallel starts.
    let Some(prompt) = operator_prompt else {
        return;
    };
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    let Ok(plan) = std::fs::read_to_string(&plan_path) else {
        return;
    };
    let banned_substrings: Vec<&str> = prompt
        .lines()
        .filter(|line| {
            let l = line.to_ascii_lowercase();
            l.contains("no new ") || l.contains("banned") || l.contains("do not create")
        })
        .flat_map(|line| {
            // Extract path-shaped tokens (contain '/' or end with .md).
            line.split(|c: char| c.is_whitespace() || c == ',' || c == '`')
                .filter(|tok| {
                    let t = tok.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '/');
                    !t.is_empty() && (t.contains('/') || t.ends_with(".md")) && !t.starts_with('-')
                })
        })
        .collect();
    if banned_substrings.is_empty() {
        return;
    }
    let mut hits: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for needle in &banned_substrings {
        let count = plan.matches(*needle).count();
        if count > 0 {
            *hits.entry(*needle).or_insert(0) += count;
        }
    }
    let total: usize = hits.values().sum();
    if total == 0 {
        return;
    }
    eprintln!(
        "warning: generated plan contains {total} mention(s) of operator-banned path prefix(es); \
         AUTO_REJECT_DOCS_ONLY_COMMITS=1 will likely reject commits that touch only these paths. \
         Consider editing IMPLEMENTATION_PLAN.md to remove the banned-pattern tasks before \
         the parallel stage starts."
    );
    let mut entries: Vec<(&&str, &usize)> = hits.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1));
    for (needle, count) in entries.iter().take(8) {
        eprintln!("warning:   {count} hits  {needle}");
    }
}

fn append_status_log(super_root: &Path, name: &str, status: &str) {
    // Tail-friendly stage-only log so operators can `tail -f status.log`
    // without wading through codex narrative. Best-effort: never fails the
    // surrounding push_stage even if the write errors.
    let path = super_root.join("status.log");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let pid = std::process::id();
    let line = format!("{now} pid={pid} stage={name} status={status}\n");
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn write_manifest(super_root: &Path, manifest: &SuperManifest) -> Result<()> {
    let path = super_root.join("manifest.json");
    atomic_write(&path, &serde_json::to_vec_pretty(manifest)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn build_super_focus_combines_production_directive_and_prompt() {
        let focus = build_super_focus(Some("ship the CLI"), Some("security first"));
        assert!(focus.contains("new CEO"));
        assert!(focus.contains("14 days"));
        assert!(focus.contains("Perfect design/runtime integrity first"));
        assert!(focus.contains("ship the CLI"));
        assert!(focus.contains("security first"));
    }

    #[test]
    fn deterministic_gate_accepts_scoped_unfinished_task() {
        let root = temp_dir("super-gate-ok");
        let plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(&plan, valid_plan("cargo test super_command::tests::deterministic_gate_accepts_scoped_unfinished_task")).unwrap();
        let summary = verify_parallel_ready_plan(&plan).unwrap();
        assert_eq!(summary.unchecked_tasks, 1);
        assert_eq!(summary.priority_tasks, 1);
    }

    #[test]
    fn deterministic_gate_rejects_package_wide_cargo_test() {
        let root = temp_dir("super-gate-broad");
        let plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(&plan, valid_plan("cargo test")).unwrap();
        let error = verify_parallel_ready_plan(&plan).expect_err("expected broad test rejection");
        assert!(error.to_string().contains("package-wide cargo test"));
    }

    #[test]
    fn super_rejects_task_missing_runtime_ui_fields() {
        let root = temp_dir("super-gate-missing-runtime-ui");
        let plan = root.join(IMPLEMENTATION_PLAN);
        let malformed = valid_plan(
            "cargo test super_command::tests::super_rejects_task_missing_runtime_ui_fields",
        )
        .replace("    Runtime owner: `src/super_command.rs`\n", "");
        fs::write(&plan, malformed).unwrap();

        let error = verify_parallel_ready_plan(&plan)
            .expect_err("expected rich runtime/UI task contract rejection");

        assert!(format!("{error:#}").contains("task `TASK-001` missing `Runtime owner:`"));
    }

    #[test]
    fn super_accepts_generated_rich_task_contract() {
        let root = temp_dir("super-gate-rich-contract");
        let plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(
            &plan,
            valid_plan(
                "cargo test super_command::tests::super_accepts_generated_rich_task_contract",
            ),
        )
        .unwrap();

        let summary = verify_parallel_ready_plan(&plan).unwrap();

        assert_eq!(summary.unchecked_tasks, 1);
        assert_eq!(summary.priority_tasks, 1);
        assert_eq!(summary.follow_on_tasks, 0);
    }

    #[test]
    fn resume_helpers_skip_terminal_stages_and_restore_gate_artifact() {
        let root = temp_dir("super-resume-manifest");
        let artifact = root.join("gen-output");
        let gate = DeterministicGateSummary {
            unchecked_tasks: 3,
            priority_tasks: 2,
            follow_on_tasks: 1,
        };
        fs::write(
            root.join("DETERMINISTIC-GATE.json"),
            serde_json::to_vec_pretty(&gate).unwrap(),
        )
        .unwrap();

        let manifest = SuperManifest {
            run_id: "run-1".to_string(),
            repo_root: "/repo".to_string(),
            planning_root: "/repo/genesis".to_string(),
            output_dir: Some("/repo/gen-out".to_string()),
            super_root: root.display().to_string(),
            prompt: Some("ship it".to_string()),
            focus: Some("market drama".to_string()),
            model: "gpt-5.5".to_string(),
            reasoning_effort: "xhigh".to_string(),
            worker_model: "gpt-5.5".to_string(),
            worker_reasoning_effort: "high".to_string(),
            max_concurrent_workers: 5,
            max_iterations: None,
            execute: true,
            design_enabled: true,
            super_review_skipped: false,
            design_resolve_passes: 3,
            with_audit: false,
            audit_threads: 0,
            audit_first_pass_retries: 0,
            audit_run_id: None,
            branch: Some("main".to_string()),
            reference_repos: vec!["/ref".to_string()],
            binary: "auto test".to_string(),
            stages: vec![
                SuperStage {
                    name: "gen".to_string(),
                    status: "complete".to_string(),
                    artifact: Some(artifact.display().to_string()),
                },
                SuperStage {
                    name: "parallel".to_string(),
                    status: "launched".to_string(),
                    artifact: Some(root.join("parallel").display().to_string()),
                },
            ],
        };
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let loaded = load_super_manifest(&root).unwrap();

        assert!(super_stage_terminal(&loaded, "gen"));
        // "launched" is intentionally NOT terminal so resume re-enters parallel
        // when an earlier run exited cleanly with everything shelved.
        assert!(!super_stage_terminal(&loaded, "parallel"));
        assert_eq!(super_stage_artifact(&loaded, "gen"), Some(artifact));
        assert_eq!(read_deterministic_gate(&root).unwrap(), gate);
    }

    fn valid_plan(verification: &str) -> String {
        format!(
            r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `TASK-001` Harden super gate

    Spec: `specs/220426-super.md`
    Why now: proves the gate works.
    Codebase evidence: `src/super_command.rs`
    Source of truth: `src/super_command.rs`
    Runtime owner: `src/super_command.rs`
    UI consumers: terminal output
    Generated artifacts: `.auto/super/*/DETERMINISTIC-GATE.json`
    Fixture boundary: production code parses the live root plan, not fixture rows.
    Retired surfaces: legacy active task rows without runtime/UI contract fields.
    Owns: `src/super_command.rs`
    Integration touchpoints: `src/main.rs`
    Scope boundary: do not launch workers.
    Acceptance criteria: scoped plan passes.
    Verification: {verification}
    Required tests: `cargo test super_command::tests::super_accepts_generated_rich_task_contract`
    Contract generation: `cargo test super_command::tests::super_accepts_generated_rich_task_contract`
    Cross-surface tests: `cargo test super_command::tests::super_accepts_generated_rich_task_contract`
    Review/closeout: reviewer checks super and generation task contracts stay aligned.
    Completion artifacts: `src/super_command.rs`
    Lane kind: code
    Dependencies: none
    Estimated scope: S
    Completion signal: tests pass.

## Follow-On Work

## Completed / Already Satisfied
"#
        )
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
