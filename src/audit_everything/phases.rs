//! The six async audit phase drivers: context, first pass, synthesis, final
//! review, file quality, and merge.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tokio::task::JoinSet;

use crate::audit_everything::context::{
    hash_doctrine, hash_file_if_exists, read_context_bundle, write_context_bundle,
    write_skill_policy_reference,
};
use crate::audit_everything::file_quality::{require_file_quality_acceptance, run_file_quality_gate_phase};
use crate::audit_everything::git::{commit_worktree_changes, remote_branch_exists};
use crate::audit_everything::inventory::{build_initial_group_reports, reconcile_file_inventory};
use crate::audit_everything::manifest::write_manifest;
use crate::audit_everything::manifest::{EverythingManifest, GroupState, StageState, StageStatus};
use crate::audit_everything::prompts::{
    build_context_prompt, build_final_review_repair_prompt, build_final_review_shard_prompt,
    build_final_review_synthesis_prompt,
};
use crate::audit_everything::run_paths::{
    change_summary_markdown_path, codebase_book_index_path, final_review_markdown_path,
    final_review_shards_path, run_status_markdown_path, PhaseConfig, RunPaths,
};
use crate::audit_everything::status::write_change_summary_markdown;
use crate::audit_everything::workers::{
    run_codex_phase, run_codex_phase_for_artifact, run_group_workers, spawn_file_worker,
};
use crate::audit_everything::{path_display, require_nonempty_file};
use crate::verdict::{exact_terminal_verdict, terminal_verdict_is};
use crate::AuditArgs;

pub(crate) async fn run_context_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if matches!(manifest.context.status, StageStatus::Complete) {
        println!("context:     complete (resume)");
        return Ok(());
    }
    manifest.context.status = StageStatus::Running;
    write_manifest(paths, manifest)?;
    fs::create_dir_all(&paths.report_root)
        .with_context(|| format!("failed to create {}", paths.report_root.display()))?;
    write_skill_policy_reference(paths)?;

    let prompt = build_context_prompt(&paths.worktree_root, &paths.report_root);
    let config = PhaseConfig {
        model: args.synthesis_model.clone(),
        effort: args.synthesis_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    run_codex_phase(paths, "init-context", &prompt, &config).await?;

    require_nonempty_file(&paths.worktree_root.join("AGENTS.md"))?;
    require_nonempty_file(&paths.worktree_root.join("ARCHITECTURE.md"))?;
    write_skill_policy_reference(paths)?;
    write_context_bundle(paths)?;

    manifest.context.status = StageStatus::Complete;
    manifest.context.agents_hash = hash_file_if_exists(&paths.worktree_root.join("AGENTS.md"))?;
    manifest.context.architecture_hash =
        hash_file_if_exists(&paths.worktree_root.join("ARCHITECTURE.md"))?;
    manifest.context.doctrine_hash = Some(hash_doctrine(&paths.worktree_root)?);
    reconcile_file_inventory(&paths.worktree_root, &paths.report_root, manifest)?;
    write_manifest(paths, manifest)?;
    Ok(())
}

pub(crate) async fn run_first_pass_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    let total_attempts = args.first_pass_retries.saturating_add(1).max(1);
    let mut last_failures: Vec<String> = Vec::new();
    for attempt in 1..=total_attempts {
        last_failures = run_first_pass_phase_once(args, paths, manifest).await?;
        if last_failures.is_empty() {
            return Ok(());
        }
        if attempt < total_attempts {
            eprintln!(
                "first pass: {} file(s) failed; retrying (round {} of {})",
                last_failures.len(),
                attempt + 1,
                total_attempts,
            );
        }
    }
    for failure in &last_failures {
        eprintln!("first pass failure: {failure}");
    }
    bail!(
        "first pass failed for {} file(s) after {} attempt(s)",
        last_failures.len(),
        total_attempts,
    );
}

