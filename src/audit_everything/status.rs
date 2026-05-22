//! Status rendering: the stdout status block, the RUN-STATUS markdown
//! document, and the engineer-readable change summary.

use std::path::Path;

use anyhow::{Context, Result};

use crate::audit_everything::git::{git_ahead_behind, git_rev_parse_short};
use crate::audit_everything::manifest::{EverythingManifest, RemediationTaskState, StageStatus};
use crate::audit_everything::phases::final_review_has_actionable_blockers;
use crate::audit_everything::remediation::{
    complete_remediation_task_ids, unmet_remediation_dependencies,
};
use crate::audit_everything::run_paths::{
    change_summary_markdown_path, codebase_book_root_path, final_review_markdown_path,
    file_quality_root_path, remediation_plan_json_path, remediation_plan_markdown_path,
    run_status_markdown_path, RunPaths,
};
use crate::audit_everything::worktree::pause_requested;
use crate::audit_everything::one_line;
use crate::qa_only_command::format_final_status_block;
use crate::util::{atomic_write, git_stdout, timestamp_slug};

pub(crate) fn write_run_status_if_possible(
    paths: &RunPaths,
    manifest: &EverythingManifest,
) -> Result<()> {
    if !paths.worktree_root.join(".git").exists() && !paths.report_root.exists() {
        return Ok(());
    }
    write_run_status_markdown(paths, manifest)
}

pub(crate) fn print_status(paths: &RunPaths, manifest: &EverythingManifest) {
    print!("{}", render_status_stdout(paths, manifest));
}

fn render_status_stdout(paths: &RunPaths, manifest: &EverythingManifest) -> String {
    let files_done = manifest
        .files
        .iter()
        .filter(|file| matches!(file.status, StageStatus::Complete))
        .count();
    let synthesis_done = manifest
        .groups
        .iter()
        .filter(|group| matches!(group.synthesis_status, StageStatus::Complete))
        .count();
    let remediation_done = manifest
        .groups
        .iter()
        .filter(|group| matches!(group.remediation_status, StageStatus::Complete))
        .count();
    let remediation_tasks_done = manifest
        .remediation_tasks
        .iter()
        .filter(|task| matches!(task.status, StageStatus::Complete))
        .count();
    let mut output = String::new();
    output.push_str("status\n");
    output.push_str(&format!("context:     {:?}\n", manifest.context.status));
    output.push_str(&format!(
        "files:       {files_done}/{}\n",
        manifest.files.len()
    ));
    output.push_str(&format!(
        "synthesis:   {synthesis_done}/{}\n",
        manifest.groups.len()
    ));
    output.push_str(&format!(
        "remediation: {remediation_done}/{}\n",
        manifest.groups.len()
    ));
    output.push_str(&format!(
        "remed plan:  {:?}\n",
        manifest.remediation_plan.status
    ));
    output.push_str(&format!(
        "remed tasks: {remediation_tasks_done}/{}",
        manifest.remediation_tasks.len()
    ));
    output.push('\n');
    let branch_summary = audit_branch_summary(manifest);
    output.push_str(&format!(
        "primary:     {} {}",
        manifest.branch,
        branch_summary
            .primary_head
            .as_deref()
            .unwrap_or("(unknown)")
    ));
    output.push('\n');
    output.push_str(&format!(
        "audit branch: {} {}",
        manifest.audit_branch,
        branch_summary.audit_head.as_deref().unwrap_or("(unknown)")
    ));
    output.push('\n');
    output.push_str(&format!("branch state: {}\n", branch_summary.state));
    output.push_str(&format!(
        "running remed: {}",
        format_task_ids_by_status(manifest, StageStatus::Running, 10)
    ));
    output.push('\n');
    output.push_str(&format!(
        "failed remed: {}",
        format_task_ids_by_status(manifest, StageStatus::Failed, 10)
    ));
    output.push('\n');
    if let Some((task, unmet)) = first_dependency_blocked_remediation_task(manifest) {
        output.push_str(&format!(
            "blocked next: {} waiting on {}\n",
            task.id,
            unmet.join(", ")
        ));
    }
    output.push_str(&format!(
        "paused:      {}",
        if pause_requested(paths) { "yes" } else { "no" }
    ));
    output.push('\n');
    output.push_str(&format!("pause file:  {}\n", paths.pause_path.display()));
    output.push_str(&format!(
        "status doc:  {}\n",
        run_status_markdown_path(paths).display()
    ));
    output.push_str(&format!(
        "remed plan:  {}",
        remediation_plan_markdown_path(paths).display()
    ));
    output.push('\n');
    output.push_str(&format!(
        "codebase book: {}",
        codebase_book_root_path(paths).display()
    ));
    output.push('\n');
    output.push_str(&format!(
        "final review:{:?}\n",
        manifest.final_review.status
    ));
    output.push_str(&format!(
        "file quality:{:?}\n",
        manifest.file_quality.status
    ));
    output.push_str(&format!(
        "change summary:{:?}\n",
        manifest.change_summary.status
    ));
    output.push_str(&format!("merge:       {:?}\n", manifest.merge.status));
    output.push('\n');
    output.push_str("final status\n");
    output.push_str(&audit_status_final_block(paths, manifest));
    output.push('\n');
    output
}

