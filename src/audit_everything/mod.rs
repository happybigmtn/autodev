//! Professional whole-repo audit pipeline for `auto audit --everything`.
//!
//! The legacy `auto audit` command is a doctrine-driven per-file fixer. This
//! module is deliberately larger: it first builds repository context, then runs
//! one clean model iteration per file, synthesizes crate/module reports, applies
//! bounded crate-by-crate improvements, and finally reviews the diff before an
//! optional merge back to the primary branch.
//!
//! The pipeline is split into focused submodules: [`manifest`] holds the data
//! model, [`run_paths`] the run layout, [`phases`] the async phase drivers,
//! [`workers`] the worker pools, [`remediation`] the lane scheduler,
//! [`file_quality`] the rerate gate, [`prompts`] the prompt builders, and
//! [`status`]/[`git`]/[`context`]/[`inventory`] the supporting machinery.

mod context;
mod file_quality;
mod git;
mod inventory;
mod manifest;
mod phases;
mod prompts;
mod remediation;
mod run_paths;
mod status;
mod worktree;
mod workers;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::audit_everything::manifest::{EverythingManifest, StageStatus};
use crate::audit_everything::manifest::write_manifest;
use crate::audit_everything::phases::{
    attempt_merge, complete_in_place_run, refresh_final_status_after_merge,
    require_context_complete, require_first_pass_complete, require_synthesis_complete,
    run_context_phase, run_final_review_and_file_quality_phase, run_final_review_phase,
    run_first_pass_phase, run_synthesis_phase,
};
use crate::audit_everything::remediation::{run_remediation_phase, run_remediation_plan_phase};
use crate::audit_everything::run_paths::{
    load_or_create_run, resolve_primary_branch, resolve_run_root_base, RunPaths,
};
use crate::audit_everything::status::{print_status, write_run_status_if_possible};
use crate::audit_everything::worktree::{clear_pause, ensure_worktree, pause_requested, request_pause};
use crate::util::{binary_provenance_line, ensure_repo_layout, git_repo_root, git_stdout};
use crate::{AuditArgs, AuditEverythingPhase};

