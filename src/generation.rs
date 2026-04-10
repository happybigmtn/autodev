use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::{Local, NaiveDate};

use crate::codex_exec::run_codex_exec;
use crate::corpus::{emit_corpus_snapshot, load_planning_corpus, PlanningCorpus};
use crate::state::{load_state, save_state};
use crate::util::{
    atomic_write, binary_provenance_line, copy_tree, ensure_repo_layout, git_repo_root,
    list_markdown_files, timestamp_slug,
};
use crate::{CorpusArgs, GenerationArgs};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenerationMode {
    Gen,
    Reverse,
}

impl GenerationMode {
    fn command_label(self) -> &'static str {
        match self {
            Self::Gen => "auto gen",
            Self::Reverse => "auto reverse",
        }
    }

    fn spec_phase_slug(self) -> &'static str {
        match self {
            Self::Gen => "gen-specs",
            Self::Reverse => "reverse-specs",
        }
    }

    fn plan_phase_slug(self) -> &'static str {
        match self {
            Self::Gen => "gen-plan",
            Self::Reverse => "reverse-plan",
        }
    }

    fn codex_review_phase_slug(self) -> &'static str {
        match self {
            Self::Gen => "gen-codex-review",
            Self::Reverse => "reverse-codex-review",
        }
    }
}

#[derive(Default)]
struct SpecSyncSummary {
    appended_paths: Vec<PathBuf>,
    skipped_count: usize,
}

struct GeneratedSpecDocument {
    path: PathBuf,
    text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlanSection {
    Priority,
    FollowOn,
    Completed,
}

struct PlanTaskBlock {
    section: PlanSection,
    task_id: String,
    checked: bool,
    markdown: String,
}

const IMPLEMENTATION_PLAN_HEADER: &str = "# IMPLEMENTATION_PLAN";
const SPEC_OBJECTIVE_HEADER: &str = "## Objective";
const SPEC_ACCEPTANCE_CRITERIA_HEADER: &str = "## Acceptance Criteria";
const SPEC_VERIFICATION_HEADER: &str = "## Verification";
const REQUIRED_PLAN_SECTIONS: [&str; 3] = [
    "## Priority Work",
    "## Follow-On Work",
    "## Completed / Already Satisfied",
];
const REQUIRED_PLAN_TASK_FIELDS: [&str; 12] = [
    "Spec:",
    "Why now:",
    "Codebase evidence:",
    "Owns:",
    "Integration touchpoints:",
    "Scope boundary:",
    "Acceptance criteria:",
    "Verification:",
    "Required tests:",
    "Dependencies:",
    "Estimated scope:",
    "Completion signal:",
];
const CORPUS_EXECPLAN_REQUIRED_SECTIONS: [&str; 15] = [
    "## Purpose / Big Picture",
    "## Requirements Trace",
    "## Scope Boundaries",
    "## Progress",
    "## Surprises & Discoveries",
    "## Decision Log",
    "## Outcomes & Retrospective",
    "## Context and Orientation",
    "## Plan of Work",
    "## Implementation Units",
    "## Concrete Steps",
    "## Validation and Acceptance",
    "## Idempotence and Recovery",
    "## Artifacts and Notes",
    "## Interfaces and Dependencies",
];
const CODEX_SKILL_BOUNDARY: &str = "IMPORTANT: Do NOT read or execute any SKILL.md files or files in skill definition directories (paths containing skills/gstack). These are AI assistant skill definitions meant for a different system. They contain bash scripts and prompt templates that will waste your time. Ignore them completely. Stay focused on the repository code only.";

pub(crate) async fn run_corpus(args: CorpusArgs) -> Result<()> {
    let run_started_at = Instant::now();
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let reference_repos = resolve_reference_repos(&repo_root, &args.reference_repos)?;
    let planning_root = args
        .planning_root
        .unwrap_or_else(|| repo_root.join("genesis"));
    print_command_header(
        "auto corpus",
        &repo_root,
        Some(&planning_root),
        run_started_at,
    );
    ensure_planning_root_ready_for_corpus(&planning_root)?;
    print_stage("prepare planning root", run_started_at);
    let previous_snapshot = if args.dry_run {
        None
    } else {
        prepare_planning_root_for_corpus(&repo_root, &planning_root)?
    };

    if let Some(idea) = args.idea.as_deref() {
        println!("idea:        {}", idea);
    }
    if let Some(focus) = args.focus.as_deref() {
        println!("focus:       {}", focus);
    }
    if !reference_repos.is_empty() {
        println!("references:  {}", reference_repos.len());
        for path in &reference_repos {
            println!("  - {}", path.display());
        }
    }
    println!("model:       {}", args.model);
    println!(
        "codex review:{}",
        if args.skip_codex_review {
            " skipped".to_string()
        } else {
            format!(
                " {} ({})",
                args.codex_review_model, args.codex_review_effort
            )
        }
    );
    println!("max turns:   {}", args.max_turns);
    println!("parallelism: {}", args.parallelism.clamp(1, 10));
    if args.dry_run {
        println!("mode:        dry-run");
        return Ok(());
    }

    print_stage("create corpus skeleton", run_started_at);
    fs::create_dir_all(planning_root.join("plans")).with_context(|| {
        format!(
            "failed to create corpus plan directory {}",
            planning_root.join("plans").display()
        )
    })?;

    let prompt = build_corpus_prompt(
        &repo_root,
        &planning_root,
        previous_snapshot.as_deref(),
        args.parallelism.clamp(1, 10),
        args.idea.as_deref(),
        args.focus.as_deref(),
        &reference_repos,
    );
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("corpus-{}-prompt.md", timestamp_slug()));
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("prompt log:  {}", prompt_path.display());

    print_stage("run corpus model", run_started_at);
    let response = run_claude_prompt(
        &repo_root,
        &prompt,
        &args.model,
        args.max_turns,
        "corpus generation",
        &prompt_path,
    )?;
    let response_path = prompt_path.with_file_name(
        prompt_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("corpus-response.txt")
            .replace("-prompt.md", "-response.txt"),
    );
    if !response.trim().is_empty() {
        atomic_write(&response_path, response.as_bytes())
            .with_context(|| format!("failed to write {}", response_path.display()))?;
    }

    let codex_review = if args.skip_codex_review {
        None
    } else {
        print_stage("run corpus codex review", run_started_at);
        let report_path = codex_review_report_path(&repo_root, "corpus-codex-review");
        let review_prompt =
            build_corpus_codex_review_prompt(&repo_root, &planning_root, &report_path);
        Some(
            run_logged_codex_review(
                &repo_root,
                "corpus-codex-review",
                &review_prompt,
                &args.codex_review_model,
                &args.codex_review_effort,
                &args.codex_bin,
                &report_path,
            )
            .await?,
        )
    };

    print_stage("verify corpus outputs", run_started_at);
    let summary = verify_corpus_outputs(&planning_root, args.focus.is_some())?;
    print_stage("save corpus state", run_started_at);
    let mut state = load_state(&repo_root)?;
    state.planning_root = Some(planning_root.clone());
    save_state(&repo_root, &state)?;

    println!();
    println!("corpus complete");
    println!("assessment:  {}", summary.assessment_path.display());
    println!("spec:        {}", summary.spec_path.display());
    println!("plans index: {}", summary.plans_index_path.display());
    println!("report:      {}", summary.report_path.display());
    if let Some(design) = summary.design_path {
        println!("design:      {}", design.display());
    }
    if let Some(focus) = summary.focus_path {
        println!("focus brief: {}", focus.display());
    }
    if let Some(idea) = summary.idea_path {
        println!("idea brief:  {}", idea.display());
    }
    if let Some(previous) = previous_snapshot {
        println!("prior input: {}", previous.display());
    }
    println!("plan files:  {}", summary.plan_count);
    println!("prompt log:  {}", prompt_path.display());
    if response_path.exists() {
        println!("model log:   {}", response_path.display());
    }
    if let Some(review) = codex_review {
        println!("codex prompt: {}", review.prompt_path.display());
        println!("codex stderr: {}", review.stderr_log_path.display());
        println!("codex report: {}", review.report_path.display());
    }
    println!("elapsed:     {}", format_duration(run_started_at.elapsed()));
    Ok(())
}

pub(crate) async fn run_gen(args: GenerationArgs) -> Result<()> {
    run_generation(args, GenerationMode::Gen).await
}

pub(crate) async fn run_reverse(args: GenerationArgs) -> Result<()> {
    run_generation(args, GenerationMode::Reverse).await
}

