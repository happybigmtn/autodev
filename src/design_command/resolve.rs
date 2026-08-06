use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::design_command::promotion::promote_design_plan_items_to_root_queue;
use crate::design_command::prompt::{
    build_design_parallel_prompt, build_design_prompt, DesignRunKind,
};
use crate::design_command::verify::{design_report_is_go, verify_design_artifacts};
use crate::design_command::{run_design_codex_phase, DesignManifest};
use crate::parallel_command;
use crate::qa_only_command::print_final_status_block;
use crate::task_parser::{parse_tasks, TaskStatus};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, binary_provenance_line, ensure_repo_layout,
    git_repo_root, git_stdout, timestamp_slug,
};
use crate::{DesignArgs, ParallelAction, ParallelArgs, ParallelCargoTarget};

pub(crate) async fn run_design_resolution(args: DesignArgs, kind: DesignRunKind) -> Result<()> {
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
    let mut pass = 1usize;
    let mut recovery_extensions = 0usize;
    let max_recovery_extensions = match kind {
        DesignRunKind::SuperResolve => max_passes,
        _ => 0,
    };
    while pass <= max_passes + max_recovery_extensions {
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
        verify_design_artifacts(&pass_dir, args.prompt.as_deref())?;
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
        if pass >= max_passes {
            let promoted = preserve_final_no_go_design_plan_items(
                &repo_root,
                &output_root,
                pass,
                max_passes,
                &pass_dir,
            )?;
            if let Some(promoted) = promoted {
                println!(
                    "status:      promoted {promoted} design task(s) into IMPLEMENTATION_PLAN.md"
                );
            }
            if recovery_extensions < max_recovery_extensions
                && root_queue_has_dependency_ready_repair_tasks(&repo_root)?
            {
                recovery_extensions += 1;
                println!(
                    "stage:       final NO-GO repair implementation {recovery_extensions}/{max_recovery_extensions}"
                );
                run_design_parallel_pass(&args, &output_root, pass).await?;
                write_design_resolution_status(
                    &output_root,
                    pass,
                    max_passes,
                    &pass_dir,
                    "final-no-go-repair-pass-complete",
                )?;
                pass += 1;
                continue;
            }
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
        pass += 1;
    }

    let report = last_report
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| output_root.display().to_string());
    if kind == DesignRunKind::SuperResolve {
        try_checkpoint_final_design_resolve_state(&repo_root, args.branch.as_deref());
    }
    bail!("design resolve did not reach `Verdict: GO` after {max_passes} pass(es); latest report: {report}")
}

fn try_checkpoint_final_design_resolve_state(repo_root: &Path, branch: Option<&str>) {
    let target_branch = branch
        .map(str::to_string)
        .or_else(|| {
            git_stdout(repo_root, ["branch", "--show-current"])
                .ok()
                .map(|branch| branch.trim().to_string())
        })
        .unwrap_or_default();
    if target_branch.is_empty() {
        eprintln!(
            "warning: design resolve ended NO-GO with possible repo edits, but no checked-out branch was available for checkpointing"
        );
        return;
    }
    match auto_checkpoint_if_needed(repo_root, &target_branch, "design resolve NO-GO checkpoint") {
        Ok(Some(commit)) => eprintln!(
            "checkpoint: committed final design resolve state at {commit} before reporting NO-GO"
        ),
        Ok(None) => {}
        Err(err) => eprintln!(
            "warning: failed to checkpoint final design resolve state before reporting NO-GO: {err:#}"
        ),
    }
}

pub(crate) fn root_queue_has_dependency_ready_repair_tasks(repo_root: &Path) -> Result<bool> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(false);
    }
    let plan = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let tasks = parse_tasks(&plan);
    let completed = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Done)
        .map(|task| task.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    Ok(tasks.iter().any(|task| {
        matches!(task.status, TaskStatus::Pending | TaskStatus::Partial)
            && task
                .dependencies
                .iter()
                .all(|dependency| completed.contains(dependency.as_str()))
    }))
}