pub(crate) async fn run_audit_everything(args: AuditArgs) -> Result<()> {
    if args.everything_threads == 0 {
        bail!("--everything-threads must be greater than 0");
    }
    if args.remediation_threads == 0 {
        bail!("--remediation-threads must be greater than 0");
    }
    if args.file_quality_passes == 0 {
        bail!("--file-quality-passes must be greater than 0");
    }

    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string();
    let branch = resolve_primary_branch(&repo_root, args.branch.as_deref(), &current_branch)?;
    let run_root_base = resolve_run_root_base(&repo_root, args.everything_run_root.as_deref());

    let (mut manifest, paths) = load_or_create_run(&repo_root, &run_root_base, &branch, &args)?;

    println!("auto audit --everything");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", manifest.branch);
    println!("audit branch: {}", manifest.audit_branch);
    println!("run id:      {}", manifest.run_id);
    println!("run root:    {}", paths.host_root.display());
    println!("worktree:    {}", paths.worktree_root.display());
    println!("reports:     {}", paths.report_root.display());
    println!(
        "mode:        {}",
        if manifest.in_place {
            "in-place"
        } else {
            "worktree"
        }
    );
    println!("phase:       {:?}", args.everything_phase);

    if matches!(args.everything_phase, AuditEverythingPhase::Pause) {
        request_pause(&paths, &manifest)?;
        print_status(&paths, &manifest);
        return Ok(());
    }

    if matches!(args.everything_phase, AuditEverythingPhase::Unpause) {
        clear_pause(&paths, &manifest)?;
        print_status(&paths, &manifest);
        return Ok(());
    }

    if matches!(args.everything_phase, AuditEverythingPhase::Status) {
        write_run_status_if_possible(&paths, &manifest)?;
        print_status(&paths, &manifest);
        return Ok(());
    }

    ensure_worktree(&repo_root, &paths, &mut manifest)?;
    write_manifest(&paths, &manifest)?;

    match args.everything_phase {
        AuditEverythingPhase::All => {
            run_context_phase(&args, &paths, &mut manifest).await?;
            run_first_pass_phase(&args, &paths, &mut manifest).await?;
            run_synthesis_phase(&args, &paths, &mut manifest).await?;
            if args.report_only {
                mark_remediation_skipped(&paths, &mut manifest, "--report-only")?;
                run_final_review_phase(&args, &paths, &mut manifest).await?;
                mark_merge_skipped(&paths, &mut manifest, "--report-only")?;
            } else {
                run_remediation_plan_phase(&paths, &mut manifest)?;
                run_remediation_phase(&args, &paths, &mut manifest).await?;
                if pause_requested(&paths) {
                    println!("professional audit paused before final review");
                    print_status(&paths, &manifest);
                    return Ok(());
                }
                run_final_review_and_file_quality_phase(&args, &paths, &mut manifest).await?;
                if manifest.in_place {
                    complete_in_place_run(&paths, &mut manifest)?;
                    refresh_final_status_after_merge(&repo_root, &paths, &mut manifest)?;
                } else if args.no_everything_merge {
                    mark_merge_skipped(&paths, &mut manifest, "--no-everything-merge")?;
                } else {
                    attempt_merge(&repo_root, &paths, &mut manifest)?;
                    refresh_final_status_after_merge(&repo_root, &paths, &mut manifest)?;
                }
            }
        }
        AuditEverythingPhase::InitContext => {
            run_context_phase(&args, &paths, &mut manifest).await?;
        }
        AuditEverythingPhase::FirstPass => {
            require_context_complete(&manifest)?;
            run_first_pass_phase(&args, &paths, &mut manifest).await?;
        }
        AuditEverythingPhase::Synthesize => {
            require_first_pass_complete(&manifest)?;
            run_synthesis_phase(&args, &paths, &mut manifest).await?;
        }
        AuditEverythingPhase::PlanRemediation => {
            require_synthesis_complete(&manifest)?;
            run_remediation_plan_phase(&paths, &mut manifest)?;
        }
        AuditEverythingPhase::Remediate => {
            require_synthesis_complete(&manifest)?;
            if args.report_only {
                mark_remediation_skipped(&paths, &mut manifest, "--report-only")?;
            } else {
                run_remediation_plan_phase(&paths, &mut manifest)?;
                run_remediation_phase(&args, &paths, &mut manifest).await?;
            }
        }
        AuditEverythingPhase::FinalReview => {
            run_final_review_and_file_quality_phase(&args, &paths, &mut manifest).await?;
        }
        AuditEverythingPhase::Merge => {
            run_final_review_and_file_quality_phase(&args, &paths, &mut manifest).await?;
            if manifest.in_place {
                complete_in_place_run(&paths, &mut manifest)?;
                refresh_final_status_after_merge(&repo_root, &paths, &mut manifest)?;
            } else {
                attempt_merge(&repo_root, &paths, &mut manifest)?;
                refresh_final_status_after_merge(&repo_root, &paths, &mut manifest)?;
            }
        }
        AuditEverythingPhase::Pause
        | AuditEverythingPhase::Unpause
        | AuditEverythingPhase::Status => unreachable!("handled above"),
    }

    print_status(&paths, &manifest);
    Ok(())
}

fn mark_remediation_skipped(
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
    reason: &str,
) -> Result<()> {
    for group in &mut manifest.groups {
        if !matches!(group.remediation_status, StageStatus::Complete) {
            group.remediation_status = StageStatus::Skipped;
        }
    }
    manifest.remediation_plan.status = StageStatus::Skipped;
    manifest.remediation_plan.note = Some(reason.to_string());
    for task in &mut manifest.remediation_tasks {
        if !matches!(task.status, StageStatus::Complete) {
            task.status = StageStatus::Skipped;
            task.note = Some(reason.to_string());
        }
    }
    manifest.merge.status = StageStatus::Skipped;
    manifest.merge.note = Some(format!("remediation skipped: {reason}"));
    write_manifest(paths, manifest)
}