async fn run_generation(args: GenerationArgs, mode: GenerationMode) -> Result<()> {
    let run_started_at = Instant::now();
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let mut state = load_state(&repo_root)?;
    let planning_root = args
        .planning_root
        .clone()
        .or_else(|| state.planning_root.clone())
        .unwrap_or_else(|| repo_root.join("genesis"));
    ensure_planning_root_exists(&planning_root)?;

    let output_dir = if args.plan_only {
        args.output_dir
            .clone()
            .or_else(|| state.latest_output_dir.clone())
            .unwrap_or_else(|| repo_root.join(format!("gen-{}", timestamp_slug())))
    } else {
        args.output_dir
            .clone()
            .unwrap_or_else(|| repo_root.join(format!("gen-{}", timestamp_slug())))
    };

    print_command_header(
        mode.command_label(),
        &repo_root,
        Some(&planning_root),
        run_started_at,
    );
    println!("output dir:  {}", output_dir.display());
    println!("model:       {}", args.model);
    println!(
        "codex review:{}",
        if args.skip_codex_review {
            " skipped".to_string()
        } else {
            format!(
                " {} ({})",
                args.codex_review_model, args.codex_review_effort
            )
        }
    );
    println!("max turns:   {}", args.max_turns);
    println!("parallelism: {}", args.parallelism.clamp(1, 10));
    println!("plan only:   {}", if args.plan_only { "yes" } else { "no" });

    if args.plan_only {
        if !output_dir.exists() {
            bail!(
                "`{} --plan-only` requires an existing output dir, but {} does not exist",
                mode.command_label(),
                output_dir.display()
            );
        }
    } else {
        print_stage("prepare output dir", run_started_at);
        prepare_generation_output_dir(&output_dir)?;
    }

    print_stage("load planning corpus", run_started_at);
    let corpus = load_planning_corpus(&planning_root).with_context(|| {
        format!(
            "failed to load planning corpus from {}",
            planning_root.display()
        )
    })?;
    print_stage("snapshot corpus into output dir", run_started_at);
    emit_corpus_snapshot(&corpus, &output_dir).with_context(|| {
        format!(
            "failed to copy planning corpus into {}",
            output_dir.join("corpus").display()
        )
    })?;

    let mut generated_specs = if args.plan_only {
        print_stage("reuse existing generated specs", run_started_at);
        verify_generated_specs(&output_dir)?
    } else {
        print_stage("generate specs", run_started_at);
        let prompt = build_spec_generation_prompt(
            mode,
            &repo_root,
            &planning_root,
            &output_dir,
            &corpus,
            args.parallelism.clamp(1, 10),
        );
        let phase = run_logged_claude_phase(
            &repo_root,
            mode.spec_phase_slug(),
            &prompt,
            &args.model,
            args.max_turns,
        )?;
        let specs = verify_generated_specs(&output_dir)?;
        println!("spec prompt: {}", phase.prompt_path.display());
        if let Some(response_path) = phase.response_path {
            println!("spec log:    {}", response_path.display());
        }
        specs
    };

    let (mut implementation_plan, plan_phase) = if args.plan_only {
        if output_dir.join("IMPLEMENTATION_PLAN.md").exists() {
            print_stage("reuse existing generated plan", run_started_at);
            (verify_generated_implementation_plan(&output_dir)?, None)
        } else {
            print_stage("generate implementation plan", run_started_at);
            let plan_prompt = build_implementation_plan_prompt(
                mode,
                &repo_root,
                &output_dir,
                &generated_specs,
                args.parallelism.clamp(1, 10),
            );
            let plan_phase = run_logged_claude_phase(
                &repo_root,
                mode.plan_phase_slug(),
                &plan_prompt,
                &args.model,
                args.max_turns,
            )?;
            (
                verify_generated_implementation_plan(&output_dir)?,
                Some(plan_phase),
            )
        }
    } else {
        print_stage("generate implementation plan", run_started_at);
        let plan_prompt = build_implementation_plan_prompt(
            mode,
            &repo_root,
            &output_dir,
            &generated_specs,
            args.parallelism.clamp(1, 10),
        );
        let plan_phase = run_logged_claude_phase(
            &repo_root,
            mode.plan_phase_slug(),
            &plan_prompt,
            &args.model,
            args.max_turns,
        )?;
        (
            verify_generated_implementation_plan(&output_dir)?,
            Some(plan_phase),
        )
    };
    let codex_review = if args.skip_codex_review {
        None
    } else {
        print_stage("run generation codex review", run_started_at);
        let report_path = codex_review_report_path(&repo_root, mode.codex_review_phase_slug());
        let review_prompt = build_generation_codex_review_prompt(
            mode,
            &repo_root,
            &planning_root,
            &output_dir,
            &report_path,
        );
        let review = run_logged_codex_review(
            &repo_root,
            mode.codex_review_phase_slug(),
            &review_prompt,
            &args.codex_review_model,
            &args.codex_review_effort,
            &args.codex_bin,
            &report_path,
        )
        .await?;
        generated_specs = verify_generated_specs(&output_dir)?;
        implementation_plan = verify_generated_implementation_plan(&output_dir)?;
        Some(review)
    };
    print_stage("sync generated specs to root", run_started_at);
    let root_specs = sync_generated_specs_to_root(&repo_root, &generated_specs)?;
    rewrite_generated_plan_spec_refs(&implementation_plan, &root_specs)?;
    let root_plan = match mode {
        GenerationMode::Gen => Some(sync_generated_plan_to_root_preserving_open_tasks(
            &repo_root,
            &implementation_plan,
        )?),
        GenerationMode::Reverse => None,
    };
    print_stage("scrub root outputs", run_started_at);
    scrub_root_generated_outputs(&repo_root, mode)?;

    print_stage("save generator state", run_started_at);
    state.planning_root = Some(planning_root.clone());
    state.latest_output_dir = Some(output_dir.clone());
    save_state(&repo_root, &state)?;

    println!("{} complete", mode.command_label());
    println!("repo root:   {}", repo_root.display());
    println!("planning:    {}", planning_root.display());
    println!("output dir:  {}", output_dir.display());
    println!("model:       {}", args.model);
    println!(
        "codex review:{}",
        if args.skip_codex_review {
            " skipped".to_string()
        } else {
            format!(
                " {} ({})",
                args.codex_review_model, args.codex_review_effort
            )
        }
    );
    println!("max turns:   {}", args.max_turns);
    println!("parallelism: {}", args.parallelism.clamp(1, 10));
    println!("specs:       {}", generated_specs.len());
    println!("plan:        {}", implementation_plan.display());
    println!(
        "root specs:  {} appended, {} skipped",
        root_specs.appended_paths.len(),
        root_specs.skipped_count
    );
    if let Some(root_plan) = root_plan {
        println!("root plan:   {}", root_plan.display());
    } else {
        println!("root plan:   unchanged");
    }
    if let Some(plan_phase) = plan_phase {
        println!("plan prompt: {}", plan_phase.prompt_path.display());
        if let Some(response_path) = plan_phase.response_path {
            println!("plan log:    {}", response_path.display());
        }
    } else {
        println!("plan prompt: reused existing generated plan");
    }
    if let Some(review) = codex_review {
        println!("codex prompt: {}", review.prompt_path.display());
        println!("codex stderr: {}", review.stderr_log_path.display());
        println!("codex report: {}", review.report_path.display());
    }
    println!("elapsed:     {}", format_duration(run_started_at.elapsed()));
    Ok(())
}

fn print_stage(stage: &str, run_started_at: Instant) {
    println!(
        "stage:       {stage} (+{})",
        format_duration(run_started_at.elapsed())
    );
}

fn print_command_header(
    label: &str,
    repo_root: &Path,
    planning_root: Option<&Path>,
    run_started_at: Instant,
) {
    println!("{label}");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    if let Some(path) = planning_root {
        println!("planning:    {}", path.display());
    }
    println!(
        "started:     +{}",
        format_duration(run_started_at.elapsed())
    );
}

fn resolve_reference_repos(repo_root: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for path in paths {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            repo_root.join(path)
        };
        let canonical = absolute
            .canonicalize()
            .with_context(|| format!("failed to resolve reference repo {}", absolute.display()))?;
        if !canonical.is_dir() {
            bail!("reference repo {} is not a directory", canonical.display());
        }
        resolved.push(canonical);
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

fn ensure_planning_root_exists(planning_root: &Path) -> Result<()> {
    if planning_root.exists() {
        return Ok(());
    }
    bail!(
        "planning corpus root {} does not exist; run `auto corpus` first",
        planning_root.display()
    );
}

fn ensure_planning_root_ready_for_corpus(planning_root: &Path) -> Result<()> {
    if !planning_root.exists() || planning_root.is_dir() {
        return Ok(());
    }
    bail!(
        "planning corpus root {} exists but is not a directory",
        planning_root.display()
    );
}

fn prepare_planning_root_for_corpus(
    repo_root: &Path,
    planning_root: &Path,
) -> Result<Option<PathBuf>> {
    if !planning_root.exists() {
        return Ok(None);
    }
    let has_contents = fs::read_dir(planning_root)
        .with_context(|| format!("failed to read {}", planning_root.display()))?
        .next()
        .transpose()?
        .is_some();
    let archived = if has_contents {
        let snapshot_root = repo_root.join(".auto").join("fresh-input").join(format!(
            "{}-previous-{}",
            planning_root
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("planning-root"),
            timestamp_slug()
        ));
        copy_tree(planning_root, &snapshot_root).with_context(|| {
            format!(
                "failed to archive existing planning corpus from {} into {}",
                planning_root.display(),
                snapshot_root.display()
            )
        })?;
        Some(snapshot_root)
    } else {
        None
    };
    fs::remove_dir_all(planning_root)
        .with_context(|| format!("failed to clear {}", planning_root.display()))?;
    Ok(archived)
}

fn prepare_generation_output_dir(output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    for path in [
        output_dir.join("corpus"),
        output_dir.join("specs"),
        output_dir.join("IMPLEMENTATION_PLAN.md"),
        output_dir.join("COMPLETED.md"),
    ] {
        if path.is_dir() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("failed to clear {}", path.display()))?;
        } else if path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

struct PhaseRunSummary {
    prompt_path: PathBuf,
    response_path: Option<PathBuf>,
}

struct CodexReviewRunSummary {
    prompt_path: PathBuf,
    stderr_log_path: PathBuf,
    report_path: PathBuf,
}

fn codex_review_report_path(repo_root: &Path, phase_slug: &str) -> PathBuf {
    repo_root
        .join(".auto")
        .join("logs")
        .join(format!("{phase_slug}-{}-report.md", timestamp_slug()))
}

async fn run_logged_codex_review(
    repo_root: &Path,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    report_path: &Path,
) -> Result<CodexReviewRunSummary> {
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("{phase_slug}-{}-prompt.md", timestamp_slug()));
    let stderr_log_path = prompt_path.with_file_name(
        prompt_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("codex-review-prompt.md")
            .replace("-prompt.md", "-stderr.log"),
    );
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("phase:       {phase_slug}");
    println!("model:       {model}");
    println!("effort:      {reasoning_effort}");
    println!("codex bin:   {}", codex_bin.display());
    println!("prompt log:  {}", prompt_path.display());
    println!("stderr log:  {}", stderr_log_path.display());
    println!("report path: {}", report_path.display());

    let status = run_codex_exec(
        repo_root,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
        &stderr_log_path,
        phase_slug,
    )
    .await?;
    if !status.success() {
        bail!(
            "Codex review phase `{phase_slug}` failed with status {status}; see {}",
            stderr_log_path.display()
        );
    }
    verify_codex_review_report(report_path)?;
    Ok(CodexReviewRunSummary {
        prompt_path,
        stderr_log_path,
        report_path: report_path.to_path_buf(),
    })
}

fn verify_codex_review_report(report_path: &Path) -> Result<()> {
    if !report_path.exists() {
        bail!(
            "Codex review completed but did not write required report {}",
            report_path.display()
        );
    }
    let report = fs::read_to_string(report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    if report.trim().is_empty() {
        bail!(
            "Codex review report {} must not be empty",
            report_path.display()
        );
    }
    Ok(())
}

fn run_logged_claude_phase(
    repo_root: &Path,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    max_turns: usize,
) -> Result<PhaseRunSummary> {
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("{phase_slug}-{}-prompt.md", timestamp_slug()));
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("phase:       {phase_slug}");
    println!("prompt log:  {}", prompt_path.display());
    let response = run_claude_prompt(
        repo_root,
        prompt,
        model,
        max_turns,
        phase_slug,
        &prompt_path,
    )?;
    let response_path = if response.trim().is_empty() {
        None
    } else {
        let path = prompt_path.with_file_name(
            prompt_path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("phase-response.txt")
                .replace("-prompt.md", "-response.txt"),
        );
        atomic_write(&path, response.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        Some(path)
    };
    Ok(PhaseRunSummary {
        prompt_path,
        response_path,
    })
}

fn run_claude_prompt(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    max_turns: usize,
    phase_label: &str,
    prompt_path: &Path,
) -> Result<String> {
    let phase_started_at = Instant::now();
    let mut child = Command::new("claude")
        .arg("-p")
        .arg("--verbose")
        .arg("--dangerously-skip-permissions")
        .arg("--model")
        .arg(model)
        .arg("--max-turns")
        .arg(max_turns.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root)
        .spawn()
        .with_context(|| format!("failed to launch Claude for {phase_label}"))?;
    let pid = child.id();
    println!("model:       {model}");
    println!("max turns:   {max_turns}");
    println!("phase start: {phase_label}");
    println!("claude pid:  {pid}");
    println!("cwd:         {}", repo_root.display());
    println!("prompt file: {}", prompt_path.display());

    use std::io::Write;
    child
        .stdin
        .as_mut()
        .context("Claude stdin missing")?
        .write_all(prompt.as_bytes())
        .with_context(|| format!("failed to write prompt for {phase_label}"))?;
    child.stdin.take();

    let output = child
        .wait_with_output()
        .with_context(|| format!("failed waiting for Claude during {phase_label}"))?;
    let stdout = String::from_utf8(output.stdout).context("Claude stdout was not valid UTF-8")?;
    let stderr = String::from_utf8(output.stderr).context("Claude stderr was not valid UTF-8")?;
    if output.status.success() {
        println!(
            "phase done:  {phase_label} (+{})",
            format_duration(phase_started_at.elapsed())
        );
        return Ok(stdout.trim().to_string());
    }
    println!(
        "phase fail:  {phase_label} (+{})",
        format_duration(phase_started_at.elapsed())
    );
    let detail = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        "no stderr/stdout".to_string()
    };
    bail!("{phase_label} failed: {detail}");
}

