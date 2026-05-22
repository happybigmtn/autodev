//! Repo-root planning and generation pipeline (`auto corpus`, `auto gen`,
//! `auto reverse`).
//!
//! The module is split into focused submodules: [`prompts`] holds the pure
//! prompt builders, [`phase_runner`] spawns Codex/Claude phases, [`markdown`]
//! is the shared section parser, and the `*_verify` / [`root_sync`] submodules
//! hold output validation and root synchronization.

mod corpus_verify;
pub(crate) mod markdown;
mod phase_runner;
mod planning_root;
mod plan_verify;
mod prompts;
mod root_sync;
mod spec_verify;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::corpus::{emit_corpus_snapshot, load_planning_corpus};
use crate::state::{load_state, save_state, AutoState};
use crate::util::{binary_provenance_line, ensure_repo_layout, git_repo_root, timestamp_slug};
use crate::{CorpusArgs, GenerationArgs};

use crate::generation::corpus_verify::{
    sanitize_and_verify_corpus_outputs, verify_corpus_outputs, verify_corpus_outputs_read_only,
    CorpusOutputSummary,
};
use crate::generation::phase_runner::{
    codex_review_report_path, run_logged_author_phase, run_logged_codex_review,
};
use crate::generation::planning_root::{
    discover_active_plan_surface, ensure_planning_root_exists,
    ensure_planning_root_ready_for_corpus, prepare_generation_output_dir,
    prepare_planning_root_for_corpus, promote_staged_planning_root, resolve_generation_planning_root,
    resolve_reference_repos,
};
use crate::generation::plan_verify::verify_generated_implementation_plan;
use crate::generation::prompts::{
    build_corpus_codex_review_prompt, build_corpus_prompt, build_generation_codex_review_prompt,
    build_implementation_plan_prompt, build_spec_generation_prompt,
};
use crate::generation::root_sync::{
    rewrite_generated_plan_spec_refs, scrub_root_generated_outputs, sync_generated_specs_to_root,
    sync_generated_plan_to_root_preserving_open_tasks, SpecSyncSummary,
};
use crate::generation::spec_verify::verify_generated_specs;

pub(crate) use crate::generation::planning_root::ActivePlanSurface;

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

pub(crate) struct GeneratedSpecDocument {
    path: PathBuf,
    text: String,
}

struct CorpusPromptInputs<'a> {
    previous_planning_snapshot: Option<&'a Path>,
    parallelism: usize,
    idea: Option<&'a str>,
    focus: Option<&'a str>,
    reference_repos: &'a [PathBuf],
    active_plan_surface: &'a ActivePlanSurface,
}

