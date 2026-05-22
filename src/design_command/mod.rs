mod promotion;
mod prompt;
mod resolve;
#[cfg(test)]
mod testkit;
mod verify;

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::codex_exec::run_codex_exec_max_context;
use crate::design_command::prompt::{build_design_prompt, DesignRunKind, DESIGN_ARTIFACTS};
use crate::design_command::resolve::run_design_resolution;
use crate::design_command::verify::{require_design_go, verify_design_artifacts};
use crate::qa_only_command::{
    allowed_report_only_dirty_paths, collect_dirty_state, print_final_status_block,
    report_only_dirty_state_report,
};
use crate::util::{
    atomic_write, binary_provenance_line, ensure_repo_layout, git_repo_root, timestamp_slug,
};
use crate::{DesignArgs, SuperArgs};

#[derive(Serialize)]
pub(crate) struct DesignManifest {
    pub(crate) run_id: String,
    pub(crate) repo_root: String,
    pub(crate) planning_root: Option<String>,
    pub(crate) output_dir: String,
    pub(crate) prompt: Option<String>,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
    pub(crate) apply: bool,
    pub(crate) resolve: bool,
    pub(crate) resolve_passes: usize,
    pub(crate) skip_qa: bool,
    pub(crate) binary: String,
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
    verify_design_artifacts(&output_dir, args.prompt.as_deref())?;
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
    verify_design_artifacts(
        &design_root,
        args.prompt.as_deref().or(args.focus.as_deref()),
    )?;
    require_design_go(&design_root)?;
    Ok(())
}

pub(crate) async fn run_design_codex_phase(
    repo_root: &Path,
    output_dir: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    context_label: &str,
) -> Result<()> {
    let stderr_path = output_dir.join(format!("{context_label}-stderr.log"));
    let claude_route = crate::claude_exec::looks_like_claude_model(model);
    println!(
        "phase:       {context_label} | backend: {}",
        if claude_route { "claude" } else { "codex" }
    );
    println!("stderr log:  {}", stderr_path.display());
    let status = if claude_route {
        crate::claude_exec::run_claude_exec(
            repo_root,
            prompt,
            model,
            reasoning_effort,
            None,
            &stderr_path,
            None,
            context_label,
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
            context_label,
        )
        .await?
    };
    if !status.success() {
        bail!(
            "{context_label} failed with status {status}; see {}",
            stderr_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::enforce_design_report_only_write_boundary;
    use crate::design_command::testkit::{run_git_in, temp_dir};
    use crate::qa_only_command::{collect_dirty_state, format_final_status_block};

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
}
