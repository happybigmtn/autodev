use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::codex_exec::run_codex_exec_max_context;
use crate::parallel_command;
use crate::qa_only_command::{
    allowed_report_only_dirty_paths, collect_dirty_state, print_final_status_block,
    report_only_dirty_state_report,
};
use crate::util::{
    atomic_write, binary_provenance_line, ensure_repo_layout, git_repo_root, timestamp_slug,
};
use crate::{DesignArgs, ParallelAction, ParallelArgs, ParallelCargoTarget, SuperArgs};

const DESIGN_ARTIFACTS: [&str; 6] = [
    "DESIGN-AUDIT.md",
    "DESIGN-SYSTEM-PROPOSAL.md",
    "ENGINE-UI-CONTRACT.md",
    "FRONTEND-QA.md",
    "DESIGN-PLAN-ITEMS.md",
    "DESIGN-REPORT.md",
];

#[derive(Serialize)]
struct DesignManifest {
    run_id: String,
    repo_root: String,
    planning_root: Option<String>,
    output_dir: String,
    prompt: Option<String>,
    model: String,
    reasoning_effort: String,
    apply: bool,
    resolve: bool,
    resolve_passes: usize,
    skip_qa: bool,
    binary: String,
}

pub(crate) async fn run_design(args: DesignArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    if args.resolve {
        return run_design_resolution(args, DesignRunKind::Resolve).await;
    }

    let run_id = timestamp_slug();
    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join(".auto").join("design").join(&run_id));
    let planning_root = args.planning_root.clone().or_else(|| {
        repo_root
            .join("genesis")
            .exists()
            .then(|| repo_root.join("genesis"))
    });

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let manifest = DesignManifest {
        run_id,
        repo_root: repo_root.display().to_string(),
        planning_root: planning_root
            .as_ref()
            .map(|path| path.display().to_string()),
        output_dir: output_dir.display().to_string(),
        prompt: args.prompt.clone(),
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        apply: args.apply,
        resolve: false,
        resolve_passes: 1,
        skip_qa: args.skip_qa,
        binary: binary_provenance_line(),
    };
    atomic_write(
        &output_dir.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            output_dir.join("manifest.json").display()
        )
    })?;

    let prompt = build_design_prompt(
        &repo_root,
        planning_root.as_deref(),
        &output_dir,
        args.prompt.as_deref(),
        args.apply,
        args.skip_qa,
        DesignRunKind::Standalone,
    );
    let prompt_path = output_dir.join("design-prompt.md");
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;

    println!("auto design");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    if let Some(planning_root) = &planning_root {
        println!("planning:    {}", planning_root.display());
    }
    println!("output dir:  {}", output_dir.display());
    println!("model:       {}", args.model);
    println!("effort:      {}", args.reasoning_effort);
    println!("apply:       {}", if args.apply { "yes" } else { "no" });
    println!(
        "qa:          {}",
        if args.skip_qa { "skipped" } else { "enabled" }
    );
    println!("prompt log:  {}", prompt_path.display());

    if args.dry_run {
        println!("\n{prompt}");
        print_final_status_block(
            "design dry-run prompt rendered",
            &[
                output_dir.join("manifest.json").display().to_string(),
                prompt_path.display().to_string(),
            ],
            "design worker not invoked",
            "run auto design without --dry-run to produce DESIGN-REPORT.md",
        );
        return Ok(());
    }

    let report_only_baseline = if args.apply {
        None
    } else {
        Some(collect_dirty_state(&repo_root)?)
    };
    let phase_result = run_design_codex_phase(
        &repo_root,
        &output_dir,
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        "auto-design",
    )
    .await;
    if let Some(baseline) = &report_only_baseline {
        enforce_design_report_only_write_boundary(&repo_root, &output_dir, baseline)?;
    }
    phase_result?;
    verify_design_artifacts(&output_dir)?;
    println!("status:      design artifacts verified");
    print_final_status_block(
        "design artifacts verified",
        &DESIGN_ARTIFACTS
            .iter()
            .map(|artifact| output_dir.join(artifact).display().to_string())
            .chain([
                output_dir.join("manifest.json").display().to_string(),
                prompt_path.display().to_string(),
                output_dir
                    .join("auto-design-stderr.log")
                    .display()
                    .to_string(),
            ])
            .collect::<Vec<_>>(),
        "none",
        "review DESIGN-REPORT.md verdict before running auto gen, auto parallel, or auto design --resolve",
    );
    Ok(())
}