fn audit_status_final_block(paths: &RunPaths, manifest: &EverythingManifest) -> String {
    let status = audit_status_label(paths, manifest);
    let files_written = audit_status_files(paths);
    let blockers = audit_status_blockers(paths, manifest);
    let next_step = audit_status_next_step(paths, manifest);
    format_final_status_block(&status, &files_written, &blockers, &next_step)
}

fn audit_status_files(paths: &RunPaths) -> Vec<String> {
    vec![
        format!(
            "RUN-STATUS.md: {}",
            run_status_markdown_path(paths).display()
        ),
        format!(
            "REMEDIATION-PLAN.md: {}",
            remediation_plan_markdown_path(paths).display()
        ),
        format!(
            "CODEBASE-BOOK: {}",
            codebase_book_root_path(paths).display()
        ),
    ]
}

fn audit_status_label(paths: &RunPaths, manifest: &EverythingManifest) -> String {
    if pause_requested(paths) {
        return "paused".to_string();
    }
    if remediation_task_count(manifest, StageStatus::Failed) > 0
        || final_review_has_actionable_blockers(paths)
    {
        return "blocked".to_string();
    }
    if remediation_task_count(manifest, StageStatus::Running) > 0 {
        return "running".to_string();
    }
    if matches!(manifest.merge.status, StageStatus::Complete)
        && matches!(manifest.final_status.status, StageStatus::Complete)
    {
        return "complete".to_string();
    }
    "ready to continue".to_string()
}

fn audit_status_blockers(paths: &RunPaths, manifest: &EverythingManifest) -> String {
    let failed_tasks = format_task_ids_by_status(manifest, StageStatus::Failed, 10);
    if pause_requested(paths) {
        return format!("pause requested via {}", paths.pause_path.display());
    }
    if failed_tasks != "none" {
        return format!("failed remediation tasks: {failed_tasks}");
    }
    if let Some((task, unmet)) = first_dependency_blocked_remediation_task(manifest) {
        return format!("{} waiting on {}", task.id, unmet.join(", "));
    }
    if final_review_has_actionable_blockers(paths) {
        return format!(
            "required blockers remain in {}",
            final_review_markdown_path(paths).display()
        );
    }
    "none".to_string()
}