/// Run a single round of first-pass workers and return the list of failure
/// messages. The function is idempotent: it filters `pending` by
/// `status != Complete`, so successive rounds only re-process still-failed
/// files. Callers wrap this in a retry loop driven by `--first-pass-retries`.
async fn run_first_pass_phase_once(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<Vec<String>> {
    reconcile_file_inventory(&paths.worktree_root, &paths.report_root, manifest)?;
    let pending = manifest
        .files
        .iter()
        .filter(|file| !matches!(file.status, StageStatus::Complete))
        .cloned()
        .collect::<Vec<_>>();
    if pending.is_empty() {
        println!("first pass:  complete (resume)");
        return Ok(Vec::new());
    }

    let context = read_context_bundle(paths)?;
    let config = PhaseConfig {
        model: args.first_pass_model.clone(),
        effort: args.first_pass_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let workers = args.everything_threads.clamp(1, 15);
    println!(
        "first pass:  {} file(s), {} worker(s)",
        pending.len(),
        workers
    );
    let mut join_set = JoinSet::new();
    let mut pending_iter = pending.into_iter();
    let mut active = 0usize;
    for _ in 0..workers {
        if let Some(file) = pending_iter.next() {
            spawn_file_worker(&mut join_set, paths, file, &context, &config);
            active += 1;
        }
    }

    let mut failures = Vec::new();
    while active > 0 {
        let Some(result) = join_set.join_next().await else {
            break;
        };
        active -= 1;
        match result {
            Ok(Ok(path)) => {
                if let Some(file) = manifest.files.iter_mut().find(|file| file.path == path) {
                    file.status = StageStatus::Complete;
                }
                write_manifest(paths, manifest)?;
            }
            Ok(Err(err)) => failures.push(format!("{err:#}")),
            Err(err) => failures.push(format!("worker task panicked: {err}")),
        }
        if let Some(file) = pending_iter.next() {
            spawn_file_worker(&mut join_set, paths, file, &context, &config);
            active += 1;
        }
    }
    write_manifest(paths, manifest)?;
    Ok(failures)
}

pub(crate) async fn run_synthesis_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    build_initial_group_reports(paths, manifest)?;
    let pending = manifest
        .groups
        .iter()
        .filter(|group| !matches!(group.synthesis_status, StageStatus::Complete))
        .cloned()
        .collect::<Vec<_>>();
    if pending.is_empty() {
        println!("synthesis:   complete (resume)");
        return Ok(());
    }

    let config = PhaseConfig {
        model: args.synthesis_model.clone(),
        effort: args.synthesis_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let workers = args.everything_threads.clamp(1, 15);
    println!(
        "synthesis:   {} group(s), {} worker(s)",
        pending.len(),
        workers
    );
    run_group_workers(paths, pending, workers, config, manifest).await
}

pub(crate) async fn run_final_review_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    let mut attempt = 0usize;
    loop {
        if matches!(manifest.final_review.status, StageStatus::Complete) {
            if final_review_is_go(paths) {
                println!("final review: complete (resume)");
                return Ok(());
            }
            if attempt == 0 {
                println!("final review: NO-GO (resume)");
            }
        } else {
            run_final_review_once(args, paths, manifest).await?;
        }

        if final_review_is_go(paths) {
            return Ok(());
        }
        if attempt >= args.final_review_retries {
            return Ok(());
        }
        if args.report_only {
            return Ok(());
        }
        if !final_review_has_actionable_blockers(paths) {
            return Ok(());
        }

        attempt += 1;
        run_final_review_repair_phase(args, paths, manifest, attempt).await?;
        manifest.final_review.status = StageStatus::Pending;
        manifest.final_review.note = Some(format!(
            "repair attempt {attempt} applied; final review pending rerun"
        ));
        write_manifest(paths, manifest)?;
    }
}

pub(crate) async fn run_final_review_and_file_quality_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    run_final_review_phase(args, paths, manifest).await?;
    if args.report_only {
        finalize_change_summary_phase(paths, manifest, !args.report_only)?;
        return Ok(());
    }
    if !final_review_is_go(paths) {
        manifest.file_quality.status = StageStatus::Failed;
        manifest.file_quality.note = Some(
            "final review is not GO; audit cannot complete until final-review blockers are resolved and every file rerates at least 9/10"
                .to_string(),
        );
        write_manifest(paths, manifest)?;
        finalize_change_summary_phase(paths, manifest, true)?;
        bail!(
            "final review is not GO; auto audit will not exit successfully until final-review blockers are resolved and every file rerates at least {:.0}/10",
            crate::audit_everything::file_quality::FILE_QUALITY_ACCEPT_SCORE
        );
    }
    let changed = run_file_quality_gate_phase(args, paths, manifest).await?;
    require_file_quality_acceptance(manifest)?;
    if changed {
        manifest.final_review.status = StageStatus::Pending;
        manifest.final_review.note = Some(
            "file-quality deliverables changed the audit branch; final review must rerun"
                .to_string(),
        );
        write_manifest(paths, manifest)?;
        run_final_review_phase(args, paths, manifest).await?;
        if !final_review_is_go(paths) {
            finalize_change_summary_phase(paths, manifest, true)?;
            bail!(
                "final review is not GO after file-quality deliverables; auto audit will not exit successfully until the rerun is GO and every file remains at least {:.0}/10",
                crate::audit_everything::file_quality::FILE_QUALITY_ACCEPT_SCORE
            );
        }
    }
    finalize_change_summary_phase(paths, manifest, true)?;
    Ok(())
}

