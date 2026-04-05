use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use chrono::Local;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::codex_exec::run_codex_exec;
use crate::codex_stream::{capture_codex_output, capture_pi_output};
use crate::pi_backend::{parse_pi_error, resolve_pi_bin, PiProvider};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, copy_tree, ensure_repo_layout, git_repo_root,
    git_stdout, opencode_agent_dir, prune_pi_runtime_state, repo_name, run_git, timestamp_slug,
};
use crate::NemesisArgs;

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
- `deferred` means the task remains valid but was intentionally left open with a truthful reason.
- `blocked` means an external dependency, ambiguity, or repo limitation prevented a truthful close.
- `{results_md}` should summarize proof-before-fix, root cause, changes made, validation, and any deferred or blocked tasks.
- JSON string values must stay valid JSON. Escape inner double quotes or rewrite them with single quotes/backticks.
"#;

const DEFAULT_CODEX_NEMESIS_MODEL: &str = "gpt-5.4";
const EMPTY_PLAN: &str = "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n";
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
}

impl NemesisBackend {
    fn label(&self) -> &'static str {
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

    fn variant(&self) -> &str {
        match self {
            Self::Codex {
                reasoning_effort, ..
            } => reasoning_effort,
            Self::Pi { thinking, .. } => thinking,
        }
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
    let auditor = PhaseConfig {
        model: if args.kimi {
            "kimi".to_string()
        } else if args.minimax {
            "minimax".to_string()
        } else {
            args.model.clone()
        },
        effort: args.reasoning_effort.clone(),
    };
    let reviewer = PhaseConfig {
        model: args.reviewer_model.clone(),
        effort: args.reviewer_effort.clone(),
    };
    let fixer = PhaseConfig {
        model: args.fixer_model.clone(),
        effort: args.fixer_effort.clone(),
    };
    ensure_pi_phase_config("auto nemesis audit pass", &auditor)?;
    ensure_pi_phase_config("auto nemesis synthesis pass", &reviewer)?;
    ensure_nemesis_fixer_config(&fixer)?;
    let audit_backend = select_backend(
        &auditor.model,
        &auditor.effort,
        &args.codex_bin,
        &args.pi_bin,
    );
    let review_backend = select_backend(
        &reviewer.model,
        &reviewer.effort,
        &args.codex_bin,
        &args.pi_bin,
    );
    let previous_snapshot = if args.dry_run {
        None
    } else {
        prepare_output_dir(&repo_root, &output_dir)?
    };

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
    if args.dry_run {
        println!("mode:        dry-run");
        return Ok(());
    }
    if !args.report_only {
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, current_branch.as_str(), "nemesis checkpoint")?
        {
            println!("checkpoint:  committed pre-existing changes at {commit}");
        }
    } else {
        println!("mode:        report-only");
    }

    print_phase_header("auditor", &audit_backend);
    let audit_response = run_nemesis_backend(&repo_root, &audit_prompt, &audit_backend).await?;
    let audit_response_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-audit-response.log", timestamp_slug()));
    if !audit_response.trim().is_empty() {
        atomic_write(&audit_response_path, audit_response.as_bytes())
            .with_context(|| format!("failed to write {}", audit_response_path.display()))?;
    }

    print_phase_header("reviewer", &review_backend);
    let review_response = run_nemesis_backend(&repo_root, &review_prompt, &review_backend).await?;
    let review_response_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-review-response.log", timestamp_slug()));
    if !review_response.trim().is_empty() {
        atomic_write(&review_response_path, review_response.as_bytes())
            .with_context(|| format!("failed to write {}", review_response_path.display()))?;
    }