fn audit_status_next_step(paths: &RunPaths, manifest: &EverythingManifest) -> String {
    if pause_requested(paths) {
        return "run `auto audit --everything --everything-phase unpause` when ready to resume"
            .to_string();
    }
    let failed_tasks = format_task_ids_by_status(manifest, StageStatus::Failed, 10);
    if failed_tasks != "none" {
        return format!(
            "inspect failed remediation tasks ({failed_tasks}), fix blockers, then rerun `auto audit --everything --everything-phase remediate`"
        );
    }
    if let Some((task, unmet)) = first_dependency_blocked_remediation_task(manifest) {
        return format!(
            "complete {} before dispatching {}",
            unmet.join(", "),
            task.id
        );
    }
    let running_tasks = format_task_ids_by_status(manifest, StageStatus::Running, 10);
    if running_tasks != "none" {
        return format!(
            "wait for running remediation tasks ({running_tasks}) or rerun `auto audit --everything --everything-phase status`"
        );
    }
    if !matches!(manifest.context.status, StageStatus::Complete) {
        return "run `auto audit --everything --everything-phase init-context`".to_string();
    }
    if manifest
        .files
        .iter()
        .any(|file| !matches!(file.status, StageStatus::Complete))
    {
        return "run `auto audit --everything --everything-phase first-pass`".to_string();
    }
    if manifest
        .groups
        .iter()
        .any(|group| !matches!(group.synthesis_status, StageStatus::Complete))
    {
        return "run `auto audit --everything --everything-phase synthesize`".to_string();
    }
    if !matches!(manifest.remediation_plan.status, StageStatus::Complete) {
        return "run `auto audit --everything --everything-phase plan-remediation`".to_string();
    }
    if manifest
        .remediation_tasks
        .iter()
        .any(|task| matches!(task.status, StageStatus::Pending | StageStatus::Running))
    {
        return "run `auto audit --everything --everything-phase remediate`".to_string();
    }
    if !matches!(manifest.final_review.status, StageStatus::Complete) {
        return "run `auto audit --everything --everything-phase final-review`".to_string();
    }
    if !matches!(manifest.merge.status, StageStatus::Complete) {
        return "run `auto audit --everything --everything-phase merge`".to_string();
    }
    "review RUN-STATUS.md and final audit artifacts".to_string()
}

fn remediation_task_count(manifest: &EverythingManifest, status: StageStatus) -> usize {
    manifest
        .remediation_tasks
        .iter()
        .filter(|task| task.status == status)
        .count()
}

