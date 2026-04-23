use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::codex_exec::MAX_CODEX_MODEL_CONTEXT_WINDOW;
use crate::codex_stream::{capture_codex_output, capture_pi_output};
use crate::kimi_backend::{
    extract_final_text as kimi_extract_final_text, kimi_exec_args, parse_kimi_error,
    preflight_kimi_cli, resolve_kimi_bin, resolve_kimi_cli_model,
};
use crate::pi_backend::{parse_pi_error, resolve_pi_bin, PiProvider};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, copy_tree, ensure_repo_layout, git_repo_root,
    git_stdout, opencode_agent_dir, push_branch_with_remote_sync, repo_name, run_git,
    sync_branch_with_remote, timestamp_slug,
};
use crate::{HardeningProfile, NemesisArgs};

const DEFAULT_NEMESIS_PROMPT: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, and staging rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, and any security- or audit-related docs already present.
0c. You are running a Nemesis-style audit inspired by the upstream `nemesis-auditor` workflow. Emulate the method directly in this run:
    - Phase 0: Recon and target selection
    - Pass 1: Feynman-style deep logic audit
    - Pass 2: State inconsistency audit enriched by Pass 1 findings
    - Pass 3+: Alternate targeted Feynman and State re-passes until convergence or a maximum of 6 total passes
    - Only keep evidence-backed findings

1. Your task is to perform a deep hardening audit of the live repository and write the audit outputs only into `nemesis/`.
   - Treat the codebase as truth.
   - Use docs and existing plans as supporting context, not authority.
   - Focus on business-logic flaws, state-desync risks, broken invariants, ordering problems, missing guards, and dangerous assumptions.

2. Do not modify root `specs/` or root `IMPLEMENTATION_PLAN.md` directly.
   - Write exactly these files:
     - `nemesis/nemesis-audit.md`
     - `nemesis/IMPLEMENTATION_PLAN.md`

3. `nemesis/nemesis-audit.md` requirements:
   - Must start with `# Specification: Nemesis Audit Findings and Hardening Requirements`
   - Capture only verified findings or verified hardening requirements
   - For each major finding or requirement, include:
     - affected surfaces
     - triggering scenario or failure mode
     - invariant or assumption that breaks
     - why this matters now
     - discovery path (`Feynman`, `State`, or `Cross-feed`)

4. `nemesis/IMPLEMENTATION_PLAN.md` requirements:
   - Must start with `# IMPLEMENTATION_PLAN`
   - Use these top-level sections exactly:
     - `## Priority Work`
     - `## Follow-On Work`
     - `## Completed / Already Satisfied`
   - Each actionable task must use this exact header format:
     - `- [ ] `TASK-ID` Short title`
   - Each task must include these exact fields:
     - `Spec:`
     - `Why now:`
     - `Codebase evidence:`
     - `Owns:`
     - `Integration touchpoints:`
     - `Scope boundary:`
     - `Required tests:`
     - `Dependencies:`
     - `Completion signal:`
   - Only put unfinished work in `Priority Work` or `Follow-On Work`
   - Put already-satisfied audit items only in `Completed / Already Satisfied`
   - Use task ids prefixed with `NEM-`

5. The resulting plan must be execution-ready:
   - concrete
   - file-grounded
   - bounded
   - high signal
   - no vague “investigate further” tasks unless the uncertainty itself is the verified issue

99999. Important: this is not a generic security scan. Use the Nemesis back-and-forth method.
999999. Important: do not invent findings that you cannot support with repo evidence.
9999999. Important: write the two required files completely into `nemesis/` and stop."#;
const DEFAULT_NEMESIS_REVIEW_PROMPT: &str = r#"You are the final Nemesis synthesis pass.

Review the draft Nemesis audit outputs below, then re-check the live repository before you keep any item.

Draft inputs:
- `{draft_audit_path}`
- `{draft_plan_path}`

Rules:
- Treat the live codebase as truth.
- Treat the draft outputs as suspect until they survive your own review.
- Remove weak, duplicated, stale, or unsupported findings instead of carrying them forward.
- Tighten tasks so they are execution-ready and bounded.

{final_prompt}

Additional requirements:
- Only keep evidence-backed findings and tasks in the final outputs.
- Prefer fewer stronger findings over a longer noisy report.
- If a draft item is directionally right but over-scoped, narrow it before keeping it.
"#;
const DEFAULT_NEMESIS_IMPLEMENT_PROMPT: &str = r#"You are the final Nemesis implementation pass.

Input audit artifacts:
- `{audit_path}`
- `{plan_path}`

Rules:
- Treat the live codebase as truth and the final Nemesis plan as the execution contract.
- Implement the unchecked `NEM-` tasks in `## Priority Work` first, then `## Follow-On Work` when their dependencies are satisfied.
- Reproduce the issue, failing invariant, or strongest direct proof first when practical. If literal reproduction is not practical, document the best executable proof you used instead of pretending.
- Fix root causes, not cosmetic symptoms.
- Add or update regression coverage when the repo exposes a real test surface for the affected behavior.
- For runtime-sensitive or user-facing issues, use runtime/browser verification when available.
- Update `{plan_path}` as tasks are truly completed. Mark completed tasks as satisfied instead of leaving them open.
- Do not edit root `specs/` or root `IMPLEMENTATION_PLAN.md` directly in this pass.
- Stay on the currently checked-out branch `{branch}`.
- Commit only truthful fix increments with a message like `repo-name: nemesis fixes`.
- Push to `origin/{branch}` after each successful commit.
- Do not create or switch branches.
- Do not stage or commit unrelated pre-existing changes already present in the worktree.
- Do not stage or commit generated workflow artifacts under `.auto/`, `bug/`, or `gen-*`.
- Only write these files directly as workflow artifacts:
  - `{results_json}`
  - `{results_md}`

`{results_json}` must be a JSON array using exactly this schema:
{{
  "task_id": "NEM-001",
  "status": "fixed|deferred|blocked",
  "summary": "What changed and why",
  "validation_commands": ["Command actually run"],
  "touched_files": ["path/to/file"],
  "residual_risks": ["Anything still not fully closed"]
}}

Requirements:
- Cover every unchecked `NEM-` task in the plan with one result entry unless the final plan already marks it satisfied.
- `fixed` means the root cause was addressed and re-verified.
- If the live repo already satisfies a `fixed` task without edits, keep `touched_files` as `[]` and say plainly in `summary` that no file changes were needed because the requirement was already satisfied.
- `deferred` means the task remains valid but was intentionally left open with a truthful reason.
- `blocked` means an external dependency, ambiguity, or repo limitation prevented a truthful close.
- `{results_md}` should summarize proof-before-fix, root cause, changes made, validation, and any deferred or blocked tasks.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
- Double-escape literal backslashes in regexes, paths, and code snippets (for example `\\d`, `C:\\tmp`, or `foo\\bar`).
"#;