fn finalize_change_summary_phase(
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
    commit: bool,
) -> Result<()> {
    manifest.change_summary.status = StageStatus::Running;
    manifest.change_summary.artifact = Some(path_display(&change_summary_markdown_path(paths)));
    write_manifest(paths, manifest)?;

    write_change_summary_markdown(paths, manifest)?;

    manifest.change_summary.status = StageStatus::Complete;
    manifest.change_summary.note = Some(
        "engineer-readable audit change summary written from audit branch git history".to_string(),
    );
    write_manifest(paths, manifest)?;
    if commit {
        commit_worktree_changes(paths, manifest)?;
    }
    Ok(())
}

async fn run_final_review_once(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    manifest.final_review.status = StageStatus::Running;
    write_manifest(paths, manifest)?;
    let config = PhaseConfig {
        model: args.final_review_model.clone(),
        effort: args.final_review_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let workers = args.everything_threads.clamp(1, 15);
    let shard_root = if workers > 1 && manifest.groups.len() > 1 {
        Some(run_final_review_shards(args, paths, manifest, &config, workers).await?)
    } else {
        None
    };
    let prompt = build_final_review_synthesis_prompt(paths, manifest, shard_root.as_deref());
    run_codex_phase(paths, "final-review", &prompt, &config).await?;
    let review_path = final_review_markdown_path(paths);
    let book_index_path = codebase_book_index_path(paths);
    require_nonempty_file(&review_path)?;
    require_nonempty_file(&book_index_path)?;
    let review = fs::read_to_string(&review_path)
        .with_context(|| format!("failed to read {}", review_path.display()))?;
    manifest.final_review.artifact = Some(path_display(&review_path));
    manifest.final_review.note = first_verdict_line(&review);
    manifest.final_review.status = StageStatus::Complete;
    write_manifest(paths, manifest)?;
    Ok(())
}

async fn run_final_review_shards(
    _args: &AuditArgs,
    paths: &RunPaths,
    manifest: &EverythingManifest,
    config: &PhaseConfig,
    workers: usize,
) -> Result<PathBuf> {
    let shard_root = final_review_shards_path(paths);
    if shard_root.exists() {
        fs::remove_dir_all(&shard_root)
            .with_context(|| format!("failed to clear {}", shard_root.display()))?;
    }
    fs::create_dir_all(&shard_root)
        .with_context(|| format!("failed to create {}", shard_root.display()))?;
    let shard_count = workers.min(manifest.groups.len()).max(1);
    println!("final review: {} group-shard reviewer(s)", shard_count);
    let mut groups = manifest.groups.clone();
    groups.sort_by(|left, right| left.slug.cmp(&right.slug));
    let mut buckets = vec![Vec::<GroupState>::new(); shard_count];
    for (idx, group) in groups.into_iter().enumerate() {
        buckets[idx % shard_count].push(group);
    }
    let mut join_set = JoinSet::new();
    for (idx, bucket) in buckets.into_iter().enumerate() {
        if bucket.is_empty() {
            continue;
        }
        let paths = paths.clone();
        let config = config.clone();
        let artifact_dir = shard_root.join(format!("shard-{idx:02}"));
        let prompt = build_final_review_shard_prompt(&paths, manifest, idx, &bucket, &artifact_dir);
        join_set.spawn(async move {
            let phase_slug = format!("final-review-shard-{idx:02}");
            run_codex_phase_for_artifact(&paths, &artifact_dir, &phase_slug, &prompt, &config)
                .await?;
            let shard_path = artifact_dir.join("shard.md");
            require_nonempty_file(&shard_path)?;
            Ok::<_, anyhow::Error>(shard_path)
        });
    }
    let mut failures = Vec::new();
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(path)) => println!("final review shard: {}", path.display()),
            Ok(Err(err)) => failures.push(format!("{err:#}")),
            Err(err) => failures.push(format!("final-review shard task panicked: {err}")),
        }
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("final review shard failure: {failure}");
        }
        bail!(
            "final review shard phase failed for {} shard(s)",
            failures.len()
        );
    }
    Ok(shard_root)
}