    let spec_path = verify_nemesis_spec(&output_dir)?;
    let plan_path = verify_nemesis_plan(&output_dir)?;
    let mut implementation_results = None::<PathBuf>;
    if !args.report_only {
        let commit_before = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        println!();
        println!("phase:       implementer");
        println!("backend:     codex");
        println!("model:       {}", fixer.model);
        println!("variant:     {}", fixer.effort);

        let exit_status = run_codex_exec(
            &repo_root,
            &implementation_prompt,
            &fixer.model,
            &fixer.effort,
            &args.codex_bin,
            &output_dir.join("codex.stderr.log"),
            "auto nemesis implementation",
        )
        .await?;
        if !exit_status.success() {
            bail!(
                "Codex Nemesis implementation failed with status {}; see {}",
                exit_status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                output_dir.join("codex.stderr.log").display()
            );
        }

        let implementation_path = verify_nemesis_implementation_results(
            &implementation_results_json_path,
            &implementation_results_md_path,
            &plan_path,
        )?;
        implementation_results = Some(implementation_path);
        let commit_after = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        if commit_before.trim() != commit_after.trim() {
            run_git(&repo_root, ["push", "origin", current_branch.as_str()])?;
        }
    }
    let root_spec = sync_nemesis_spec_to_root(&repo_root, &spec_path)?;
    let appended = append_nemesis_plan_to_root(&repo_root, &plan_path)?;
    let trailing_commit = if args.report_only {
        None
    } else {
        commit_nemesis_outputs_if_needed(&repo_root, current_branch.as_str())?
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
        println!("implementation: report-only");
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

fn select_backend(model: &str, effort: &str, codex_bin: &Path, pi_bin: &Path) -> NemesisBackend {
    let pi_provider = PiProvider::detect(model);
    if let Some(provider) = pi_provider {
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

fn ensure_pi_phase_config(label: &str, config: &PhaseConfig) -> Result<()> {
    if PiProvider::detect(&config.model).is_none() {
        bail!(
            "{label} must use a MiniMax or Kimi PI model; got `{}`",
            config.model
        );
    }
    Ok(())
}

fn ensure_nemesis_fixer_config(config: &PhaseConfig) -> Result<()> {
    if PiProvider::detect(&config.model).is_some() {
        bail!(
            "auto nemesis implementation pass must use a Codex model; got `{}`",
            config.model
        );
    }
    Ok(())
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
        } => run_pi(repo_root, prompt, model, thinking, pi_bin).await,
    }
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
    prune_pi_runtime_state(repo_root)?;
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

fn verify_nemesis_spec(output_dir: &Path) -> Result<PathBuf> {
    let spec_path = output_dir.join("nemesis-audit.md");
    if !spec_path.exists() {
        bail!("Nemesis run did not write {}", spec_path.display());
    }
    let markdown = fs::read_to_string(&spec_path)
        .with_context(|| format!("failed to read {}", spec_path.display()))?;
    if !markdown.starts_with("# Specification:") {
        bail!(
            "Nemesis spec {} must start with `# Specification:`",
            spec_path.display()
        );
    }
    Ok(spec_path)
}

fn verify_nemesis_plan(output_dir: &Path) -> Result<PathBuf> {
    let plan_path = output_dir.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        bail!("Nemesis run did not write {}", plan_path.display());
    }
    let markdown = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    for required in [
        "# IMPLEMENTATION_PLAN",
        "## Priority Work",
        "## Follow-On Work",
        "## Completed / Already Satisfied",
    ] {
        if !markdown.contains(required) {
            bail!("Nemesis implementation plan is missing `{required}`");
        }
    }
    Ok(plan_path)
}

fn verify_nemesis_implementation_results(
    results_json_path: &Path,
    results_md_path: &Path,
    plan_path: &Path,
) -> Result<PathBuf> {
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
    let expected_ids = extract_plan_task_blocks(
        &fs::read_to_string(plan_path)
            .with_context(|| format!("failed to read {}", plan_path.display()))?,
    )?
    .into_iter()
    .filter(|block| !block.checked)
    .map(|block| block.task_id)
    .collect::<std::collections::BTreeSet<_>>();
    let actual_ids = results
        .iter()
        .map(|result| result.task_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for task_id in &expected_ids {
        if !actual_ids.contains(task_id.as_str()) {
            bail!("Nemesis implementation results missing task `{task_id}`");
        }
    }
    Ok(results_json_path.to_path_buf())
}

fn load_nemesis_fix_results(path: &Path) -> Result<Vec<NemesisFixResult>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let results: Vec<NemesisFixResult> = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    for result in &results {
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
            if result.touched_files.is_empty() {
                bail!(
                    "Nemesis implementation result for `{}` must include touched files",
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
    Ok(results)
}

fn sync_nemesis_spec_to_root(repo_root: &Path, spec_path: &Path) -> Result<PathBuf> {
    let root_specs_dir = repo_root.join("specs");
    fs::create_dir_all(&root_specs_dir)
        .with_context(|| format!("failed to create {}", root_specs_dir.display()))?;

    let date_prefix = Local::now().format("%d%m%y").to_string();
    let slug = spec_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("nemesis-audit");
    let extension = spec_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("md");

    let mut counter = 1usize;
    let destination = loop {
        let candidate = if counter == 1 {
            root_specs_dir.join(format!("{date_prefix}-{slug}.{extension}"))
        } else {
            root_specs_dir.join(format!("{date_prefix}-{slug}-{counter}.{extension}"))
        };
        if !candidate.exists() {
            break candidate;
        }
        counter += 1;
    };

    fs::copy(spec_path, &destination).with_context(|| {
        format!(
            "failed to copy {} -> {}",
            spec_path.display(),
            destination.display()
        )
    })?;
    Ok(destination)
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

fn commit_nemesis_outputs_if_needed(repo_root: &Path, branch: &str) -> Result<Option<String>> {
    let status = git_stdout(
        repo_root,
        [
            "status",
            "--short",
            "--",
            ".",
            ":(exclude).auto",
            ":(exclude)bug",
            ":(exclude)gen-*",
        ],
    )?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    run_git(
        repo_root,
        [
            "add",
            "-u",
            "--",
            ".",
            ":(exclude).auto",
            ":(exclude)bug",
            ":(exclude)gen-*",
        ],
    )?;
    let untracked = git_stdout(
        repo_root,
        ["ls-files", "-z", "--others", "--exclude-standard"],
    )?;
    let stageable = untracked
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| {
            !path.starts_with(".auto/")
                && !path.starts_with("bug/")
                && !path.starts_with("nemesis/codex.stderr.log")
                && !path
                    .split('/')
                    .next()
                    .map(|segment| segment.starts_with("gen-"))
                    .unwrap_or(false)
        })
        .map(|path| path.to_string())
        .collect::<Vec<_>>();
    for chunk in stageable.chunks(100) {
        let mut add_args = vec!["add".to_string(), "--".to_string()];
        add_args.extend(chunk.iter().cloned());
        run_git(repo_root, add_args.iter().map(|arg| arg.as_str()))?;
    }

    let message = format!("{}: record nemesis outputs", repo_name(repo_root));
    run_git(repo_root, ["commit", "-m", &message])?;
    run_git(repo_root, ["push", "origin", branch])?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    Ok(Some(commit.trim().to_string()))
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
    use std::path::{Path, PathBuf};

    use super::{
        append_new_open_tasks, build_implementation_prompt, ensure_nemesis_fixer_config,
        ensure_pi_phase_config, select_backend, PhaseConfig,
    };
    use crate::NemesisArgs;

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
            model: model.to_string(),
            reasoning_effort: "high".to_string(),
            reviewer_model: "kimi".to_string(),
            reviewer_effort: "high".to_string(),
            kimi: false,
            minimax: false,
            report_only: false,
            branch: None,
            dry_run: true,
            fixer_model: "gpt-5.4".to_string(),
            fixer_effort: "xhigh".to_string(),
            codex_bin: PathBuf::from("codex"),
            pi_bin: PathBuf::from("pi"),
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
        );
        assert_eq!(backend.label(), "pi-minimax");
        assert_eq!(backend.model(), "minimax/MiniMax-M2.7-highspeed");
        assert_eq!(backend.variant(), "high");
    }

    #[test]
    fn select_backend_treats_kimi_model_alias_as_pi() {
        let args = sample_args("kimi");
        let backend = select_backend(
            &args.model,
            &args.reasoning_effort,
            Path::new("codex"),
            Path::new("pi"),
        );
        assert_eq!(backend.label(), "pi-kimi");
        assert_eq!(backend.model(), "kimi-coding/k2p5");
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
        );
        assert_eq!(backend.label(), "pi-minimax");
        assert_eq!(backend.model(), "minimax/MiniMax-M2.7-highspeed");
    }

    #[test]
    fn nemesis_phase_rejects_non_pi_models() {
        let config = PhaseConfig {
            model: "gpt-5.4".to_string(),
            effort: "xhigh".to_string(),
        };
        assert!(ensure_pi_phase_config("nemesis", &config).is_err());
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
    fn nemesis_fixer_must_not_use_pi_model() {
        let config = PhaseConfig {
            model: "kimi".to_string(),
            effort: "high".to_string(),
        };
        let error = ensure_nemesis_fixer_config(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("must use a Codex model"));
    }
}