fn write_run_status_markdown(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    let files_done = manifest
        .files
        .iter()
        .filter(|file| matches!(file.status, StageStatus::Complete))
        .count();
    let synthesis_done = manifest
        .groups
        .iter()
        .filter(|group| matches!(group.synthesis_status, StageStatus::Complete))
        .count();
    let remediation_done = manifest
        .groups
        .iter()
        .filter(|group| matches!(group.remediation_status, StageStatus::Complete))
        .count();
    let task_count = |status| {
        manifest
            .remediation_tasks
            .iter()
            .filter(|task| task.status == status)
            .count()
    };
    let mut body = String::new();
    body.push_str("# Auto Audit Run Status\n\n");
    body.push_str(&format!("Run: `{}`  \n", manifest.run_id));
    body.push_str(&format!("Repository: `{}`  \n", manifest.repo_root));
    body.push_str(&format!("Audit branch: `{}`  \n", manifest.audit_branch));
    body.push_str(&format!("Primary branch: `{}`  \n", manifest.branch));
    body.push_str(&format!("Status captured: `{}`\n\n", timestamp_slug()));
    let branch_summary = audit_branch_summary(manifest);
    body.push_str("## Current State\n\n");
    body.push_str(&format!(
        "- Context engineering: {:?}\n",
        manifest.context.status
    ));
    body.push_str(&format!(
        "- File pass: {files_done} / {} complete\n",
        manifest.files.len()
    ));
    body.push_str(&format!(
        "- Synthesis: {synthesis_done} / {} complete\n",
        manifest.groups.len()
    ));
    body.push_str(&format!(
        "- Remediation groups: {remediation_done} / {} complete\n",
        manifest.groups.len()
    ));
    body.push_str(&format!(
        "- Remediation plan: {:?}\n",
        manifest.remediation_plan.status
    ));
    if let Some(note) = manifest.remediation_plan.note.as_deref() {
        body.push_str(&format!("- Remediation plan note: {}\n", one_line(note)));
    }
    body.push_str(&format!(
        "- Remediation tasks: {} complete, {} running, {} pending, {} failed, {} skipped\n",
        task_count(StageStatus::Complete),
        task_count(StageStatus::Running),
        task_count(StageStatus::Pending),
        task_count(StageStatus::Failed),
        task_count(StageStatus::Skipped)
    ));
    body.push_str(&format!("- Branch position: {}\n", branch_summary.state));
    body.push_str(&format!(
        "- Running remediation tasks: {}\n",
        format_task_ids_by_status(manifest, StageStatus::Running, 10)
    ));
    body.push_str(&format!(
        "- Failed remediation tasks: {}\n",
        format_task_ids_by_status(manifest, StageStatus::Failed, 10)
    ));
    if let Some((task, unmet)) = first_dependency_blocked_remediation_task(manifest) {
        body.push_str(&format!(
            "- First dependency-blocked remediation task: `{}` waiting on {}\n",
            task.id,
            unmet
                .iter()
                .map(|dependency| format!("`{dependency}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    body.push_str(&format!(
        "- Pause requested: {}\n",
        if pause_requested(paths) { "yes" } else { "no" }
    ));
    body.push_str(&format!(
        "- Final review: {:?}\n",
        manifest.final_review.status
    ));
    body.push_str(&format!(
        "- File quality: {:?}\n",
        manifest.file_quality.status
    ));
    if let Some(note) = manifest.file_quality.note.as_deref() {
        body.push_str(&format!("- File quality note: {}\n", one_line(note)));
    }
    body.push_str(&format!(
        "- Change summary: {:?}\n",
        manifest.change_summary.status
    ));
    if let Some(note) = manifest.change_summary.note.as_deref() {
        body.push_str(&format!("- Change summary note: {}\n", one_line(note)));
    }
    body.push_str(&format!("- Merge: {:?}\n", manifest.merge.status));
    body.push_str(&format!(
        "- Final status refresh: {:?}\n",
        manifest.final_status.status
    ));
    if let Some(note) = manifest.final_status.note.as_deref() {
        body.push_str(&format!("- Final status note: {}\n", one_line(note)));
    }
    body.push('\n');
    body.push_str("## Final Status\n\n");
    body.push_str("```text\n");
    body.push_str(&audit_status_final_block(paths, manifest));
    body.push_str("\n```\n\n");
    body.push_str("## Branch State\n\n");
    body.push_str(&format!("- Primary branch: `{}`", manifest.branch));
    if let Some(head) = branch_summary.primary_head.as_deref() {
        body.push_str(&format!(" at `{head}`"));
    }
    body.push('\n');
    body.push_str(&format!("- Audit branch: `{}`", manifest.audit_branch));
    if let Some(head) = branch_summary.audit_head.as_deref() {
        body.push_str(&format!(" at `{head}`"));
    }
    body.push('\n');
    if let (Some(ahead), Some(behind)) = (branch_summary.ahead, branch_summary.behind) {
        body.push_str(&format!(
            "- Audit branch delta: {ahead} ahead, {behind} behind\n"
        ));
    }
    body.push_str(&format!("- Interpretation: {}\n\n", branch_summary.state));
    body.push_str("## Evidence Class Checklist\n\n");
    body.push_str(
        "Final review must classify each evidence class as `pass`, `not run`, `blocked`, or `not applicable`, with exact artifact or command evidence. Treat local, fixture, regtest, and synthetic proof as non-production evidence unless live production/mainnet artifacts are cited.\n\n",
    );
    body.push_str("- Local static/build/unit validation\n");
    body.push_str("- Generated contract/binding validation\n");
    body.push_str("- Browser QA or visual/product workflow validation\n");
    body.push_str("- Deployment/canary/health validation\n");
    body.push_str("- Live production or mainnet/on-chain validation\n");
    body.push_str("- External-owner or cross-repo validation\n");
    body.push_str("- Documentation/status artifact integrity\n\n");
    body.push_str("## Important Paths\n\n");
    body.push_str(&format!(
        "- Manifest: `{}`\n",
        paths.manifest_path.display()
    ));
    body.push_str(&format!("- Pause file: `{}`\n", paths.pause_path.display()));
    body.push_str(&format!(
        "- Audit worktree: `{}`\n",
        paths.worktree_root.display()
    ));
    body.push_str(&format!(
        "- Audit reports: `{}`\n",
        paths.report_root.display()
    ));
    body.push_str(&format!(
        "- Remediation plan: `{}`\n",
        remediation_plan_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Remediation plan JSON: `{}`\n",
        remediation_plan_json_path(paths).display()
    ));
    body.push_str(&format!(
        "- Final review: `{}`\n",
        final_review_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Change summary: `{}`\n",
        change_summary_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Codebase book: `{}`\n\n",
        codebase_book_root_path(paths).display()
    ));
    body.push_str(&format!(
        "- File quality reratings: `{}`\n\n",
        file_quality_root_path(paths).display()
    ));
    body.push_str("## Remediation Tasks\n\n");
    append_status_tasks(&mut body, "Running", manifest, StageStatus::Running);
    append_status_tasks(&mut body, "Failed", manifest, StageStatus::Failed);
    append_status_tasks(&mut body, "Pending", manifest, StageStatus::Pending);
    append_status_tasks(&mut body, "Complete", manifest, StageStatus::Complete);
    append_status_tasks(&mut body, "Skipped", manifest, StageStatus::Skipped);

    atomic_write(&run_status_markdown_path(paths), body.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            run_status_markdown_path(paths).display()
        )
    })
}

fn append_status_tasks(
    body: &mut String,
    heading: &str,
    manifest: &EverythingManifest,
    status: StageStatus,
) {
    let tasks = manifest
        .remediation_tasks
        .iter()
        .filter(|task| task.status == status)
        .collect::<Vec<_>>();
    if tasks.is_empty() {
        return;
    }
    body.push_str(&format!("### {heading}\n\n"));
    for task in tasks {
        body.push_str(&format!("- `{}` `{}`", task.id, task.group));
        if let Some(note) = task.note.as_deref().filter(|note| !note.trim().is_empty()) {
            body.push_str(&format!(" - {}", one_line(note)));
        }
        body.push('\n');
    }
    body.push('\n');
}

#[derive(Debug)]
struct AuditBranchSummary {
    primary_head: Option<String>,
    audit_head: Option<String>,
    ahead: Option<usize>,
    behind: Option<usize>,
    state: String,
}

fn audit_branch_summary(manifest: &EverythingManifest) -> AuditBranchSummary {
    let repo_root = Path::new(&manifest.repo_root);
    let primary_head = git_rev_parse_short(repo_root, &manifest.branch);
    let audit_head = git_rev_parse_short(repo_root, &manifest.audit_branch);
    let (ahead, behind) = git_ahead_behind(repo_root, &manifest.branch, &manifest.audit_branch)
        .map(|(ahead, behind)| (Some(ahead), Some(behind)))
        .unwrap_or((None, None));
    let state = match (
        primary_head.as_deref(),
        audit_head.as_deref(),
        ahead,
        behind,
    ) {
        (None, _, _, _) => format!("unable to resolve primary branch `{}`", manifest.branch),
        (_, None, _, _) => format!("unable to resolve audit branch `{}`", manifest.audit_branch),
        (Some(primary), Some(audit), _, _) if primary == audit => {
            "audit branch matches primary branch".to_string()
        }
        (Some(_), Some(_), Some(ahead), Some(0)) if ahead > 0 => format!(
            "audit branch is {ahead} {} ahead of primary; merge back is still pending",
            commit_word(ahead)
        ),
        (Some(_), Some(_), Some(0), Some(behind)) if behind > 0 => format!(
            "audit branch is {behind} {} behind primary; refresh before continuing",
            commit_word(behind)
        ),
        (Some(_), Some(_), Some(ahead), Some(behind)) if ahead > 0 && behind > 0 => {
            format!("audit branch has diverged from primary ({ahead} ahead, {behind} behind)")
        }
        (Some(_), Some(_), _, _) => "audit branch differs from primary".to_string(),
    };

    AuditBranchSummary {
        primary_head,
        audit_head,
        ahead,
        behind,
        state,
    }
}

fn format_task_ids_by_status(
    manifest: &EverythingManifest,
    status: StageStatus,
    limit: usize,
) -> String {
    let matching = manifest
        .remediation_tasks
        .iter()
        .filter(|task| task.status == status)
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return "none".to_string();
    }
    let mut formatted = matching
        .iter()
        .take(limit)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if matching.len() > limit {
        formatted.push_str(&format!(" (+{} more)", matching.len() - limit));
    }
    formatted
}