async fn run_final_review_repair_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
    attempt: usize,
) -> Result<()> {
    let review_path = final_review_markdown_path(paths);
    require_nonempty_file(&review_path)?;
    let archived_path = paths
        .report_root
        .join(format!("FINAL-REVIEW.no-go-attempt-{attempt}.md"));
    fs::copy(&review_path, &archived_path).with_context(|| {
        format!(
            "failed to archive {} to {}",
            review_path.display(),
            archived_path.display()
        )
    })?;
    let prompt = build_final_review_repair_prompt(paths, manifest, attempt, &archived_path);
    let config = PhaseConfig {
        model: args.remediation_model.clone(),
        effort: args.remediation_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let phase_slug = format!("final-review-repair-{attempt}");
    run_codex_phase(paths, &phase_slug, &prompt, &config).await?;
    let repair_path = paths
        .report_root
        .join(format!("FINAL-REVIEW-REPAIR-{attempt}.md"));
    require_nonempty_file(&repair_path)?;
    manifest.final_review_repairs.push(StageState {
        status: StageStatus::Complete,
        artifact: Some(path_display(&repair_path)),
        note: Some(format!("repaired NO-GO final review attempt {attempt}")),
    });
    write_manifest(paths, manifest)?;
    commit_worktree_changes(paths, manifest)?;
    Ok(())
}

pub(crate) fn attempt_merge(
    repo_root: &std::path::Path,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if matches!(manifest.merge.status, StageStatus::Complete) {
        println!("merge:       complete (resume)");
        return Ok(());
    }
    if !final_review_is_go(paths) {
        manifest.merge.status = StageStatus::Skipped;
        manifest.merge.note = Some("final review did not contain `Verdict: GO`".to_string());
        write_manifest(paths, manifest)?;
        bail!("final review is not GO; not attempting merge");
    }
    require_file_quality_acceptance(manifest)?;
    if manifest.in_place {
        return complete_in_place_run(paths, manifest);
    }

    commit_worktree_changes(paths, manifest)?;

    let current_branch = crate::util::git_stdout(repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string();
    if current_branch != manifest.branch {
        bail!(
            "merge requires canonical checkout on `{}` (current: `{}`)",
            manifest.branch,
            current_branch
        );
    }
    let status = crate::util::git_stdout(repo_root, ["status", "--short"])?;
    if !status.trim().is_empty() {
        bail!(
            "canonical checkout is dirty; clean it before merging professional audit branch:\n{}",
            status
        );
    }
    if remote_branch_exists(repo_root, &manifest.branch) {
        let _ = crate::util::run_git(repo_root, ["pull", "--rebase", "origin", &manifest.branch]);
    }
    crate::util::run_git(repo_root, ["merge", "--ff-only", &manifest.audit_branch])?;
    if remote_branch_exists(repo_root, &manifest.branch) {
        crate::util::run_git(repo_root, ["push", "origin", &manifest.branch])?;
    }
    manifest.merge.status = StageStatus::Complete;
    manifest.merge.note = Some(format!("merged {}", manifest.audit_branch));
    write_manifest(paths, manifest)?;
    Ok(())
}

pub(crate) fn complete_in_place_run(
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if matches!(manifest.merge.status, StageStatus::Complete) {
        println!("merge:       complete (in-place resume)");
        return Ok(());
    }
    if !final_review_is_go(paths) {
        manifest.merge.status = StageStatus::Skipped;
        manifest.merge.note =
            Some("in-place run did not reach `Verdict: GO`; changes left for review".to_string());
        write_manifest(paths, manifest)?;
        bail!("final review is not GO; in-place run left changes for review");
    }
    require_file_quality_acceptance(manifest)?;
    commit_worktree_changes(paths, manifest)?;
    manifest.merge.status = StageStatus::Complete;
    manifest.merge.note =
        Some("in-place run: audit changes are committed on the primary checkout".to_string());
    write_manifest(paths, manifest)?;
    Ok(())
}

pub(crate) fn refresh_final_status_after_merge(
    repo_root: &std::path::Path,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if !matches!(manifest.merge.status, StageStatus::Complete) {
        return Ok(());
    }
    manifest.final_status.status = StageStatus::Complete;
    manifest.final_status.artifact = Some(path_display(&run_status_markdown_path(paths)));
    manifest.final_status.note = Some(
        "RUN-STATUS refreshed after merge completion; committed status is generated immediately before the final status commit, so use `git rev-parse` for exact post-commit heads"
            .to_string(),
    );
    write_manifest(paths, manifest)?;
    commit_worktree_changes(paths, manifest)?;

    if manifest.in_place {
        return Ok(());
    }

    let current_branch = crate::util::git_stdout(repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string();
    if current_branch != manifest.branch {
        bail!(
            "final status refresh requires canonical checkout on `{}` (current: `{}`)",
            manifest.branch,
            current_branch
        );
    }
    let status = crate::util::git_stdout(repo_root, ["status", "--short"])?;
    if !status.trim().is_empty() {
        bail!(
            "canonical checkout is dirty after merge; cannot land final status refresh:\n{}",
            status
        );
    }
    crate::util::run_git(repo_root, ["merge", "--ff-only", &manifest.audit_branch])?;
    if remote_branch_exists(repo_root, &manifest.branch) {
        crate::util::run_git(repo_root, ["push", "origin", &manifest.branch])?;
    }
    Ok(())
}

pub(crate) fn require_context_complete(manifest: &EverythingManifest) -> Result<()> {
    if !matches!(manifest.context.status, StageStatus::Complete) {
        bail!("context phase is not complete; run --everything-phase init-context first");
    }
    Ok(())
}

pub(crate) fn require_first_pass_complete(manifest: &EverythingManifest) -> Result<()> {
    let incomplete = manifest
        .files
        .iter()
        .filter(|file| !matches!(file.status, StageStatus::Complete))
        .count();
    if incomplete > 0 {
        bail!("first pass has {incomplete} incomplete file(s)");
    }
    Ok(())
}

pub(crate) fn require_synthesis_complete(manifest: &EverythingManifest) -> Result<()> {
    require_first_pass_complete(manifest)?;
    let incomplete = manifest
        .groups
        .iter()
        .filter(|group| !matches!(group.synthesis_status, StageStatus::Complete))
        .count();
    if incomplete > 0 {
        bail!("synthesis has {incomplete} incomplete group(s)");
    }
    Ok(())
}

pub(crate) fn final_review_is_go(paths: &RunPaths) -> bool {
    let path = final_review_markdown_path(paths);
    fs::read_to_string(path)
        .map(|text| terminal_verdict_is(&text, "Verdict: GO", &["Verdict: GO", "Verdict: NO-GO"]))
        .unwrap_or(false)
}

pub(crate) fn final_review_has_actionable_blockers(paths: &RunPaths) -> bool {
    let path = final_review_markdown_path(paths);
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    if !terminal_verdict_is(&text, "Verdict: NO-GO", &["Verdict: GO", "Verdict: NO-GO"]) {
        return false;
    }
    let Some(section) = markdown_section(&text, "Required blockers before merge") else {
        return false;
    };
    section.lines().any(actionable_blocker_line)
}

/// Return the body of the markdown section whose heading text matches `heading`
/// (case-insensitively, at any `#` level), ending before the next heading.
///
/// This is a heading-name parser distinct from the substring-anchored
/// `crate::generation::markdown` parsers; it intentionally stops at headings of
/// any level.
fn markdown_section<'a>(text: &'a str, heading: &str) -> Option<&'a str> {
    let mut start = None;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let line_start = offset + line.len();
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let normalized = trimmed.trim_start_matches('#').trim();
            if let Some(start) = start {
                return Some(&text[start..offset]);
            }
            if normalized.eq_ignore_ascii_case(heading) {
                start = Some(line_start);
            }
        }
        offset += line.len();
    }
    start.map(|start| &text[start..])
}