fn format_duration(duration: std::time::Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m{seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

struct CorpusOutputSummary {
    assessment_path: PathBuf,
    spec_path: PathBuf,
    plans_index_path: PathBuf,
    report_path: PathBuf,
    design_path: Option<PathBuf>,
    focus_path: Option<PathBuf>,
    idea_path: Option<PathBuf>,
    plan_count: usize,
}

fn verify_corpus_outputs(
    planning_root: &Path,
    focus_requested: bool,
) -> Result<CorpusOutputSummary> {
    let assessment_path = planning_root.join("ASSESSMENT.md");
    let spec_path = planning_root.join("SPEC.md");
    let plans_index_path = planning_root.join("PLANS.md");
    let report_path = planning_root.join("GENESIS-REPORT.md");
    let design_path = planning_root.join("DESIGN.md");
    let focus_path = planning_root.join("FOCUS.md");
    let plans_dir = planning_root.join("plans");

    for path in [
        &assessment_path,
        &spec_path,
        &plans_index_path,
        &report_path,
    ] {
        if !path.exists() {
            bail!("corpus generation did not write {}", path.display());
        }
    }
    let plan_files = list_markdown_files(&plans_dir)?;
    if plan_files.is_empty() {
        bail!(
            "corpus generation did not write any plans under {}",
            plans_dir.display()
        );
    }
    for plan_path in &plan_files {
        verify_corpus_execplan(plan_path)?;
    }
    if focus_requested && !focus_path.exists() {
        bail!("corpus generation did not write {}", focus_path.display());
    }
    Ok(CorpusOutputSummary {
        assessment_path,
        spec_path,
        plans_index_path,
        report_path,
        design_path: design_path.exists().then_some(design_path),
        focus_path: focus_path.exists().then_some(focus_path),
        idea_path: planning_root
            .join("IDEA.md")
            .exists()
            .then_some(planning_root.join("IDEA.md")),
        plan_count: plan_files.len(),
    })
}

fn verify_corpus_execplan(plan_path: &Path) -> Result<()> {
    let markdown = fs::read_to_string(plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let trimmed = markdown.trim_start();
    if trimmed.starts_with("```") {
        bail!(
            "corpus plan {} must be a markdown file containing the ExecPlan directly, not a fenced code block",
            plan_path.display()
        );
    }
    if !trimmed.starts_with("# ") {
        bail!(
            "corpus plan {} must start with a markdown title",
            plan_path.display()
        );
    }
    for section in CORPUS_EXECPLAN_REQUIRED_SECTIONS {
        if !markdown_section_has_nonempty_body(&markdown, section) {
            bail!(
                "corpus plan {} is missing non-empty ExecPlan section `{}`",
                plan_path.display(),
                section
            );
        }
    }
    if !markdown_section_contains(&markdown, "## Progress", |line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- [ ]") || trimmed.starts_with("- [x]")
    }) {
        bail!(
            "corpus plan {} must include at least one checkbox item in `## Progress`",
            plan_path.display()
        );
    }
    for required_fragment in ["goal", "files", "test"] {
        if !markdown_section_contains(&markdown, "## Implementation Units", |line| {
            line.to_ascii_lowercase().contains(required_fragment)
        }) {
            bail!(
                "corpus plan {} must describe {} in `## Implementation Units`",
                plan_path.display(),
                required_fragment
            );
        }
    }
    Ok(())
}

fn markdown_section_has_nonempty_body(markdown: &str, heading: &str) -> bool {
    markdown_section_contains(markdown, heading, |line| !line.trim().is_empty())
}

fn markdown_section_contains(
    markdown: &str,
    heading: &str,
    predicate: impl Fn(&str) -> bool,
) -> bool {
    let mut in_section = false;
    for line in markdown.lines() {
        let trimmed_end = line.trim_end();
        if trimmed_end == heading {
            in_section = true;
            continue;
        }
        if in_section && trimmed_end.starts_with("## ") {
            return false;
        }
        if in_section && predicate(line) {
            return true;
        }
    }
    false
}

fn build_corpus_prompt(
    repo_root: &Path,
    planning_root: &Path,
    previous_planning_snapshot: Option<&Path>,
    parallelism: usize,
    idea: Option<&str>,
    focus: Option<&str>,
    reference_repos: &[PathBuf],
) -> String {
    let planning_root = planning_root
        .strip_prefix(repo_root)
        .unwrap_or(planning_root)
        .display()
        .to_string();
    let previous_snapshot_clause = previous_planning_snapshot
        .map(|path| {
            format!(
                "- Archived previous planning snapshot for optional historical context: `{}`\n",
                path.display()
            )
        })
        .unwrap_or_default();
    let idea_output_clause = if idea.is_some() {
        format!("- `{planning_root}/IDEA.md`\n")
    } else {
        String::new()
    };
    let focus_output_clause = if focus.is_some() {
        format!("- `{planning_root}/FOCUS.md`\n")
    } else {
        String::new()
    };
    let reference_repo_clause = if reference_repos.is_empty() {
        String::new()
    } else {
        let listing = reference_repos
            .iter()
            .map(|path| format!("- Mandatory reference repo: `{}`", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Reference repositories to inspect as required input:\n{listing}\n\nWhen reference repos are listed:\n- Inspect them directly; do not treat them as optional background.\n- Use them to distinguish reusable code, architectural inspiration, and non-reusable coupling.\n- Be explicit about which conclusions came from the target repo vs the reference repos.\n\n"
        )
    };
    let idea_context_clause = idea
        .map(|idea| {
            format!(
                r#"- Idea seed from the operator: `{idea}`

Run a non-interactive office-hours shaping pass first:
- Treat the idea seed as the intended future state.
- Do not ask follow-up questions. Infer the strongest truthful framing from the idea, the repo, and the surrounding code reality.
- Pressure-test the idea the way office-hours would: demand reality, status quo, desperate specificity, narrowest wedge, observation risk, and future-fit.
- If evidence is missing because the idea is early, label those sections as hypotheses or open questions instead of pretending certainty.
- Infer whether this is closer to startup mode or builder mode and say why.
- Write the result to `{planning_root}/IDEA.md` as a durable seed brief before expanding the rest of the corpus.

`IDEA.md` must include:
- the raw idea in normalized form
- inferred mode: startup or builder, with a short rationale
- problem statement
- target user or audience
- strongest demand evidence currently available vs what is still hypothetical
- status quo / current workaround
- narrowest wedge
- success criteria
- constraints
- assumptions and open questions
- key assumptions to validate next, with the fastest credible validation path for each
- candidate approaches
- alternatives considered and why they were rejected
- risks
- explicit non-goals
- one recommended direction
"#
            )
        })
        .unwrap_or_default();
    let focus_context_clause = focus
        .map(|focus| {
            format!(
                r#"- Focus steering from the operator: `{focus}`

Treat this as an attention and prioritization signal, not a blinders command:
- Still perform a wide repo sweep and do not ignore critical issues outside the focus
- Spend extra review budget on the focused surfaces, likely failure modes, and next-priority decisions
- Use the focus to rank recommendations and plans, not to invent scope unsupported by the codebase
- Write the normalized focus brief to `{planning_root}/FOCUS.md`

`FOCUS.md` must include:
- the raw focus string
- the normalized focus themes
- the likely code, product, and operational surfaces this implies
- what still requires repo-wide review despite the focus
- the main questions the focus should answer
- how the focus changed priority ordering, if it did
"#
            )
        })
        .unwrap_or_default();
    format!(
        r#"You are the interim CEO/CTO of this repository at `{target_repo}`. Your job is to perform a deep repo review and author a detailed planning corpus.

Write all output files with tools into `{planning_root}/`; do not print the corpus to stdout.

Use up to {parallelism} parallel subagents when helpful for code review, repo-history analysis, and topic decomposition.

Additional operator-provided context:
{previous_snapshot_clause}
{reference_repo_clause}

Mandatory output files:
- `{planning_root}/ASSESSMENT.md`
- `{planning_root}/SPEC.md`
- `{planning_root}/PLANS.md`
- `{planning_root}/GENESIS-REPORT.md`
- `{planning_root}/DESIGN.md` if the repo has meaningful user-facing surfaces
{idea_output_clause}{focus_output_clause}- `{planning_root}/plans/001-master-plan.md`
- `{planning_root}/plans/002-*.md` through `plans/NNN-*.md`

Review the actual codebase first, not just docs:
- Read the main entry points, state definitions, and user-facing routes
- Review security boundaries, input validation, observability, tests, CI, and git history
- Treat completed docs and plans as claims that must be verified against code
- If an archived previous planning snapshot exists, use it only as historical context, not truth
- If an idea seed is present, use it as intentional product direction, then reconcile it against repo reality, reusable assets, and the actual gaps.
- If a focus seed is present, use it to bias depth and plan ordering while still preserving full-repo coverage.
- The current codebase is still the truth for current state, constraints, and what can be reused.
- When the repo needs an agent-instruction file, prefer the repo's actual primary convention.
  - In Codex-first repos, prefer `AGENTS.md`.
  - Do not choose the instruction filename based on which planning model ran the corpus pass.
- Start by framing the repo as a real product/system:
  - write a crisp "How Might We" style problem statement grounded in the current code reality
  - identify the primary users/operators and what success should look like for them
  - surface the biggest constraints, hidden assumptions, and trade-offs
  - consider 2-3 plausible future directions before choosing the recommended one
  - make a clear "Not Doing" list so the corpus reflects focus, not wishful scope
  - if the repo is developer-facing, also assess the first-run developer experience: zero friction at T0, learn-by-doing, uncertainty reduction, and whether the onboarding examples are honest about the real work
- Every exact version, dependency tag, timeout, threshold, benchmark target, chain choice, or protocol detail must be handled explicitly as one of:
  - verified from code or a primary source
  - recommendation for the new system
  - hypothesis / open question
- Do not present guessed values as settled requirements.
- For future phases with unresolved feasibility, keep the artifacts at research/design level instead of pretending the implementation details are already locked.
- Apply the current gstack `/autoplan` review discipline while authoring the corpus:
  - Run the review in the sequence CEO -> Design when UI/UX is in scope -> Eng -> DX when the repo is developer-facing or has meaningful setup/API/operator experience.
  - CEO review must challenge the premise, map existing code leverage before proposing new work, compare plausible future states, state alternatives considered, preserve a real Not Doing list, and capture major failure modes and rescue paths.
  - Design review must cover information architecture, state coverage, user journeys, accessibility, responsive behavior, and AI-slop risk when the repo has user-facing surfaces; if it does not, say why the design pass is not applicable.
  - Eng review must cover architecture, dependency order, data flow, integration seams, persistence/migrations, error handling, observability, performance, and testing; every no-issue conclusion must still say what was examined and why it is acceptable.
  - DX review must cover first-run developer/operator experience, learn-by-doing paths, error clarity, time-to-hello-world, honest examples, and uncertainty-reducing docs or tooling when applicable.
  - Classify important planning decisions as `Mechanical`, `Taste`, or `User Challenge`. Treat model disagreements and close alternatives as taste decisions that need a short rationale. Treat any point that would change the operator's stated direction as a user challenge instead of silently auto-deciding it.
  - Use these decision principles: choose completeness, inspect broadly when the problem requires it, stay pragmatic, avoid redundant artifacts, prefer explicit contracts over clever prose, and bias toward action when evidence is sufficient.

{idea_context_clause}
{focus_context_clause}

ASSESSMENT.md must include:
- what the project says it is vs what the code shows it is
- what works, what is broken, what is half-built
- tech debt inventory
- security risks
- test gaps
- documentation staleness
- implementation-status table for prior claims and plans
- code-review coverage list proving which source files were actually read
- target users, success criteria, and repo constraints
- assumption ledger: what seems true, what is verified, and what still needs proof
- focus-response section: what the operator focus emphasized, what the code says about it, and any non-focused risks that still outrank it
- opportunity framing: strongest direction, rejected directions, and why they were rejected
- for developer-facing repos: a short DX assessment covering first-run friction, copy-paste onboarding honesty, error clarity, and whether the fastest path produces a meaningful success moment

SPEC.md must summarize the repo as a product/system with concrete behaviors grounded in the code and near-term direction.

`{planning_root}/PLANS.md` must index the generated plan set and explain sequencing, dependency order, and why the chosen slice order is preferable to obvious alternatives. This file is an index, not the ExecPlan authoring standard. If the target repo has a root `PLANS.md`, read the entire file before writing numbered plans, treat it as the governing ExecPlan standard, and make the generated index say that numbered plans follow the root `PLANS.md` standard.

GENESIS-REPORT.md must summarize the corpus refresh, major findings, recommended direction, top next priorities, and the explicit "Not Doing" list.
If a focus seed exists, GENESIS-REPORT.md must also say how it changed the recommended priority order and call out any higher-priority issues that escaped the requested focus.
GENESIS-REPORT.md must also include a concise decision audit trail with `Mechanical`, `Taste`, and `User Challenge` classifications for major scope and sequencing choices.

Each numbered plan under `{planning_root}/plans/` must be a full ExecPlan, not a high-level task stub. The generated plan file itself is the ExecPlan, so omit surrounding triple-backtick fences and do not nest fenced code blocks inside it; use indented command blocks when examples are needed.

ExecPlan requirements for every numbered plan:
- start with a markdown H1 title
- include the living-document paragraph from the root `PLANS.md` skeleton: "This ExecPlan is a living document..." and say it must be maintained in accordance with root `PLANS.md` when that file exists
- be fully self-contained for a novice who has only the current working tree and that single plan file
- define every non-obvious term in plain language and tie it to concrete repo files or commands
- describe one concrete vertical slice or research gate, not a vague epic
- if a slice feels larger than one focused implementation session, split it into additional numbered plans
- keep future-phase plans with unresolved feasibility research-shaped, with explicit decision gates before implementation promises
- after every 2-3 numbered plans or at meaningful phase boundaries, include an explicit checkpoint or decision-gate plan file that says what must be true before later work proceeds

Every numbered plan under `{planning_root}/plans/` must include these non-empty sections, using these exact headings:
- `## Purpose / Big Picture`
- `## Requirements Trace`
- `## Scope Boundaries`
- `## Progress`
- `## Surprises & Discoveries`
- `## Decision Log`
- `## Outcomes & Retrospective`
- `## Context and Orientation`
- `## Plan of Work`
- `## Implementation Units`
- `## Concrete Steps`
- `## Validation and Acceptance`
- `## Idempotence and Recovery`
- `## Artifacts and Notes`
- `## Interfaces and Dependencies`

Section requirements for numbered ExecPlans:
- `## Purpose / Big Picture` explains what a user or operator gains and how they can see it working
- `## Requirements Trace` uses requirement labels such as `R1`, `R2`, and states the contracts or success criteria the work must satisfy
- `## Scope Boundaries` states what the plan intentionally does not change and what adjacent surfaces remain unchanged
- `## Progress` uses checkbox bullets with timestamps; unchecked items are allowed for newly generated plans, but the section must reflect the current state truthfully
- `## Surprises & Discoveries`, `## Decision Log`, and `## Outcomes & Retrospective` must exist even before implementation starts; use "None yet" only when that is true, and include the rationale for initial plan-shaping decisions in the decision log
- `## Context and Orientation` names the relevant repository-relative files, functions, modules, commands, and current behavior so a novice can navigate without prior context
- `## Plan of Work` describes the sequence of edits and additions in prose, with file paths and concrete locations where possible
- `## Implementation Units` breaks work into independently verifiable units; each unit must name the goal, requirements advanced, dependencies, files to create or modify, tests to add or modify, approach, and specific test scenarios. For research-only or checkpoint plans, include the artifact to create and write `Test expectation: none -- <reason>` only when no code behavior changes.
- `## Concrete Steps` gives exact commands to run from the repository root and short expected observations where useful
- `## Validation and Acceptance` phrases acceptance as observable behavior with specific inputs, commands, outputs, or artifacts; name tests that should fail before the work and pass after when applicable
- `## Idempotence and Recovery` explains how to rerun steps safely and how to recover from partial completion
- `## Artifacts and Notes` captures concise evidence snippets, logs, or diffs that prove success or will be filled in as work proceeds
- `## Interfaces and Dependencies` names the concrete modules, APIs, traits, commands, services, or external dependencies the plan uses or changes

Do not use the short `## Objective` / `## Description` / `## Acceptance Criteria` / `## Verification` / `## Dependencies` shape for numbered plans. That shape is too high-level for this corpus. Use the full ExecPlan envelope above.

Never trust docs over code. If docs claim something the code does not support, say so clearly."#,
        target_repo = repo_root.display(),
        planning_root = planning_root,
        parallelism = parallelism,
        previous_snapshot_clause = previous_snapshot_clause,
        reference_repo_clause = reference_repo_clause,
        idea_output_clause = idea_output_clause,
        focus_output_clause = focus_output_clause,
        idea_context_clause = idea_context_clause,
        focus_context_clause = focus_context_clause,
    )
}

fn build_corpus_codex_review_prompt(
    repo_root: &Path,
    planning_root: &Path,
    report_path: &Path,
) -> String {
    format!(
        r#"{skill_boundary}

You are the mandatory GPT-5.4 xhigh Codex outside-voice review step for `auto corpus`.

Claude Opus 4.6 has already produced the initial planning corpus under `{planning_root}` for the repository at `{repo_root}`. Your job is to conduct an independent review and validation pass, then amend the generated corpus in place when the documents fall short.

Edit boundary:
- You may read the repository at `{repo_root}` and the generated corpus at `{planning_root}`.
- You may edit only markdown files under `{planning_root}` and the review report at `{report_path}`.
- Do not edit source code, root specs, root implementation plans, generated output dirs outside `{planning_root}`, or any skill definition directory.
- Do not ask the user questions. Make conservative, code-grounded decisions and record uncertainty.

Review method adapted from the latest gstack `/autoplan` workflow:
- Run review phases in order: CEO, Design when user-facing UI or UX is in scope, Eng, and DX when the repo is developer-facing or has a meaningful setup/API/operator experience.
- Use these decision principles: choose completeness over shortcuts; be willing to inspect broadly when needed; be pragmatic; avoid duplicate/redundant artifacts; prefer explicit contracts over clever prose; bias toward action when evidence is sufficient.
- Classify important review decisions in the report as `Mechanical`, `Taste`, or `User Challenge`.
- Treat a `User Challenge` as any point where both the Opus output and your independent review would recommend changing the user's stated direction. Do not silently auto-decide those; preserve the challenge explicitly in `GENESIS-REPORT.md`, `ASSESSMENT.md`, or `{report_path}`.
- Treat Codex-vs-Opus disagreements that are not mechanical as `Taste` decisions, explain why you chose one direction, and amend the corpus only when the repository evidence supports the change.

CEO review pass:
- Re-test the premise, product direction, opportunity cost, and "Not Doing" list against the actual code.
- Map existing code leverage before recommending new work.
- Check that alternatives were considered and rejected for concrete reasons.
- Look for hidden assumptions, failure modes, rescue paths, and unclear scope boundaries.

Design review pass, when applicable:
- Check information architecture, user journeys, empty/loading/error/success states, accessibility, responsive behavior, and AI-slop risk.
- If the repo has no meaningful UI, say that in the report and skip UI-specific rewrites.

Eng review pass:
- Check architecture, data flow, dependency order, integration points, migrations/persistence, error handling, observability, performance risks, and test strategy.
- Verify current-state claims against files, commands, or code structure. Docs are claims, not truth.

DX review pass, when applicable:
- Check first-run developer/operator experience, learn-by-doing path, error clarity, time-to-hello-world, honest examples, and uncertainty-reducing docs or tooling.
- If the repo is not developer-facing, say that in the report and skip DX-specific rewrites.

Corpus-specific validation:
- `ASSESSMENT.md` must say what was actually inspected, separate verified facts from assumptions, and call out stale doc claims.
- `SPEC.md` must describe concrete current behavior and intended near-term direction without presenting guesses as settled facts.
- `PLANS.md` under `{planning_root}` must be an index to the generated plan set, not a substitute for the repo root ExecPlan standard.
- Every numbered plan under `{planning_root}/plans/` must be a full ExecPlan rather than the old high-level `Objective` / `Description` / `Acceptance Criteria` / `Verification` / `Dependencies` stub shape.
- Numbered ExecPlans must be self-contained, novice-readable, vertically sliced where possible, and grounded in repository-relative files and commands.
- Every numbered ExecPlan must include non-empty sections for `Purpose / Big Picture`, `Requirements Trace`, `Scope Boundaries`, `Progress`, `Surprises & Discoveries`, `Decision Log`, `Outcomes & Retrospective`, `Context and Orientation`, `Plan of Work`, `Implementation Units`, `Concrete Steps`, `Validation and Acceptance`, `Idempotence and Recovery`, `Artifacts and Notes`, and `Interfaces and Dependencies`.
- `Progress` must include checkbox bullets. `Implementation Units` must name goal, requirements advanced, dependencies, files to create or modify, tests to add or modify, approach, and specific test scenarios. For research-only work, name the artifact and explain why no code test is expected.
- Add checkpoint or decision-gate plans after each risky cluster or every 2-3 numbered plans when later work depends on unresolved evidence.

Validation expectations:
- Use lightweight local inspection commands as needed, such as `rg`, `ls`, and targeted file reads. Do not run long integration suites or production-affecting commands for this document review pass.
- After edits, re-check the generated corpus shape yourself before finishing.
- Write `{report_path}` with these sections: `# Codex Corpus Review`, `## Summary`, `## Files Reviewed`, `## Changes Made`, `## Decision Audit Trail`, `## User Challenges`, `## Taste Decisions`, `## Validation`, and `## Remaining Risks`.
- If no corpus edits are needed, still write the report and explain what you checked.
"#,
        skill_boundary = CODEX_SKILL_BOUNDARY,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        report_path = report_path.display(),
    )
}

fn build_generation_codex_review_prompt(
    mode: GenerationMode,
    repo_root: &Path,
    planning_root: &Path,
    output_dir: &Path,
    report_path: &Path,
) -> String {
    let mode_clause = match mode {
        GenerationMode::Gen => {
            "This is an `auto gen` review. The corpus represents intended future direction, but current code remains authoritative for every current-state fact. Preserve future intent only when it is labeled as a recommendation, hypothesis, or decision gate until evidence proves it."
        }
        GenerationMode::Reverse => {
            "This is an `auto reverse` review. The live codebase is the source of truth, and the corpus is supporting context only."
        }
    };
    format!(
        r#"{skill_boundary}

You are the mandatory GPT-5.4 xhigh Codex outside-voice review step for `{command_label}`.

Claude Opus 4.6 has already produced initial generated specs and an implementation plan in `{output_dir}` for the repository at `{repo_root}`.

{mode_clause}

Edit boundary:
- You may read the repository at `{repo_root}`, the planning corpus at `{planning_root}`, and generated outputs at `{output_dir}`.
- You may edit only `{output_dir}/specs/*.md`, `{output_dir}/IMPLEMENTATION_PLAN.md`, and the review report at `{report_path}`.
- Do not edit root `specs/`, root `IMPLEMENTATION_PLAN.md`, source code, the planning corpus, or any skill definition directory. The generator will sync reviewed outputs to the root after your pass.
- Do not ask the user questions. Make conservative, code-grounded decisions and record uncertainty.

Review method adapted from the latest gstack `/autoplan` workflow:
- Run review phases in order: CEO, Design when user-facing UI or UX is in scope, Eng, and DX when the repo is developer-facing or has a meaningful setup/API/operator experience.
- Use these decision principles: choose completeness over shortcuts; be willing to inspect broadly when needed; be pragmatic; avoid duplicate/redundant artifacts; prefer explicit contracts over clever prose; bias toward action when evidence is sufficient.
- Classify important review decisions in the report as `Mechanical`, `Taste`, or `User Challenge`.
- Treat a `User Challenge` as any point where both the Opus output and your independent review would recommend changing the user's stated direction. Do not silently auto-decide those; preserve the challenge explicitly in the generated docs or `{report_path}`.
- Treat Codex-vs-Opus disagreements that are not mechanical as `Taste` decisions, explain why you chose one direction, and amend generated docs only when repository evidence supports the change.

CEO review pass:
- Check whether the generated specs and plan preserve the right product/system direction, scope boundaries, non-goals, alternatives, and hidden assumptions.
- Ensure future-facing recommendations do not outrun evidence or dependency order.

Design review pass, when applicable:
- Check whether specs and plan tasks account for information architecture, user journeys, empty/loading/error/success states, accessibility, responsive behavior, and AI-slop risk.
- If the repo has no meaningful UI, say that in the report and skip UI-specific rewrites.

Eng review pass:
- Check architecture, data flow, dependency order, integration points, persistence/migrations, error handling, observability, performance risks, and test strategy.
- Verify exact current-state claims against files, commands, or code structure. Docs are claims, not truth.
- Ensure implementation tasks are dependency-ordered, small enough for one focused worker session where possible, and include explicit checkpoint tasks after risky clusters or every 2-3 priority tasks.

DX review pass, when applicable:
- Check first-run developer/operator experience, learn-by-doing path, error clarity, time-to-hello-world, honest examples, and uncertainty-reducing docs or tooling.
- If the repo is not developer-facing, say that in the report and skip DX-specific rewrites.

Generated spec validation:
- Every spec under `{output_dir}/specs/` must start with `# Specification:`.
- Every spec must include non-empty `## Objective`, `## Evidence Status`, `## Acceptance Criteria`, `## Verification`, and `## Open Questions`.
- `## Evidence Status` must separate verified code facts from recommendations, hypotheses, and unresolved questions.
- Acceptance criteria must be observable, testable outcomes, not vague capability prose.
- Specs must cite concrete files, commands, APIs, or primary-source documentation for exact current-state claims.

Generated implementation plan validation:
- `{output_dir}/IMPLEMENTATION_PLAN.md` must start with `# IMPLEMENTATION_PLAN`.
- It must include `## Priority Work`, `## Follow-On Work`, and `## Completed / Already Satisfied`.
- Every unfinished task must include `Spec:`, `Why now:`, `Codebase evidence:`, `Owns:`, `Integration touchpoints:`, `Scope boundary:`, `Acceptance criteria:`, `Verification:`, `Required tests:`, `Dependencies:`, `Estimated scope:`, and `Completion signal:`.
- Every `Spec:` reference must point to a spec file that exists under `{output_dir}/specs/`.
- Behavior-changing tasks should prefer a prove-it validation path: failing test or repro first, green proof, then broader regression check.
- Research or design tasks must name the closing artifact or decision and must not promise implementation details before the prerequisite evidence exists.

Validation expectations:
- Use lightweight local inspection commands as needed, such as `rg`, `ls`, and targeted file reads. Do not run long integration suites or production-affecting commands for this document review pass.
- After edits, re-check the generated docs' shape yourself before finishing.
- Write `{report_path}` with these sections: `# Codex Generation Review`, `## Summary`, `## Files Reviewed`, `## Changes Made`, `## Decision Audit Trail`, `## User Challenges`, `## Taste Decisions`, `## Validation`, and `## Remaining Risks`.
- If no generated-doc edits are needed, still write the report and explain what you checked.
"#,
        skill_boundary = CODEX_SKILL_BOUNDARY,
        command_label = mode.command_label(),
        mode_clause = mode_clause,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        output_dir = output_dir.display(),
        report_path = report_path.display(),
    )
}

fn build_spec_generation_prompt(
    mode: GenerationMode,
    repo_root: &Path,
    planning_root: &Path,
    output_dir: &Path,
    corpus: &PlanningCorpus,
    parallelism: usize,
) -> String {
    let mode_clause = match mode {
        GenerationMode::Gen => {
            "This is a generation pass guided by the planning corpus. Use the corpus for intended future direction, but treat the live codebase as authoritative for every current-state fact, concrete filename, metric name, command, count, API shape, and behavior claim."
        }
        GenerationMode::Reverse => {
            "This is a reverse-engineering pass. The live codebase is the source of truth. Use the planning corpus only as supporting context."
        }
    };
    let spec_listing = corpus
        .spec_documents
        .iter()
        .map(|spec| format!("- `{}` — {}", spec.path, spec.title))
        .collect::<Vec<_>>()
        .join("\n");
    let plan_listing = corpus
        .primary_plans
        .iter()
        .map(|plan| format!("- `{}` — {}", plan.path, plan.title))
        .collect::<Vec<_>>()
        .join("\n");
    let idea_clause = corpus
        .idea_path
        .as_deref()
        .map(|path| {
            format!(
                "If `{path}` exists in the corpus snapshot, treat it as the office-hours-style seed brief for intended future direction. Preserve its product framing unless later corpus evidence or code reality clearly overrides it."
            )
        })
        .unwrap_or_else(|| "No IDEA.md seed is present for this corpus.".to_string());
    let focus_clause = corpus
        .focus_path
        .as_deref()
        .map(|path| {
            format!(
                "If `{path}` exists in the corpus snapshot, treat it as operator steering for what deserved extra attention in the planning pass. Preserve the full-system view, but use the focus brief to understand why certain priorities may have been ranked ahead of equally plausible alternatives."
            )
        })
        .unwrap_or_else(|| "No FOCUS.md steering brief is present for this corpus.".to_string());
    format!(
        r#"You are generating a new spec snapshot for `{repo_root}`.

{mode_clause}

Write all generated specs under `{output_dir}/specs/`. Do not print the specs to stdout.
Use `{planning_root}` as supporting planning context for this generation pass.

Use up to {parallelism} parallel subagents where helpful.

Existing corpus spec documents:
{spec_listing}

Existing corpus plans:
{plan_listing}

Idea-seed context:
{idea_clause}

Focus context:
{focus_clause}

Required output contract:
- Write one markdown file per generated spec into `{output_dir}/specs/`
- Filenames must use `ddmmyy-topic-slug.md`
- Each file must start with `# Specification: ...`
- Each file must include `## Objective`
- Each file must include `## Evidence Status`
- Each file must include a `## Acceptance Criteria` section
- Each file must include a `## Verification` section
- Each file must include `## Open Questions`
- Acceptance criteria must be concrete, testable, and phrased as truthful observable outcomes
- Acceptance criteria should use flat bullet points, not prose paragraphs
- Specs must be concrete, file-grounded, and implementation-oriented
- Avoid placeholders and abstract framework prose
- Surface important assumptions or spec/code conflicts explicitly instead of smoothing them over
- Include commands, boundaries, or open questions when they materially affect implementation or verification
- Every exact current-state fact should be backed by a file path, command, or primary-source documentation citation in `## Evidence Status`
- `## Evidence Status` must separate:
  - verified facts grounded in code or primary-source documentation
  - recommendations for the intended system
  - hypotheses / unresolved questions
- Treat the live codebase as authoritative for current-state facts in every mode
- Any exact version, timeout, threshold, dependency tag, benchmark target, chain choice, or protocol step that is not verified must be labeled as a recommendation or hypothesis instead of stated as settled fact
- If a spec describes a future phase or unresolved surface, keep it at research/design level and avoid implementation detail that the evidence does not yet support
- If the repo is developer-facing, capture onboarding, error handling, and first-success expectations truthfully enough that a future worker can improve the DX without guessing
- Preserve proven current behavior in reverse mode
- In gen mode, preserve intended future direction from the corpus, but keep future intent under recommendations or hypotheses until code or primary-source evidence proves otherwise

Cover the main product and system surfaces represented in the repo. Use the codebase and the planning corpus to decide the right spec set."#,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        mode_clause = mode_clause,
        output_dir = output_dir.display(),
        parallelism = parallelism.max(1),
        spec_listing = if spec_listing.is_empty() {
            "- none".to_string()
        } else {
            spec_listing
        },
        idea_clause = idea_clause,
        focus_clause = focus_clause,
        plan_listing = if plan_listing.is_empty() {
            "- none".to_string()
        } else {
            plan_listing
        },
    )
}

fn build_implementation_plan_prompt(
    mode: GenerationMode,
    repo_root: &Path,
    output_dir: &Path,
    generated_specs: &[GeneratedSpecDocument],
    parallelism: usize,
) -> String {
    let mode_clause = match mode {
        GenerationMode::Gen => {
            "This is a planning pass grounded in the generated specs plus current code review. Use the specs to preserve intended direction, but treat the live codebase as authoritative for current-state facts, repo shape, counts, commands, metric names, and existing coverage."
        }
        GenerationMode::Reverse => {
            "This is a reverse-engineering planning pass. Use the generated specs and current code reality to identify the next actionable work."
        }
    };
    let spec_listing = generated_specs
        .iter()
        .map(|path| {
            format!(
                "- `{}`",
                path.path
                    .strip_prefix(output_dir)
                    .unwrap_or(&path.path)
                    .display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"You are writing `{output_dir}/IMPLEMENTATION_PLAN.md` for `{repo_root}`.

{mode_clause}

Use up to {parallelism} parallel subagents where helpful.

Generated specs for this run:
{spec_listing}

Before writing the plan, do the real planning work:
- operate in read-only planning mode first
- map dependency order and existing code patterns
- identify the highest-risk unknowns
- prefer vertical slices over horizontal layer dumps
- keep tasks small enough for one focused worker session
- do not hide ambiguity; encode real blockers and assumptions in the task contracts
- if the repo is developer-facing, explicitly consider zero-friction onboarding, learn-by-doing examples, error clarity, and uncertainty-reducing docs or tooling as first-class planning concerns
- treat spec statements labeled as hypotheses, recommendations, design-phase, or research-required as non-binding until the plan closes the corresponding decision gate
- do not create implementation tasks whose contract depends on unverified future-phase details; write a research, validation, or decision task first
- verify every exact current-state fact in the plan from code, tests, or concrete commands before you write it down
- add explicit checkpoint tasks after each risky cluster or every 2-3 priority tasks so a future worker knows when to stop and re-evaluate before widening scope

Output requirements:
- Write exactly one file: `{output_dir}/IMPLEMENTATION_PLAN.md`
- The first non-empty line must be exactly `{IMPLEMENTATION_PLAN_HEADER}`
- Use these top-level sections:
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
  - `Acceptance criteria:`
  - `Verification:`
  - `Required tests:`
  - `Dependencies:`
  - `Estimated scope:`
  - `Completion signal:`
- `Spec:` values must point to `specs/*.md`
- Every `Spec:` reference must exactly match one of the generated spec paths listed for this run; do not invent alternate dates or filenames
- Keep the plan concrete, file-grounded, and executable
- Do not include lane prose, staffing prose, or meta commentary
- Keep tasks dependency-ordered and bounded; if a task feels bigger than one focused implementation session, break it down again
- Front-load risk where practical, but never at the cost of violating dependency order
- `Acceptance criteria:` must be specific, testable, and truthful
- `Verification:` must name the concrete commands or runtime checks a worker should run
- For behavior-changing tasks, `Verification:` should prefer a prove-it path: failing test or repro first, then green proof, then broader regression checks
- `Estimated scope:` should be `XS`, `S`, `M`, or `L`; avoid `L` unless the codebase reality truly leaves no smaller slice
- Put only unfinished work in the unchecked queue sections
- Put already-satisfied items only in `## Completed / Already Satisfied`
- Future-phase work with unresolved feasibility must stay in research-shaped tasks until the prerequisite evidence exists

The goal is a truthful, execution-ready implementation queue."#,
        repo_root = repo_root.display(),
        output_dir = output_dir.display(),
        IMPLEMENTATION_PLAN_HEADER = IMPLEMENTATION_PLAN_HEADER,
        mode_clause = mode_clause,
        parallelism = parallelism.max(1),
        spec_listing = if spec_listing.is_empty() {
            "- none".to_string()
        } else {
            spec_listing
        },
    )
}

fn verify_generated_specs(output_dir: &Path) -> Result<Vec<GeneratedSpecDocument>> {
    let specs_dir = output_dir.join("specs");
    if !specs_dir.is_dir() {
        bail!("spec generation did not write {}", specs_dir.display());
    }
    let specs = list_markdown_files(&specs_dir)?;
    if specs.is_empty() {
        bail!(
            "spec generation did not write any markdown files under {}",
            specs_dir.display()
        );
    }
    let mut docs = Vec::new();
    for spec in &specs {
        let original = fs::read_to_string(spec)
            .with_context(|| format!("failed to read {}", spec.display()))?;
        let normalized = normalize_generated_spec_markdown(&original);
        if normalized != original {
            atomic_write(spec, normalized.as_bytes())
                .with_context(|| format!("failed to normalize {}", spec.display()))?;
        }
        if !normalized.starts_with("# Specification:") {
            bail!(
                "generated spec {} must start with `# Specification:`",
                spec.display()
            );
        }
        for section in [
            SPEC_OBJECTIVE_HEADER,
            SPEC_ACCEPTANCE_CRITERIA_HEADER,
            SPEC_VERIFICATION_HEADER,
        ] {
            if !generated_spec_has_section(&normalized, section) {
                bail!(
                    "generated spec {} must include `{}`",
                    spec.display(),
                    section
                );
            }
        }
        if !generated_spec_has_section(&normalized, "## Evidence Status") {
            bail!(
                "generated spec {} must include `## Evidence Status`",
                spec.display()
            );
        }
        if !generated_spec_has_section(&normalized, "## Open Questions") {
            bail!(
                "generated spec {} must include `## Open Questions`",
                spec.display()
            );
        }
        if !generated_spec_has_acceptance_criteria(&normalized) {
            bail!(
                "generated spec {} must include `{}` with at least one bullet",
                spec.display(),
                SPEC_ACCEPTANCE_CRITERIA_HEADER
            );
        }
        docs.push(GeneratedSpecDocument {
            path: spec.clone(),
            text: normalized,
        });
    }
    lint_generated_spec_set(&docs)?;
    Ok(docs)
}

fn generated_spec_has_section(markdown: &str, header: &str) -> bool {
    split_markdown_section(markdown, header)
        .map(|(_, body)| !body.trim().is_empty())
        .unwrap_or(false)
}

fn generated_spec_has_acceptance_criteria(markdown: &str) -> bool {
    let Some((_, section_body)) = split_markdown_section(markdown, SPEC_ACCEPTANCE_CRITERIA_HEADER)
    else {
        return false;
    };

    section_body.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- ") || trimmed.starts_with("* ")
    }) || acceptance_criteria_has_structured_items(section_body)
}

fn acceptance_criteria_has_structured_items(section_body: &str) -> bool {
    let mut saw_heading = false;
    let mut saw_body = false;

    for line in section_body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("### ") {
            if saw_heading && saw_body {
                return true;
            }
            saw_heading = true;
            saw_body = false;
            continue;
        }
        if saw_heading && !trimmed.is_empty() && !trimmed.starts_with("## ") {
            saw_body = true;
        }
    }

    saw_heading && saw_body
}

fn normalize_generated_spec_markdown(markdown: &str) -> String {
    normalize_ordered_acceptance_list(markdown)
}

fn normalize_ordered_acceptance_list(markdown: &str) -> String {
    let Some((body_start, section_end)) =
        markdown_section_body_bounds(markdown, SPEC_ACCEPTANCE_CRITERIA_HEADER)
    else {
        return markdown.to_string();
    };
    let section_body = &markdown[body_start..section_end];
    let normalized_body = normalize_ordered_list_to_bullets(section_body);
    if normalized_body == section_body {
        return markdown.to_string();
    }

    let mut normalized = String::with_capacity(markdown.len() + 16);
    normalized.push_str(&markdown[..body_start]);
    normalized.push_str(&normalized_body);
    normalized.push_str(&markdown[section_end..]);
    normalized
}

fn normalize_ordered_list_to_bullets(section_body: &str) -> String {
    let mut normalized = String::with_capacity(section_body.len());
    for raw_line in section_body.split_inclusive('\n') {
        let (line, newline) = raw_line
            .strip_suffix('\n')
            .map(|line| (line, "\n"))
            .unwrap_or((raw_line, ""));
        let trimmed = line.trim_start();
        if let Some(content) = strip_ordered_list_marker(trimmed) {
            let indent_len = line.len().saturating_sub(trimmed.len());
            normalized.push_str(&line[..indent_len]);
            normalized.push_str("- ");
            normalized.push_str(content.trim_start());
            normalized.push_str(newline);
        } else {
            normalized.push_str(raw_line);
        }
    }
    normalized
}

fn strip_ordered_list_marker(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == 0 || index >= bytes.len() {
        return None;
    }
    if bytes[index] != b'.' && bytes[index] != b')' {
        return None;
    }
    index += 1;
    if index >= bytes.len() || !bytes[index].is_ascii_whitespace() {
        return None;
    }
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    Some(&line[index..])
}

fn split_markdown_section<'a>(markdown: &'a str, header: &str) -> Option<(&'a str, &'a str)> {
    let start = markdown.find(header)?;
    let after_header = &markdown[start + header.len()..];
    let section_end = after_header
        .find("\n## ")
        .map(|offset| start + header.len() + offset)
        .unwrap_or(markdown.len());
    Some((
        &markdown[start..section_end],
        &markdown[start + header.len()..section_end],
    ))
}

fn markdown_section_body_bounds(markdown: &str, header: &str) -> Option<(usize, usize)> {
    let start = markdown.find(header)?;
    let body_start = start + header.len();
    let after_header = &markdown[body_start..];
    let section_end = after_header
        .find("\n## ")
        .map(|offset| body_start + offset)
        .unwrap_or(markdown.len());
    Some((body_start, section_end))
}

fn lint_generated_spec_set(specs: &[GeneratedSpecDocument]) -> Result<()> {
    lint_duplicate_spec_topics(specs)?;
    lint_signature_policy_consistency(specs)?;
    lint_session_resume_wire_contract(specs)?;
    lint_session_persistence_abort_language(specs)?;
    Ok(())
}

fn lint_duplicate_spec_topics(specs: &[GeneratedSpecDocument]) -> Result<()> {
    let mut seen = std::collections::BTreeMap::<String, &GeneratedSpecDocument>::new();
    for spec in specs {
        let slug = spec
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(spec_topic_slug)
            .context("generated spec must have a file stem")?;
        if let Some(previous) = seen.insert(slug.clone(), spec) {
            bail!(
                "generated specs duplicate the `{}` topic: {} and {}",
                slug,
                previous.path.display(),
                spec.path.display()
            );
        }
    }
    Ok(())
}

fn lint_signature_policy_consistency(specs: &[GeneratedSpecDocument]) -> Result<()> {
    let Some(transcript) = find_generated_spec(specs, "deterministic-transcripts") else {
        return Ok(());
    };
    let Some(adversarial) = find_generated_spec(specs, "adversarial-robustness") else {
        return Ok(());
    };
    let transcript_requires_cosign = transcript.text.contains("requires both signatures")
        || transcript.text.contains("requires both player signatures")
        || transcript
            .text
            .contains("rejects `build()` without both player signatures");
    let adversarial_allows_unsigned = adversarial.text.contains("recorded as unsigned");
    if transcript_requires_cosign && adversarial_allows_unsigned {
        bail!(
            "generated specs disagree about transcript signature policy: {} requires both player signatures, but {} allows unsigned completed transcripts",
            transcript.path.display(),
            adversarial.path.display()
        );
    }
    Ok(())
}

fn lint_session_resume_wire_contract(specs: &[GeneratedSpecDocument]) -> Result<()> {
    let Some(session) = find_generated_spec(specs, "session-persistence") else {
        return Ok(());
    };
    let Some(wire) = find_generated_spec(specs, "wire-protocol") else {
        return Ok(());
    };

    let hello_line = markdown_line_containing(&wire.text, "| `Hello` |").unwrap_or_default();
    if session.text.contains("resume_session") && !hello_line.contains("resume_session") {
        bail!(
            "generated specs disagree about the Hello message: {} extends Hello with `resume_session`, but {} does not include that field",
            session.path.display(),
            wire.path.display()
        );
    }
    if session.text.contains("last_hand_digests") && !hello_line.contains("last_hand_digests") {
        bail!(
            "generated specs disagree about the Hello message: {} extends Hello with `last_hand_digests`, but {} does not include that field",
            session.path.display(),
            wire.path.display()
        );
    }

    let hello_ack_line = markdown_line_containing(&wire.text, "| `HelloAck` |").unwrap_or_default();
    if session.text.contains("HelloAck` with `resumed: true`")
        && !hello_ack_line.contains("resumed")
    {
        bail!(
            "generated specs disagree about HelloAck: {} requires a `resumed` field, but {} does not include it",
            session.path.display(),
            wire.path.display()
        );
    }

    Ok(())
}

fn lint_session_persistence_abort_language(specs: &[GeneratedSpecDocument]) -> Result<()> {
    let Some(session) = find_generated_spec(specs, "session-persistence") else {
        return Ok(());
    };
    if session.text.contains("not silently lost") && session.text.contains("silently aborted") {
        bail!(
            "generated spec {} contradicts itself about in-flight hand recovery: it says hands are not silently lost and also says they are silently aborted",
            session.path.display()
        );
    }
    Ok(())
}

fn find_generated_spec<'a>(
    specs: &'a [GeneratedSpecDocument],
    needle: &str,
) -> Option<&'a GeneratedSpecDocument> {
    specs.iter().find(|doc| {
        doc.path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(|stem| stem.contains(needle))
            .unwrap_or(false)
    })
}