const DEFAULT_CODEX_NEMESIS_MODEL: &str = "gpt-5.5";
#[allow(dead_code)]
const DEFAULT_NEMESIS_AUDIT_MODEL: &str = "gpt-5.5";
const EMPTY_PLAN: &str = "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n";
const JSON_REPAIR_MAX_BYTES: usize = 256 * 1024;
const REQUIRED_PLAN_SECTIONS: [&str; 3] = [
    "## Priority Work",
    "## Follow-On Work",
    "## Completed / Already Satisfied",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlanSection {
    Priority,
    FollowOn,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlanTaskBlock {
    section: PlanSection,
    task_id: String,
    checked: bool,
    markdown: String,
}

#[derive(Clone, Debug)]
struct PhaseConfig {
    model: String,
    effort: String,
}

#[derive(Debug, Deserialize)]
struct NemesisFixResult {
    task_id: String,
    status: String,
    summary: String,
    validation_commands: Vec<String>,
    touched_files: Vec<String>,
    residual_risks: Vec<String>,
}

#[derive(Debug)]
struct VerifiedNemesisOutputs {
    spec_path: PathBuf,
    plan_path: PathBuf,
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

enum NemesisBackend {
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
    KimiCli {
        model: String,
        thinking: String,
        kimi_bin: PathBuf,
    },
}

impl NemesisBackend {
    fn label(&self) -> &'static str {
        match self {
            Self::Codex { .. } => "codex",
            Self::Pi { provider_label, .. } => provider_label,
            Self::KimiCli { .. } => "kimi-cli",
        }
    }

    fn model(&self) -> &str {
        match self {
            Self::Codex { model, .. } => model,
            Self::Pi { model, .. } => model,
            Self::KimiCli { model, .. } => model,
        }
    }

    fn variant(&self) -> &str {
        match self {
            Self::Codex {
                reasoning_effort, ..
            } => reasoning_effort,
            Self::Pi { thinking, .. } => thinking,
            Self::KimiCli { thinking, .. } => thinking,
        }
    }

    #[allow(dead_code)]
    fn is_kimi_family(&self) -> bool {
        matches!(self, Self::KimiCli { .. })
            || matches!(self, Self::Pi { provider_label, .. } if *provider_label == "pi-kimi")
    }
}

fn apply_nemesis_profile(
    profile: HardeningProfile,
    auditor: &mut PhaseConfig,
    reviewer: &mut PhaseConfig,
    fixer: &mut PhaseConfig,
    finalizer: &mut PhaseConfig,
) {
    match profile {
        HardeningProfile::Balanced => {}
        HardeningProfile::Fast => {
            set_default_effort(auditor, "medium");
            set_default_effort(reviewer, "medium");
            set_default_effort(fixer, "high");
            set_default_effort(finalizer, "high");
        }
        HardeningProfile::MaxQuality => {
            for config in [auditor, reviewer, fixer, finalizer] {
                set_default_effort(config, "xhigh");
            }
        }
    }
}

fn set_default_effort(config: &mut PhaseConfig, effort: &str) {
    if config.model == DEFAULT_CODEX_NEMESIS_MODEL && config.effort == "high" {
        config.effort = effort.to_string();
    }
}

pub(crate) async fn run_nemesis(args: NemesisArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    if !args.dry_run && !args.report_only && current_branch.is_empty() {
        bail!(
            "auto nemesis requires a checked-out branch so implementation commits can push to origin"
        );
    }
    if let Some(required_branch) = args.branch.as_deref() {
        if current_branch != required_branch {
            bail!(
                "auto nemesis must run on branch `{}` (current: `{}`)",
                required_branch,
                current_branch
            );
        }
    }

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join("nemesis"));
    let mut auditor = PhaseConfig {
        model: resolve_auditor_model(&args),
        effort: args.reasoning_effort.clone(),
    };
    let mut reviewer = PhaseConfig {
        model: args.reviewer_model.clone(),
        effort: args.reviewer_effort.clone(),
    };
    let mut fixer = PhaseConfig {
        model: args.fixer_model.clone(),
        effort: args.fixer_effort.clone(),
    };
    let mut finalizer = PhaseConfig {
        model: args.finalizer_model.clone(),
        effort: args.finalizer_effort.clone(),
    };
    apply_nemesis_profile(
        args.profile,
        &mut auditor,
        &mut reviewer,
        &mut fixer,
        &mut finalizer,
    );
    ensure_nemesis_phase_config("auto nemesis audit pass", &auditor)?;
    ensure_nemesis_phase_config("auto nemesis synthesis pass", &reviewer)?;
    ensure_nemesis_fixer_config(&fixer)?;
    ensure_nemesis_finalizer_config(&finalizer)?;
    let kimi_preflight_model = [&auditor, &reviewer, &fixer]
        .iter()
        .find(|config| is_kimi_model(&config.model))
        .map(|config| config.model.as_str());
    if args.use_kimi_cli {
        if let Some(model) = kimi_preflight_model {
            let kimi_bin = resolve_kimi_bin(&args.kimi_bin);
            preflight_kimi_cli(&kimi_bin, model)?;
        }
    }
    let audit_backend = select_backend(
        &auditor.model,
        &auditor.effort,
        &args.codex_bin,
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    let review_backend = select_backend(
        &reviewer.model,
        &reviewer.effort,
        &args.codex_bin,
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    let fix_backend = select_backend(
        &fixer.model,
        &fixer.effort,
        &args.codex_bin,
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    validate_nemesis_backend_binaries(
        &audit_backend,
        &review_backend,
        &fix_backend,
        args.report_only,
        &args,
    )?;
    let previous_snapshot =
        maybe_prepare_output_dir(&repo_root, &output_dir, args.dry_run, args.resume)?;

    let prompt_template = match &args.prompt_file {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display()))?,
        None => DEFAULT_NEMESIS_PROMPT.to_string(),
    };
    let draft_audit_path = output_dir.join("draft-nemesis-audit.md");
    let draft_plan_path = output_dir.join("draft-IMPLEMENTATION_PLAN.md");
    let final_audit_path = output_dir.join("nemesis-audit.md");
    let final_plan_path = output_dir.join("IMPLEMENTATION_PLAN.md");
    let implementation_results_json_path = output_dir.join("implementation-results.json");
    let implementation_results_md_path = output_dir.join("implementation-results.md");
    let audit_prompt = build_audit_prompt(&prompt_template, &draft_audit_path, &draft_plan_path);
    let review_prompt = build_review_prompt(
        &prompt_template,
        &draft_audit_path,
        &draft_plan_path,
        &final_audit_path,
        &final_plan_path,
    );
    let audit_prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-audit-prompt.md", timestamp_slug()));
    atomic_write(&audit_prompt_path, audit_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", audit_prompt_path.display()))?;
    let review_prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-review-prompt.md", timestamp_slug()));
    atomic_write(&review_prompt_path, review_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", review_prompt_path.display()))?;
    let implementation_prompt = build_implementation_prompt(
        &final_audit_path,
        &final_plan_path,
        &implementation_results_json_path,
        &implementation_results_md_path,
        args.branch.as_deref().unwrap_or(&current_branch),
    );
    let implementation_prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-implement-prompt.md", timestamp_slug()));
    atomic_write(
        &implementation_prompt_path,
        implementation_prompt.as_bytes(),
    )
    .with_context(|| format!("failed to write {}", implementation_prompt_path.display()))?;

    println!("auto nemesis");
    println!("repo root:   {}", repo_root.display());
    println!("output dir:  {}", output_dir.display());
    println!("profile:     {:?}", args.profile);
    println!(
        "auditor:     {} ({})",
        audit_backend.model(),
        audit_backend.variant()
    );
    println!(
        "reviewer:    {} ({})",
        review_backend.model(),
        review_backend.variant()
    );
    if !args.report_only {
        println!("fixer:       {} ({})", fixer.model, fixer.effort);
        println!(
            "branch:      {}",
            args.branch.as_deref().unwrap_or(&current_branch)
        );
    }
    if let Some(previous) = &previous_snapshot {
        println!("prior input: {}", previous.display());
    }
    if args.resume {
        println!("resume:      reusing valid nemesis artifacts when present");
    }
    if args.dry_run {
        println!("mode:        dry-run");
        return Ok(());
    }
    if !args.report_only {
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, current_branch.as_str(), "nemesis checkpoint")?
        {
            println!("checkpoint:  committed pre-existing changes at {commit}");
        } else if sync_branch_with_remote(&repo_root, current_branch.as_str())? {
            println!("remote sync: rebased onto origin/{}", current_branch);
        }
    } else {
        println!("mode:        report-only");
    }

    let audit_response_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-audit-response.log", timestamp_slug()));
    let review_response_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-review-response.log", timestamp_slug()));

    let final_outputs_reusable = args.resume && verify_nemesis_outputs(&output_dir).is_ok();
    if final_outputs_reusable {
        println!("resume:      reusing verified final audit and plan");
    } else {
        let draft_outputs_reusable =
            args.resume && draft_nemesis_outputs_valid(&draft_audit_path, &draft_plan_path).is_ok();
        if draft_outputs_reusable {
            println!("resume:      reusing draft audit and plan");
        } else {
            print_phase_header("auditor", &audit_backend);
            let audit_response =
                run_nemesis_backend(&repo_root, &audit_prompt, &audit_backend, &args.codex_bin)
                    .await
                    .map_err(|err| {
                        annotate_output_recovery(
                            err,
                            &output_dir,
                            previous_snapshot.as_deref(),
                            "Nemesis audit pass failed",
                        )
                    })?;
            if !audit_response.trim().is_empty() {
                atomic_write(&audit_response_path, audit_response.as_bytes()).with_context(
                    || format!("failed to write {}", audit_response_path.display()),
                )?;
            }
        }

        print_phase_header("reviewer", &review_backend);
        let review_response =
            run_nemesis_backend(&repo_root, &review_prompt, &review_backend, &args.codex_bin)
                .await
                .map_err(|err| {
                    annotate_output_recovery(
                        err,
                        &output_dir,
                        previous_snapshot.as_deref(),
                        "Nemesis synthesis pass failed",
                    )
                })?;
        if !review_response.trim().is_empty() {
            atomic_write(&review_response_path, review_response.as_bytes())
                .with_context(|| format!("failed to write {}", review_response_path.display()))?;
        }
    }

    let VerifiedNemesisOutputs {
        spec_path,
        plan_path,
    } = verify_nemesis_outputs(&output_dir).map_err(|err| {
        annotate_output_recovery(
            err,
            &output_dir,
            previous_snapshot.as_deref(),
            "Nemesis output verification failed",
        )
    })?;
    let mut implementation_results = None::<PathBuf>;
    let mut implementation_summary = "report-only".to_string();
    if !args.report_only {
        let pending_tasks = load_unchecked_nemesis_task_ids(&plan_path)?;
        let commit_before = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        println!();
        println!("phase:       implementer");
        println!("backend:     {}", fix_backend.label());
        println!("model:       {}", fix_backend.model());
        println!("variant:     {}", fix_backend.variant());
        if pending_tasks.is_empty() {
            println!("status:      no unchecked Nemesis tasks; skipping implementer");
            implementation_summary =
                format!("skipped (no unchecked tasks in {})", plan_path.display());
        } else if args.resume
            && verify_nemesis_implementation_results_once(
                &implementation_results_json_path,
                &implementation_results_md_path,
                &plan_path,
            )
            .is_ok()
        {
            println!("resume:      reusing implementation results");
            implementation_summary = implementation_results_json_path.display().to_string();
            implementation_results = Some(implementation_results_json_path.clone());
        } else {
            // Route the implementer through the selected backend. Codex stays
            // as the finalizer that reviews the landed diff after this phase.
            let stderr_log = output_dir.join("implementer.stderr.log");
            let response = run_nemesis_backend(
                &repo_root,
                &implementation_prompt,
                &fix_backend,
                &args.codex_bin,
            )
            .await?;
            let response_path = output_dir.join("implementation-response.log");
            if !response.trim().is_empty() {
                atomic_write(&response_path, response.as_bytes())
                    .with_context(|| format!("failed to write {}", response_path.display()))?;
            }
            let _ = stderr_log; // stderr capture already handled by backend helpers

            let implementation_path = verify_nemesis_implementation_results(
                &repo_root,
                &fix_backend,
                &args.codex_bin,
                &spec_path,
                &implementation_results_json_path,
                &implementation_results_md_path,
                &plan_path,
            )
            .await?;
            implementation_summary = implementation_path.display().to_string();
            implementation_results = Some(implementation_path);
        }
        if implementation_results.is_some() {
            // Codex finalizer: independent review of the diff just produced.
            // Fails loudly if it finds regressions; audit record is written to
            // `nemesis/final-review.md`.
            let finalizer_backend = NemesisBackend::Codex {
                model: finalizer.model.clone(),
                reasoning_effort: finalizer.effort.clone(),
                codex_bin: args.codex_bin.clone(),
            };
            let finalizer_prompt = build_finalizer_prompt(
                &spec_path,
                &plan_path,
                &implementation_results_json_path,
                &implementation_results_md_path,
                args.branch.as_deref().unwrap_or(&current_branch),
            );
            let finalizer_prompt_path = repo_root
                .join(".auto")
                .join("logs")
                .join(format!("nemesis-{}-finalizer-prompt.md", timestamp_slug()));
            atomic_write(&finalizer_prompt_path, finalizer_prompt.as_bytes())
                .with_context(|| format!("failed to write {}", finalizer_prompt_path.display()))?;
            let finalizer_response_path = output_dir.join("final-review.md");
            if args.resume && nonempty_file(&finalizer_response_path) {
                println!("resume:      reusing finalizer review");
            } else {
                print_phase_header("finalizer", &finalizer_backend);
                let finalizer_response = run_nemesis_backend(
                    &repo_root,
                    &finalizer_prompt,
                    &finalizer_backend,
                    &args.codex_bin,
                )
                .await?;
                atomic_write(&finalizer_response_path, finalizer_response.as_bytes())
                    .with_context(|| {
                        format!("failed to write {}", finalizer_response_path.display())
                    })?;
            }
            println!(
                "finalizer:   wrote review to {}",
                finalizer_response_path.display()
            );

            let commit_after = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
            if commit_before.trim() != commit_after.trim()
                && push_branch_with_remote_sync(&repo_root, current_branch.as_str())?
            {
                println!("remote sync: rebased onto origin/{}", current_branch);
            }
        }
    }
    let root_spec = sync_nemesis_spec_to_root(&repo_root, &spec_path)?;
    let appended = append_nemesis_plan_to_root(&repo_root, &plan_path)?;
    let trailing_commit = if args.report_only {
        None
    } else {
        commit_nemesis_outputs_if_needed(
            &repo_root,
            current_branch.as_str(),
            &output_dir,
            &root_spec,
            &repo_root.join("IMPLEMENTATION_PLAN.md"),
        )?
    };

    println!();
    println!("nemesis complete");
    println!("spec:        {}", spec_path.display());
    println!("plan:        {}", plan_path.display());
    println!("root spec:   {}", root_spec.display());
    println!("root tasks:  {} appended", appended);
    if let Some(path) = implementation_results {
        println!("implementation: {}", path.display());
    } else {
        println!("implementation: {}", implementation_summary);
    }
    println!("audit prompt: {}", audit_prompt_path.display());
    println!("review prompt: {}", review_prompt_path.display());
    if !args.report_only {
        println!("implement prompt: {}", implementation_prompt_path.display());
    }
    if audit_response_path.exists() {
        println!("audit log:   {}", audit_response_path.display());
    }
    if review_response_path.exists() {
        println!("review log:  {}", review_response_path.display());
    }
    if let Some(commit) = trailing_commit {
        println!("outputs commit: {}", commit);
    }

    Ok(())
}

fn select_backend(
    model: &str,
    effort: &str,
    codex_bin: &Path,
    pi_bin: &Path,
    kimi_bin: &Path,
    use_kimi_cli: bool,
) -> NemesisBackend {
    let is_kimi = is_kimi_model(model);
    if is_kimi && use_kimi_cli {
        return NemesisBackend::KimiCli {
            model: resolve_kimi_cli_model(model),
            thinking: effort.to_string(),
            kimi_bin: resolve_kimi_bin(kimi_bin),
        };
    }

    if let Some(provider) = PiProvider::detect(model) {
        return NemesisBackend::Pi {
            provider_label: provider.provider_label(),
            model: provider.resolve_model(model, DEFAULT_CODEX_NEMESIS_MODEL),
            thinking: effort.to_string(),
            pi_bin: resolve_pi_bin(pi_bin),
        };
    }

    NemesisBackend::Codex {
        model: model.to_string(),
        reasoning_effort: effort.to_string(),
        codex_bin: codex_bin.to_path_buf(),
    }
}

fn is_kimi_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower.contains("kimi") || lower.starts_with("k2.") || lower.starts_with("k2p")
}

fn ensure_nemesis_phase_config(label: &str, config: &PhaseConfig) -> Result<()> {
    if config.model.trim().is_empty() {
        bail!("{label} model is required");
    }
    Ok(())
}

/// Accept any concrete model for remediation. The finalizer phase has its own
/// Codex-only gate so implementation can use Codex by default or an explicit
/// Kimi/PI opt-in.
fn ensure_nemesis_fixer_config(config: &PhaseConfig) -> Result<()> {
    if config.model.trim().is_empty() {
        bail!("auto nemesis fixer model is required");
    }
    Ok(())
}

/// Finalizer MUST be Codex so the last pass is independent of any optional
/// Kimi/PI implementation backend.
fn ensure_nemesis_finalizer_config(config: &PhaseConfig) -> Result<()> {
    if is_kimi_model(&config.model) || PiProvider::detect(&config.model).is_some() {
        bail!(
            "auto nemesis finalizer must use a Codex model (e.g. `gpt-5.5`); got `{}`",
            config.model
        );
    }
    Ok(())
}

fn resolve_auditor_model(args: &NemesisArgs) -> String {
    if args.model != DEFAULT_NEMESIS_AUDIT_MODEL {
        return args.model.clone();
    }
    // Explicit legacy opt-in still honoured so operators who want a MiniMax
    // second-opinion run can force it with `--minimax`.
    if args.minimax {
        return "minimax".to_string();
    }
    // Explicit legacy opt-in for the Kimi audit model remains available.
    if args.kimi {
        return "k2.6".to_string();
    }
    args.model.clone()
}

fn validate_nemesis_backend_binaries(
    audit_backend: &NemesisBackend,
    review_backend: &NemesisBackend,
    fix_backend: &NemesisBackend,
    report_only: bool,
    args: &NemesisArgs,
) -> Result<()> {
    validate_backend_binary("Nemesis audit backend", audit_backend)?;
    validate_backend_binary("Nemesis synthesis backend", review_backend)?;
    if !report_only {
        validate_backend_binary("Nemesis implementation backend", fix_backend)?;
        ensure_executable_available("Nemesis finalizer backend", &args.codex_bin)?;
    }
    Ok(())
}

fn validate_backend_binary(label: &str, backend: &NemesisBackend) -> Result<()> {
    match backend {
        NemesisBackend::Codex { codex_bin, .. } => ensure_executable_available(label, codex_bin),
        NemesisBackend::Pi { pi_bin, .. } => ensure_executable_available(label, pi_bin),
        NemesisBackend::KimiCli { kimi_bin, .. } => ensure_executable_available(label, kimi_bin),
    }
}

fn ensure_executable_available(label: &str, executable: &Path) -> Result<()> {
    if executable.components().count() > 1 || executable.is_absolute() {
        let metadata = fs::metadata(executable).with_context(|| {
            format!(
                "{label} executable {} is not available",
                executable.display()
            )
        })?;
        if !metadata.is_file() {
            bail!("{label} executable {} is not a file", executable.display());
        }
        return Ok(());
    }

    let Some(path) = std::env::var_os("PATH") else {
        bail!(
            "PATH is not set, so {label} executable `{}` cannot be resolved",
            executable.display()
        );
    };
    for directory in std::env::split_paths(&path) {
        if directory.join(executable).is_file() {
            return Ok(());
        }
    }
    bail!(
        "{label} executable `{}` was not found on PATH",
        executable.display()
    );
}

fn maybe_prepare_output_dir(
    repo_root: &Path,
    output_dir: &Path,
    dry_run: bool,
    resume: bool,
) -> Result<Option<PathBuf>> {
    if dry_run {
        return Ok(None);
    }
    if resume {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        return Ok(None);
    }
    prepare_output_dir(repo_root, output_dir)
}

fn annotate_output_recovery(
    error: anyhow::Error,
    output_dir: &Path,
    previous_snapshot: Option<&Path>,
    context: &str,
) -> anyhow::Error {
    let mut message = format!("{context} for {}", output_dir.display());
    if let Some(snapshot) = previous_snapshot {
        message.push_str(&format!(
            ". Previous outputs were archived at {}; restore from that snapshot after fixing \
             the backend failure.",
            snapshot.display()
        ));
    }
    error.context(message)
}

fn build_audit_prompt(prompt_template: &str, audit_path: &Path, plan_path: &Path) -> String {
    let prompt = render_prompt_outputs(prompt_template, audit_path, plan_path);
    format!(
        "You are the initial Nemesis audit pass.\n\n{prompt}\n\nAdditional requirements:\n- This pass should maximize useful recall while staying grounded in evidence.\n- Treat these outputs as draft artifacts that will be challenged by a second-stage review.\n"
    )
}

fn build_review_prompt(
    prompt_template: &str,
    draft_audit_path: &Path,
    draft_plan_path: &Path,
    final_audit_path: &Path,
    final_plan_path: &Path,
) -> String {
    let final_prompt = render_prompt_outputs(prompt_template, final_audit_path, final_plan_path);
    DEFAULT_NEMESIS_REVIEW_PROMPT
        .replace(
            "{draft_audit_path}",
            &draft_audit_path.display().to_string(),
        )
        .replace("{draft_plan_path}", &draft_plan_path.display().to_string())
        .replace("{final_prompt}", &final_prompt)
}

fn build_implementation_prompt(
    audit_path: &Path,
    plan_path: &Path,
    results_json: &Path,
    results_md: &Path,
    branch: &str,
) -> String {
    DEFAULT_NEMESIS_IMPLEMENT_PROMPT
        .replace("{audit_path}", &audit_path.display().to_string())
        .replace("{plan_path}", &plan_path.display().to_string())
        .replace("{results_json}", &results_json.display().to_string())
        .replace("{results_md}", &results_md.display().to_string())
        .replace("{branch}", branch)
}

/// Codex finalizer prompt. Reads the produced spec + plan + implementation
/// results and produces an independent review of the landed diff. Fails the
/// audit if the reviewer finds regressions, missing test coverage, or a fix
/// that claims `status: fixed` without touching the cited files.
fn build_finalizer_prompt(
    audit_path: &Path,
    plan_path: &Path,
    results_json: &Path,
    results_md: &Path,
    branch: &str,
) -> String {
    format!(
        r#"You are the Codex finalizer for an `auto nemesis` run.

The audit, synthesis, and implementation passes have just produced the landed
diff. Your job is to give that diff an independent code review and decide
whether the run is safe to ship as-is.

## Inputs

- Audit: `{audit}`
- Plan: `{plan}`
- Implementation results: `{results_json}`
- Implementation summary: `{results_md}`
- Branch: `{branch}`

## What to verify

1. For every task marked `status: fixed` in `{results_json}`:
   - Re-read each cited path in `touched_files` and confirm the code change
     actually addresses the root cause the audit + plan describe.
   - Run the listed `validation_commands` and record pass/fail.
   - Surface any regression, missing test coverage, or silent scope creep.
2. For every task marked `deferred` or `blocked`, verify the stated reason is
   truthful against the code.
3. Flag any fix that claims `touched_files: []` but the codebase still contains
   the documented failure mode.
4. Look for usual agent failure modes: over-wide refactors, speculative cleanup,
   silent suppression of warnings, hard-coded test fixtures.

## Deliverables

Write your review to a markdown file at `nemesis/final-review.md`. Structure:

```
# Final Review — auto nemesis

## Verdict
PASS | CONCERNS | FAIL

## Per-task verdicts
- TASK_ID: PASS | CONCERNS | FAIL — rationale in 2-3 lines

## Regressions observed
(if any)

## Validation commands rerun
(which ones you executed; outcomes)
```

If you find `FAIL`-severity issues, fix them in place with a minimal diff and
record them under `## Regressions observed`. Do not rewrite passing work.
Do not touch `nemesis/` artifacts other than `nemesis/final-review.md`.

Stay on branch `{branch}`. Commit any remediation with the message
`codex-finalizer: address nemesis regressions`.
"#,
        audit = audit_path.display(),
        plan = plan_path.display(),
        results_json = results_json.display(),
        results_md = results_md.display(),
        branch = branch,
    )
}

fn render_prompt_outputs(prompt_template: &str, audit_path: &Path, plan_path: &Path) -> String {
    prompt_template
        .replace(
            "nemesis/nemesis-audit.md",
            &audit_path.display().to_string(),
        )
        .replace(
            "nemesis/IMPLEMENTATION_PLAN.md",
            &plan_path.display().to_string(),
        )
}

fn print_phase_header(phase: &str, backend: &NemesisBackend) {
    println!();
    println!("phase:       {phase}");
    println!("backend:     {}", backend.label());
    println!("model:       {}", backend.model());
    println!("variant:     {}", backend.variant());
}

fn prepare_output_dir(repo_root: &Path, output_dir: &Path) -> Result<Option<PathBuf>> {
    if !output_dir.exists() {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        return Ok(None);
    }
    if !output_dir.is_dir() {
        bail!(
            "Nemesis output path {} is not a directory",
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
                .unwrap_or("nemesis"),
            timestamp_slug()
        ));
        copy_tree(output_dir, &snapshot_root).with_context(|| {
            format!(
                "failed to archive existing Nemesis output from {} into {}",
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

async fn run_nemesis_backend(
    repo_root: &Path,
    prompt: &str,
    backend: &NemesisBackend,
    codex_bin: &Path,
) -> Result<String> {
    match backend {
        NemesisBackend::Codex {
            model,
            reasoning_effort,
            codex_bin,
        } => run_codex(repo_root, prompt, model, reasoning_effort, codex_bin).await,
        NemesisBackend::Pi {
            model,
            thinking,
            pi_bin,
            ..
        } => match run_pi(repo_root, prompt, model, thinking, pi_bin).await {
            Ok(output) => Ok(output),
            Err(e) => {
                eprintln!("[auto-nemesis] Kimi (pi) backend failed: {e:#}");
                eprintln!("[auto-nemesis] falling back to Codex");
                let fallback = NemesisBackend::Codex {
                    model: DEFAULT_CODEX_NEMESIS_MODEL.to_string(),
                    reasoning_effort: "high".to_string(),
                    codex_bin: codex_bin.to_path_buf(),
                };
                print_phase_header("fallback", &fallback);
                run_codex(
                    repo_root,
                    prompt,
                    DEFAULT_CODEX_NEMESIS_MODEL,
                    "high",
                    codex_bin,
                )
                .await
            }
        },
        NemesisBackend::KimiCli {
            model,
            thinking,
            kimi_bin,
        } => match run_kimi_cli(repo_root, prompt, model, thinking, kimi_bin).await {
            Ok(output) => Ok(output),
            Err(e) => {
                eprintln!("[auto-nemesis] kimi-cli backend failed: {e:#}");
                eprintln!("[auto-nemesis] falling back to Codex");
                let fallback = NemesisBackend::Codex {
                    model: DEFAULT_CODEX_NEMESIS_MODEL.to_string(),
                    reasoning_effort: "high".to_string(),
                    codex_bin: codex_bin.to_path_buf(),
                };
                print_phase_header("fallback", &fallback);
                run_codex(
                    repo_root,
                    prompt,
                    DEFAULT_CODEX_NEMESIS_MODEL,
                    "high",
                    codex_bin,
                )
                .await
            }
        },
    }
}

async fn run_kimi_cli(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    thinking: &str,
    kimi_bin: &Path,
) -> Result<String> {
    let args = kimi_exec_args(model, thinking, prompt);
    let mut command = TokioCommand::new(kimi_bin);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch kimi-cli at {} from {}",
            kimi_bin.display(),
            repo_root.display()
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .context("kimi-cli stdout should be piped for nemesis")?;
    let stderr = child
        .stderr
        .take()
        .context("kimi-cli stderr should be piped for nemesis")?;

    let stdout_task =
        tokio::spawn(async move { capture_pi_output(stdout, "auto nemesis kimi-cli", 15).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child
        .wait()
        .await
        .context("failed waiting for kimi-cli nemesis run")?;
    let stdout = stdout_task
        .await
        .context("kimi-cli stdout capture task panicked")??;
    let stderr = stderr_task
        .await
        .context("kimi-cli stderr capture task panicked")??;
    if !status.success() {
        bail!(
            "kimi-cli nemesis run failed: {}",
            stderr.trim().if_empty_then(
                parse_kimi_error(&stdout)
                    .as_deref()
                    .unwrap_or(stdout.trim())
            )
        );
    }
    if let Some(detail) = parse_kimi_error(&stdout) {
        bail!("kimi-cli nemesis run failed: {detail}");
    }
    let mut final_text = String::new();
    for line in stdout.lines() {
        if let Some(chunk) = kimi_extract_final_text(line) {
            if !final_text.is_empty() {
                final_text.push('\n');
            }
            final_text.push_str(&chunk);
        }
    }
    if final_text.trim().is_empty() {
        return Ok(stdout);
    }
    Ok(final_text)
}

async fn run_codex(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<String> {
    let mut child = TokioCommand::new(codex_bin)
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
        .arg("-c")
        .arg(format!(
            "model_context_window={MAX_CODEX_MODEL_CONTEXT_WINDOW}"
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root)
        .spawn()
        .with_context(|| {
            format!(
                "failed to launch Codex at {} from {}",
                codex_bin.display(),
                repo_root.display()
            )
        })?;

    let mut stdin = child
        .stdin
        .take()
        .context("Codex stdin missing for Nemesis run")?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .context("failed to write Nemesis prompt to Codex")?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .context("Codex stdout missing for Nemesis run")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex stderr missing for Nemesis run")?;

    let stdout_task = tokio::spawn(async move { capture_codex_output(stdout).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child
        .wait()
        .await
        .context("failed waiting for Codex Nemesis run")?;
    let stdout = stdout_task
        .await
        .context("Codex stdout capture task panicked")??;
    let stderr = stderr_task
        .await
        .context("Codex stderr capture task panicked")??;
    if status.success() {
        return Ok(stdout);
    }
    bail!(
        "Codex Nemesis run failed: {}",
        stderr.trim().if_empty_then(stdout.trim())
    );
}

async fn run_pi(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    thinking: &str,
    pi_bin: &Path,
) -> Result<String> {
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
        .context("PI stdout missing for Nemesis run")?;
    let stderr = child
        .stderr
        .take()
        .context("PI stderr missing for Nemesis run")?;

    let stream_label = "nemesis".to_string();
    let stdout_task =
        tokio::spawn(async move { capture_pi_output(stdout, &stream_label, 15).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child
        .wait()
        .await
        .context("failed waiting for PI Nemesis run")?;
    let stdout = stdout_task
        .await
        .context("PI stdout capture task panicked")??;
    let stderr = stderr_task
        .await
        .context("PI stderr capture task panicked")??;
    if status.success() {
        if let Some(detail) = parse_pi_error(&stdout) {
            bail!("PI Nemesis run failed: {detail}");
        }
        return Ok(stdout);
    }
    bail!(
        "PI Nemesis run failed: {}",
        stderr
            .trim()
            .if_empty_then(parse_pi_error(&stdout).as_deref().unwrap_or(stdout.trim()))
    );
}

fn configure_pi_env(command: &mut TokioCommand, repo_root: &Path) -> Result<()> {
    let agent_dir = opencode_agent_dir(repo_root);
    fs::create_dir_all(&agent_dir)
        .with_context(|| format!("failed to create {}", agent_dir.display()))?;
    command.env("PI_CODING_AGENT_DIR", &agent_dir);
    command.env("OPENCODE_CODING_AGENT_DIR", &agent_dir);
    Ok(())
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

fn verify_nemesis_outputs(output_dir: &Path) -> Result<VerifiedNemesisOutputs> {
    let spec_path = output_dir.join("nemesis-audit.md");
    let plan_path = output_dir.join("IMPLEMENTATION_PLAN.md");
    let has_spec = spec_path.exists();
    let has_plan = plan_path.exists();
    match (has_spec, has_plan) {
        (true, true) => {}
        (false, false) => {
            bail!(
                "Nemesis run did not write either {} or {}. Check the model logs and rerun.",
                spec_path.display(),
                plan_path.display()
            );
        }
        (false, true) => {
            bail!(
                "Nemesis run only partially completed: missing {} but found {}. Review the \
                 model logs, remove the partial output, and rerun.",
                spec_path.display(),
                plan_path.display()
            );
        }
        (true, false) => {
            bail!(
                "Nemesis run only partially completed: found {} but missing {}. Review the \
                 model logs, remove the partial output, and rerun.",
                spec_path.display(),
                plan_path.display()
            );
        }
    }

    let spec_markdown = fs::read_to_string(&spec_path)
        .with_context(|| format!("failed to read {}", spec_path.display()))?;
    if !spec_markdown.starts_with("# Specification:") {
        bail!(
            "Nemesis spec {} must start with `# Specification:`",
            spec_path.display()
        );
    }

    let plan_markdown = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    for required in [
        "# IMPLEMENTATION_PLAN",
        "## Priority Work",
        "## Follow-On Work",
        "## Completed / Already Satisfied",
    ] {
        if !plan_markdown.contains(required) {
            bail!("Nemesis implementation plan is missing `{required}`");
        }
    }
    Ok(VerifiedNemesisOutputs {
        spec_path,
        plan_path,
    })
}

fn draft_nemesis_outputs_valid(draft_audit_path: &Path, draft_plan_path: &Path) -> Result<()> {
    if !draft_audit_path.exists() || !draft_plan_path.exists() {
        bail!("draft Nemesis outputs are incomplete");
    }
    let audit = fs::read_to_string(draft_audit_path)
        .with_context(|| format!("failed to read {}", draft_audit_path.display()))?;
    let plan = fs::read_to_string(draft_plan_path)
        .with_context(|| format!("failed to read {}", draft_plan_path.display()))?;
    if !audit.starts_with("# Specification:") {
        bail!("draft Nemesis audit must start with `# Specification:`");
    }
    if !plan.contains("# IMPLEMENTATION_PLAN") {
        bail!("draft Nemesis plan must contain `# IMPLEMENTATION_PLAN`");
    }
    Ok(())
}

fn nonempty_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

async fn verify_nemesis_implementation_results(
    repo_root: &Path,
    backend: &NemesisBackend,
    codex_bin: &Path,
    audit_path: &Path,
    results_json_path: &Path,
    results_md_path: &Path,
    plan_path: &Path,
) -> Result<PathBuf> {
    match verify_nemesis_implementation_results_once(results_json_path, results_md_path, plan_path)
    {
        Ok(_) => {}
        Err(original_error) => {
            println!(
                "warning: attempting backend repair for Nemesis implementation artifacts in {}",
                results_json_path.display()
            );
            repair_nemesis_implementation_outputs(
                repo_root,
                backend,
                codex_bin,
                audit_path,
                plan_path,
                results_json_path,
                results_md_path,
            )
            .await
            .with_context(|| {
                format!(
                    "backend repair failed for Nemesis implementation artifacts in {}",
                    results_json_path.display()
                )
            })?;
            verify_nemesis_implementation_results_once(results_json_path, results_md_path, plan_path)
                .map_err(|repair_error| {
                    anyhow::anyhow!(
                        "failed to recover Nemesis implementation artifacts after backend repair; original error: {}; repair error: {}",
                        original_error,
                        repair_error
                    )
                })?;
        }
    }
    Ok(results_json_path.to_path_buf())
}

fn verify_nemesis_implementation_results_once(
    results_json_path: &Path,
    results_md_path: &Path,
    plan_path: &Path,
) -> Result<Vec<NemesisFixResult>> {
    if !results_json_path.exists() {
        bail!(
            "Nemesis implementation did not write {}",
            results_json_path.display()
        );
    }
    if !results_md_path.exists() {
        bail!(
            "Nemesis implementation did not write {}",
            results_md_path.display()
        );
    }

    let results = load_nemesis_fix_results(results_json_path)?;
    let expected_ids = load_unchecked_nemesis_task_ids(plan_path)?;
    let actual_ids = results
        .iter()
        .map(|result| result.task_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for task_id in &expected_ids {
        if !actual_ids.contains(task_id.as_str()) {
            bail!("Nemesis implementation results missing task `{task_id}`");
        }
    }
    Ok(results)
}

fn load_unchecked_nemesis_task_ids(plan_path: &Path) -> Result<std::collections::BTreeSet<String>> {
    unchecked_nemesis_task_ids(
        &fs::read_to_string(plan_path)
            .with_context(|| format!("failed to read {}", plan_path.display()))?,
    )
}

fn unchecked_nemesis_task_ids(markdown: &str) -> Result<std::collections::BTreeSet<String>> {
    Ok(extract_plan_task_blocks(markdown)?
        .into_iter()
        .filter(|block| !block.checked)
        .filter(|block| block.task_id.starts_with("NEM-"))
        .map(|block| block.task_id)
        .collect())
}

fn load_nemesis_fix_results(path: &Path) -> Result<Vec<NemesisFixResult>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let results = match serde_json::from_str::<Vec<NemesisFixResult>>(&content) {
        Ok(results) => results,
        Err(original_error) => {
            let Some(repaired) = repair_nemesis_json(&content) else {
                bail!("failed to parse {}: {}", path.display(), original_error);
            };
            match serde_json::from_str::<Vec<NemesisFixResult>>(&repaired) {
                Ok(results) => {
                    println!(
                        "warning: repaired invalid or incomplete JSON in {}",
                        path.display()
                    );
                    if repaired != content {
                        atomic_write(path, repaired.as_bytes())?;
                    }
                    results
                }
                Err(repair_error) => bail!(
                    "failed to parse {}: {}; automatic repair also failed: {}",
                    path.display(),
                    original_error,
                    repair_error
                ),
            }
        }
    };
    validate_nemesis_fix_results(&results)?;
    Ok(results)
}

fn validate_nemesis_fix_results(results: &[NemesisFixResult]) -> Result<()> {
    for result in results {
        match result.status.trim().to_ascii_lowercase().as_str() {
            "fixed" | "deferred" | "blocked" => {}
            other => bail!(
                "invalid Nemesis fix status `{other}` for {}",
                result.task_id
            ),
        }
        if result.task_id.trim().is_empty() || result.summary.trim().is_empty() {
            bail!(
                "Nemesis implementation result is missing required fields for `{}`",
                result.task_id
            );
        }
        if result.status.eq_ignore_ascii_case("fixed") {
            if result.validation_commands.is_empty() {
                bail!(
                    "Nemesis implementation result for `{}` must include validation commands",
                    result.task_id
                );
            }
            if result.touched_files.is_empty() && !fixed_nemesis_result_is_truthful_noop(result) {
                bail!(
                    "Nemesis implementation result for `{}` must include touched files unless the summary explicitly states that no file changes were needed",
                    result.task_id
                );
            }
        }
        if (result.status.eq_ignore_ascii_case("deferred")
            || result.status.eq_ignore_ascii_case("blocked"))
            && result.residual_risks.is_empty()
        {
            bail!(
                "Nemesis {} result for `{}` must explain residual risks",
                result.status,
                result.task_id
            );
        }
    }
    Ok(())
}

fn fixed_nemesis_result_is_truthful_noop(result: &NemesisFixResult) -> bool {
    if !result.status.eq_ignore_ascii_case("fixed") || !result.touched_files.is_empty() {
        return false;
    }

    let summary = result.summary.to_ascii_lowercase();
    summary.contains("no file changes were needed")
        || summary.contains("no code changes were needed")
        || summary.contains("no changes were needed")
}

fn repair_nemesis_json(content: &str) -> Option<String> {
    let candidate = extract_fenced_json_block(content).unwrap_or_else(|| content.to_string());
    if candidate.len() > JSON_REPAIR_MAX_BYTES {
        return None;
    }
    let repaired = escape_unescaped_quotes_in_json_strings(&candidate);
    let repaired = extract_complete_json_value_prefix(&repaired).unwrap_or(repaired);
    (repaired != content).then_some(repaired)
}

fn extract_complete_json_value_prefix(content: &str) -> Option<String> {
    let content = content.trim_start();
    let mut stream = serde_json::Deserializer::from_str(content).into_iter::<serde_json::Value>();
    stream.next()?.ok()?;
    let end = stream.byte_offset();
    if content[end..].trim().is_empty() {
        return None;
    }
    Some(content[..end].trim_end().to_string())
}

async fn repair_nemesis_implementation_outputs(
    repo_root: &Path,
    backend: &NemesisBackend,
    codex_bin: &Path,
    audit_path: &Path,
    plan_path: &Path,
    results_json_path: &Path,
    results_md_path: &Path,
) -> Result<()> {
    let prompt = build_nemesis_results_repair_prompt(
        audit_path,
        plan_path,
        results_json_path,
        results_md_path,
    );
    let repair_response = run_nemesis_backend(repo_root, &prompt, backend, codex_bin).await?;
    if !repair_response.trim().is_empty() {
        let log_path = repo_root.join(".auto").join("logs").join(format!(
            "nemesis-{}-implementation-repair-response.log",
            timestamp_slug()
        ));
        atomic_write(&log_path, repair_response.as_bytes())
            .with_context(|| format!("failed to write {}", log_path.display()))?;
    }
    Ok(())
}

fn build_nemesis_results_repair_prompt(
    audit_path: &Path,
    plan_path: &Path,
    results_json_path: &Path,
    results_md_path: &Path,
) -> String {
    format!(
        r#"You are repairing malformed implementation artifacts for auto nemesis.

Input context:
- Audit: `{audit_path}`
- Plan: `{plan_path}`

Artifacts to repair:
- `{results_json_path}`
- `{results_md_path}`

Rules:
- Do not modify code, tests, git state, or any workflow artifacts other than the two files above.
- Read the audit, the plan, and the current repository state to recover the truthful implementation summary.
- Rewrite `{results_json_path}` as valid JSON only. No markdown fences. No commentary.
- Rewrite `{results_md_path}` as a concise markdown summary of the same results.
- Preserve every recoverable task result. Do not invent work that did not happen.
- `{results_json_path}` must be a JSON array using exactly this schema:
[
  {{
    "task_id": "NEM-001",
    "status": "fixed|deferred|blocked",
    "summary": "What changed and why",
    "validation_commands": ["Command actually run"],
    "touched_files": ["path/to/file"],
    "residual_risks": ["Anything still not fully closed"]
  }}
]
- If a `fixed` task was already satisfied before this pass and required no edits, keep `touched_files` as `[]` and state explicitly in `summary` that no file changes were needed because the live repo already satisfied the requirement.
- JSON strings must stay valid JSON. Escape embedded quotes when needed.
- Double-escape literal backslashes in regexes, paths, and code snippets.
"#,
        audit_path = audit_path.display(),
        plan_path = plan_path.display(),
        results_json_path = results_json_path.display(),
        results_md_path = results_md_path.display(),
    )
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
            match ch {
                '\\' => {
                    if valid_json_string_escape_at(&chars, index) {
                        repaired.push('\\');
                        repaired.push(chars[index + 1]);
                        if chars[index + 1] == 'u' {
                            repaired.extend(chars[index + 2..index + 6].iter().copied());
                            index += 6;
                        } else {
                            index += 2;
                        }
                    } else {
                        repaired.push('\\');
                        repaired.push('\\');
                        index += 1;
                    }
                    continue;
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

fn valid_json_string_escape_at(chars: &[char], index: usize) -> bool {
    let Some(next) = chars.get(index + 1).copied() else {
        return false;
    };

    match next {
        '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' => true,
        'u' => chars.get(index + 2..index + 6).is_some_and(|digits| {
            digits.len() == 4 && digits.iter().all(|digit| digit.is_ascii_hexdigit())
        }),
        _ => false,
    }
}

fn sync_nemesis_spec_to_root(repo_root: &Path, spec_path: &Path) -> Result<PathBuf> {
    let root_specs_dir = repo_root.join("specs");
    fs::create_dir_all(&root_specs_dir)
        .with_context(|| format!("failed to create {}", root_specs_dir.display()))?;
    let destination = next_nemesis_spec_destination(&root_specs_dir, spec_path, Local::now());

    fs::copy(spec_path, &destination).with_context(|| {
        format!(
            "failed to copy {} -> {}",
            spec_path.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

fn next_nemesis_spec_destination(
    root_specs_dir: &Path,
    spec_path: &Path,
    timestamp: DateTime<Local>,
) -> PathBuf {
    let date_prefix = timestamp.format("%d%m%y-%H%M%S").to_string();
    let slug = spec_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("nemesis-audit");
    let extension = spec_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("md");

    let mut counter = 1usize;
    loop {
        let candidate = if counter == 1 {
            root_specs_dir.join(format!("{date_prefix}-{slug}.{extension}"))
        } else {
            root_specs_dir.join(format!("{date_prefix}-{slug}-{counter}.{extension}"))
        };
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

fn append_nemesis_plan_to_root(repo_root: &Path, nemesis_plan_path: &Path) -> Result<usize> {
    let root_plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    let existing = if root_plan_path.exists() {
        fs::read_to_string(&root_plan_path)
            .with_context(|| format!("failed to read {}", root_plan_path.display()))?
    } else {
        EMPTY_PLAN.to_string()
    };
    let nemesis_plan = fs::read_to_string(nemesis_plan_path)
        .with_context(|| format!("failed to read {}", nemesis_plan_path.display()))?;

    let (merged, appended) = append_new_open_tasks(&existing, &nemesis_plan)?;
    atomic_write(&root_plan_path, merged.as_bytes())
        .with_context(|| format!("failed to write {}", root_plan_path.display()))?;
    Ok(appended)
}

fn commit_nemesis_outputs_if_needed(
    repo_root: &Path,
    branch: &str,
    output_dir: &Path,
    root_spec: &Path,
    root_plan: &Path,
) -> Result<Option<String>> {
    let pathspecs = nemesis_commit_pathspecs(repo_root, output_dir, root_spec, root_plan);
    if pathspecs.is_empty() {
        return Ok(None);
    }

    let mut status_args = vec!["status", "--short", "--"];
    status_args.extend(pathspecs.iter().map(String::as_str));
    let status = git_stdout(repo_root, status_args)?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    let mut snapshot_args = vec!["diff", "--cached", "--binary", "--"];
    snapshot_args.extend(pathspecs.iter().map(String::as_str));
    let staged_snapshot = git_stdout(repo_root, snapshot_args)?;
    let message = format!("{}: record nemesis outputs", repo_name(repo_root));
    let commit_result = (|| -> Result<()> {
        let mut add_args = vec!["add", "--all", "--"];
        add_args.extend(pathspecs.iter().map(String::as_str));
        run_git(repo_root, add_args)?;

        let mut commit_args = vec!["commit", "-m", &message, "--"];
        commit_args.extend(pathspecs.iter().map(String::as_str));
        run_git(repo_root, commit_args)?;
        Ok(())
    })();
    if let Err(error) = commit_result {
        restore_nemesis_commit_index(repo_root, &pathspecs, &staged_snapshot)
            .context("failed to restore index after Nemesis output commit error")?;
        return Err(error);
    }

    push_branch_with_remote_sync(repo_root, branch)?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    Ok(Some(commit.trim().to_string()))
}

fn nemesis_commit_pathspecs(
    repo_root: &Path,
    output_dir: &Path,
    root_spec: &Path,
    root_plan: &Path,
) -> Vec<String> {
    let mut pathspecs = Vec::<String>::new();
    push_unique_pathspec(&mut pathspecs, repo_relative_path(repo_root, output_dir));
    if let Some(relative_output_dir) = repo_relative_path(repo_root, output_dir) {
        pathspecs.push(format!(":(exclude){relative_output_dir}/codex.stderr.log"));
    }
    push_unique_pathspec(&mut pathspecs, repo_relative_path(repo_root, root_spec));
    push_unique_pathspec(&mut pathspecs, repo_relative_path(repo_root, root_plan));
    pathspecs
}

fn push_unique_pathspec(pathspecs: &mut Vec<String>, candidate: Option<String>) {
    let Some(candidate) = candidate else {
        return;
    };
    if !pathspecs.iter().any(|existing| existing == &candidate) {
        pathspecs.push(candidate);
    }
}

fn repo_relative_path(repo_root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(repo_root).ok()?;
    let display = relative.to_string_lossy().replace('\\', "/");
    if display.is_empty() {
        return None;
    }
    Some(display)
}

fn restore_nemesis_commit_index(
    repo_root: &Path,
    pathspecs: &[String],
    staged_snapshot: &str,
) -> Result<()> {
    let mut reset_args = vec!["reset", "--"];
    reset_args.extend(pathspecs.iter().map(String::as_str));
    run_git(repo_root, reset_args)?;
    if staged_snapshot.trim().is_empty() {
        return Ok(());
    }

    let patch_path = std::env::temp_dir().join(format!(
        "autodev-nemesis-index-{}-{}.patch",
        std::process::id(),
        timestamp_slug()
    ));
    fs::write(&patch_path, staged_snapshot)
        .with_context(|| format!("failed to write {}", patch_path.display()))?;
    let patch_path_text = patch_path.display().to_string();
    let apply_result = run_git(repo_root, ["apply", "--cached", &patch_path_text]);
    let cleanup_result = fs::remove_file(&patch_path);
    if let Err(error) = apply_result {
        cleanup_result.with_context(|| format!("failed to remove {}", patch_path.display()))?;
        return Err(error);
    }
    cleanup_result.with_context(|| format!("failed to remove {}", patch_path.display()))?;
    Ok(())
}

fn append_new_open_tasks(existing: &str, nemesis_plan: &str) -> Result<(String, usize)> {
    let normalized_existing = normalize_root_plan(existing);
    let existing_blocks = extract_plan_task_blocks(&normalized_existing)?;
    let existing_ids = existing_blocks
        .iter()
        .map(|block| block.task_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    let new_blocks = extract_plan_task_blocks(nemesis_plan)?
        .into_iter()
        .filter(|block| !block.checked)
        .filter(|block| !existing_ids.contains(block.task_id.as_str()))
        .collect::<Vec<_>>();

    if new_blocks.is_empty() {
        return Ok((normalized_existing, 0));
    }

    let mut merged = normalized_existing;
    append_blocks_to_section(&mut merged, PlanSection::Priority, &new_blocks)?;
    append_blocks_to_section(&mut merged, PlanSection::FollowOn, &new_blocks)?;
    Ok((merged, new_blocks.len()))
}

fn normalize_root_plan(markdown: &str) -> String {
    if markdown.trim().is_empty() {
        return EMPTY_PLAN.to_string();
    }

    let mut normalized = markdown.to_string();
    let mut changed = false;
    for section in REQUIRED_PLAN_SECTIONS {
        if markdown_has_line(&normalized, section) {
            continue;
        }
        if !normalized.ends_with('\n') {
            normalized.push('\n');
        }
        if !normalized.ends_with("\n\n") {
            normalized.push('\n');
        }
        normalized.push_str(section);
        normalized.push('\n');
        changed = true;
    }

    if changed && !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    normalized
}

fn markdown_has_line(markdown: &str, expected: &str) -> bool {
    markdown.lines().any(|line| line.trim() == expected)
}

fn append_blocks_to_section(
    markdown: &mut String,
    section: PlanSection,
    blocks: &[PlanTaskBlock],
) -> Result<()> {
    let section_header = match section {
        PlanSection::Priority => "## Priority Work",
        PlanSection::FollowOn => "## Follow-On Work",
        PlanSection::Completed => return Ok(()),
    };
    let section_blocks = blocks
        .iter()
        .filter(|block| block.section == section)
        .collect::<Vec<_>>();
    if section_blocks.is_empty() {
        return Ok(());
    }

    let insert_at = markdown
        .find(section_header)
        .with_context(|| format!("root plan is missing section `{section_header}`"))?;
    let section_end = markdown[insert_at + section_header.len()..]
        .find("\n## ")
        .map(|offset| insert_at + section_header.len() + offset)
        .unwrap_or(markdown.len());

    let mut addition = String::new();
    if !markdown[..section_end].ends_with('\n') {
        addition.push('\n');
    }
    if !markdown[..section_end].ends_with("\n\n") {
        addition.push('\n');
    }
    for block in section_blocks {
        addition.push_str(block.markdown.trim_end());
        addition.push_str("\n\n");
    }
    markdown.insert_str(section_end, &addition);
    Ok(())
}

fn extract_plan_task_blocks(markdown: &str) -> Result<Vec<PlanTaskBlock>> {
    let mut blocks = Vec::new();
    let mut current_section = None::<PlanSection>;
    let mut current_lines = Vec::<String>::new();

    for line in markdown.lines() {
        if let Some(section) = parse_section_header(line) {
            if let Some(block) = finalize_plan_block(current_section, &current_lines)? {
                blocks.push(block);
            }
            current_section = Some(section);
            current_lines.clear();
            continue;
        }

        if parse_plan_task_header(line).is_some() {
            if let Some(block) = finalize_plan_block(current_section, &current_lines)? {
                blocks.push(block);
            }
            current_lines = vec![line.to_string()];
            continue;
        }

        if !current_lines.is_empty() {
            current_lines.push(line.to_string());
        }
    }

    if let Some(block) = finalize_plan_block(current_section, &current_lines)? {
        blocks.push(block);
    }

    Ok(blocks)
}

fn finalize_plan_block(
    section: Option<PlanSection>,
    lines: &[String],
) -> Result<Option<PlanTaskBlock>> {
    if lines.is_empty() {
        return Ok(None);
    }
    let Some((checked, task_id, _title)) = parse_plan_task_header(&lines[0]) else {
        return Ok(None);
    };
    Ok(Some(PlanTaskBlock {
        section: section.unwrap_or(PlanSection::Priority),
        task_id,
        checked,
        markdown: lines.join("\n"),
    }))
}

fn parse_section_header(line: &str) -> Option<PlanSection> {
    match line.trim() {
        "## Priority Work" => Some(PlanSection::Priority),
        "## Follow-On Work" => Some(PlanSection::FollowOn),
        "## Completed / Already Satisfied" => Some(PlanSection::Completed),
        _ => None,
    }
}

fn parse_plan_task_header(line: &str) -> Option<(bool, String, String)> {
    let trimmed = line.trim_start();
    let checked = if trimmed.starts_with("- [ ] ") {
        false
    } else if trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ") {
        true
    } else {
        return None;
    };
    let rest = trimmed[6..].trim_start();
    let rest = rest.strip_prefix('`')?;
    let tick = rest.find('`')?;
    let task_id = rest[..tick].trim().to_string();
    let title = rest[tick + 1..].trim().to_string();
    Some((checked, task_id, title))
}

trait EmptyFallback {
    fn if_empty_then<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl EmptyFallback for str {
    fn if_empty_then<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.trim().is_empty() {
            fallback
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::{Local, TimeZone};

    use super::{
        annotate_output_recovery, append_new_open_tasks, build_implementation_prompt,
        build_nemesis_results_repair_prompt, commit_nemesis_outputs_if_needed,
        ensure_nemesis_finalizer_config, ensure_nemesis_fixer_config, ensure_nemesis_phase_config,
        load_nemesis_fix_results, maybe_prepare_output_dir, next_nemesis_spec_destination,
        prepare_output_dir, resolve_auditor_model, select_backend, unchecked_nemesis_task_ids,
        verify_nemesis_outputs, PhaseConfig, DEFAULT_NEMESIS_AUDIT_MODEL,
    };
    use crate::NemesisArgs;

    fn temp_repo_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "autodev-nemesis-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn run_git_in<'a>(repo: &Path, args: impl IntoIterator<Item = &'a str>) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to launch git");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git stdout should be utf-8")
    }

    fn init_repo(name: &str) -> PathBuf {
        let repo = temp_repo_path(name);
        fs::create_dir_all(&repo).expect("failed to create temp repo");
        run_git_in(&repo, ["init"]);
        run_git_in(&repo, ["config", "user.name", "autodev tests"]);
        run_git_in(&repo, ["config", "user.email", "autodev@example.com"]);
        fs::write(repo.join("README.md"), "# temp\n").expect("failed to write README");
        run_git_in(&repo, ["add", "README.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);
        repo
    }

    fn init_remote_and_worker(name: &str, branch: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = temp_repo_path(name);
        let remote = root.join("remote.git");
        let upstream = root.join("upstream");
        let worker = root.join("worker");

        fs::create_dir_all(&root).expect("failed to create temp root");
        run_git_in(
            &root,
            [
                "init",
                "--bare",
                remote.to_str().expect("remote path should be utf-8"),
            ],
        );
        run_git_in(
            &root,
            [
                "clone",
                remote.to_str().expect("remote path should be utf-8"),
                upstream.to_str().expect("upstream path should be utf-8"),
            ],
        );
        run_git_in(&upstream, ["config", "user.name", "autodev tests"]);
        run_git_in(&upstream, ["config", "user.email", "autodev@example.com"]);
        fs::write(upstream.join("README.md"), "# init\n").expect("failed to write README");
        run_git_in(&upstream, ["add", "README.md"]);
        run_git_in(&upstream, ["commit", "-m", "init"]);
        run_git_in(&upstream, ["branch", "-M", branch]);
        run_git_in(&upstream, ["push", "-u", "origin", branch]);

        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                branch,
                remote.to_str().expect("remote path should be utf-8"),
                worker.to_str().expect("worker path should be utf-8"),
            ],
        );
        run_git_in(&worker, ["config", "user.name", "autodev tests"]);
        run_git_in(&worker, ["config", "user.email", "autodev@example.com"]);
        (root, remote, upstream, worker)
    }

    #[test]
    fn appends_only_new_unchecked_nemesis_tasks() {
        let existing = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate query
Spec: specs/020426-query.md

## Follow-On Work

## Completed / Already Satisfied
"#;

        let nemesis = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `NEM-001` Harden cross-surface invariant
Spec: specs/020426-nemesis-audit.md

- [ ] `VAL-001` Validate query
Spec: specs/020426-query.md

## Follow-On Work

- [ ] `NEM-002` Add state-sync regression coverage
Spec: specs/020426-nemesis-audit.md

## Completed / Already Satisfied

- [x] `NEM-003` Already satisfied
Spec: specs/020426-nemesis-audit.md
"#;

        let (merged, appended) = append_new_open_tasks(existing, nemesis).unwrap();
        assert_eq!(appended, 2);
        assert!(merged.contains("`NEM-001`"));
        assert!(merged.contains("`NEM-002`"));
        assert_eq!(merged.matches("`VAL-001`").count(), 1);
        assert!(!merged.contains("`NEM-003`"));
    }

    #[test]
    fn appends_nemesis_tasks_when_existing_plan_is_missing_sections() {
        let existing = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate query
Spec: specs/020426-query.md
"#;

        let nemesis = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `NEM-001` Harden cross-surface invariant
Spec: specs/020426-nemesis-audit.md

## Follow-On Work

- [ ] `NEM-002` Add state-sync regression coverage
Spec: specs/020426-nemesis-audit.md

## Completed / Already Satisfied
"#;

        let (merged, appended) = append_new_open_tasks(existing, nemesis).unwrap();
        assert_eq!(appended, 2);
        assert!(merged.contains("## Follow-On Work"));
        assert!(merged.contains("## Completed / Already Satisfied"));
        assert!(merged.contains("`NEM-001`"));
        assert!(merged.contains("`NEM-002`"));
    }

    fn sample_args(model: &str) -> NemesisArgs {
        NemesisArgs {
            prompt_file: None,
            output_dir: None,
            resume: false,
            profile: crate::HardeningProfile::Balanced,
            model: model.to_string(),
            reasoning_effort: "high".to_string(),
            reviewer_model: "kimi".to_string(),
            reviewer_effort: "high".to_string(),
            kimi: false,
            minimax: false,
            report_only: false,
            branch: None,
            dry_run: true,
            fixer_model: "gpt-5.5".to_string(),
            fixer_effort: "high".to_string(),
            finalizer_model: "gpt-5.5".to_string(),
            finalizer_effort: "high".to_string(),
            audit_passes: 1,
            codex_bin: PathBuf::from("codex"),
            pi_bin: PathBuf::from("pi"),
            kimi_bin: PathBuf::from("kimi-cli"),
            use_kimi_cli: false,
        }
    }

    #[test]
    fn select_backend_treats_minimax_model_alias_as_pi() {
        let args = sample_args("minimax");
        let backend = select_backend(
            &args.model,
            &args.reasoning_effort,
            Path::new("codex"),
            Path::new("pi"),
            Path::new("kimi-cli"),
            false,
        );
        assert_eq!(backend.label(), "pi-minimax");
        assert_eq!(backend.model(), "minimax/MiniMax-M2.7-highspeed");
        assert_eq!(backend.variant(), "high");
    }

    #[test]
    fn select_backend_routes_kimi_through_kimi_cli_when_flag_is_on() {
        let args = sample_args("k2.6");
        let backend = select_backend(
            &args.model,
            &args.reasoning_effort,
            Path::new("codex"),
            Path::new("pi"),
            Path::new("kimi-cli"),
            true,
        );
        assert_eq!(backend.label(), "kimi-cli");
        // `k2.6` is the short id; it must be resolved to the provider-qualified
        // name kimi-cli actually reads from ~/.kimi/config.toml.
        assert_eq!(backend.model(), "kimi-code/kimi-for-coding");
        assert_eq!(backend.variant(), "high");
    }

    #[test]
    fn select_backend_treats_kimi_model_alias_as_pi_when_kimi_cli_off() {
        let args = sample_args("kimi");
        let backend = select_backend(
            &args.model,
            &args.reasoning_effort,
            Path::new("codex"),
            Path::new("pi"),
            Path::new("kimi-cli"),
            false,
        );
        assert_eq!(backend.label(), "pi-kimi");
        assert_eq!(backend.model(), "kimi-coding/k2p6");
        assert_eq!(backend.variant(), "high");
    }

    #[test]
    fn select_backend_normalizes_explicit_minimax_model_override() {
        let args = sample_args("minimax-m2.7-highspeed");
        let backend = select_backend(
            &args.model,
            &args.reasoning_effort,
            Path::new("codex"),
            Path::new("pi"),
            Path::new("kimi-cli"),
            false,
        );
        assert_eq!(backend.label(), "pi-minimax");
        assert_eq!(backend.model(), "minimax/MiniMax-M2.7-highspeed");
    }

    #[test]
    fn explicit_model_takes_precedence_over_minimax_flag() {
        let mut args = sample_args("kimi-coding/k2p5");
        args.minimax = true;
        assert_eq!(resolve_auditor_model(&args), "kimi-coding/k2p5");
    }

    #[test]
    fn explicit_model_takes_precedence_over_kimi_flag() {
        let mut args = sample_args("minimax/MiniMax-M2.7-highspeed");
        args.kimi = true;
        assert_eq!(
            resolve_auditor_model(&args),
            "minimax/MiniMax-M2.7-highspeed"
        );
    }

    #[test]
    fn minimax_flag_selects_minimax_when_model_is_default() {
        let mut args = sample_args(DEFAULT_NEMESIS_AUDIT_MODEL);
        args.minimax = true;
        assert_eq!(resolve_auditor_model(&args), "minimax");
    }

    #[test]
    fn kimi_flag_selects_k2p6_when_model_is_default() {
        let mut args = sample_args(DEFAULT_NEMESIS_AUDIT_MODEL);
        args.kimi = true;
        assert_eq!(resolve_auditor_model(&args), "k2.6");
    }

    #[test]
    fn no_flags_and_default_model_resolves_to_new_default() {
        let args = sample_args(DEFAULT_NEMESIS_AUDIT_MODEL);
        assert_eq!(resolve_auditor_model(&args), DEFAULT_NEMESIS_AUDIT_MODEL);
    }

    #[test]
    fn nemesis_phase_accepts_codex_default_models() {
        let config = PhaseConfig {
            model: "gpt-5.5".to_string(),
            effort: "high".to_string(),
        };
        assert!(ensure_nemesis_phase_config("nemesis", &config).is_ok());
    }

    #[test]
    fn nemesis_phase_rejects_empty_models() {
        let config = PhaseConfig {
            model: "   ".to_string(),
            effort: "high".to_string(),
        };
        assert!(ensure_nemesis_phase_config("nemesis", &config).is_err());
    }

    #[test]
    fn implementation_prompt_requires_commit_and_push_on_current_branch() {
        let prompt = build_implementation_prompt(
            Path::new("nemesis/nemesis-audit.md"),
            Path::new("nemesis/IMPLEMENTATION_PLAN.md"),
            Path::new("nemesis/implementation-results.json"),
            Path::new("nemesis/implementation-results.md"),
            "main",
        );

        assert!(prompt.contains("Commit only truthful fix increments"));
        assert!(prompt.contains("Push to `origin/main`"));
        assert!(
            prompt.contains("Do not edit root `specs/` or root `IMPLEMENTATION_PLAN.md` directly")
        );
    }

    #[test]
    fn nemesis_results_repair_prompt_is_file_scoped() {
        let prompt = build_nemesis_results_repair_prompt(
            Path::new("nemesis/nemesis-audit.md"),
            Path::new("nemesis/IMPLEMENTATION_PLAN.md"),
            Path::new("nemesis/implementation-results.json"),
            Path::new("nemesis/implementation-results.md"),
        );

        assert!(prompt.contains("Do not modify code, tests, git state"));
        assert!(prompt.contains("implementation-results.json"));
        assert!(prompt.contains("implementation-results.md"));
        assert!(prompt.contains("\"task_id\": \"NEM-001\""));
    }

    #[test]
    fn load_nemesis_fix_results_repairs_invalid_backslash_escapes() {
        let path = temp_repo_path("nemesis-invalid-escapes").join("implementation-results.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        fs::write(
            &path,
            r#"[
  {
    "task_id": "NEM-001",
    "status": "blocked",
    "summary": "The pattern \d+\_suffix still appears in the copied output.",
    "validation_commands": [],
    "touched_files": [],
    "residual_risks": ["Needs manual review"]
  }
]"#,
        )
        .expect("failed to write invalid json");

        let results = load_nemesis_fix_results(&path).expect("repair should recover JSON");
        assert_eq!(results.len(), 1);
        assert!(results[0].summary.contains("\\d+\\_suffix"));
    }

    #[test]
    fn load_nemesis_fix_results_repairs_trailing_backend_wrapper() {
        let path = temp_repo_path("nemesis-trailing-wrapper").join("implementation-results.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        fs::write(
            &path,
            r#"[
  {
    "task_id": "NEM-001",
    "status": "blocked",
    "summary": "The implementation stopped before editing code.",
    "validation_commands": [],
    "touched_files": [],
    "residual_risks": ["Needs a follow-up run"]
  }
]
</invoke>"#,
        )
        .expect("failed to write invalid json");

        let results = load_nemesis_fix_results(&path).expect("repair should recover JSON");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, "NEM-001");
    }

    #[test]
    fn load_nemesis_fix_results_allows_truthful_noop_fixed_results() {
        let path = temp_repo_path("nemesis-fixed-noop").join("implementation-results.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        fs::write(
            &path,
            r#"[
  {
    "task_id": "NEM-010",
    "status": "fixed",
    "summary": "No file changes were needed because the live repo already satisfied the requirement.",
    "validation_commands": ["rg -n 'alerts' docs/ops/alerts.md -S"],
    "touched_files": [],
    "residual_risks": []
  }
]"#,
        )
        .expect("failed to write noop json");

        let results = load_nemesis_fix_results(&path).expect("noop fixed result should load");
        assert_eq!(results.len(), 1);
        assert!(results[0].touched_files.is_empty());
    }

    #[test]
    fn load_nemesis_fix_results_rejects_fixed_results_without_files_or_noop_summary() {
        let path =
            temp_repo_path("nemesis-fixed-missing-files").join("implementation-results.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        fs::write(
            &path,
            r#"[
  {
    "task_id": "NEM-011",
    "status": "fixed",
    "summary": "Updated the validation surface.",
    "validation_commands": ["cargo test -p barely-human observatory"],
    "touched_files": [],
    "residual_risks": []
  }
]"#,
        )
        .expect("failed to write invalid noop json");

        let error = load_nemesis_fix_results(&path).expect_err("result should be rejected");
        assert!(error
            .to_string()
            .contains("must include touched files unless the summary explicitly states"));
    }

    #[test]
    fn nemesis_fixer_accepts_kimi_model_now_that_it_drives_remediation() {
        let config = PhaseConfig {
            model: "k2.6".to_string(),
            effort: "high".to_string(),
        };
        assert!(
            ensure_nemesis_fixer_config(&config).is_ok(),
            "explicit Kimi fixer opt-ins should continue to work"
        );
    }

    #[test]
    fn nemesis_fixer_rejects_empty_model() {
        let config = PhaseConfig {
            model: "   ".to_string(),
            effort: "high".to_string(),
        };
        assert!(ensure_nemesis_fixer_config(&config).is_err());
    }

    #[test]
    fn nemesis_finalizer_rejects_kimi_model() {
        let config = PhaseConfig {
            model: "k2.6".to_string(),
            effort: "high".to_string(),
        };
        let error = ensure_nemesis_finalizer_config(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("must use a Codex model"));
    }

    #[test]
    fn prepare_output_dir_failure_points_to_archived_snapshot() {
        let repo = init_repo("output-dir-recovery");
        let output_dir = repo.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        fs::write(output_dir.join("nemesis-audit.md"), "# old\n")
            .expect("failed to seed old output");

        let archived = prepare_output_dir(&repo, &output_dir).expect("prepare should archive");
        let annotated = annotate_output_recovery(
            anyhow::anyhow!("simulated model failure"),
            &output_dir,
            archived.as_deref(),
            "Nemesis audit pass failed",
        );
        let message = format!("{annotated:#}");
        assert!(message.contains("simulated model failure"));
        assert!(message.contains("Previous outputs were archived at"));
        assert!(message.contains(
            archived
                .as_ref()
                .expect("snapshot should exist")
                .display()
                .to_string()
                .as_str()
        ));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn dry_run_output_dir_prep_is_non_destructive() {
        let repo = init_repo("dry-run-output-dir");
        let output_dir = repo.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        let original = output_dir.join("nemesis-audit.md");
        fs::write(&original, "# keep me\n").expect("failed to seed old output");

        let archived = maybe_prepare_output_dir(&repo, &output_dir, true, false)
            .expect("dry-run should succeed");
        assert!(archived.is_none());
        assert!(
            original.exists(),
            "dry-run should not delete existing outputs"
        );
        assert!(
            !repo.join(".auto").join("fresh-input").exists(),
            "dry-run should not archive output snapshots"
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn verify_nemesis_outputs_reports_partial_state() {
        let repo = init_repo("partial-nemesis-output");
        let output_dir = repo.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        fs::write(
            output_dir.join("nemesis-audit.md"),
            "# Specification: partial\n",
        )
        .expect("failed to write partial spec");

        let error = verify_nemesis_outputs(&output_dir)
            .expect_err("partial output should fail verification")
            .to_string();
        assert!(error.contains("only partially completed"));
        assert!(error.contains("IMPLEMENTATION_PLAN.md"));
        assert!(error.contains("rerun"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn unchecked_task_preflight_skips_satisfied_plans() {
        let unchecked = unchecked_nemesis_task_ids(
            r#"# IMPLEMENTATION_PLAN

## Priority Work

## Follow-On Work

## Completed / Already Satisfied

- [x] `NEM-001` Already done
Spec: nemesis/nemesis-audit.md
"#,
        )
        .expect("plan should parse");
        assert!(unchecked.is_empty());
    }

    #[test]
    fn spec_sync_destination_uses_time_and_collision_suffix() {
        let root = temp_repo_path("spec-destination");
        let specs_dir = root.join("specs");
        fs::create_dir_all(&specs_dir).expect("failed to create specs dir");
        let spec_path = root.join("nemesis-audit.md");
        fs::write(&spec_path, "# Specification:\n").expect("failed to write spec");
        let timestamp = Local
            .with_ymd_and_hms(2026, 4, 5, 12, 34, 56)
            .single()
            .expect("timestamp should exist");

        let first = next_nemesis_spec_destination(&specs_dir, &spec_path, timestamp);
        assert!(first
            .file_name()
            .and_then(|value| value.to_str())
            .expect("file name should be utf-8")
            .starts_with("050426-123456-nemesis-audit"));
        fs::write(&first, "# existing\n").expect("failed to create existing collision file");
        let second = next_nemesis_spec_destination(&specs_dir, &spec_path, timestamp);
        assert_ne!(first, second);
        assert!(second
            .file_name()
            .and_then(|value| value.to_str())
            .expect("file name should be utf-8")
            .contains("-2."));

        fs::remove_dir_all(&root).expect("failed to remove temp dir");
    }

    #[test]
    fn output_commit_ignores_preexisting_staged_changes() {
        let (root, _remote, _upstream, worker) = init_remote_and_worker("commit-isolation", "main");
        let output_dir = worker.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        fs::create_dir_all(worker.join("specs")).expect("failed to create specs dir");
        fs::create_dir_all(worker.join("src")).expect("failed to create src dir");
        fs::write(output_dir.join("nemesis-audit.md"), "# Specification:\n")
            .expect("failed to write audit");
        fs::write(
            output_dir.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n",
        )
        .expect("failed to write nemesis plan");
        fs::write(worker.join("specs").join("nemesis.md"), "# spec\n")
            .expect("failed to write root spec");
        fs::write(
            worker.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
        )
        .expect("failed to write root plan");
        fs::write(worker.join("src").join("lib.rs"), "pub fn untouched() {}\n")
            .expect("failed to write unrelated file");
        run_git_in(&worker, ["add", "src/lib.rs"]);

        let commit = commit_nemesis_outputs_if_needed(
            &worker,
            "main",
            &output_dir,
            &worker.join("specs").join("nemesis.md"),
            &worker.join("IMPLEMENTATION_PLAN.md"),
        )
        .expect("output commit should succeed")
        .expect("output commit should produce a commit");
        assert!(!commit.is_empty());

        let committed = run_git_in(&worker, ["show", "--name-only", "--format=", "HEAD"]);
        assert!(committed.contains("nemesis/nemesis-audit.md"));
        assert!(committed.contains("nemesis/IMPLEMENTATION_PLAN.md"));
        assert!(committed.contains("specs/nemesis.md"));
        assert!(committed.contains("IMPLEMENTATION_PLAN.md"));
        assert!(!committed.contains("src/lib.rs"));

        let staged = run_git_in(&worker, ["diff", "--cached", "--name-only"]);
        assert_eq!(staged, "src/lib.rs\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repos");
    }

    #[test]
    fn output_commit_restores_index_after_commit_failure() {
        let repo = init_repo("commit-rollback");
        let output_dir = repo.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        fs::create_dir_all(repo.join("specs")).expect("failed to create specs dir");
        fs::create_dir_all(repo.join("src")).expect("failed to create src dir");
        fs::write(output_dir.join("nemesis-audit.md"), "# Specification:\n")
            .expect("failed to write audit");
        fs::write(
            output_dir.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n",
        )
        .expect("failed to write nemesis plan");
        fs::write(repo.join("specs").join("nemesis.md"), "# spec\n")
            .expect("failed to write root spec");
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
        )
        .expect("failed to write root plan");
        fs::write(repo.join("src").join("lib.rs"), "pub fn staged() {}\n")
            .expect("failed to write unrelated file");
        run_git_in(&repo, ["add", "src/lib.rs"]);
        run_git_in(&repo, ["config", "user.useConfigOnly", "true"]);
        run_git_in(&repo, ["config", "--unset", "user.name"]);
        run_git_in(&repo, ["config", "--unset", "user.email"]);
        let branch = run_git_in(&repo, ["branch", "--show-current"]);

        let error = commit_nemesis_outputs_if_needed(
            &repo,
            branch.trim(),
            &output_dir,
            &repo.join("specs").join("nemesis.md"),
            &repo.join("IMPLEMENTATION_PLAN.md"),
        )
        .expect_err("commit should fail without user identity")
        .to_string();
        assert!(error.contains("git command failed"));

        let staged = run_git_in(&repo, ["diff", "--cached", "--name-only"]);
        assert_eq!(staged, "src/lib.rs\n");
        let status = run_git_in(&repo, ["status", "--short"]);
        assert!(status.contains("A  src/lib.rs"));
        assert!(output_dir.join("IMPLEMENTATION_PLAN.md").exists());
        assert!(output_dir.join("nemesis-audit.md").exists());
        assert!(repo.join("specs").join("nemesis.md").exists());
        assert!(repo.join("IMPLEMENTATION_PLAN.md").exists());

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }
}