fn actionable_blocker_line(line: &str) -> bool {
    let trimmed = line.trim();
    let Some(item) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .map(str::trim)
    else {
        return false;
    };
    if item.is_empty() {
        return false;
    }
    let lowered = item.to_ascii_lowercase();
    !matches!(
        lowered.as_str(),
        "none" | "n/a" | "na" | "no blockers" | "no required blockers"
    )
}

fn first_verdict_line(text: &str) -> Option<String> {
    exact_terminal_verdict(text, &["Verdict: GO", "Verdict: NO-GO"])
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::final_review_has_actionable_blockers;
    use crate::audit_everything::run_paths::{RunPaths, PAUSE_REQUEST_FILE};
    use std::fs;

    #[test]
    fn final_review_blocker_detection_requires_no_go_and_real_bullet() {
        let dir = std::env::temp_dir().join(format!(
            "auto-audit-final-review-blockers-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let report_root = dir.join("audit/everything/test-run");
        fs::create_dir_all(&report_root).expect("failed to create report root");
        let paths = RunPaths {
            host_root: dir.clone(),
            manifest_path: dir.join("MANIFEST.json"),
            latest_path: dir.join("latest-run"),
            worktree_root: dir.clone(),
            report_root,
            pause_path: dir.join(PAUSE_REQUEST_FILE),
            in_place: true,
        };
        fs::write(
            crate::audit_everything::run_paths::final_review_markdown_path(&paths),
            "# FINAL REVIEW\n\nVerdict: NO-GO\n\n## Required blockers before merge\n\n- Fix missing validation proof\n\n## Optional follow-ups\n\n- Later\n",
        )
        .expect("failed to write final review");
        assert!(final_review_has_actionable_blockers(&paths));

        fs::write(
            crate::audit_everything::run_paths::final_review_markdown_path(&paths),
            "# FINAL REVIEW\n\nVerdict: NO-GO\n\n## Required blockers before merge\n\n- none\n",
        )
        .expect("failed to rewrite final review");
        assert!(!final_review_has_actionable_blockers(&paths));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }
}