fn markdown_line_containing<'a>(markdown: &'a str, needle: &str) -> Option<&'a str> {
    markdown.lines().find(|line| line.contains(needle))
}

fn verify_generated_implementation_plan(output_dir: &Path) -> Result<PathBuf> {
    let plan_path = output_dir.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        bail!("generation did not write {}", plan_path.display());
    }
    let markdown = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let normalized = normalize_generated_implementation_plan(&markdown);
    for required in [IMPLEMENTATION_PLAN_HEADER]
        .into_iter()
        .chain(REQUIRED_PLAN_SECTIONS)
    {
        if !normalized.contains(required) {
            bail!("generated implementation plan is missing `{required}`");
        }
    }
    let blocks = extract_plan_task_blocks(&normalized)?;
    for block in &blocks {
        if block.checked {
            continue;
        }
        for field in REQUIRED_PLAN_TASK_FIELDS {
            if !block.markdown.contains(field) {
                bail!(
                    "generated implementation plan task `{}` is missing `{}`",
                    block.task_id,
                    field
                );
            }
        }
    }
    let available_specs = collect_available_spec_refs(&output_dir.join("specs"))?;
    validate_plan_spec_refs(
        &normalized,
        &available_specs,
        &format!("generated implementation plan {}", plan_path.display()),
    )?;
    if normalized != markdown {
        atomic_write(&plan_path, normalized.as_bytes())
            .with_context(|| format!("failed to normalize {}", plan_path.display()))?;
    }
    Ok(plan_path)
}