async fn run_design_resolution(args: DesignArgs, kind: DesignRunKind) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let run_id = timestamp_slug();
    let output_root = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join(".auto").join("design").join(&run_id));
    let planning_root = args.planning_root.clone().or_else(|| {
        repo_root
            .join("genesis")
            .exists()
            .then(|| repo_root.join("genesis"))
    });
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;

    let max_passes = args.resolve_passes.max(1);
    let manifest = DesignManifest {
        run_id,
        repo_root: repo_root.display().to_string(),
        planning_root: planning_root
            .as_ref()
            .map(|path| path.display().to_string()),
        output_dir: output_root.display().to_string(),
        prompt: args.prompt.clone(),
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        apply: true,
        resolve: true,
        resolve_passes: max_passes,
        skip_qa: args.skip_qa,
        binary: binary_provenance_line(),
    };
    atomic_write(
        &output_root.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            output_root.join("manifest.json").display()
        )
    })?;

    println!("auto design --resolve");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    if let Some(planning_root) = &planning_root {
        println!("planning:    {}", planning_root.display());
    }
    println!("output root: {}", output_root.display());
    println!("model:       {}", args.model);
    println!("effort:      {}", args.reasoning_effort);
    println!("passes:      {max_passes}");
    println!("workers:     {}", args.max_concurrent_workers.max(1));
    println!(
        "qa:          {}",
        if args.skip_qa { "skipped" } else { "enabled" }
    );

    if args.dry_run {
        let prompt = build_design_prompt(
            &repo_root,
            planning_root.as_deref(),
            &output_root.join("pass-01"),
            args.prompt.as_deref(),
            true,
            args.skip_qa,
            kind,
        );
        println!("\n{prompt}");
        print_final_status_block(
            "design resolve dry-run prompt rendered",
            &[output_root.join("manifest.json").display().to_string()],
            "design worker not invoked",
            "run auto design --resolve without --dry-run to produce DESIGN-REPORT.md",
        );
        return Ok(());
    }

    let mut last_report = None;
    for pass in 1..=max_passes {
        let pass_dir = output_root.join(format!("pass-{pass:02}"));
        fs::create_dir_all(&pass_dir)
            .with_context(|| format!("failed to create {}", pass_dir.display()))?;
        println!("stage:       design resolve pass {pass}/{max_passes}");
        let prompt = build_design_prompt(
            &repo_root,
            planning_root.as_deref(),
            &pass_dir,
            args.prompt.as_deref(),
            true,
            args.skip_qa,
            kind,
        );
        let prompt_path = pass_dir.join("design-prompt.md");
        atomic_write(&prompt_path, prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        run_design_codex_phase(
            &repo_root,
            &pass_dir,
            &prompt,
            &args.model,
            &args.reasoning_effort,
            &args.codex_bin,
            &format!("auto-design-resolve-pass-{pass:02}"),
        )
        .await?;
        verify_design_artifacts(&pass_dir)?;
        last_report = Some(pass_dir.join("DESIGN-REPORT.md"));
        write_design_resolution_status(&output_root, pass, max_passes, &pass_dir, "audited")?;
        if design_report_is_go(&pass_dir)? {
            write_design_resolution_status(&output_root, pass, max_passes, &pass_dir, "verified")?;
            println!("status:      design resolve verified");
            println!("pass dir:    {}", pass_dir.display());
            print_final_status_block(
                "design resolve verified",
                &[
                    pass_dir.join("DESIGN-REPORT.md").display().to_string(),
                    output_root
                        .join("DESIGN-RESOLVE-STATUS.md")
                        .display()
                        .to_string(),
                ],
                "none",
                "continue the production campaign or run auto gen with the promoted design contract",
            );
            return Ok(());
        }
        if pass == max_passes {
            break;
        }
        if let Some(promoted) = promote_design_plan_items_to_root_queue(&repo_root, &pass_dir)? {
            println!("status:      promoted {promoted} design task(s) into IMPLEMENTATION_PLAN.md");
        }
        println!("stage:       design implementation pass {pass}/{max_passes}");
        run_design_parallel_pass(&args, &output_root, pass).await?;
        write_design_resolution_status(
            &output_root,
            pass,
            max_passes,
            &pass_dir,
            "implementation-pass-complete",
        )?;
    }

    let report = last_report
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| output_root.display().to_string());
    bail!("design resolve did not reach `Verdict: GO` after {max_passes} pass(es); latest report: {report}")
}