async fn run_design_parallel_pass(
    args: &DesignArgs,
    output_root: &Path,
    pass: usize,
) -> Result<()> {
    let run_root = output_root.join("parallel").join(format!("pass-{pass:02}"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let prompt_path = run_root.join("design-resolve-parallel-prompt.md");
    let prompt = build_design_parallel_prompt(output_root, pass);
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    parallel_command::run_parallel_inline(ParallelArgs {
        action: None::<ParallelAction>,
        apply_receipt_backfill_handoffs: false,
        json: false,
        apply: false,
        include_caches: false,
        max_iterations: args.max_iterations,
        max_concurrent_workers: args.max_concurrent_workers.max(1),
        cargo_build_jobs: None,
        cargo_target: ParallelCargoTarget::Auto,
        prompt_file: Some(prompt_path),
        model: args.worker_model.clone(),
        reasoning_effort: args.worker_reasoning_effort.clone(),
        branch: args.branch.clone(),
        reference_repos: args.reference_repos.clone(),
        include_siblings: false,
        run_root: Some(run_root),
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

pub(crate) fn preserve_final_no_go_design_plan_items(
    repo_root: &Path,
    output_root: &Path,
    pass: usize,
    max_passes: usize,
    pass_dir: &Path,
) -> Result<Option<usize>> {
    let promoted = promote_design_plan_items_to_root_queue(repo_root, pass_dir)?;
    let status = if promoted.is_some() {
        "no-go-promoted-design-tasks"
    } else {
        "no-go-no-new-design-tasks"
    };
    write_design_no_go_resolution_status(
        output_root,
        repo_root,
        pass,
        max_passes,
        pass_dir,
        status,
    )?;
    Ok(promoted)
}

fn write_design_no_go_resolution_status(
    output_root: &Path,
    repo_root: &Path,
    pass: usize,
    max_passes: usize,
    pass_dir: &Path,
    status: &str,
) -> Result<()> {
    let markdown = format!(
        "# Design Resolve Status\n\n- Status: `{status}`\n- Pass: `{pass}/{max_passes}`\n- Latest artifacts: `{}`\n- Latest report: `{}`\n- Design plan items: `{}`\n- Executor queue: `{}`\n- Recovery: final NO-GO preserved design repair work in the executor queue when parser-visible tasks were present; otherwise inspect the latest report and plan-items artifact for blockers.\n",
        pass_dir.display(),
        pass_dir.join("DESIGN-REPORT.md").display(),
        pass_dir.join("DESIGN-PLAN-ITEMS.md").display(),
        repo_root.join("IMPLEMENTATION_PLAN.md").display(),
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        preserve_final_no_go_design_plan_items, root_queue_has_dependency_ready_repair_tasks,
    };
    use crate::design_command::testkit::temp_dir;
    use crate::task_parser::{parse_tasks, TaskStatus};

    #[test]
    fn final_no_go_promotes_design_tasks_before_failure() {
        let root = temp_dir("design-final-no-go-promotion");
        let output_root = root.join(".auto/design/run");
        let pass_dir = output_root.join("pass-01");
        fs::create_dir_all(&pass_dir).unwrap();
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n",
        )
        .unwrap();
        fs::write(
            pass_dir.join("DESIGN-REPORT.md"),
            "Remaining design/runtime gaps.\n\nVerdict: NO-GO\n",
        )
        .unwrap();
        fs::write(
            pass_dir.join("DESIGN-PLAN-ITEMS.md"),
            "- [ ] `DESIGN-999` Final NO-GO repair\n\n    Source of truth: `src/design_command/mod.rs`\n    Runtime owner: `src/design_command/mod.rs`\n    UI consumers: root `IMPLEMENTATION_PLAN.md`\n    Generated artifacts: `.auto/design/run/pass-01/DESIGN-PLAN-ITEMS.md`, root `IMPLEMENTATION_PLAN.md`\n    Fixture boundary: tests use temporary pass directories only.\n    Verification: `cargo test design_command::resolve::tests::final_no_go_promotes_design_tasks_before_failure`\n    Dependencies: none\n",
        )
        .unwrap();

        assert_eq!(
            preserve_final_no_go_design_plan_items(&root, &output_root, 1, 1, &pass_dir).unwrap(),
            Some(1)
        );

        let root_plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        let tasks = parse_tasks(&root_plan);
        let promoted = tasks
            .iter()
            .find(|task| task.id == "DESIGN-999")
            .expect("final NO-GO design task should be parser-visible");
        assert_eq!(promoted.status, TaskStatus::Pending);
        assert!(promoted.dependencies.is_empty());

        let status = fs::read_to_string(output_root.join("DESIGN-RESOLVE-STATUS.md")).unwrap();
        assert!(status.contains("no-go-promoted-design-tasks"));
        assert!(status.contains("DESIGN-PLAN-ITEMS.md"));

        assert_eq!(
            preserve_final_no_go_design_plan_items(&root, &output_root, 1, 1, &pass_dir).unwrap(),
            None
        );
    }

    #[test]
    fn final_no_go_existing_root_repair_task_is_recoverable() {
        let root = temp_dir("design-final-no-go-existing-repair");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [x] `DESIGN-001` Done\n    Dependencies: none\n\n- [ ] `DESIGN-008` Active ledger reconciliation before generation\n    Dependencies: `DESIGN-001`\n\n## Follow-On Work\n\n",
        )
        .unwrap();

        assert!(root_queue_has_dependency_ready_repair_tasks(&root).unwrap());
    }

    #[test]
    fn final_no_go_blocked_root_repair_task_is_not_recoverable() {
        let root = temp_dir("design-final-no-go-blocked-repair");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [ ] `DESIGN-008` Active ledger reconciliation before generation\n    Dependencies: `DESIGN-007`\n\n## Follow-On Work\n\n",
        )
        .unwrap();

        assert!(!root_queue_has_dependency_ready_repair_tasks(&root).unwrap());
    }
}