fn normalize_generated_implementation_plan(markdown: &str) -> String {
    let mut lines = markdown.lines().map(str::to_string).collect::<Vec<_>>();
    let Some(first_non_empty) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return markdown.to_string();
    };

    let first_line = lines[first_non_empty].trim();
    let mut changed = false;
    if first_line == IMPLEMENTATION_PLAN_HEADER {
    } else if first_line.starts_with("# ") {
        lines[first_non_empty] = IMPLEMENTATION_PLAN_HEADER.to_string();
        changed = true;
    }

    let candidate = if changed {
        let mut normalized = lines.join("\n");
        if markdown.ends_with('\n') {
            normalized.push('\n');
        }
        normalized
    } else {
        markdown.to_string()
    };
    ensure_required_plan_sections(&candidate)
}

fn sync_generated_specs_to_root(
    repo_root: &Path,
    generated_specs: &[GeneratedSpecDocument],
) -> Result<SpecSyncSummary> {
    sync_generated_specs_to_root_for_date(repo_root, generated_specs, Local::now().date_naive())
}

fn sync_generated_specs_to_root_for_date(
    repo_root: &Path,
    generated_specs: &[GeneratedSpecDocument],
    today: NaiveDate,
) -> Result<SpecSyncSummary> {
    let root_specs_dir = repo_root.join("specs");
    fs::create_dir_all(&root_specs_dir)
        .with_context(|| format!("failed to create {}", root_specs_dir.display()))?;
    let mut summary = SpecSyncSummary::default();
    let date_prefix = today.format("%d%m%y").to_string();

    for spec in generated_specs {
        let source_name = spec
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .context("generated spec must have a file stem")?;
        let slug = spec_topic_slug(source_name);
        let extension = spec
            .path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("md");
        remove_same_day_topic_snapshots(&root_specs_dir, &date_prefix, &slug, extension)?;
        let destination = root_specs_dir.join(format!("{date_prefix}-{slug}.{extension}"));
        fs::copy(&spec.path, &destination).with_context(|| {
            format!(
                "failed to copy {} -> {}",
                spec.path.display(),
                destination.display()
            )
        })?;
        summary.appended_paths.push(destination);
    }

    Ok(summary)
}