fn enforce_design_report_only_write_boundary(
    repo_root: &Path,
    output_dir: &Path,
    baseline: &[crate::qa_only_command::DirtyEntry],
) -> Result<()> {
    let allowed_paths =
        allowed_report_only_dirty_paths(repo_root, output_dir, ".auto/design", ".auto/design");
    let dirty_report = report_only_dirty_state_report(repo_root, baseline, &allowed_paths)?;
    if dirty_report.has_violations() {
        bail!(
            "{}",
            dirty_report.render("auto design", "the design output directory")
        );
    }
    if dirty_report.has_preexisting_dirty_state() {
        eprintln!("{}", dirty_report.render_preexisting());
    }
    Ok(())
}

async fn run_design_parallel_pass(
    args: &DesignArgs,
    output_root: &Path,
    pass: usize,
) -> Result<()> {
    parallel_command::run_parallel_inline(ParallelArgs {
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
        run_root: Some(output_root.join("parallel").join(format!("pass-{pass:02}"))),
        codex_bin: args.codex_bin.clone(),
        claude: false,
        max_turns: None,
        max_retries: 2,
    })
    .await
}

fn write_design_resolution_status(
    output_root: &Path,
    pass: usize,
    max_passes: usize,
    pass_dir: &Path,
    status: &str,
) -> Result<()> {
    let markdown = format!(
        "# Design Resolve Status\n\n- Status: `{status}`\n- Pass: `{pass}/{max_passes}`\n- Latest artifacts: `{}`\n- Latest report: `{}`\n",
        pass_dir.display(),
        pass_dir.join("DESIGN-REPORT.md").display()
    );
    atomic_write(
        &output_root.join("DESIGN-RESOLVE-STATUS.md"),
        markdown.as_bytes(),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            output_root.join("DESIGN-RESOLVE-STATUS.md").display()
        )
    })
}

pub(crate) async fn run_super_design_module(
    args: &SuperArgs,
    repo_root: &Path,
    planning_root: &Path,
    super_root: &Path,
) -> Result<()> {
    if !args.no_execute && args.design_resolve_passes > 1 {
        let design_args = DesignArgs {
            prompt: args.prompt.clone().or_else(|| args.focus.clone()),
            planning_root: Some(planning_root.to_path_buf()),
            output_dir: Some(super_root.join("design")),
            apply: true,
            resolve: true,
            resolve_passes: args.design_resolve_passes,
            max_concurrent_workers: args.max_concurrent_workers.max(1),
            max_iterations: args.max_iterations,
            worker_model: args.worker_model.clone(),
            worker_reasoning_effort: args.worker_reasoning_effort.clone(),
            branch: args.branch.clone(),
            reference_repos: args.reference_repos.clone(),
            skip_qa: false,
            model: args.model.clone(),
            reasoning_effort: args.reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            dry_run: false,
        };
        return run_design_resolution(design_args, DesignRunKind::SuperResolve).await;
    }

    let design_root = super_root.join("design");
    fs::create_dir_all(&design_root)
        .with_context(|| format!("failed to create {}", design_root.display()))?;
    let prompt = build_design_prompt(
        repo_root,
        Some(planning_root),
        &design_root,
        args.prompt.as_deref().or(args.focus.as_deref()),
        true,
        false,
        DesignRunKind::Super,
    );
    let prompt_path = design_root.join("design-prompt.md");
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    run_design_codex_phase(
        repo_root,
        &design_root,
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        "auto-super-design",
    )
    .await?;
    verify_design_artifacts(&design_root)?;
    require_design_go(&design_root)?;
    Ok(())
}