pub(crate) async fn run_corpus(args: CorpusArgs) -> Result<()> {
    let run_started_at = Instant::now();
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let reference_repos = resolve_reference_repos(&repo_root, &args.reference_repos)?;
    let active_plan_surface = discover_active_plan_surface(&repo_root)?;
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
    if active_plan_surface.has_active_plans() {
        println!(
            "active plans: {}",
            active_plan_surface.active_plan_paths.len()
        );
        if let Some(primary) = active_plan_surface.primary_plan_path() {
            println!("primary plan: {}", primary);
        }
    }
    println!("model:       {}", args.model);
    println!("effort:      {}", args.reasoning_effort);
    println!(
        "review pass: {}",
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
    if args.verify_only {
        println!("mode:        verify-only");
    }
    if args.dry_run {
        println!("mode:        dry-run");
        return Ok(());
    }
    if args.verify_only {
        ensure_planning_root_exists(&planning_root)?;
        let summary = verify_corpus_outputs_read_only(
            &repo_root,
            &planning_root,
            args.focus.is_some(),
            &active_plan_surface,
            run_started_at,
        )?;
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
        println!("plan files:  {}", summary.plan_count);
        println!("elapsed:     {}", format_duration(run_started_at.elapsed()));
        return Ok(());
    }

    print_stage("prepare planning root", run_started_at);
    let preparation = prepare_planning_root_for_corpus(&repo_root, &planning_root)?;
    let authoring_root = preparation.authoring_root;

    print_stage("create corpus skeleton", run_started_at);
    fs::create_dir_all(authoring_root.join("plans")).with_context(|| {
        format!(
            "failed to create corpus plan directory {}",
            authoring_root.join("plans").display()
        )
    })?;

    let prompt = build_corpus_prompt(
        &repo_root,
        &authoring_root,
        CorpusPromptInputs {
            previous_planning_snapshot: preparation.previous_snapshot.as_deref(),
            parallelism: args.parallelism.clamp(1, 10),
            idea: args.idea.as_deref(),
            focus: args.focus.as_deref(),
            reference_repos: &reference_repos,
            active_plan_surface: &active_plan_surface,
        },
    );
    print_stage("run corpus model", run_started_at);
    let author_phase = run_logged_author_phase(
        &repo_root,
        "corpus",
        &prompt,
        &args.model,
        &args.reasoning_effort,
        args.max_turns,
        &args.codex_bin,
    )
    .await
    .context("corpus generation failed")?;

    let codex_review = if args.skip_codex_review {
        None
    } else {
        print_stage("run corpus independent review", run_started_at);
        let report_path = codex_review_report_path(&repo_root, "corpus-codex-review");
        let review_prompt = build_corpus_codex_review_prompt(
            &repo_root,
            &authoring_root,
            &report_path,
            &reference_repos,
            &active_plan_surface,
        );
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

    let _staged_summary = sanitize_and_verify_corpus_outputs(
        &repo_root,
        &authoring_root,
        args.focus.is_some(),
        &active_plan_surface,
        run_started_at,
    )?;
    print_stage("promote staged corpus", run_started_at);
    promote_staged_planning_root(&authoring_root, &planning_root)?;
    let summary = save_verified_corpus_state(
        &repo_root,
        &planning_root,
        args.focus.is_some(),
        &active_plan_surface,
        run_started_at,
    )?;

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
    if let Some(previous) = preparation.previous_snapshot {
        println!("prior input: {}", previous.display());
    }
    println!("plan files:  {}", summary.plan_count);
    println!("prompt log:  {}", author_phase.prompt_path.display());
    if let Some(response_path) = &author_phase.response_path {
        if response_path.exists() {
            println!("model log:   {}", response_path.display());
        }
    }
    if let Some(review) = codex_review {
        println!("codex prompt: {}", review.prompt_path.display());
        println!("codex stderr: {}", review.stderr_log_path.display());
        println!("codex report: {}", review.report_path.display());
    }
    println!("elapsed:     {}", format_duration(run_started_at.elapsed()));
    Ok(())
}

fn save_verified_corpus_state(
    repo_root: &Path,
    planning_root: &Path,
    focus_requested: bool,
    active_plan_surface: &ActivePlanSurface,
    run_started_at: Instant,
) -> Result<CorpusOutputSummary> {
    print_stage("verify promoted corpus outputs", run_started_at);
    let summary = verify_corpus_outputs(
        repo_root,
        planning_root,
        focus_requested,
        active_plan_surface,
    )?;
    print_stage("save corpus state", run_started_at);
    let mut state = load_state(repo_root)?;
    state.planning_root = Some(planning_root.to_path_buf());
    save_state(repo_root, &state)?;
    Ok(summary)
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
    if args.snapshot_only && args.sync_only {
        bail!(
            "`{} --snapshot-only` cannot be combined with `--sync-only`; use --sync-only later to promote a reviewed snapshot",
            mode.command_label()
        );
    }
    let mut state = load_state(&repo_root)?;
    let resolved_planning_root =
        resolve_generation_planning_root(&repo_root, args.planning_root.as_deref(), &state)?;
    let planning_root = resolved_planning_root.path;
    ensure_planning_root_exists(&planning_root)?;

    let output_dir = if args.plan_only || args.sync_only {
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
    println!("effort:      {}", args.reasoning_effort);
    println!(
        "review pass: {}",
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
    println!("planning source: {}", resolved_planning_root.source.label());
    println!("plan only:   {}", if args.plan_only { "yes" } else { "no" });
    println!(
        "snapshot:    {}",
        if args.snapshot_only { "yes" } else { "no" }
    );
    println!("sync only:   {}", if args.sync_only { "yes" } else { "no" });

    if args.plan_only || args.sync_only {
        if !output_dir.exists() {
            bail!(
                "`{} {}` requires an existing output dir, but {} does not exist",
                mode.command_label(),
                if args.sync_only {
                    "--sync-only"
                } else {
                    "--plan-only"
                },
                output_dir.display()
            );
        }
    } else {
        print_stage("prepare output dir", run_started_at);
        prepare_generation_output_dir(&output_dir)?;
    }

    if args.sync_only {
        print_stage("verify generated outputs", run_started_at);
        let generated_specs = verify_generated_specs(&output_dir)?;
        let implementation_plan = verify_generated_implementation_plan(&output_dir)?;
        let sync_summary = sync_verified_generation_outputs(SyncVerifiedGenerationOutputs {
            repo_root: &repo_root,
            mode,
            planning_root: &planning_root,
            output_dir: &output_dir,
            generated_specs: &generated_specs,
            implementation_plan: &implementation_plan,
            state: &mut state,
            run_started_at,
        })?;
        println!("{} complete", mode.command_label());
        println!("output dir:  {}", output_dir.display());
        println!(
            "root specs:  {} appended, {} skipped",
            sync_summary.root_specs.appended_paths.len(),
            sync_summary.root_specs.skipped_count
        );
        if let Some(root_plan) = sync_summary.root_plan {
            println!("root plan:   {}", root_plan.display());
        } else {
            println!("root plan:   unchanged");
        }
        println!("elapsed:     {}", format_duration(run_started_at.elapsed()));
        return Ok(());
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
        let phase = run_logged_author_phase(
            &repo_root,
            mode.spec_phase_slug(),
            &prompt,
            &args.model,
            &args.reasoning_effort,
            args.max_turns,
            &args.codex_bin,
        )
        .await?;
        let specs = verify_generated_specs(&output_dir)?;
        println!("spec prompt: {}", phase.prompt_path.display());
        if let Some(response_path) = phase.response_path {
            println!("spec log:    {}", response_path.display());
        }
        specs
    };

    let (mut implementation_plan, plan_phase) = {
        print_stage("generate implementation plan", run_started_at);
        let plan_prompt = build_implementation_plan_prompt(
            mode,
            &repo_root,
            &output_dir,
            &generated_specs,
            args.parallelism.clamp(1, 10),
        );
        let plan_phase = run_logged_author_phase(
            &repo_root,
            mode.plan_phase_slug(),
            &plan_prompt,
            &args.model,
            &args.reasoning_effort,
            args.max_turns,
            &args.codex_bin,
        )
        .await?;
        (
            verify_generated_implementation_plan(&output_dir)?,
            Some(plan_phase),
        )
    };
    let codex_review = if args.skip_codex_review {
        None
    } else {
        print_stage("run generation independent review", run_started_at);
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
    let sync_summary = finalize_verified_generation_outputs(
        SyncVerifiedGenerationOutputs {
            repo_root: &repo_root,
            mode,
            planning_root: &planning_root,
            output_dir: &output_dir,
            generated_specs: &generated_specs,
            implementation_plan: &implementation_plan,
            state: &mut state,
            run_started_at,
        },
        args.snapshot_only,
    )?;

    println!("{} complete", mode.command_label());
    println!("repo root:   {}", repo_root.display());
    println!("planning:    {}", planning_root.display());
    println!("output dir:  {}", output_dir.display());
    println!("model:       {}", args.model);
    println!("effort:      {}", args.reasoning_effort);
    println!(
        "review pass: {}",
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
    if let Some(sync_summary) = sync_summary {
        println!(
            "root specs:  {} appended, {} skipped",
            sync_summary.root_specs.appended_paths.len(),
            sync_summary.root_specs.skipped_count
        );
        if let Some(root_plan) = sync_summary.root_plan {
            println!("root plan:   {}", root_plan.display());
        } else {
            println!("root plan:   unchanged");
        }
    } else {
        println!("root specs:  unchanged (snapshot only)");
        println!("root plan:   unchanged (snapshot only)");
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

struct SyncGeneratedOutputsSummary {
    root_specs: SpecSyncSummary,
    root_plan: Option<PathBuf>,
}

struct SyncVerifiedGenerationOutputs<'a> {
    repo_root: &'a Path,
    mode: GenerationMode,
    planning_root: &'a Path,
    output_dir: &'a Path,
    generated_specs: &'a [GeneratedSpecDocument],
    implementation_plan: &'a Path,
    state: &'a mut AutoState,
    run_started_at: Instant,
}

fn finalize_verified_generation_outputs(
    input: SyncVerifiedGenerationOutputs<'_>,
    snapshot_only: bool,
) -> Result<Option<SyncGeneratedOutputsSummary>> {
    if snapshot_only {
        print_stage("save generator state", input.run_started_at);
        save_generation_state(
            input.repo_root,
            input.planning_root,
            input.output_dir,
            input.state,
        )?;
        return Ok(None);
    }

    sync_verified_generation_outputs(input).map(Some)
}

fn sync_verified_generation_outputs(
    input: SyncVerifiedGenerationOutputs<'_>,
) -> Result<SyncGeneratedOutputsSummary> {
    let repo_root = input.repo_root;
    let mode = input.mode;
    let planning_root = input.planning_root;
    let output_dir = input.output_dir;
    let generated_specs = input.generated_specs;
    let implementation_plan = input.implementation_plan;
    let state = input.state;
    let run_started_at = input.run_started_at;

    print_stage("sync generated specs to root", run_started_at);
    let root_specs = sync_generated_specs_to_root(repo_root, generated_specs)?;
    rewrite_generated_plan_spec_refs(implementation_plan, &root_specs)?;
    let root_plan = match mode {
        GenerationMode::Gen => Some(sync_generated_plan_to_root_preserving_open_tasks(
            repo_root,
            implementation_plan,
        )?),
        GenerationMode::Reverse => None,
    };
    print_stage("scrub root outputs", run_started_at);
    scrub_root_generated_outputs(repo_root, mode)?;

    print_stage("save generator state", run_started_at);
    save_generation_state(repo_root, planning_root, output_dir, state)?;
    Ok(SyncGeneratedOutputsSummary {
        root_specs,
        root_plan,
    })
}

fn save_generation_state(
    repo_root: &Path,
    planning_root: &Path,
    output_dir: &Path,
    state: &mut AutoState,
) -> Result<()> {
    state.planning_root = Some(planning_root.to_path_buf());
    state.latest_output_dir = Some(output_dir.to_path_buf());
    save_state(repo_root, state)
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

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        finalize_verified_generation_outputs, GeneratedSpecDocument, GenerationMode,
        SyncVerifiedGenerationOutputs,
    };
    use crate::generation::spec_verify::verify_generated_specs;
    use crate::generation::plan_verify::verify_generated_implementation_plan;
    use crate::state::{load_state, AutoState};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    pub(crate) fn generated_spec(slug: &str, text: &str) -> GeneratedSpecDocument {
        GeneratedSpecDocument {
            path: PathBuf::from(format!("/tmp/{slug}.md")),
            text: text.to_string(),
        }
    }

    pub(crate) fn temp_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("autodev-{label}-{suffix}"));
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    pub(crate) fn write_real_spec(root: &Path) {
        let specs_dir = root.join("specs");
        fs::create_dir_all(&specs_dir).unwrap();
        fs::write(
            specs_dir.join("050426-real.md"),
            "# Specification: Real\n\n## Objective\n\n- ok\n\n## Source Of Truth\n\n- docs owns this fact; runtime owner none; UI consumers none; generated artifacts none; retired surfaces none\n\n## Evidence Status\n\n- verified\n\n## Runtime Contract\n\n- none\n\n## UI Contract\n\n- none\n\n## Generated Artifacts\n\n- none\n\n## Fixture Policy\n\n- production code does not import fixture data\n\n## Retired / Superseded Surfaces\n\n- none\n\n## Acceptance Criteria\n\n- ok\n\n## Verification\n\n- ok\n\n## Review And Closeout\n\n- grep/assertion proof checks the documented requirement\n\n## Open Questions\n\n- none\n",
        )
        .unwrap();
    }

    pub(crate) fn write_valid_corpus(root: &Path) {
        fs::create_dir_all(root.join("plans")).unwrap();
        for (relative, body) in [
            ("ASSESSMENT.md", "# Assessment\n\nReady.\n"),
            ("SPEC.md", "# Spec\n\nRuntime contract.\n"),
            ("PLANS.md", "# Plans\n\n- plans/001-build.md\n"),
        ] {
            fs::write(root.join(relative), body).unwrap();
        }
        fs::write(root.join("GENESIS-REPORT.md"), valid_corpus_report()).unwrap();
        fs::write(root.join("plans/001-build.md"), valid_corpus_execplan()).unwrap();
    }

    pub(crate) fn valid_corpus_report() -> String {
        [
            "# Report",
            "",
            "## Priority Focus",
            "Runtime and user-facing production blockers outrank broad evidence generation.",
            "",
            "## Next Autodev Lever",
            "Run `auto design` before `auto gen` because the UI/runtime contract is the immediate lever in this fixture.",
            "",
            "## Delete Or Demote",
            "Demote stale evidence-only and lower-priority docs-only tracks unless they unblock a named implementation slice.",
            "",
        ]
        .join("\n")
    }

    pub(crate) fn valid_corpus_execplan() -> String {
        [
            "# Build",
            "",
            "## Purpose / Big Picture",
            "Build the thing.",
            "",
            "## Requirements Trace",
            "Trace to SPEC.md.",
            "",
            "## Scope Boundaries",
            "Stay narrow.",
            "",
            "## Progress",
            "- [ ] Implement.",
            "",
            "## Surprises & Discoveries",
            "None yet.",
            "",
            "## Decision Log",
            "None yet.",
            "",
            "## Outcomes & Retrospective",
            "Pending.",
            "",
            "## Context and Orientation",
            "Context.",
            "",
            "## Plan of Work",
            "Work plan.",
            "",
            "## Implementation Units",
            "- Goal: implement docs.",
            "- Files: README.md.",
            "- Test: cargo test exact_filter.",
            "",
            "## Concrete Steps",
            "Do it.",
            "",
            "## Validation and Acceptance",
            "Validate it.",
            "",
            "## Idempotence and Recovery",
            "Safe to rerun.",
            "",
            "## Artifacts and Notes",
            "Notes.",
            "",
            "## Interfaces and Dependencies",
            "None.",
            "",
        ]
        .join("\n")
    }

    pub(crate) fn valid_generated_plan_task() -> String {
        [
            "Spec: `specs/050426-real.md`",
            "Why now: needed",
            "Codebase evidence: present",
            "Source of truth: docs",
            "Runtime owner: none",
            "UI consumers: none",
            "Generated artifacts: none",
            "Fixture boundary: production code cannot import fixture/demo/sample data",
            "Retired surfaces: none",
            "Owns: docs",
            "Integration touchpoints: docs",
            "Scope boundary: docs only",
            "Acceptance criteria: docs land",
            "Verification:",
            "    ```",
            "    cargo test -p docs exact_docs_test",
            "    ```",
            "Required tests:",
            "    - `cargo test -p docs exact_docs_test`",
            "Contract generation: none -- no generated contract",
            "Cross-surface tests: none -- no UI/runtime boundary",
            "Review/closeout: `grep -n docs docs/README.md` plus exact_docs_test catches drift",
            "Completion artifacts: none",
            "Dependencies: none",
            "Estimated scope: S",
            "Completion signal: merged",
        ]
        .join("\n")
    }

    pub(crate) fn write_generated_plan(root: &Path, task_contract: &str) {
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!(
                "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [ ] `DOC-001` Write docs\n{task_contract}\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn snapshot_only_generation_does_not_sync_root_outputs() {
        let repo_root = temp_dir("snapshot-only-root");
        let planning_root = repo_root.join("genesis");
        let output_dir = repo_root.join("gen-050426-000000");
        fs::create_dir_all(&planning_root).unwrap();
        fs::create_dir_all(repo_root.join("specs")).unwrap();
        fs::create_dir_all(repo_root.join("src")).unwrap();
        fs::write(planning_root.join("PLANS.md"), "seed corpus\n").unwrap();
        fs::write(
            repo_root.join("specs").join("050426-real.md"),
            "root spec stays put\n",
        )
        .unwrap();
        fs::write(
            repo_root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\nroot plan stays put\n",
        )
        .unwrap();
        fs::write(repo_root.join("src").join("main.rs"), "fn main() {}\n").unwrap();
        write_real_spec(&output_dir);
        write_generated_plan(&output_dir, &valid_generated_plan_task());

        let original_root_spec =
            fs::read_to_string(repo_root.join("specs").join("050426-real.md")).unwrap();
        let original_root_plan =
            fs::read_to_string(repo_root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        let original_genesis = fs::read_to_string(planning_root.join("PLANS.md")).unwrap();
        let original_source = fs::read_to_string(repo_root.join("src").join("main.rs")).unwrap();
        let generated_specs = verify_generated_specs(&output_dir).unwrap();
        let implementation_plan = verify_generated_implementation_plan(&output_dir).unwrap();
        let mut state = AutoState::default();

        let summary = finalize_verified_generation_outputs(
            SyncVerifiedGenerationOutputs {
                repo_root: &repo_root,
                mode: GenerationMode::Gen,
                planning_root: &planning_root,
                output_dir: &output_dir,
                generated_specs: &generated_specs,
                implementation_plan: &implementation_plan,
                state: &mut state,
                run_started_at: Instant::now(),
            },
            true,
        )
        .unwrap();

        assert!(summary.is_none());
        assert_eq!(
            fs::read_to_string(repo_root.join("specs").join("050426-real.md")).unwrap(),
            original_root_spec
        );
        assert_eq!(
            fs::read_to_string(repo_root.join("IMPLEMENTATION_PLAN.md")).unwrap(),
            original_root_plan
        );
        assert_eq!(
            fs::read_to_string(planning_root.join("PLANS.md")).unwrap(),
            original_genesis
        );
        assert_eq!(
            fs::read_to_string(repo_root.join("src").join("main.rs")).unwrap(),
            original_source
        );
        assert_eq!(
            fs::read_to_string(output_dir.join("IMPLEMENTATION_PLAN.md")).unwrap(),
            format!(
                "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [ ] `DOC-001` Write docs\n{}\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
                valid_generated_plan_task()
            )
        );
        let saved_state = load_state(&repo_root).unwrap();
        assert_eq!(
            saved_state.planning_root.as_deref(),
            Some(planning_root.as_path())
        );
        assert_eq!(
            saved_state.latest_output_dir.as_deref(),
            Some(output_dir.as_path())
        );
    }
}