fn sync_generated_plan_to_root_preserving_open_tasks(
    repo_root: &Path,
    generated_plan: &Path,
) -> Result<PathBuf> {
    let root_plan = repo_root.join("IMPLEMENTATION_PLAN.md");
    let generated_markdown = fs::read_to_string(generated_plan)
        .with_context(|| format!("failed to read {}", generated_plan.display()))?;
    let merged = if root_plan.exists() {
        let existing = fs::read_to_string(&root_plan)
            .with_context(|| format!("failed to read {}", root_plan.display()))?;
        merge_generated_plan_with_existing_open_tasks(&generated_markdown, &existing)?
    } else {
        generated_markdown
    };
    atomic_write(&root_plan, merged.as_bytes())
        .with_context(|| format!("failed to write {}", root_plan.display()))?;
    Ok(root_plan)
}

fn rewrite_generated_plan_spec_refs(
    generated_plan: &Path,
    root_specs: &SpecSyncSummary,
) -> Result<()> {
    if root_specs.appended_paths.is_empty() {
        return Ok(());
    }

    let markdown = fs::read_to_string(generated_plan)
        .with_context(|| format!("failed to read {}", generated_plan.display()))?;
    let rewritten = rewrite_plan_spec_refs_to_root(&markdown, root_specs);
    if rewritten == markdown {
        return Ok(());
    }

    atomic_write(generated_plan, rewritten.as_bytes())
        .with_context(|| format!("failed to rewrite {}", generated_plan.display()))?;
    Ok(())
}

