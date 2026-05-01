use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::Serialize;

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
    CorpusArgs, GenerationArgs, ParallelAction, ParallelArgs, ParallelCargoTarget, SuperArgs,
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

#[derive(Serialize)]
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
    design_resolve_passes: usize,
    branch: Option<String>,
    reference_repos: Vec<String>,
    binary: String,
    stages: Vec<SuperStage>,
}

#[derive(Serialize)]
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
    let run_id = timestamp_slug();
    let super_root = repo_root.join(".auto").join("super").join(&run_id);
    let planning_root = args
        .planning_root
        .clone()
        .unwrap_or_else(|| repo_root.join("genesis"));
    let focus = build_super_focus(args.prompt.as_deref(), args.focus.as_deref());

    println!("auto super");
    println!("binary:      {}", binary_provenance_line());
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
    let mut manifest = SuperManifest {
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
        design_resolve_passes: if args.no_execute || args.skip_design {
            0
        } else {
            args.design_resolve_passes.max(1)
        },
        branch: args.branch.clone(),
        reference_repos: args
            .reference_repos
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        binary: binary_provenance_line(),
        stages: Vec::new(),
    };
    write_manifest(&super_root, &manifest)?;
    write_super_cross_repo_manifest(&super_root, &repo_root, &planning_root, &args)?;

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

    if args.skip_design {
        push_stage(
            &super_root,
            &mut manifest,
            "design perfection gate",
            "skipped",
            None,
        )?;
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
        push_stage(
            &super_root,
            &mut manifest,
            "super corpus review",
            "skipped",
            None,
        )?;
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

    if args.skip_super_review {
        push_stage(
            &super_root,
            &mut manifest,
            "execution gate review",
            "skipped",
            None,
        )?;
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
    println!("ready tasks: {}", gate.unchecked_tasks);

    if args.no_execute {
        println!("auto super complete");
        println!("parallel:    skipped (--no-execute)");
        println!("super root:  {}", super_root.display());
        println!("elapsed:     {:?}", started_at.elapsed());
        return Ok(());
    }

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

    println!("auto super complete");
    println!("super root:  {}", super_root.display());
    println!("elapsed:     {:?}", started_at.elapsed());
    Ok(())
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

#[derive(Serialize, Debug, Eq, PartialEq)]
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
    for task in &unchecked {
        verify_super_task(task, &all_task_ids)?;
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

#[allow(dead_code)]
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
    write_manifest(super_root, manifest)
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

        assert!(error
            .to_string()
            .contains("task `TASK-001` is missing `Runtime owner:`"));
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