fn mark_merge_skipped(
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
    reason: &str,
) -> Result<()> {
    manifest.merge.status = StageStatus::Skipped;
    manifest.merge.note = Some(reason.to_string());
    write_manifest(paths, manifest)
}

fn require_nonempty_file(path: &Path) -> Result<()> {
    let metadata =
        std::fs::metadata(path).with_context(|| format!("missing {}", path.display()))?;
    if metadata.len() == 0 {
        bail!("{} is empty", path.display());
    }
    Ok(())
}

fn collect_regular_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("failed to inspect {}", dir.display()))?;
            let path = entry.path();
            let ty = entry
                .file_type()
                .with_context(|| format!("failed to stat {}", path.display()))?;
            if ty.is_dir() {
                stack.push(path);
            } else if ty.is_file() {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn short_hash(hash: &str) -> String {
    hash.chars().take(16).collect()
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "root".to_string()
    } else {
        slug
    }
}

fn path_display(path: &Path) -> String {
    path.display().to_string()
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn one_line(text: &str) -> String {
    text.trim().replace('\n', " ")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::slugify;
    use crate::audit_everything::manifest::{
        ContextState, EverythingManifest, GroupState, RemediationTaskState, StageState,
        StageStatus,
    };

    pub(crate) fn manifest_with_groups(groups: Vec<GroupState>) -> EverythingManifest {
        EverythingManifest {
            run_id: "test-run".to_string(),
            repo_root: ".".to_string(),
            worktree_root: ".".to_string(),
            report_root: "audit/everything/test-run".to_string(),
            in_place: false,
            branch: "main".to_string(),
            audit_branch: "auto-audit/test".to_string(),
            base_commit: "base".to_string(),
            created_at: "now".to_string(),
            context: ContextState::default(),
            files: Vec::new(),
            groups,
            remediation_plan: StageState::default(),
            remediation_tasks: Vec::new(),
            final_review_repairs: Vec::new(),
            file_quality: StageState::default(),
            file_quality_passes: Vec::new(),
            change_summary: StageState::default(),
            final_review: StageState::default(),
            merge: StageState::default(),
            final_status: StageState::default(),
        }
    }

    pub(crate) fn group_for_test(name: &str, files: &[&str]) -> GroupState {
        GroupState {
            name: name.to_string(),
            slug: slugify(name),
            files: files.iter().map(|file| file.to_string()).collect(),
            report_path: format!("audit/everything/test-run/reports/{}.md", slugify(name)),
            synthesis_status: StageStatus::Complete,
            remediation_status: StageStatus::Pending,
        }
    }

    pub(crate) fn task_for_test(
        id: &str,
        group: &str,
        dependencies: &[&str],
    ) -> RemediationTaskState {
        RemediationTaskState {
            id: id.to_string(),
            group: group.to_string(),
            slug: slugify(group),
            report_path: format!("audit/everything/test-run/reports/{}.md", slugify(group)),
            owned_paths: Vec::new(),
            dependencies: dependencies
                .iter()
                .map(|dependency| dependency.to_string())
                .collect(),
            lane_index: 1,
            lane_root: ".auto/audit-everything/test/remediation-lanes/lane-1".to_string(),
            lane_repo_root: ".auto/audit-everything/test/remediation-lanes/lane-1/repo".to_string(),
            base_commit: None,
            status: StageStatus::Pending,
            note: None,
        }
    }

    #[test]
    fn slugify_keeps_group_names_path_safe() {
        assert_eq!(slugify("crates/bitino-house"), "crates-bitino-house");
        assert_eq!(slugify("Autonomy Core!"), "autonomy-core");
    }
}