fn rewrite_plan_spec_refs_to_root(markdown: &str, root_specs: &SpecSyncSummary) -> String {
    let slug_to_root = root_specs
        .appended_paths
        .iter()
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?;
            let slug = spec_topic_slug(stem);
            let relative = Path::new("specs").join(path.file_name()?);
            Some((slug, relative.display().to_string()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    let mut changed = false;
    let rewritten_lines = markdown
        .lines()
        .map(|line| rewrite_plan_spec_line(line, &slug_to_root, &mut changed))
        .collect::<Vec<_>>();
    if !changed {
        return markdown.to_string();
    }

    let mut rewritten = rewritten_lines.join("\n");
    if markdown.ends_with('\n') {
        rewritten.push('\n');
    }
    rewritten
}

fn rewrite_plan_spec_line(
    line: &str,
    slug_to_root: &std::collections::BTreeMap<String, String>,
    changed: &mut bool,
) -> String {
    let Some(spec_index) = line.find("Spec:") else {
        return line.to_string();
    };
    let prefix = &line[..spec_index];
    let rest = line[spec_index + "Spec:".len()..].trim();
    let unquoted = rest.trim_matches('`');
    let path = Path::new(unquoted);
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return line.to_string();
    };
    let slug = spec_topic_slug(stem);
    let Some(root_path) = slug_to_root.get(&slug) else {
        return line.to_string();
    };
    let normalized = format!("{prefix}Spec: `{root_path}`");
    if normalized != line {
        *changed = true;
    }
    normalized
}

fn remove_same_day_topic_snapshots(
    root_specs_dir: &Path,
    date_prefix: &str,
    slug: &str,
    extension: &str,
) -> Result<()> {
    for existing in find_same_day_topic_snapshots(root_specs_dir, date_prefix, slug, extension)? {
        fs::remove_file(&existing)
            .with_context(|| format!("failed to remove {}", existing.display()))?;
    }
    Ok(())
}

fn find_same_day_topic_snapshots(
    root_specs_dir: &Path,
    date_prefix: &str,
    slug: &str,
    extension: &str,
) -> Result<Vec<PathBuf>> {
    let canonical_stem = format!("{date_prefix}-{slug}");
    let duplicate_prefix = format!("{canonical_stem}-");
    let mut matches = Vec::new();
    for entry in fs::read_dir(root_specs_dir)
        .with_context(|| format!("failed to read {}", root_specs_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read {}", root_specs_dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some(extension) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if stem == canonical_stem {
            matches.push(path);
            continue;
        }
        let Some(suffix) = stem.strip_prefix(&duplicate_prefix) else {
            continue;
        };
        if !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()) {
            matches.push(path);
        }
    }
    Ok(matches)
}

fn collect_available_spec_refs(specs_dir: &Path) -> Result<std::collections::BTreeSet<String>> {
    let mut refs = std::collections::BTreeSet::new();
    if !specs_dir.is_dir() {
        return Ok(refs);
    }
    for path in list_markdown_files(specs_dir)? {
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        refs.insert(format!("specs/{name}"));
    }
    Ok(refs)
}

fn validate_plan_spec_refs(
    markdown: &str,
    available_specs: &std::collections::BTreeSet<String>,
    context_label: &str,
) -> Result<()> {
    for (line_index, line) in markdown.lines().enumerate() {
        if !line.contains("Spec:") {
            continue;
        }
        let refs = extract_spec_refs_from_line(line);
        if refs.is_empty() {
            bail!(
                "{context_label} line {} contains `Spec:` but no `specs/*.md` path",
                line_index + 1
            );
        }
        for spec_ref in refs {
            if !available_specs.contains(&spec_ref) {
                bail!(
                    "{context_label} references missing spec `{spec_ref}` on line {}",
                    line_index + 1
                );
            }
        }
    }
    Ok(())
}

fn extract_spec_refs_from_line(line: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut search_start = 0usize;

    while let Some(relative_start) = line[search_start..].find("specs/") {
        let start = search_start + relative_start;
        let candidate = &line[start..];
        let end = candidate
            .char_indices()
            .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-')))
            .map(|(index, _)| index)
            .unwrap_or(candidate.len());
        let path = &candidate[..end];
        if path.ends_with(".md") {
            refs.push(path.to_string());
        }
        search_start = start + end.max(1);
    }

    refs
}

fn scrub_root_generated_outputs(repo_root: &Path, mode: GenerationMode) -> Result<()> {
    let available_specs = collect_available_spec_refs(&repo_root.join("specs"))?;
    if mode == GenerationMode::Gen {
        let root_plan = repo_root.join("IMPLEMENTATION_PLAN.md");
        if root_plan.exists() {
            let markdown = fs::read_to_string(&root_plan)
                .with_context(|| format!("failed to read {}", root_plan.display()))?;
            validate_plan_spec_refs(
                &markdown,
                &available_specs,
                &format!("root implementation plan {}", root_plan.display()),
            )?;
        }
    }
    Ok(())
}

