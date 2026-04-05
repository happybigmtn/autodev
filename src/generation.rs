use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::{Local, NaiveDate};

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
    if !reference_repos.is_empty() {
        println!("references:  {}", reference_repos.len());
        for path in &reference_repos {
            println!("  - {}", path.display());
        }
    }
    println!("model:       {}", args.model);
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

    print_stage("verify corpus outputs", run_started_at);
    let summary = verify_corpus_outputs(&planning_root)?;
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

    let generated_specs = if args.plan_only {
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

    let (implementation_plan, plan_phase) = if args.plan_only {
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
    print_stage("sync generated specs to root", run_started_at);
    let root_specs = sync_generated_specs_to_root(&repo_root, &generated_specs)?;
    let root_plan = match mode {
        GenerationMode::Gen => Some(sync_generated_plan_to_root_preserving_open_tasks(
            &repo_root,
            &implementation_plan,
        )?),
        GenerationMode::Reverse => None,
    };

    print_stage("save generator state", run_started_at);
    state.planning_root = Some(planning_root.clone());
    state.latest_output_dir = Some(output_dir.clone());
    save_state(&repo_root, &state)?;

    println!("{} complete", mode.command_label());
    println!("repo root:   {}", repo_root.display());
    println!("planning:    {}", planning_root.display());
    println!("output dir:  {}", output_dir.display());
    println!("model:       {}", args.model);
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
    idea_path: Option<PathBuf>,
    plan_count: usize,
}

fn verify_corpus_outputs(planning_root: &Path) -> Result<CorpusOutputSummary> {
    let assessment_path = planning_root.join("ASSESSMENT.md");
    let spec_path = planning_root.join("SPEC.md");
    let plans_index_path = planning_root.join("PLANS.md");
    let report_path = planning_root.join("GENESIS-REPORT.md");
    let design_path = planning_root.join("DESIGN.md");
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
    let plan_count = fs::read_dir(&plans_dir)
        .with_context(|| format!("failed to read {}", plans_dir.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("md"))
        .count();
    if plan_count == 0 {
        bail!(
            "corpus generation did not write any plans under {}",
            plans_dir.display()
        );
    }
    Ok(CorpusOutputSummary {
        assessment_path,
        spec_path,
        plans_index_path,
        report_path,
        design_path: design_path.exists().then_some(design_path),
        idea_path: planning_root
            .join("IDEA.md")
            .exists()
            .then_some(planning_root.join("IDEA.md")),
        plan_count,
    })
}

fn build_corpus_prompt(
    repo_root: &Path,
    planning_root: &Path,
    previous_planning_snapshot: Option<&Path>,
    parallelism: usize,
    idea: Option<&str>,
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
- candidate approaches
- risks
- explicit non-goals
- one recommended direction
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
{idea_output_clause}- `{planning_root}/plans/001-master-plan.md`
- `{planning_root}/plans/002-*.md` through `plans/NNN-*.md`

Review the actual codebase first, not just docs:
- Read the main entry points, state definitions, and user-facing routes
- Review security boundaries, input validation, observability, tests, CI, and git history
- Treat completed docs and plans as claims that must be verified against code
- If an archived previous planning snapshot exists, use it only as historical context, not truth
- If an idea seed is present, use it as intentional product direction, then reconcile it against repo reality, reusable assets, and the actual gaps.
- The current codebase is still the truth for current state, constraints, and what can be reused.
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

{idea_context_clause}

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
- opportunity framing: strongest direction, rejected directions, and why they were rejected
- for developer-facing repos: a short DX assessment covering first-run friction, copy-paste onboarding honesty, error clarity, and whether the fastest path produces a meaningful success moment

SPEC.md must summarize the repo as a product/system with concrete behaviors grounded in the code and near-term direction.

PLANS.md must index the plan set and explain sequencing, dependency order, and why the chosen slice order is preferable to obvious alternatives.

GENESIS-REPORT.md must summarize the corpus refresh, major findings, recommended direction, top next priorities, and the explicit "Not Doing" list.

Each numbered plan under `{planning_root}/plans/` must be implementation-ready, explicit about owned surfaces, vertically sliced where possible, and scoped to a concrete deliverable that a single focused worker can close truthfully.
Future-phase plans with unresolved feasibility must say so clearly and center research gates before implementation promises.

Never trust docs over code. If docs claim something the code does not support, say so clearly."#,
        target_repo = repo_root.display(),
        planning_root = planning_root,
        parallelism = parallelism,
        previous_snapshot_clause = previous_snapshot_clause,
        reference_repo_clause = reference_repo_clause,
        idea_output_clause = idea_output_clause,
        idea_context_clause = idea_context_clause,
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
            "This is a corpus-first generation pass. The planning corpus defines the intended system shape. Use the codebase to verify gaps, current implementation status, and naming."
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
- `## Evidence Status` must separate:
  - verified facts grounded in code or primary-source documentation
  - recommendations for the intended system
  - hypotheses / unresolved questions
- Any exact version, timeout, threshold, dependency tag, benchmark target, chain choice, or protocol step that is not verified must be labeled as a recommendation or hypothesis instead of stated as settled fact
- If a spec describes a future phase or unresolved surface, keep it at research/design level and avoid implementation detail that the evidence does not yet support
- If the repo is developer-facing, capture onboarding, error handling, and first-success expectations truthfully enough that a future worker can improve the DX without guessing
- Preserve proven current behavior in reverse mode
- Preserve intended future behavior from the corpus in gen mode when the code has not caught up yet

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
            "This is a corpus-first planning pass. Use the generated specs plus current code review to surface the true remaining work."
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
- Keep the plan concrete, file-grounded, and executable
- Do not include lane prose, staffing prose, or meta commentary
- Keep tasks dependency-ordered and bounded; if a task feels bigger than one focused implementation session, break it down again
- Front-load risk where practical, but never at the cost of violating dependency order
- `Acceptance criteria:` must be specific, testable, and truthful
- `Verification:` must name the concrete commands or runtime checks a worker should run
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
        let text = fs::read_to_string(spec)
            .with_context(|| format!("failed to read {}", spec.display()))?;
        if !text.starts_with("# Specification:") {
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
            if !generated_spec_has_section(&text, section) {
                bail!(
                    "generated spec {} must include `{}`",
                    spec.display(),
                    section
                );
            }
        }
        if !generated_spec_has_section(&text, "## Evidence Status") {
            bail!(
                "generated spec {} must include `## Evidence Status`",
                spec.display()
            );
        }
        if !generated_spec_has_section(&text, "## Open Questions") {
            bail!(
                "generated spec {} must include `## Open Questions`",
                spec.display()
            );
        }
        if !generated_spec_has_acceptance_criteria(&text) {
            bail!(
                "generated spec {} must include `{}` with at least one bullet",
                spec.display(),
                SPEC_ACCEPTANCE_CRITERIA_HEADER
            );
        }
        docs.push(GeneratedSpecDocument {
            path: spec.clone(),
            text,
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
    })
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

fn lint_generated_spec_set(specs: &[GeneratedSpecDocument]) -> Result<()> {
    lint_signature_policy_consistency(specs)?;
    lint_session_resume_wire_contract(specs)?;
    lint_session_persistence_abort_language(specs)?;
    lint_future_phase_research_discipline(specs)?;
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

fn lint_future_phase_research_discipline(specs: &[GeneratedSpecDocument]) -> Result<()> {
    for doc in specs {
        let slug = doc
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let looks_future_phase = slug.contains("settlement") || slug.contains("ring-game");
        if !looks_future_phase {
            continue;
        }
        let has_research_guard = doc.text.contains("research")
            || doc.text.contains("Research")
            || doc.text.contains("design phase")
            || doc.text.contains("future phase")
            || doc.text.contains("not yet implemented");
        if !has_research_guard {
            bail!(
                "future-phase generated spec {} must explicitly stay at research/design level until feasibility is proven",
                doc.path.display()
            );
        }
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
        generated_spec_has_acceptance_criteria, lint_future_phase_research_discipline,
        lint_session_resume_wire_contract, lint_signature_policy_consistency,
        merge_generated_plan_with_existing_open_tasks, normalize_generated_implementation_plan,
        GeneratedSpecDocument, IMPLEMENTATION_PLAN_HEADER,
    };
    use std::path::PathBuf;

    fn generated_spec(slug: &str, text: &str) -> GeneratedSpecDocument {
        GeneratedSpecDocument {
            path: PathBuf::from(format!("/tmp/{slug}.md")),
            text: text.to_string(),
        }
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
    fn rejects_future_phase_specs_without_research_framing() {
        let specs = vec![generated_spec(
            "settlement-architecture",
            "# Specification: Settlement Architecture\n\n## Objective\nShip full escrow replay.\n",
        )];

        let error = lint_future_phase_research_discipline(&specs)
            .expect_err("expected future-phase research lint");

        assert!(error.to_string().contains("research/design level"));
    }
}