fn first_dependency_blocked_remediation_task(
    manifest: &EverythingManifest,
) -> Option<(&RemediationTaskState, Vec<String>)> {
    let complete = complete_remediation_task_ids(manifest);
    manifest
        .remediation_tasks
        .iter()
        .filter(|task| matches!(task.status, StageStatus::Pending))
        .find_map(|task| {
            let unmet = unmet_remediation_dependencies(task, &complete);
            (!unmet.is_empty()).then_some((task, unmet))
        })
}

fn commit_word(count: usize) -> &'static str {
    if count == 1 {
        "commit"
    } else {
        "commits"
    }
}

pub(crate) fn write_change_summary_markdown(
    paths: &RunPaths,
    manifest: &EverythingManifest,
) -> Result<()> {
    std::fs::create_dir_all(&paths.report_root)
        .with_context(|| format!("failed to create {}", paths.report_root.display()))?;

    let head = git_stdout(&paths.worktree_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let range = format!("{}..{head}", manifest.base_commit);
    let changed_files = git_stdout(
        &paths.worktree_root,
        ["diff", "--name-status", "--find-renames", &range],
    )?;
    let diff_stat = git_stdout(
        &paths.worktree_root,
        ["diff", "--stat", "--find-renames", &range],
    )?;
    let commit_log = git_stdout(
        &paths.worktree_root,
        [
            "log",
            "--reverse",
            "--no-merges",
            "--stat",
            "--find-renames",
            "--date=short",
            "--pretty=format:## Commit %h%n%n- Subject: %s%n- Author: %an <%ae>%n- Date: %ad%n",
            &range,
        ],
    )?;

    let mut body = String::new();
    body.push_str("# Auto Audit Change Summary\n\n");
    body.push_str("This artifact summarizes repository changes made by the `auto audit --everything` run. It is intended to let an engineer understand what changed without reconstructing the run from scattered commits, lane logs, and review notes.\n\n");
    body.push_str("## Run\n\n");
    body.push_str(&format!("- Run id: `{}`\n", manifest.run_id));
    body.push_str(&format!("- Primary branch: `{}`\n", manifest.branch));
    body.push_str(&format!("- Audit branch: `{}`\n", manifest.audit_branch));
    body.push_str(&format!("- Base commit: `{}`\n", manifest.base_commit));
    body.push_str(&format!("- Audit head: `{head}`\n"));
    body.push_str(&format!(
        "- Final review: {:?}\n",
        manifest.final_review.status
    ));
    body.push_str(&format!(
        "- File quality: {:?}\n",
        manifest.file_quality.status
    ));
    body.push_str(&format!("- Merge: {:?}\n\n", manifest.merge.status));

    body.push_str("## High-Level Diff Stat\n\n");
    if diff_stat.trim().is_empty() {
        body.push_str("No source or report changes are present on the audit branch beyond the base commit.\n\n");
    } else {
        body.push_str("```text\n");
        body.push_str(diff_stat.trim_end());
        body.push_str("\n```\n\n");
    }

    body.push_str("## Changed Files\n\n");
    if changed_files.trim().is_empty() {
        body.push_str("- none\n\n");
    } else {
        for line in changed_files.lines() {
            body.push_str(&format!("- `{}`\n", line.trim()));
        }
        body.push('\n');
    }

    body.push_str("## Remediation Task Summary\n\n");
    if manifest.remediation_tasks.is_empty() {
        body.push_str("- No remediation tasks were generated for this run.\n\n");
    } else {
        for task in &manifest.remediation_tasks {
            body.push_str(&format!(
                "- `{}` ({:?}) group `{}`: {}\n",
                task.id,
                task.status,
                task.group,
                task.note
                    .as_deref()
                    .map(one_line)
                    .unwrap_or_else(|| "no note recorded".to_string())
            ));
        }
        body.push('\n');
    }

    body.push_str("## Commit-by-Commit Detail\n\n");
    if commit_log.trim().is_empty() {
        body.push_str("No audit commits were created after the base commit.\n\n");
    } else {
        body.push_str(commit_log.trim_end());
        body.push_str("\n\n");
    }

    body.push_str("## Related Artifacts\n\n");
    body.push_str(&format!(
        "- Run status: `{}`\n",
        run_status_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Remediation plan: `{}`\n",
        remediation_plan_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Final review: `{}`\n",
        final_review_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- File quality reratings: `{}`\n",
        file_quality_root_path(paths).display()
    ));

    atomic_write(&change_summary_markdown_path(paths), body.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            change_summary_markdown_path(paths).display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{render_status_stdout, write_run_status_markdown};
    use crate::audit_everything::file_quality::{
        FILE_QUALITY_ACCEPT_SCORE, FILE_QUALITY_TARGET_SCORE,
    };
    use crate::audit_everything::manifest::{
        FileQualityPassState, FileQualityRatingState, FileState, StageStatus,
    };
    use crate::audit_everything::run_paths::{
        file_quality_file_path, file_quality_pass_path, run_status_markdown_path, RunPaths,
        LATEST_RUN_FILE, PAUSE_REQUEST_FILE, PROFESSIONAL_AUDIT_DIR,
    };
    use crate::audit_everything::file_quality::require_file_quality_acceptance;
    use crate::audit_everything::tests::{group_for_test, manifest_with_groups, task_for_test};
    use std::fs;

    #[test]
    fn professional_audit_status_and_file_quality_contract_is_runtime_owned() {
        assert_eq!(PROFESSIONAL_AUDIT_DIR, ".auto/audit-everything");
        assert_eq!(LATEST_RUN_FILE, "latest-run");
        assert_eq!(PAUSE_REQUEST_FILE, "PAUSE");
        assert_eq!(FILE_QUALITY_ACCEPT_SCORE, 9.0);
        assert_eq!(FILE_QUALITY_TARGET_SCORE, 10.0);

        let dir = std::env::temp_dir().join(format!(
            "auto-audit-status-quality-contract-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let report_root = dir.join("worktree/audit/everything/test-run");
        fs::create_dir_all(&report_root).expect("failed to create report root");
        let paths = RunPaths {
            host_root: dir.clone(),
            manifest_path: dir
                .join(PROFESSIONAL_AUDIT_DIR)
                .join("test-run/MANIFEST.json"),
            latest_path: dir.join(PROFESSIONAL_AUDIT_DIR).join(LATEST_RUN_FILE),
            worktree_root: dir.join("worktree"),
            report_root,
            pause_path: dir
                .join(PROFESSIONAL_AUDIT_DIR)
                .join("test-run")
                .join(PAUSE_REQUEST_FILE),
            in_place: false,
        };
        let mut manifest = manifest_with_groups(vec![group_for_test("src", &["src/lib.rs"])]);
        manifest.files = vec![FileState {
            path: "src/lib.rs".to_string(),
            group: "src".to_string(),
            content_hash: "hash-a".to_string(),
            artifact_dir: "artifact-a".to_string(),
            status: StageStatus::Complete,
        }];
        manifest.final_review.status = StageStatus::Complete;
        manifest.file_quality.status = StageStatus::Complete;
        manifest.file_quality.note = Some("all tracked files rerated at least 9/10".to_string());
        manifest.file_quality_passes = vec![FileQualityPassState {
            pass_index: 1,
            status: StageStatus::Complete,
            artifact_dir: file_quality_pass_path(&paths, 1).display().to_string(),
            ratings: vec![FileQualityRatingState {
                path: "src/lib.rs".to_string(),
                score_out_of_10: Some(FILE_QUALITY_ACCEPT_SCORE),
                status: StageStatus::Complete,
                artifact_dir: file_quality_file_path(&paths, 1, &manifest.files[0])
                    .display()
                    .to_string(),
                note: None,
            }],
            note: Some("accepted at the merge floor while target remains 10/10".to_string()),
        }];
        manifest.final_status.status = StageStatus::Complete;
        manifest.final_status.note = Some("refreshed after merge completion".to_string());

        require_file_quality_acceptance(&manifest)
            .expect("9/10 ratings satisfy the runtime acceptance gate");
        manifest.file_quality_passes[0].ratings[0].score_out_of_10 =
            Some(FILE_QUALITY_ACCEPT_SCORE - 0.1);
        let below_floor = require_file_quality_acceptance(&manifest).unwrap_err();
        assert!(format!("{below_floor:#}").contains("below 9"));
        manifest.file_quality_passes[0].ratings[0].score_out_of_10 =
            Some(FILE_QUALITY_ACCEPT_SCORE);

        write_run_status_markdown(&paths, &manifest).expect("failed to write run status");
        let status = fs::read_to_string(run_status_markdown_path(&paths))
            .expect("failed to read run status");
        assert!(status.contains("- Final review: Complete"));
        assert!(status.contains("- File quality: Complete"));
        assert!(status.contains("all tracked files rerated at least 9/10"));
        assert!(status.contains("- Final status refresh: Complete"));
        assert!(status.contains("Evidence Class Checklist"));
        assert!(status.contains("File quality reratings:"));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn audit_status_prints_final_status_block_and_next_step() {
        let dir = std::env::temp_dir().join(format!(
            "auto-audit-status-final-block-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let report_root = dir.join("worktree/audit/everything/test-run");
        fs::create_dir_all(&report_root).expect("failed to create report root");
        let paths = RunPaths {
            host_root: dir.clone(),
            manifest_path: dir.join("MANIFEST.json"),
            latest_path: dir.join("latest-run"),
            worktree_root: dir.join("worktree"),
            report_root,
            pause_path: dir.join(PAUSE_REQUEST_FILE),
            in_place: false,
        };
        let mut manifest = manifest_with_groups(vec![group_for_test(
            "crates/core",
            &["crates/core/src/lib.rs"],
        )]);
        manifest.files = vec![FileState {
            path: "crates/core/src/lib.rs".to_string(),
            group: "crates/core".to_string(),
            content_hash: "hash".to_string(),
            artifact_dir: "artifact".to_string(),
            status: StageStatus::Complete,
        }];
        manifest.remediation_tasks = vec![task_for_test("AUD-REM-001", "crates/core", &[])];
        manifest.remediation_tasks[0].status = StageStatus::Running;

        let status = render_status_stdout(&paths, &manifest);

        assert!(status.contains("final status\n"), "{status}");
        assert!(status.contains("status:"), "{status}");
        assert!(status.contains("files written:"), "{status}");
        assert!(status.contains("RUN-STATUS.md"), "{status}");
        assert!(status.contains("blockers:"), "{status}");
        assert!(status.contains("next step:"), "{status}");
        assert!(
            status.contains("wait for running remediation tasks"),
            "{status}"
        );

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn run_status_markdown_records_pause_paths_and_task_counts() {
        let dir =
            std::env::temp_dir().join(format!("auto-audit-run-status-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let report_root = dir.join("worktree/audit/everything/test-run");
        fs::create_dir_all(&report_root).expect("failed to create report root");
        let paths = RunPaths {
            host_root: dir.clone(),
            manifest_path: dir.join("MANIFEST.json"),
            latest_path: dir.join("latest-run"),
            worktree_root: dir.join("worktree"),
            report_root,
            pause_path: dir.join(PAUSE_REQUEST_FILE),
            in_place: false,
        };
        fs::write(&paths.pause_path, "pause requested\n").expect("failed to write pause file");
        let mut manifest = manifest_with_groups(vec![
            group_for_test("crates/core", &["crates/core/src/lib.rs"]),
            group_for_test("docs", &["docs/architecture.md"]),
        ]);
        manifest.files = vec![FileState {
            path: "crates/core/src/lib.rs".to_string(),
            group: "crates/core".to_string(),
            content_hash: "hash".to_string(),
            artifact_dir: "artifact".to_string(),
            status: StageStatus::Complete,
        }];
        manifest.remediation_tasks = vec![
            task_for_test("AUD-REM-001", "crates/core", &[]),
            task_for_test("AUD-REM-002", "docs", &["AUD-REM-001"]),
        ];
        manifest.remediation_tasks[0].status = StageStatus::Complete;
        manifest.remediation_tasks[0].note = Some("landed 2 changed file(s)".to_string());
        manifest.remediation_tasks[1].status = StageStatus::Running;

        write_run_status_markdown(&paths, &manifest).expect("failed to write run status");
        let status = fs::read_to_string(run_status_markdown_path(&paths))
            .expect("failed to read run status");

        assert!(status.contains("Pause requested: yes"));
        assert!(status.contains("1 complete, 1 running, 0 pending, 0 failed, 0 skipped"));
        assert!(status.contains("`AUD-REM-002` `docs`"));
        assert!(status.contains("REMEDIATION-PLAN.md"));
        assert!(status.contains("CODEBASE-BOOK"));
        assert!(status.contains("Final status refresh:"));
        assert!(status.contains("## Final Status"));
        assert!(status.contains("files written:"));
        assert!(status.contains("blockers:    pause requested via"));
        assert!(status
            .contains("next step:   run `auto audit --everything --everything-phase unpause`"));
        assert!(status.contains("Evidence Class Checklist"));
        assert!(status.contains("Live production or mainnet/on-chain validation"));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }
}