fn promote_design_plan_items_to_root_queue(
    repo_root: &Path,
    pass_dir: &Path,
) -> Result<Option<usize>> {
    let plan_items_path = pass_dir.join("DESIGN-PLAN-ITEMS.md");
    let root_plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_items_path.exists() || !root_plan_path.exists() {
        return Ok(None);
    }

    let plan_items = fs::read_to_string(&plan_items_path)
        .with_context(|| format!("failed to read {}", plan_items_path.display()))?;
    let mut root_plan = fs::read_to_string(&root_plan_path)
        .with_context(|| format!("failed to read {}", root_plan_path.display()))?;
    let blocks = extract_unchecked_design_plan_item_blocks(&plan_items);
    if blocks.is_empty() {
        return Ok(None);
    }

    let mut missing = Vec::new();
    for block in blocks {
        let Some(task_id) = design_plan_block_task_id(&block) else {
            continue;
        };
        let needle = format!("`{task_id}`");
        if !root_plan.contains(&needle) {
            missing.push(block);
        }
    }
    if missing.is_empty() {
        return Ok(None);
    }

    let insertion = format!(
        "\n<!-- auto design promoted unresolved design/runtime tasks from {} -->\n{}\n",
        plan_items_path.display(),
        missing.join("\n\n")
    );
    if let Some(index) = root_plan.find("\n## Follow-On Work") {
        root_plan.insert_str(index, &insertion);
    } else {
        if !root_plan.ends_with('\n') {
            root_plan.push('\n');
        }
        root_plan.push_str(&insertion);
    }
    atomic_write(&root_plan_path, root_plan.as_bytes())
        .with_context(|| format!("failed to write {}", root_plan_path.display()))?;
    Ok(Some(missing.len()))
}

fn extract_unchecked_design_plan_item_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("- [ ] `") || line.trim_start().starts_with("- [~] `") {
            if !current.is_empty() {
                blocks.push(current.join("\n"));
                current.clear();
            }
            current.push(line.to_string());
        } else if !current.is_empty() {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        blocks.push(current.join("\n"));
    }
    blocks
        .into_iter()
        .filter(|block| {
            let lower = block.to_ascii_lowercase();
            block.contains("Dependencies:")
                && block.contains("Verification:")
                && (lower.contains("runtime owner")
                    || lower.contains("source of truth")
                    || lower.contains("ui consumer"))
        })
        .collect()
}

fn design_plan_block_task_id(block: &str) -> Option<String> {
    let header = block.lines().next()?.trim_start();
    let rest = header
        .strip_prefix("- [ ] `")
        .or_else(|| header.strip_prefix("- [~] `"))?;
    let end = rest.find('`')?;
    Some(rest[..end].trim().to_string())
}