fn merge_generated_plan_with_existing_open_tasks(
    generated: &str,
    existing: &str,
) -> Result<String> {
    let generated = ensure_required_plan_sections(generated);
    let generated_blocks = extract_plan_task_blocks(&generated)?;
    let existing_blocks = extract_plan_task_blocks(existing)?;
    let generated_ids = generated_blocks
        .iter()
        .map(|block| block.task_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let preserved_blocks = existing_blocks
        .into_iter()
        .filter(|block| !block.checked && !generated_ids.contains(block.task_id.as_str()))
        .collect::<Vec<_>>();
    if preserved_blocks.is_empty() {
        return Ok(generated);
    }
    let mut merged = generated;
    append_blocks_to_section(&mut merged, PlanSection::Priority, &preserved_blocks)?;
    append_blocks_to_section(&mut merged, PlanSection::FollowOn, &preserved_blocks)?;
    Ok(merged)
}

fn ensure_required_plan_sections(markdown: &str) -> String {
    if markdown.trim().is_empty() {
        return markdown.to_string();
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
        .with_context(|| format!("generated plan is missing section `{section_header}`"))?;
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

fn spec_topic_slug(source_name: &str) -> String {
    strip_known_prefix(source_name)
        .trim_matches('-')
        .trim()
        .replace('_', "-")
        .to_ascii_lowercase()
}

fn strip_known_prefix(name: &str) -> String {
    let mut value = strip_fixed_numeric_prefix(name);
    if value.len() >= 7
        && value.chars().take(6).all(|ch| ch.is_ascii_digit())
        && value.as_bytes().get(6) == Some(&b'-')
    {
        value = value[7..].to_string();
    }
    value
}

fn strip_fixed_numeric_prefix(name: &str) -> String {
    let bytes = name.as_bytes();
    if bytes.len() > 4 && bytes[0..3].iter().all(u8::is_ascii_digit) && bytes[3] == b'-' {
        name[4..].to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_corpus_codex_review_prompt, build_corpus_prompt,
        build_generation_codex_review_prompt, build_implementation_plan_prompt,
        generated_spec_has_acceptance_criteria, lint_session_resume_wire_contract,
        lint_signature_policy_consistency, merge_generated_plan_with_existing_open_tasks,
        normalize_generated_implementation_plan, normalize_generated_spec_markdown,
        rewrite_plan_spec_refs_to_root, sync_generated_specs_to_root_for_date,
        verify_corpus_execplan, verify_generated_implementation_plan, GeneratedSpecDocument,
        GenerationMode, SpecSyncSummary, IMPLEMENTATION_PLAN_HEADER,
    };
    use chrono::NaiveDate;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn generated_spec(slug: &str, text: &str) -> GeneratedSpecDocument {
        GeneratedSpecDocument {
            path: PathBuf::from(format!("/tmp/{slug}.md")),
            text: text.to_string(),
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("autodev-{label}-{suffix}"));
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    #[test]
    fn normalizes_noncanonical_plan_heading() {
        let generated = r#"# Bitino Implementation Plan

Generated: 2026-04-02

## Priority Work

## Follow-On Work

## Completed / Already Satisfied
"#;

        let normalized = normalize_generated_implementation_plan(generated);

        assert!(normalized.starts_with(&format!("{IMPLEMENTATION_PLAN_HEADER}\n")));
        assert!(normalized.contains("Generated: 2026-04-02"));
    }

    #[test]
    fn preserves_canonical_plan_heading() {
        let generated = r#"# IMPLEMENTATION_PLAN

## Priority Work

## Follow-On Work

## Completed / Already Satisfied
"#;

        assert_eq!(
            normalize_generated_implementation_plan(generated),
            generated.to_string()
        );
    }

    #[test]
    fn normalizes_missing_required_sections() {
        let generated = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate user query input
Spec: specs/020426-query-validation.md
"#;

        let normalized = normalize_generated_implementation_plan(generated);

        assert!(normalized.contains("## Follow-On Work"));
        assert!(normalized.contains("## Completed / Already Satisfied"));
    }

    #[test]
    fn merges_existing_open_tasks_not_present_in_new_plan() {
        let generated = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate user query input
Spec: specs/020426-query-validation.md

## Follow-On Work

## Completed / Already Satisfied
"#;

        let existing = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `SEC-001` Harden auth checks
Spec: specs/010426-auth.md

## Follow-On Work

- [ ] `OPS-001` Improve metrics
Spec: specs/010426-observability.md

## Completed / Already Satisfied

- [x] `OLD-001` Finished task
Spec: specs/310326-finished.md
"#;

        let merged = merge_generated_plan_with_existing_open_tasks(generated, existing).unwrap();

        assert!(merged.contains("`VAL-001`"));
        assert!(merged.contains("`SEC-001`"));
        assert!(merged.contains("`OPS-001`"));
        assert!(!merged.contains("`OLD-001`"));
    }

    #[test]
    fn detects_acceptance_criteria_section_with_bullets() {
        let spec = r#"# Specification: Example

## Overview

Something.

## Acceptance Criteria

- One
- Two
"#;

        assert!(generated_spec_has_acceptance_criteria(spec));
    }

    #[test]
    fn rejects_acceptance_criteria_section_without_bullets() {
        let spec = r#"# Specification: Example

## Acceptance Criteria

This should be bulletized.
"#;

        assert!(!generated_spec_has_acceptance_criteria(spec));
    }

    #[test]
    fn normalizes_numbered_acceptance_criteria_into_bullets() {
        let spec = r#"# Specification: Example

## Acceptance Criteria

1. One
2. Two

## Verification

- Check
"#;

        let normalized = normalize_generated_spec_markdown(spec);

        assert!(normalized.contains("## Acceptance Criteria\n\n- One\n- Two"));
        assert!(generated_spec_has_acceptance_criteria(&normalized));
    }

    #[test]
    fn accepts_structured_acceptance_items_with_subheadings() {
        let spec = r#"# Specification: Example

## Acceptance Criteria

### AC-01: One

This is a concrete acceptance item.

### AC-02: Two

This is another acceptance item.
"#;

        assert!(generated_spec_has_acceptance_criteria(spec));
    }

    #[test]
    fn rejects_conflicting_signature_policy_specs() {
        let specs = vec![
            generated_spec(
                "deterministic-transcripts",
                "# Specification: Deterministic Transcripts\n\nrequires both signatures\n",
            ),
            generated_spec(
                "adversarial-robustness",
                "# Specification: Adversarial Robustness\n\nrecorded as unsigned\n",
            ),
        ];

        let error =
            lint_signature_policy_consistency(&specs).expect_err("expected signature mismatch");

        assert!(error.to_string().contains("signature policy"));
    }

    #[test]
    fn rejects_session_resume_contract_drift() {
        let specs = vec![
            generated_spec(
                "session-persistence",
                "# Specification: Session Persistence\n\nresume_session\nlast_hand_digests\nHelloAck` with `resumed: true`\n",
            ),
            generated_spec(
                "wire-protocol",
                "# Specification: Wire Protocol\n\n| `Hello` | `session_id` |\n| `HelloAck` | `session_id` |\n",
            ),
        ];

        let error = lint_session_resume_wire_contract(&specs).expect_err("expected Hello mismatch");

        assert!(error.to_string().contains("Hello message"));
    }

    #[test]
    fn rewrites_plan_spec_refs_to_actual_root_snapshots() {
        let markdown = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `WS-01` Scaffold workspace
Spec: `specs/050426-workspace-build-system.md`

## Follow-On Work

- [ ] `TR-01` Build transcripts
Spec: `specs/050426-deterministic-transcripts.md`

## Completed / Already Satisfied
"#;

        let rewritten = rewrite_plan_spec_refs_to_root(
            markdown,
            &SpecSyncSummary {
                appended_paths: vec![
                    PathBuf::from("/tmp/specs/040426-workspace-build-system.md"),
                    PathBuf::from("/tmp/specs/040426-deterministic-transcripts.md"),
                ],
                skipped_count: 0,
            },
        );

        assert!(rewritten.contains("Spec: `specs/040426-workspace-build-system.md`"));
        assert!(rewritten.contains("Spec: `specs/040426-deterministic-transcripts.md`"));
        assert!(!rewritten.contains("050426"));
    }

    #[test]
    fn corpus_prompt_requires_assumption_validation_and_checkpoint_plans() {
        let prompt = build_corpus_prompt(
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            None,
            4,
            Some("build a thing"),
            None,
            &[],
        );

        assert!(prompt.contains("key assumptions to validate next"));
        assert!(prompt.contains("alternatives considered"));
        assert!(prompt.contains("explicit checkpoint or decision-gate plan file"));
        assert!(prompt.contains("prefer `AGENTS.md`"));
        assert!(prompt.contains("must be a full ExecPlan"));
        assert!(prompt.contains("## Purpose / Big Picture"));
        assert!(prompt.contains("## Requirements Trace"));
        assert!(prompt.contains("## Implementation Units"));
        assert!(prompt.contains("Do not use the short `## Objective`"));
        assert!(prompt.contains("current gstack `/autoplan` review discipline"));
        assert!(prompt.contains("CEO -> Design"));
        assert!(prompt.contains(
            "Classify important planning decisions as `Mechanical`, `Taste`, or `User Challenge`"
        ));
        assert!(prompt.contains("concise decision audit trail"));
    }

    #[test]
    fn corpus_prompt_can_require_focus_brief_without_losing_repo_wide_sweep() {
        let prompt = build_corpus_prompt(
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            None,
            4,
            None,
            Some("wire reconnects, TLS failures, session-token handling"),
            &[],
        );

        assert!(prompt.contains("`genesis/FOCUS.md`"));
        assert!(prompt.contains("Still perform a wide repo sweep"));
        assert!(prompt.contains("attention and prioritization signal"));
    }

    #[test]
    fn codex_review_prompts_encode_autoplan_boundary_and_edit_scope() {
        let corpus_prompt = build_corpus_codex_review_prompt(
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            std::path::Path::new("/tmp/repo/.auto/logs/corpus-report.md"),
        );

        assert!(corpus_prompt.contains("GPT-5.4 xhigh Codex outside-voice review"));
        assert!(corpus_prompt.contains("Do NOT read or execute any SKILL.md files"));
        assert!(
            corpus_prompt.contains("You may edit only markdown files under `/tmp/repo/genesis`")
        );
        assert!(corpus_prompt.contains("Run review phases in order: CEO, Design"));
        assert!(corpus_prompt.contains("`Mechanical`, `Taste`, or `User Challenge`"));
        assert!(corpus_prompt.contains(
            "Every numbered plan under `/tmp/repo/genesis/plans/` must be a full ExecPlan"
        ));
        assert!(corpus_prompt.contains("# Codex Corpus Review"));

        let generation_prompt = build_generation_codex_review_prompt(
            GenerationMode::Gen,
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/genesis"),
            std::path::Path::new("/tmp/repo/gen-010203"),
            std::path::Path::new("/tmp/repo/.auto/logs/gen-report.md"),
        );

        assert!(generation_prompt.contains("outside-voice review step for `auto gen`"));
        assert!(generation_prompt.contains("Do NOT read or execute any SKILL.md files"));
        assert!(generation_prompt.contains("You may edit only `/tmp/repo/gen-010203/specs/*.md`"));
        assert!(generation_prompt
            .contains("The generator will sync reviewed outputs to the root after your pass"));
        assert!(generation_prompt.contains("Run review phases in order: CEO, Design"));
        assert!(generation_prompt.contains("# Codex Generation Review"));
    }

    #[test]
    fn corpus_execplan_validator_accepts_full_plans_md_shape() {
        let root = temp_dir("corpus-execplan-ok");
        let plan_path = root.join("001-example.md");
        fs::write(
            &plan_path,
            r#"# Example Slice

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

After this change, an operator can run a concrete proof and observe the generated artifact.

## Requirements Trace

R1: The proof artifact is generated from the live repo state.

## Scope Boundaries

This plan does not change production runtime behavior.

## Progress

- [ ] (2026-04-10 00:00Z) Implement the proof artifact.

## Surprises & Discoveries

None yet.

## Decision Log

- Decision: Keep the first slice bounded to one artifact.
  Rationale: It gives a reviewer a concrete proof before runtime changes.
  Date/Author: 2026-04-10 / auto corpus

## Outcomes & Retrospective

None yet.

## Context and Orientation

The relevant files are `docs/example.md` and `crates/example/src/lib.rs`.

## Plan of Work

Update `docs/example.md`, then add a focused regression test in `crates/example/src/lib.rs`.

## Implementation Units

Unit 1: Proof artifact.
Goal: Create the proof artifact.
Requirements advanced: R1.
Dependencies: none.
Files to create or modify: `docs/example.md`, `crates/example/src/lib.rs`.
Tests to add or modify: add `example_proof_is_generated`.
Approach: write the artifact first, then cover it with the focused test.
Specific test scenarios: invoke the proof function and expect the artifact path to be returned.

## Concrete Steps

From the repository root, run:

    cargo test -p example example_proof_is_generated -- --nocapture

## Validation and Acceptance

The focused test passes and prints the generated artifact path.

## Idempotence and Recovery

Rerunning the test overwrites the same deterministic artifact.

## Artifacts and Notes

Add the final test transcript here after implementation.

## Interfaces and Dependencies

Use the existing `example::proof` module; no new external service is required.
"#,
        )
        .unwrap();

        verify_corpus_execplan(&plan_path).unwrap();
    }

    #[test]
    fn corpus_execplan_validator_rejects_old_task_stub_shape() {
        let root = temp_dir("corpus-execplan-stub");
        let plan_path = root.join("004-autonomous-evidence-retention-dr.md");
        fs::write(
            &plan_path,
            r#"# 004 - Autonomous Evidence Retention And DR

## Objective

Add backup, retention, and disaster-recovery treatment.

## Description

This is too high level to guide a novice implementation.

## Acceptance Criteria

- Backup is documented.

## Verification

    cargo test -p bitino-house ops_event -- --nocapture

## Dependencies

- 002 local validation baseline.
"#,
        )
        .unwrap();

        let error = verify_corpus_execplan(&plan_path)
            .expect_err("expected old high-level plan shape to be rejected");

        assert!(error.to_string().contains("Purpose / Big Picture"));
    }

    #[test]
    fn implementation_plan_prompt_requires_checkpoint_tasks_and_prove_it_verification() {
        let prompt = build_implementation_plan_prompt(
            GenerationMode::Gen,
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/gen-123"),
            &[generated_spec(
                "workspace-build-system",
                "# Specification: Workspace Build System\n",
            )],
            4,
        );

        assert!(prompt.contains("checkpoint tasks"));
        assert!(prompt.contains("failing test or repro first"));
        assert!(prompt.contains("generated spec paths listed for this run"));
        assert!(prompt.contains("verify every exact current-state fact"));
    }

    #[test]
    fn generation_prompt_makes_code_authoritative_for_current_state_facts() {
        let prompt = build_implementation_plan_prompt(
            GenerationMode::Gen,
            std::path::Path::new("/tmp/repo"),
            std::path::Path::new("/tmp/repo/gen-123"),
            &[generated_spec(
                "workspace-build-system",
                "# Specification: Workspace Build System\n",
            )],
            4,
        );

        assert!(prompt.contains("authoritative for current-state facts"));
        assert!(prompt.contains("metric names"));
        assert!(prompt.contains("do not invent alternate dates or filenames"));
    }

    #[test]
    fn generated_plan_rejects_missing_spec_refs() {
        let root = temp_dir("missing-spec-ref");
        let specs_dir = root.join("specs");
        fs::create_dir_all(&specs_dir).unwrap();
        fs::write(
            specs_dir.join("050426-real.md"),
            "# Specification: Real\n\n## Objective\n\n- ok\n\n## Acceptance Criteria\n\n- ok\n\n## Verification\n\n- ok\n\n## Evidence Status\n\n- ok\n\n## Open Questions\n\n- none\n",
        )
        .unwrap();
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [ ] `DOC-001` Write docs\nSpec: `specs/060426-missing.md`\nWhy now: needed\nCodebase evidence: present\nOwns: docs\nIntegration touchpoints: none\nScope boundary: docs only\nAcceptance criteria: docs land\nVerification: check file\nRequired tests: none\nDependencies: none\nEstimated scope: S\nCompletion signal: merged\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
        )
        .unwrap();

        let error =
            verify_generated_implementation_plan(&root).expect_err("expected missing spec failure");

        assert!(error.to_string().contains("references missing spec"));
    }

    #[test]
    fn sync_replaces_same_day_duplicate_root_specs_with_canonical_snapshot() {
        let repo_root = temp_dir("spec-sync");
        let root_specs = repo_root.join("specs");
        fs::create_dir_all(&root_specs).unwrap();
        fs::write(
            root_specs.join("050426-example-topic.md"),
            "old canonical snapshot\n",
        )
        .unwrap();
        fs::write(
            root_specs.join("050426-example-topic-2.md"),
            "stale duplicate snapshot\n",
        )
        .unwrap();

        let output_dir = temp_dir("spec-output");
        let generated_path = output_dir.join("050426-example-topic.md");
        fs::write(&generated_path, "fresh generated snapshot\n").unwrap();
        let generated = GeneratedSpecDocument {
            path: generated_path,
            text: "fresh generated snapshot\n".to_string(),
        };

        let summary = sync_generated_specs_to_root_for_date(
            &repo_root,
            &[generated],
            NaiveDate::from_ymd_opt(2026, 4, 5).unwrap(),
        )
        .unwrap();

        assert_eq!(summary.appended_paths.len(), 1);
        assert_eq!(
            fs::read_to_string(root_specs.join("050426-example-topic.md")).unwrap(),
            "fresh generated snapshot\n"
        );
        assert!(!root_specs.join("050426-example-topic-2.md").exists());
    }
}