async fn run_design_codex_phase(
    repo_root: &Path,
    output_dir: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    context_label: &str,
) -> Result<()> {
    let stderr_path = output_dir.join(format!("{context_label}-stderr.log"));
    println!("phase:       {context_label}");
    println!("stderr log:  {}", stderr_path.display());
    let status = run_codex_exec_max_context(
        repo_root,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
        &stderr_path,
        None,
        context_label,
    )
    .await?;
    if !status.success() {
        bail!(
            "{context_label} failed with status {status}; see {}",
            stderr_path.display()
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DesignRunKind {
    Standalone,
    Resolve,
    Super,
    SuperResolve,
}

fn build_design_prompt(
    repo_root: &Path,
    planning_root: Option<&Path>,
    output_dir: &Path,
    operator_prompt: Option<&str>,
    apply: bool,
    skip_qa: bool,
    kind: DesignRunKind,
) -> String {
    let planning_clause = planning_root
        .map(|path| {
            format!(
                "- Planning corpus root: `{}`. If present, treat its `DESIGN.md` as planning input, not automatically as live product truth.",
                path.display()
            )
        })
        .unwrap_or_else(|| "- Planning corpus root: none detected.".to_string());
    let prompt_clause = operator_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\nOperator focus:\n{value}\n"))
        .unwrap_or_default();
    let edit_clause = if apply {
        match kind {
            DesignRunKind::Standalone => {
                "- You may make bounded edits to root `DESIGN.md`, design-relevant `specs/*.md`, and `IMPLEMENTATION_PLAN.md` when they are necessary to encode design/runtime truth. Do not edit application source code."
            }
            DesignRunKind::Resolve => {
                "- You may make bounded edits to root `DESIGN.md`, design-relevant `specs/*.md`, and `IMPLEMENTATION_PLAN.md` when they are necessary to encode design/runtime truth. Do not edit application source code in this design pass; unresolved source/runtime work must become executable implementation-plan tasks for the following `auto parallel` pass."
            }
            DesignRunKind::Super => {
                "- You may amend root `DESIGN.md` and the planning corpus design files so `auto gen` inherits the design contract. Do not edit source code, root specs, or root `IMPLEMENTATION_PLAN.md` in this pre-generation super module."
            }
            DesignRunKind::SuperResolve => {
                "- You may make bounded edits to root `DESIGN.md`, design-relevant `specs/*.md`, planning corpus design files, and `IMPLEMENTATION_PLAN.md` when needed to encode design/runtime truth. Do not edit application source code in this design pass; unresolved source/runtime work must become executable implementation-plan tasks for the following `auto parallel` pass."
            }
        }
    } else {
        "- Report-only mode: do not edit repo files outside the output directory. Put proposed patches and plan items in the artifacts."
    };
    let qa_clause = if skip_qa {
        "- Browser/runtime QA is explicitly skipped. Still inspect code-level UI/runtime contracts and list the skipped QA as a blocker where it matters."
    } else {
        "- Run the narrowest truthful frontend QA available: existing browser/Playwright/gstack/agent-browser tooling, local dev server smoke, route/API probes, console-error checks, and responsive checks. If no app can run, record the exact blocker and still audit static frontend/runtime bindings."
    };
    let stage_clause = match kind {
        DesignRunKind::Standalone => "You are running standalone `auto design`.",
        DesignRunKind::Resolve => {
            "You are running `auto design --resolve`: diagnose design/runtime drift, encode durable doctrine and queue-ready implementation tasks, then let implementation lanes repair source code before you re-verify."
        }
        DesignRunKind::Super => {
            "You are the `auto super` design perfection gate running after corpus and before generation. Design is first-class and blocking: do not subordinate, soften, or defer design/runtime integrity findings into a later generic review."
        }
        DesignRunKind::SuperResolve => {
            "You are the `auto super` design repair gate running after corpus and before generation. Design is first-class and blocking: diagnose design/runtime drift, encode executable repair work, let implementation lanes fix it, and only allow the CEO production campaign to continue after `Verdict: GO`."
        }
    };

    format!(
        r#"{stage_clause}

Repository: `{repo_root}`
{planning_clause}
Output directory: `{output_dir}`
{prompt_clause}

Your job is to synthesize expert design review, design-system consultation, web interface guidelines, frontend design craft, and QA into a repo-native design contract. This is not a fake mockup generator. This is a design/runtime integrity pass that must be perfected before broader functional lanes proceed.

Use these lenses together:
- Plan design review: rate and close gaps in information architecture, interaction states, journey, AI-slop risk, design-system alignment, responsive behavior, accessibility, and unresolved design decisions.
- Design consultation: infer or improve a coherent product-specific system: aesthetic direction, safe category conventions, deliberate creative risks, typography, color, spacing, layout, motion, and component vocabulary.
- Web interface guidelines: fetch or recall current web UI/a11y best practices and apply them to actual frontend files, not generic screenshots.
- Frontend design craft: avoid generic AI aesthetics, overused fonts, purple-gradient defaults, meaningless cards, generic dashboard widgets, and product-copy fog. Existing design tokens and component patterns outrank generic advice.
- QA discipline: test what a real user can do, check console/runtime errors after interactions, verify responsive states, and capture evidence or exact blockers.
- Additional skills.sh design synthesis: use product-frontend critique for message clarity, frontend-ui-ux engineering for accessible polish and micro-interactions, and design-token extraction discipline from design-system skills. Do not require external paid design tools or infinite-canvas mockup systems.

Required first reads:
- `AGENTS.md` or repo-local agent instructions.
- Product doctrine: `README.md`, `DESIGN.md`, GDD/OS/invariant docs when present.
- Planning truth: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, active `specs/`, and `{planning_root_display}` when present.
- Frontend code: app/routes/components/styles/design tokens/tests/build scripts.
- Runtime/engine/API code that owns facts displayed by UI.
- Generated bindings/schemas/client code and their regeneration commands when present.

Hard rules:
- Do not create fake mockups as acceptance evidence. Preview pages are allowed only as proposals and must be labeled non-authoritative.
- Do not invent frontend bindings, constants, catalogs, balances, settlement math, eligibility rules, risk classes, or status derivations. UI must consume runtime/API/generated truth.
- If the design calls for new data, name the runtime owner, API/schema change, generator, consumer, and test/readback proof.
- Prefer existing helpers, generated clients, hooks, stores, route loaders, and design tokens over new manual glue.
- Production code must not import fixture/demo/sample data as fallback truth.
- Retired or superseded screens/specs must be deleted, archived, tombstoned, or explicitly blocked from active implementation.
- A design improvement is not complete unless it names the engine/API contract and the proof that would fail if UI drifts again.

{edit_clause}
{qa_clause}

Write these non-empty artifacts under `{output_dir}`:
1. `DESIGN-AUDIT.md`
   - Current UI/design-system inventory.
   - Existing frontend design signals and reusable components/tokens.
   - 0-10 ratings for the seven plan-design-review dimensions.
   - AI-slop risks and modern/stunning UI opportunities specific to this product.
2. `DESIGN-SYSTEM-PROPOSAL.md`
   - Proposed or revised `DESIGN.md` doctrine.
   - Aesthetic thesis, safe choices, deliberate risks, typography, color, spacing, layout, motion, components, empty/error/loading states, responsive and accessibility rules.
   - Explicitly explain what belongs in real product UI versus non-authoritative concept previews.
3. `ENGINE-UI-CONTRACT.md`
   - Table of UI surfaces, runtime/API source of truth, existing helpers/bindings, generated artifacts, fixture boundary, and required drift guard.
   - Call out every manual binding or duplicated frontend derivation found.
4. `FRONTEND-QA.md`
   - Commands/URLs/tools used, screenshots or artifact paths if produced, console/runtime findings, responsive findings, and exact blockers.
   - Separate confirmed breaks from hypotheses and from skipped/unavailable checks.
5. `DESIGN-PLAN-ITEMS.md`
   - Queue-ready plan items for unresolved design/runtime gaps using the repo's implementation-plan field style.
   - Every item must include runtime owner, UI consumers, generated artifacts, contract generation, cross-surface proof, and closeout review.
6. `DESIGN-REPORT.md`
   - Executive summary, files changed if any, recommended next workflow step, and GO/NO-GO for design-aware implementation.
   - In the `auto super` flow, `Verdict: NO-GO` blocks the CEO production campaign until design/runtime integrity is repaired.

If `{apply_status}`:
- Update `DESIGN.md` only with durable doctrine grounded in the live product and existing frontend.
- In standalone mode, add or amend plan/spec items only for real unresolved work. In super mode, prefer amending the planning corpus so `auto gen` emits the queue unless this is a resolve pass.
- In resolve mode, every unresolved NO-GO issue that requires source/runtime/UI changes must also be inserted into root `IMPLEMENTATION_PLAN.md` as an unchecked, dependency-ready task unless it has a concrete dependency. Use stable `DESIGN-*` task IDs, machine-readable `Dependencies:`, narrow `Owns:`, runtime owner, UI consumer, generated artifact, fixture boundary, and executable verification fields so `auto parallel` can pick it up immediately.
- In resolve mode, do not leave the only actionable repair work inside `DESIGN-PLAN-ITEMS.md`; that file is an audit artifact, while `IMPLEMENTATION_PLAN.md` is the executor queue.
- Do not mark any implementation item complete.

Final line of `DESIGN-REPORT.md` must be exactly one of:
- `Verdict: GO`
- `Verdict: NO-GO`
"#,
        stage_clause = stage_clause,
        repo_root = repo_root.display(),
        planning_clause = planning_clause,
        output_dir = output_dir.display(),
        prompt_clause = prompt_clause,
        planning_root_display = planning_root
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "no planning corpus".to_string()),
        edit_clause = edit_clause,
        qa_clause = qa_clause,
        apply_status = if apply {
            "applying edits is enabled"
        } else {
            "report-only mode is enabled"
        },
    )
}

fn verify_design_artifacts(output_dir: &Path) -> Result<()> {
    for artifact in DESIGN_ARTIFACTS {
        require_nonempty_file(&output_dir.join(artifact))?;
    }
    let report_path = output_dir.join("DESIGN-REPORT.md");
    let report = fs::read_to_string(&report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    if !report
        .lines()
        .any(|line| matches!(line.trim(), "Verdict: GO" | "Verdict: NO-GO"))
    {
        bail!(
            "{} must contain `Verdict: GO` or `Verdict: NO-GO`",
            report_path.display()
        );
    }
    Ok(())
}

fn require_design_go(output_dir: &Path) -> Result<()> {
    if design_report_is_go(output_dir)? {
        return Ok(());
    }
    let report_path = output_dir.join("DESIGN-REPORT.md");
    bail!(
        "design perfection gate did not approve downstream generation; expected `Verdict: GO` in {}",
        report_path.display()
    );
}

fn design_report_is_go(output_dir: &Path) -> Result<bool> {
    let report_path = output_dir.join("DESIGN-REPORT.md");
    let report = fs::read_to_string(&report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    Ok(report.lines().any(|line| line.trim() == "Verdict: GO"))
}

fn require_nonempty_file(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("required design artifact missing: {}", path.display()))?;
    if metadata.len() == 0 {
        bail!("required design artifact is empty: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_design_prompt, enforce_design_report_only_write_boundary,
        promote_design_plan_items_to_root_queue, DesignRunKind,
    };
    use crate::qa_only_command::{collect_dirty_state, format_final_status_block};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn design_prompt_rejects_fake_mockup_and_requires_runtime_truth() {
        let prompt = build_design_prompt(
            &PathBuf::from("/repo"),
            Some(&PathBuf::from("/repo/genesis")),
            &PathBuf::from("/repo/.auto/design/run"),
            Some("make the UI better"),
            true,
            false,
            DesignRunKind::Standalone,
        );

        assert!(prompt.contains("not a fake mockup generator"));
        assert!(prompt.contains("Do not create fake mockups as acceptance evidence"));
        assert!(prompt.contains("UI must consume runtime/API/generated truth"));
        assert!(prompt.contains("ENGINE-UI-CONTRACT.md"));
        assert!(prompt.contains("FRONTEND-QA.md"));
    }

    #[test]
    fn design_report_only_rejects_disallowed_dirty_state() {
        let root = temp_dir("design-report-only-boundary");
        run_git_in(&root, ["init"]);
        run_git_in(&root, ["config", "user.name", "autodev tests"]);
        run_git_in(&root, ["config", "user.email", "autodev@example.com"]);
        fs::write(root.join("README.md"), "# temp\n").unwrap();
        run_git_in(&root, ["add", "README.md"]);
        run_git_in(&root, ["commit", "-m", "init"]);
        let output_dir = root.join(".auto/design/run");
        fs::create_dir_all(&output_dir).unwrap();
        let baseline = collect_dirty_state(&root).unwrap();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn changed() {}\n").unwrap();

        let err = enforce_design_report_only_write_boundary(&root, &output_dir, &baseline)
            .expect_err("source edits should violate report-only design boundary");
        assert!(err.to_string().contains("write boundary violation"));
        assert!(err.to_string().contains("src/lib.rs"));
    }

    #[test]
    fn design_final_status_block_names_operator_contract_fields() {
        let block = format_final_status_block(
            "design artifacts verified",
            &[".auto/design/run/DESIGN-REPORT.md".to_string()],
            "none",
            "review DESIGN-REPORT.md verdict",
        );

        assert!(block.contains("status:"));
        assert!(block.contains("files written:"));
        assert!(block.contains("blockers:"));
        assert!(block.contains("next step:"));
        assert!(block.contains("DESIGN-REPORT.md"));
    }

    #[test]
    fn super_design_prompt_keeps_pre_generation_edit_boundary() {
        let prompt = build_design_prompt(
            &PathBuf::from("/repo"),
            Some(&PathBuf::from("/repo/genesis")),
            &PathBuf::from("/repo/.auto/super/run/design"),
            None,
            true,
            false,
            DesignRunKind::Super,
        );

        assert!(prompt.contains("auto super` design perfection gate"));
        assert!(prompt.contains("Design is first-class and blocking"));
        assert!(prompt
            .contains("Do not edit source code, root specs, or root `IMPLEMENTATION_PLAN.md`"));
    }

    #[test]
    fn design_plan_items_promote_missing_executor_tasks_to_root_queue() {
        let root = temp_dir("design-plan-promotion");
        let pass_dir = root.join(".auto/design/pass-01");
        fs::create_dir_all(&pass_dir).unwrap();
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n",
        )
        .unwrap();
        fs::write(
            pass_dir.join("DESIGN-PLAN-ITEMS.md"),
            "- [ ] `DESIGN-001` Runtime-backed surface\n\n    Runtime owner: `src/api.rs`\n    UI consumers: `src/App.tsx`\n    Verification: `cargo test design_001`\n    Dependencies: none\n",
        )
        .unwrap();

        assert_eq!(
            promote_design_plan_items_to_root_queue(&root, &pass_dir).unwrap(),
            Some(1)
        );
        let root_plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        assert!(root_plan.contains("`DESIGN-001`"));
        assert!(
            root_plan.find("`DESIGN-001`").unwrap() < root_plan.find("## Follow-On Work").unwrap()
        );

        assert_eq!(
            promote_design_plan_items_to_root_queue(&root, &pass_dir).unwrap(),
            None
        );
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("autodev-{label}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn run_git_in<'a>(repo: &Path, args: impl IntoIterator<Item = &'a str>) {
        let output = std::process::Command::new("git")
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
    }
}
