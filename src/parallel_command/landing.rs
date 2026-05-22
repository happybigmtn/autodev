use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn try_spawn_lane_recovery_attempt(
    join_set: &mut JoinSet<LaneAttemptResult>,
    lane_config: &LaneRunConfig,
    prompt_template: &str,
    plan: &LoopPlanSnapshot,
    assignment: &mut ActiveLaneAssignment,
    target_branch: &str,
    max_retries: usize,
    parallel_logger: &ParallelEventLogger,
    reason: &str,
    recovery_note: String,
) -> Result<bool> {
    if assignment.attempts > max_retries {
        return Ok(false);
    }

    let next_attempt = assignment.attempts + 1;
    let total_attempts = max_retries + 1;
    parallel_logger.info(format!(
        "retry-needed: lane-{} `{}` {}; retrying attempt {}/{}",
        assignment.lane_index, assignment.task.id, reason, next_attempt, total_attempts
    ));
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &format!("retry-needed: {reason}; retrying attempt {next_attempt}/{total_attempts}"),
    );
    assignment.host_recovery_note = Some(recovery_note);
    spawn_parallel_lane_attempt(
        join_set,
        lane_config,
        prompt_template,
        plan,
        assignment,
        target_branch,
    )?;
    Ok(true)
}

pub(crate) fn repair_parallel_canonical_before_dispatch(
    repo_root: &Path,
    target_branch: &str,
    parallel_logger: &ParallelEventLogger,
) -> Result<()> {
    let rebase_merge = git_path(repo_root, "rebase-merge");
    if let Some(path) = rebase_merge.as_ref().filter(|path| path.exists()) {
        let issue = lane_repo_rebase_recovery_issue(repo_root);
        if issue.is_some() {
            let _ = run_git(repo_root, ["rebase", "--abort"]);
            if path.exists() {
                fs::remove_dir_all(path)
                    .with_context(|| format!("failed to remove stale {}", path.display()))?;
            }
            parallel_logger.warn(format!(
                "repair: removed stale canonical rebase metadata before dispatch ({})",
                issue.unwrap_or_else(|| "rebase-merge".to_string())
            ));
        } else {
            bail!(
                "canonical repo has active rebase metadata at {}; resolve it before dispatch",
                path.display()
            );
        }
    }
    repair_stale_git_index_lock(repo_root, parallel_logger, "before dispatch")?;
    let dirty = git_stdout(repo_root, ["status", "--short", "--untracked-files=all"])?;
    let dirty_paths = dirty
        .lines()
        .filter_map(parse_parallel_status_path)
        .filter(|path| !parallel_dispatch_path_is_ignored(path))
        .collect::<Vec<_>>();
    if !dirty_paths.is_empty() {
        let dirty_summary = dirty_paths.join(", ");
        if let Some(commit) = checkpoint_parallel_dispatch_paths(
            repo_root,
            target_branch,
            &dirty_paths,
            "auto parallel checkpoint",
        )? {
            parallel_logger.info(format!(
                "checkpoint: committed dirty canonical dispatch paths at {commit} before dispatch ({dirty_summary})"
            ));
        }

        let remaining_dirty =
            git_stdout(repo_root, ["status", "--short", "--untracked-files=all"])?;
        let remaining_dirty_paths = remaining_dirty
            .lines()
            .filter_map(parse_parallel_status_path)
            .filter(|path| !parallel_dispatch_path_is_ignored(path))
            .collect::<Vec<_>>();
        if !remaining_dirty_paths.is_empty() {
            bail!(
                "canonical repo has dirty tracked dispatch paths before auto parallel dispatch and automatic checkpointing did not clear them: {}. Commit, stash, or revert them before launching lanes",
                remaining_dirty_paths.join(", ")
            );
        }
    }
    Ok(())
}

pub(crate) fn checkpoint_parallel_dispatch_paths(
    repo_root: &Path,
    target_branch: &str,
    dirty_paths: &[String],
    message_suffix: &str,
) -> Result<Option<String>> {
    if dirty_paths.is_empty() {
        return Ok(None);
    }
    let current_branch = git_stdout(repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim();
    if current_branch.is_empty() {
        bail!("refusing to checkpoint dirty dispatch paths from detached HEAD");
    }
    if current_branch != target_branch {
        bail!(
            "refusing to checkpoint branch `{target_branch}` while checked out on `{current_branch}`; checkout `{target_branch}` or pass the current branch explicitly"
        );
    }
    let mut add_args = vec!["add".to_string(), "--all".to_string(), "--".to_string()];
    add_args.extend(dirty_paths.iter().cloned());
    run_git(repo_root, add_args.iter().map(|arg| arg.as_str()))?;
    let staged = git_stdout(repo_root, ["diff", "--cached", "--name-only"])?;
    if staged.trim().is_empty() {
        return Ok(None);
    }
    let message = format!("{}: {message_suffix}", repo_name(repo_root));
    run_git(repo_root, ["commit", "-m", &message])?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    let commit = commit.trim().to_string();
    if let Err(err) = push_branch_with_remote_sync(repo_root, target_branch) {
        bail!(
            "created checkpoint commit {} but failed to sync/push: {err}",
            commit
        );
    }
    Ok(Some(commit))
}

pub(crate) fn checkpoint_parallel_host_queue_changes(
    repo_root: &Path,
    target_branch: &str,
    parallel_logger: &ParallelEventLogger,
) -> Result<Option<String>> {
    repair_stale_git_index_lock(repo_root, parallel_logger, "before host queue sync")?;
    let queue_files = host_queue_state_files_for_repo(repo_root);
    if queue_files.is_empty() {
        return Ok(None);
    }

    let mut status_args = vec!["status", "--short", "--"];
    status_args.extend(queue_files.iter().copied());
    let status = git_stdout(repo_root, status_args)?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    let mut add_args = vec!["add", "--all", "--"];
    add_args.extend(queue_files.iter().copied());
    run_git(repo_root, add_args)?;
    let message = format!("{}: parallel host queue sync", repo_name(repo_root));
    run_git(repo_root, ["commit", "-m", &message])?;
    let commit = git_stdout(repo_root, ["rev-parse", "--short", "HEAD"])?;
    let commit = commit.trim().to_string();
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        parallel_logger.info(format!(
            "remote sync: rebased onto origin/{} after host queue sync",
            target_branch
        ));
    }
    parallel_logger.info(format!(
        "host sync:  committed queue-state changes at {commit}"
    ));
    Ok(Some(commit))
}

pub(crate) fn try_checkpoint_parallel_host_queue_changes(
    repo_root: &Path,
    target_branch: &str,
    parallel_logger: &ParallelEventLogger,
) {
    if let Err(err) =
        checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger)
    {
        parallel_logger.warn(format!(
            "warning: failed syncing host-owned queue state; continuing without a host queue commit: {err:#}"
        ));
    }
}

pub(crate) fn host_queue_state_files_for_repo(repo_root: &Path) -> Vec<&'static str> {
    HOST_QUEUE_STATE_FILES
        .into_iter()
        .filter(|relative| repo_path_exists_or_is_tracked(repo_root, relative))
        .collect()
}

pub(crate) fn repo_path_exists_or_is_tracked(repo_root: &Path, relative: &str) -> bool {
    if repo_root.join(relative).exists() {
        return true;
    }
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "--error-unmatch", "--", relative])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(crate) fn inspect_lane_repo_progress_or_shelve(
    assignment: &ActiveLaneAssignment,
    parallel_logger: &ParallelEventLogger,
    shelved_tasks: &mut BTreeMap<String, String>,
    action: &str,
) -> Option<LaneRepoProgress> {
    match inspect_lane_repo_progress(&assignment.lane_repo_root, &assignment.base_commit) {
        Ok(progress) => Some(progress),
        Err(err) => {
            shelve_lane_after_host_failure(
                assignment,
                parallel_logger,
                shelved_tasks,
                &format!("{action}: {err:#}"),
            );
            None
        }
    }
}

pub(crate) fn shelve_lane_after_host_failure(
    assignment: &ActiveLaneAssignment,
    parallel_logger: &ParallelEventLogger,
    shelved_tasks: &mut BTreeMap<String, String>,
    reason: &str,
) {
    parallel_logger.warn(format!(
        "warning: host-side bookkeeping failed for lane-{} `{}`; shelving for the rest of this run: {}",
        assignment.lane_index, assignment.task.id, reason
    ));
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &format!("shelved: host-side bookkeeping failure: {reason}"),
    );
    shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ReceiptDriftTriageEntry {
    pub(crate) task_id: String,
    pub(crate) title: String,
    pub(crate) status: LoopTaskStatus,
    pub(crate) reasons: Vec<String>,
}

pub(crate) fn audit_parallel_completion_drift(
    repo_root: &Path,
    target_branch: &str,
    plan_text: &str,
    parallel_logger: &ParallelEventLogger,
) -> Result<String> {
    let snapshot = parse_loop_plan(plan_text);
    let mut updated_plan_text = plan_text.to_string();
    let mut completed_drift = Vec::new();
    let mut backfilled_receipts = Vec::new();
    let mut closed_partial_receipts = Vec::new();

    for task in snapshot
        .tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Done)
    {
        let evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
        if evidence.is_fully_evidenced() {
            continue;
        }
        if backfill_completed_legacy_receipt_footer(repo_root, task, &evidence)? {
            let refreshed = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
            if refreshed.is_fully_evidenced() {
                backfilled_receipts.push(task.id.clone());
                continue;
            }
        }
        let entry = ReceiptDriftTriageEntry {
            task_id: task.id.clone(),
            title: task.title.clone(),
            status: task.status,
            reasons: evidence.missing_reasons(),
        };
        completed_drift.push(entry);
    }

    for task in snapshot
        .tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Partial)
    {
        let evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
        if !evidence.is_fully_evidenced() {
            continue;
        }
        updated_plan_text =
            update_task_completion_in_plan_text(&updated_plan_text, &task.id, LoopTaskStatus::Done);
        closed_partial_receipts.push(task.id.clone());
    }

    if updated_plan_text != plan_text {
        let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
        atomic_write(&plan_path, updated_plan_text.as_bytes())
            .with_context(|| format!("failed to write {}", plan_path.display()))?;
    }
    let triage_changed =
        if !completed_drift.is_empty() || repo_root.join("RECEIPTS-DRIFT.md").exists() {
            write_receipts_drift_triage(repo_root, completed_drift.as_slice(), &[])?
        } else {
            false
        };
    if !backfilled_receipts.is_empty() {
        if push_branch_with_remote_sync(repo_root, target_branch)? {
            parallel_logger.info(format!(
                "remote sync: rebased onto origin/{} after receipt footer backfill",
                target_branch
            ));
        }
        parallel_logger.info(format!(
            "receipt-backfill: footerized {} completed task receipt(s) ({})",
            backfilled_receipts.len(),
            backfilled_receipts.join(", ")
        ));
    }
    if !closed_partial_receipts.is_empty() {
        parallel_logger.info(format!(
            "receipt-closeout: closed {} partial task(s) from canonical repo-local evidence ({})",
            closed_partial_receipts.len(),
            closed_partial_receipts.join(", ")
        ));
    }
    if triage_changed && !completed_drift.is_empty() {
        parallel_logger.warn(format!(
            "warning: repo-local completion evidence drifted for {} completed task(s); wrote RECEIPTS-DRIFT.md and left IMPLEMENTATION_PLAN.md unchanged ({})",
            completed_drift.len(),
            completed_drift
                .iter()
                .map(|entry| entry.task_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(updated_plan_text)
}

pub(crate) fn backfill_completed_legacy_receipt_footer(
    repo_root: &Path,
    task: &LoopTask,
    evidence: &crate::completion_artifacts::TaskCompletionEvidence,
) -> Result<bool> {
    if !evidence.has_review_handoff
        || !evidence.missing_completion_artifacts.is_empty()
        || !evidence.unresolved_audit_findings.is_empty()
    {
        return Ok(false);
    }
    let Some(footer) =
        legacy_verification_receipt_backfill_footer(repo_root, &task.id, &task.markdown)?
    else {
        return Ok(false);
    };
    if repo_has_staged_queue_updates(repo_root)? {
        return Ok(false);
    }
    let message = format!(
        "{}: {} receipt footer backfill",
        repo_name(repo_root),
        task.id
    );
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("commit")
        .arg("--allow-empty")
        .arg("-m")
        .arg(&message)
        .arg("-m")
        .arg(&footer)
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(true);
    }
    bail!(
        "git command failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(crate) fn write_receipts_drift_triage(
    repo_root: &Path,
    completed_drift: &[ReceiptDriftTriageEntry],
    manual_closeout_candidates: &[ReceiptDriftTriageEntry],
) -> Result<bool> {
    let path = repo_root.join("RECEIPTS-DRIFT.md");
    let body = render_receipts_drift_triage(completed_drift, manual_closeout_candidates);
    if fs::read_to_string(&path).is_ok_and(|existing| existing == body) {
        return Ok(false);
    }
    atomic_write(&path, body.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

pub(crate) fn render_receipts_drift_triage(
    completed_drift: &[ReceiptDriftTriageEntry],
    manual_closeout_candidates: &[ReceiptDriftTriageEntry],
) -> String {
    let mut body = String::from("# Receipt Drift Triage\n\n");
    body.push_str(
        "This file is generated by `auto parallel` when repo-local completion evidence no longer matches queue status. Sync passes warn here instead of mutating `IMPLEMENTATION_PLAN.md`.\n\n",
    );

    if completed_drift.is_empty() && manual_closeout_candidates.is_empty() {
        body.push_str("No repo-local receipt drift detected.\n");
        return body;
    }

    body.push_str("## Completed Tasks With Drift\n\n");
    if completed_drift.is_empty() {
        body.push_str("- None\n\n");
    } else {
        for entry in completed_drift {
            body.push_str(&render_receipts_drift_entry(entry));
        }
        body.push('\n');
    }

    body.push_str("## Manual Closeout Candidates\n\n");
    if manual_closeout_candidates.is_empty() {
        body.push_str("- None\n");
    } else {
        for entry in manual_closeout_candidates {
            body.push_str(&render_receipts_drift_entry(entry));
        }
    }

    body
}

pub(crate) fn render_receipts_drift_entry(entry: &ReceiptDriftTriageEntry) -> String {
    let marker = match entry.status {
        LoopTaskStatus::Pending => "[ ]",
        LoopTaskStatus::Blocked => "[!]",
        LoopTaskStatus::Partial => "[~]",
        LoopTaskStatus::Done => "[x]",
    };
    let mut rendered = format!("- {marker} `{}` {}\n", entry.task_id, entry.title);
    if entry.reasons.is_empty() {
        rendered.push_str("  - Reason: no specific evidence gap reported\n");
    } else {
        for reason in &entry.reasons {
            rendered.push_str(&format!("  - Reason: {reason}\n"));
        }
    }
    rendered
}

pub(crate) fn land_parallel_lane_result(
    repo_root: &Path,
    target_branch: &str,
    assignment: &mut ActiveLaneAssignment,
) -> Result<LaneLandingOutcome> {
    let mut auto_repaired = false;
    let mut canonical_checkpointed = false;
    let (final_lane_head, final_range_base) = loop {
        let lane_head = git_stdout(&assignment.lane_repo_root, ["rev-parse", "HEAD"])?;
        let lane_head = lane_head.trim().to_string();
        fetch_lane_commit(repo_root, &assignment.lane_repo_root, &lane_head)?;
        let landing_base = git_stdout(repo_root, ["merge-base", "HEAD", "FETCH_HEAD"])?;
        let landing_base = landing_base.trim().to_string();
        let range_base = if landing_base.is_empty() {
            assignment.base_commit.clone()
        } else {
            landing_base
        };
        if !git_ref_is_ancestor(repo_root, "FETCH_HEAD", "HEAD")? {
            if let Err(err) = cherry_pick_lane_range(
                repo_root,
                &range_base,
                "FETCH_HEAD",
                CherryPickFailurePolicy::Abort,
            ) {
                if !canonical_checkpointed
                    && landing_error_suggests_dirty_canonical_worktree(&err)
                    && try_auto_checkpoint_canonical_for_landing(
                        repo_root,
                        target_branch,
                        assignment,
                        "before retrying lane landing against local canonical changes",
                    )?
                {
                    canonical_checkpointed = true;
                    continue;
                }
                if auto_repaired {
                    return Err(err).with_context(|| {
                        format!(
                            "failed landing lane-{} task `{}` from {} after host auto-repair",
                            assignment.lane_index,
                            assignment.task.id,
                            assignment.lane_repo_root.display()
                        )
                    });
                }
                match prepare_lane_landing_recovery(
                    assignment,
                    target_branch,
                    &range_base,
                    &format!("{err:#}"),
                )
                .with_context(|| {
                    format!(
                        "failed preparing lane-{} task `{}` for landing recovery",
                        assignment.lane_index, assignment.task.id
                    )
                })? {
                    LaneLandingRecoveryPrep::RebasedCleanly => {
                        auto_repaired = true;
                        continue;
                    }
                    LaneLandingRecoveryPrep::NeedsWorkerResolution(recovery_note) => {
                        return Ok(LaneLandingOutcome::NeedsRecovery(recovery_note));
                    }
                }
            }
        }
        break (lane_head, range_base);
    };
    let changed_files = lane_changed_files(
        &assignment.lane_repo_root,
        &final_range_base,
        &final_lane_head,
    )?;
    // Receipts the worker wrote to its lane worktree don't reach canonical via
    // cherry-pick — the worker prompt forbids committing them (`.auto/symphony/
    // verification-receipts/*.json` is "staging evidence"). Without those files
    // in canonical, `reconcile_parallel_landed_task` -> `inspect_task_completion_
    // evidence` cannot see `verification_receipt_present`, so every landing
    // returns Partial and tasks stay [~] forever. Copy the lane's receipts here
    // so the host can read them in the same harvest cycle.
    if let Err(err) =
        propagate_lane_receipts(&assignment.lane_repo_root, repo_root, &assignment.task.id)
    {
        eprintln!(
            "warning: failed propagating lane-{} receipts for `{}`: {err:#}",
            assignment.lane_index, assignment.task.id
        );
    }
    let completion_status = reconcile_parallel_landed_task(repo_root, assignment, &changed_files)?;
    if completion_status == LoopTaskStatus::Done {
        assignment.task.status = LoopTaskStatus::Done;
    } else if completion_status == LoopTaskStatus::Partial {
        assignment.task.status = LoopTaskStatus::Partial;
    }
    if repo_has_staged_queue_updates(repo_root)? {
        let message = format!(
            "{}: {} queue sync",
            repo_name(repo_root),
            assignment.task.id
        );
        commit_task_closeout(repo_root, &assignment.task.id, &message, false)?;
    }
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        println!("remote sync: rebased onto origin/{}", target_branch);
    }
    Ok(LaneLandingOutcome::Landed {
        auto_repaired,
        completion_status,
    })
}

/// Copy a lane worker's `.auto/symphony/verification-receipts/<task>.json` and
/// `.auto/task-receipts/<task>/` into canonical so the host's `inspect_task_
/// completion_evidence` can see them when deciding `[~]` vs `[x]`. Workers are
/// explicitly forbidden from committing these files (per the lane prompt at
/// parallel_command.rs around line 6493), so without this propagation step the
/// host's evidence inspector always reports `verification_receipt_present:
/// false` and every landing returns Partial.
///
/// Only the named task's receipts are propagated; other lanes' receipts in the
/// lane worktree are left alone. Missing files are not an error — the lane
/// may legitimately have skipped the wrapper for non-verifiable tasks.
pub(crate) fn propagate_lane_receipts(
    lane_repo_root: &Path,
    canonical_root: &Path,
    task_id: &str,
) -> Result<()> {
    let receipt_rel = std::path::PathBuf::from(".auto/symphony/verification-receipts")
        .join(format!("{task_id}.json"));
    let src_receipt = lane_repo_root.join(&receipt_rel);
    let dst_receipt = canonical_root.join(&receipt_rel);
    if src_receipt.is_file() {
        if let Some(parent) = dst_receipt.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create canonical receipts dir {}",
                    parent.display()
                )
            })?;
        }
        // Read the lane's receipt, rewrite the `commit` and `dirty_state`
        // fields to match canonical HEAD before writing to canonical. The
        // lane worker recorded its own local commit SHA in the receipt,
        // but cherry-pick creates new commit SHAs in canonical — so the
        // recorded SHA is neither the current HEAD nor an ancestor of it,
        // which the host's freshness check rejects as "commit mismatch".
        // Rewriting to canonical's current state restores freshness.
        let lane_text = std::fs::read_to_string(&src_receipt).with_context(|| {
            format!(
                "failed to read lane symphony receipt {}",
                src_receipt.display()
            )
        })?;
        let mut value: serde_json::Value = serde_json::from_str(&lane_text).with_context(|| {
            format!(
                "failed to parse lane symphony receipt {} as JSON",
                src_receipt.display()
            )
        })?;
        if let Ok(canonical_commit) = git_stdout(canonical_root, ["rev-parse", "HEAD"]) {
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "commit".to_string(),
                    serde_json::Value::String(canonical_commit.trim().to_string()),
                );
            }
        }
        if let Ok(porcelain) = std::process::Command::new("git")
            .arg("-C")
            .arg(canonical_root)
            .args(["status", "--porcelain=v1", "-z"])
            .output()
        {
            if porcelain.status.success() {
                use sha2::{Digest, Sha256};
                let fp = format!("{:x}", Sha256::digest(&porcelain.stdout));
                if let Some(obj) = value.as_object_mut() {
                    let mut dirty = serde_json::Map::new();
                    dirty.insert("fingerprint".to_string(), serde_json::Value::String(fp));
                    obj.insert("dirty_state".to_string(), serde_json::Value::Object(dirty));
                }
            }
        }
        let pretty = serde_json::to_string_pretty(&value)
            .context("failed to re-serialize symphony receipt for canonical write")?;
        std::fs::write(&dst_receipt, pretty + "\n").with_context(|| {
            format!(
                "failed to write canonical symphony receipt {}",
                dst_receipt.display()
            )
        })?;
    }

    let task_receipts_rel = std::path::PathBuf::from(".auto/task-receipts").join(task_id);
    let src_task_dir = lane_repo_root.join(&task_receipts_rel);
    let dst_task_dir = canonical_root.join(&task_receipts_rel);
    if src_task_dir.is_dir() {
        std::fs::create_dir_all(&dst_task_dir).with_context(|| {
            format!(
                "failed to create canonical task-receipts dir {}",
                dst_task_dir.display()
            )
        })?;
        for entry in std::fs::read_dir(&src_task_dir).with_context(|| {
            format!(
                "failed to read lane task-receipts {}",
                src_task_dir.display()
            )
        })? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() {
                continue;
            }
            let dst = dst_task_dir.join(entry.file_name());
            if dst.exists() {
                continue;
            }
            std::fs::copy(entry.path(), &dst).with_context(|| {
                format!(
                    "failed to copy lane task-receipt {} -> {}",
                    entry.path().display(),
                    dst.display()
                )
            })?;
        }
    }
    Ok(())
}

pub(crate) fn reconcile_parallel_clean_no_commit(
    repo_root: &Path,
    target_branch: &str,
    assignment: &ActiveLaneAssignment,
    parallel_logger: &ParallelEventLogger,
) -> Result<bool> {
    write_clean_no_commit_verdict(
        assignment,
        "needs-human-triage",
        "lane exited cleanly without a local commit; canonical evidence will be inspected before shelving",
    )?;
    let evidence_before =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    let review_can_complete_evidence = !evidence_before.has_review_handoff
        && evidence_before.verification_receipt_present
        && evidence_before.missing_completion_artifacts.is_empty()
        && evidence_before.unresolved_audit_findings.is_empty();
    let review_added = if evidence_before.is_fully_evidenced() || review_can_complete_evidence {
        ensure_host_review_handoff(repo_root, &assignment.task.id, &[], &evidence_before)?
    } else {
        false
    };
    let evidence_after =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    if !evidence_after.is_fully_evidenced() {
        return Ok(false);
    }

    let plan_updated =
        update_task_completion_in_plan(repo_root, &assignment.task.id, LoopTaskStatus::Done)?;
    if review_added || plan_updated {
        let mut queue_files = Vec::new();
        if review_added {
            queue_files.push("REVIEW.md");
        }
        if plan_updated {
            queue_files.push("IMPLEMENTATION_PLAN.md");
        }
        let mut args = vec!["add"];
        args.extend(queue_files);
        run_git(repo_root, args)?;
        if repo_has_staged_queue_updates(repo_root)? {
            let message = format!(
                "{}: {} evidence self-heal",
                repo_name(repo_root),
                assignment.task.id
            );
            commit_task_closeout(repo_root, &assignment.task.id, &message, false)?;
            if push_branch_with_remote_sync(repo_root, target_branch)? {
                parallel_logger.info(format!(
                    "remote sync: rebased onto origin/{} after evidence self-heal",
                    target_branch
                ));
            }
        }
    } else {
        let message = format!(
            "{}: {} evidence closeout",
            repo_name(repo_root),
            assignment.task.id
        );
        commit_task_closeout(repo_root, &assignment.task.id, &message, true)?;
        if push_branch_with_remote_sync(repo_root, target_branch)? {
            parallel_logger.info(format!(
                "remote sync: rebased onto origin/{} after empty evidence closeout",
                target_branch
            ));
        }
    }
    write_clean_no_commit_verdict(
        assignment,
        "task-already-done",
        "canonical review, receipt, and declared artifact evidence are complete; host created an evidence closeout",
    )?;

    Ok(true)
}

pub(crate) fn recover_shelved_tasks_from_canonical_evidence(
    repo_root: &Path,
    target_branch: &str,
    shelved_tasks: &mut BTreeMap<String, String>,
    parallel_logger: &ParallelEventLogger,
) -> Result<usize> {
    let mut recovered = Vec::new();
    for (task_id, markdown) in shelved_tasks.clone() {
        let evidence = inspect_task_completion_evidence(repo_root, &task_id, &markdown);
        if !evidence.is_fully_evidenced() {
            continue;
        }
        let review_added = ensure_host_review_handoff(repo_root, &task_id, &[], &evidence)?;
        let plan_updated =
            update_task_completion_in_plan(repo_root, &task_id, LoopTaskStatus::Done)?;
        if review_added {
            run_git(repo_root, ["add", "REVIEW.md"])?;
        }
        if plan_updated {
            run_git(repo_root, ["add", "IMPLEMENTATION_PLAN.md"])?;
        }
        let message = format!("{}: {} evidence recovery", repo_name(repo_root), task_id);
        if repo_has_staged_queue_updates(repo_root)? {
            commit_task_closeout(repo_root, &task_id, &message, false)?;
        } else {
            commit_task_closeout(repo_root, &task_id, &message, true)?;
        }
        recovered.push(task_id);
    }

    if recovered.is_empty() {
        return Ok(0);
    }
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        parallel_logger.info(format!(
            "remote sync: rebased onto origin/{} after shelved evidence recovery",
            target_branch
        ));
    }
    for task_id in &recovered {
        shelved_tasks.remove(task_id);
    }
    parallel_logger.info(format!(
        "self-heal: recovered {} shelved task(s) from canonical evidence before NO-GO ({})",
        recovered.len(),
        recovered.join(", ")
    ));
    Ok(recovered.len())
}

pub(crate) fn write_clean_no_commit_verdict(
    assignment: &ActiveLaneAssignment,
    verdict: &str,
    reason: &str,
) -> Result<()> {
    let path = assignment.lane_root.join("clean-no-commit-verdict.json");
    let payload = serde_json::json!({
        "task_id": assignment.task.id,
        "lane_index": assignment.lane_index,
        "verdict": verdict,
        "reason": reason,
    });
    let text = serde_json::to_vec_pretty(&payload)?;
    atomic_write(&path, &text).with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn reconcile_parallel_landed_task(
    repo_root: &Path,
    assignment: &ActiveLaneAssignment,
    changed_files: &[String],
) -> Result<LoopTaskStatus> {
    let evidence_before =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    let review_added = ensure_host_review_handoff(
        repo_root,
        &assignment.task.id,
        changed_files,
        &evidence_before,
    )?;
    let evidence_after =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    let completion_status = if evidence_after.is_fully_evidenced() {
        LoopTaskStatus::Done
    } else {
        LoopTaskStatus::Partial
    };

    let plan_updated =
        update_task_completion_in_plan(repo_root, &assignment.task.id, completion_status)?;
    if review_added || plan_updated {
        let mut queue_files = Vec::new();
        if review_added {
            queue_files.push("REVIEW.md");
        }
        if plan_updated {
            queue_files.push("IMPLEMENTATION_PLAN.md");
        }
        if !queue_files.is_empty() {
            let mut args = vec!["add"];
            args.extend(queue_files);
            run_git(repo_root, args)?;
        }
    }
    Ok(completion_status)
}

pub(crate) fn repo_has_staged_queue_updates(repo_root: &Path) -> Result<bool> {
    let output = git_stdout(repo_root, ["diff", "--cached", "--name-only"])?;
    Ok(output.lines().any(|line| !line.trim().is_empty()))
}

pub(crate) fn commit_task_closeout(
    repo_root: &Path,
    task_id: &str,
    message: &str,
    allow_empty: bool,
) -> Result<()> {
    let footer = verification_receipt_commit_footer(repo_root, task_id)?;
    let mut command = Command::new("git");
    command.arg("-C").arg(repo_root).arg("commit");
    if allow_empty {
        command.arg("--allow-empty");
    }
    command.arg("-m").arg(message);
    if let Some(footer) = footer {
        command.arg("-m").arg(footer);
    }
    let output = command
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git command failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

pub(crate) fn prepare_lane_landing_recovery(
    assignment: &mut ActiveLaneAssignment,
    target_branch: &str,
    range_base: &str,
    landing_error: &str,
) -> Result<LaneLandingRecoveryPrep> {
    let status = git_stdout(&assignment.lane_repo_root, ["status", "--short"])?;
    let status = status.trim();
    if !status.is_empty() {
        bail!(
            "lane-{} `{}` cannot enter landing recovery because its repo is already dirty:\n{}",
            assignment.lane_index,
            assignment.task.id,
            status
        );
    }

    let original_lane_head = git_stdout(&assignment.lane_repo_root, ["rev-parse", "HEAD"])?;
    let original_lane_head = original_lane_head.trim().to_string();
    let remote_name = lane_remote_name(&assignment.lane_repo_root)?;
    run_git(
        &assignment.lane_repo_root,
        ["fetch", "--quiet", &remote_name, target_branch],
    )?;
    let recovery_base = git_stdout(&assignment.lane_repo_root, ["rev-parse", "FETCH_HEAD"])?;
    let recovery_base = recovery_base.trim().to_string();
    if recovery_base.is_empty() {
        bail!(
            "lane-{} `{}` landing recovery could not resolve FETCH_HEAD",
            assignment.lane_index,
            assignment.task.id
        );
    }

    run_git(
        &assignment.lane_repo_root,
        ["reset", "--hard", recovery_base.as_str()],
    )?;
    assignment.base_commit = recovery_base;
    match cherry_pick_lane_range(
        &assignment.lane_repo_root,
        range_base,
        &original_lane_head,
        CherryPickFailurePolicy::LeaveInProgress,
    ) {
        Ok(()) => Ok(LaneLandingRecoveryPrep::RebasedCleanly),
        Err(err) => Ok(LaneLandingRecoveryPrep::NeedsWorkerResolution(
            prepared_landing_recovery_note(target_branch, landing_error, &format!("{err:#}")),
        )),
    }
}

pub(crate) fn cherry_pick_lane_range(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
    failure_policy: CherryPickFailurePolicy,
) -> Result<()> {
    if lane_changed_files(repo_root, base_commit, head_ref)?.is_empty() {
        return Ok(());
    }

    scrub_parallel_receipt_staging(repo_root)?;
    let range = format!("{base_commit}..{head_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cherry-pick")
        .arg("--empty=drop")
        .arg(&range)
        .output()
        .with_context(|| format!("failed to cherry-pick {range} in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }

    if failure_policy == CherryPickFailurePolicy::Abort {
        let _ = run_git(repo_root, ["cherry-pick", "--abort"]);
    }
    bail!(
        "git cherry-pick failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

pub(crate) fn scrub_parallel_receipt_staging(repo_root: &Path) -> Result<()> {
    let receipt_dir = repo_root.join(".auto/symphony/verification-receipts");
    if !receipt_dir.exists() {
        return Ok(());
    }
    let _ = run_git(
        repo_root,
        [
            "restore",
            "--staged",
            "--worktree",
            "--",
            ".auto/symphony/verification-receipts",
        ],
    );
    let _ = run_git(
        repo_root,
        ["clean", "-fd", "--", ".auto/symphony/verification-receipts"],
    );
    Ok(())
}

pub(crate) fn landing_error_suggests_dirty_canonical_worktree(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("would be overwritten by merge")
        || message.contains("please commit your changes or stash them")
        || message.contains("untracked working tree files would be overwritten")
}

pub(crate) fn try_auto_checkpoint_canonical_for_landing(
    repo_root: &Path,
    target_branch: &str,
    assignment: &ActiveLaneAssignment,
    reason: &str,
) -> Result<bool> {
    let Some(commit) =
        auto_checkpoint_if_needed(repo_root, target_branch, "auto parallel checkpoint")?
    else {
        return Ok(false);
    };
    println!(
        "checkpoint:  committed canonical changes at {commit} {reason} for lane-{} `{}`",
        assignment.lane_index, assignment.task.id
    );
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &format!("checkpoint: committed canonical changes at {commit} {reason}"),
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use crate::parallel_command::*;
    use anyhow::anyhow;
    use std::time::UNIX_EPOCH;

    fn init_git_repo(path: &PathBuf) {
        fs::create_dir_all(path).expect("failed to create repo dir");
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(path)
            .status()
            .expect("failed to run git init");
        assert!(status.success(), "git init should succeed");
        git_ok(path, ["config", "user.email", "test@example.com"]);
        git_ok(path, ["config", "user.name", "Autodev Test"]);
    }

    fn run_git_in<'a>(repo: &std::path::Path, args: impl IntoIterator<Item = &'a str>) -> String {
        let output = Command::new("git")
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
        String::from_utf8(output.stdout).expect("git stdout should be utf-8")
    }

    fn init_remote_and_clones(name: &str, branch: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = unique_temp_dir(name);
        let remote = root.join("remote.git");
        let upstream = root.join("upstream");
        let worker = root.join("worker");

        fs::create_dir_all(&root).expect("failed to create temp root");
        run_git_in(
            &root,
            [
                "init",
                "--bare",
                remote.to_str().expect("remote path should be utf-8"),
            ],
        );
        run_git_in(
            &root,
            [
                "clone",
                remote.to_str().expect("remote path should be utf-8"),
                upstream.to_str().expect("upstream path should be utf-8"),
            ],
        );
        run_git_in(&upstream, ["config", "user.name", "autodev tests"]);
        run_git_in(&upstream, ["config", "user.email", "autodev@example.com"]);
        fs::write(upstream.join("README.md"), "# init\n").expect("failed to write README");
        run_git_in(&upstream, ["add", "README.md"]);
        run_git_in(&upstream, ["commit", "-m", "init"]);
        run_git_in(&upstream, ["branch", "-M", branch]);
        run_git_in(&upstream, ["push", "-u", "origin", branch]);

        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                branch,
                remote.to_str().expect("remote path should be utf-8"),
                worker.to_str().expect("worker path should be utf-8"),
            ],
        );
        run_git_in(&worker, ["config", "user.name", "autodev tests"]);
        run_git_in(&worker, ["config", "user.email", "autodev@example.com"]);

        (root, remote, upstream, worker)
    }

    fn git_ok<const N: usize>(repo: &PathBuf, args: [&str; N]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output<const N: usize>(repo: &PathBuf, args: [&str; N]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    fn set_file_mtime_epoch(path: &std::path::Path) {
        let status = Command::new("touch")
            .args(["-d", "@1"])
            .arg(path)
            .status()
            .expect("failed to run touch");
        assert!(status.success(), "touch should update test file mtime");
    }

    #[test]
    fn host_queue_sync_failures_are_logged_without_aborting() {
        let run_root = unique_temp_dir("parallel-host-queue-warning");
        let repo_root = unique_temp_dir("parallel-host-queue-warning-repo");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        fs::create_dir_all(&repo_root).expect("failed to create repo root");
        fs::write(repo_root.join("IMPLEMENTATION_PLAN.md"), "# plan\n")
            .expect("failed to write queue file");

        let logger = ParallelEventLogger::new(&run_root).expect("parallel logger should init");
        try_checkpoint_parallel_host_queue_changes(&repo_root, "main", &logger);

        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("live log should be readable");
        assert!(live_log.contains("failed syncing host-owned queue state"));
        assert!(live_log.contains("continuing without a host queue commit"));

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
        fs::remove_dir_all(&repo_root).expect("failed to remove repo root");
    }

    #[test]
    fn dirty_canonical_landing_errors_are_detected() {
        let err = anyhow!(
            "git cherry-pick failed in /tmp/repo: error: Your local changes to the following files would be overwritten by merge:\n  src/lib.rs\nPlease commit your changes or stash them before you merge.\nAborting\nfatal: cherry-pick failed"
        );
        assert!(landing_error_suggests_dirty_canonical_worktree(&err));
    }

    #[test]
    fn repair_parallel_canonical_checkpoints_dirty_dispatch_paths() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-checkpoint", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        fs::write(worker.join("README.md"), "# dirty\n").expect("failed to dirty README");

        repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect("dirty dispatch paths should be checkpointed");

        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        let log = run_git_in(&worker, ["log", "--format=%s", "-1"]);
        assert_eq!(log.trim(), "worker: auto parallel checkpoint");
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn repair_parallel_canonical_ignores_verification_receipt_staging() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-receipts", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let receipt_dir = worker.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("failed to create receipts dir");
        fs::write(receipt_dir.join("TASK-1.json"), "{\"status\":\"passed\"}\n")
            .expect("failed to write receipt");

        repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect("receipt staging should not block dispatch");

        let status = run_git_in(&worker, ["status", "--short", "--untracked-files=all"]);
        assert!(status.contains(".auto/symphony/verification-receipts/TASK-1.json"));
        let log = run_git_in(&worker, ["log", "--format=%s", "-1"]);
        assert_ne!(log.trim(), "worker: auto parallel checkpoint");
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn repair_parallel_canonical_removes_stale_zero_byte_index_lock() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-stale-index-lock", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let lock = worker.join(".git").join("index.lock");
        fs::write(&lock, "").expect("failed to write stale index lock");
        set_file_mtime_epoch(&lock);

        repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect("stale zero-byte index lock should be repaired");

        assert!(!lock.exists(), "stale index lock should be removed");
        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("live log should be readable");
        assert!(live_log.contains("removed stale canonical git index lock"));
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn repair_parallel_canonical_refuses_fresh_zero_byte_index_lock() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-fresh-index-lock", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let lock = worker.join(".git").join("index.lock");
        fs::write(&lock, "").expect("failed to write fresh index lock");

        let err = repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect_err("fresh index lock should require operator confirmation");

        assert!(lock.exists(), "fresh index lock should remain in place");
        let message = err.to_string();
        assert!(message.contains("active git index lock"));
        assert!(message.contains("context=before dispatch"));
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn repair_parallel_canonical_refuses_non_empty_stale_index_lock() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-nonempty-index-lock", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let lock = worker.join(".git").join("index.lock");
        fs::write(&lock, "git pid maybe alive\n").expect("failed to write non-empty index lock");
        set_file_mtime_epoch(&lock);

        let err = repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect_err("non-empty index lock should not be auto-removed");

        assert!(lock.exists(), "non-empty index lock should remain in place");
        let message = err.to_string();
        assert!(message.contains("active git index lock"));
        assert!(message.contains("size=20"));
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn host_queue_checkpoint_removes_stale_zero_byte_index_lock_before_status() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-host-queue-stale-index-lock", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        fs::write(worker.join("IMPLEMENTATION_PLAN.md"), "# plan\n").expect("failed to write plan");
        run_git_in(&worker, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&worker, ["commit", "-m", "plan"]);
        run_git_in(&worker, ["push", "origin", "trunk"]);
        fs::write(
            worker.join("IMPLEMENTATION_PLAN.md"),
            "# plan\n\n- [x] done\n",
        )
        .expect("failed to dirty plan");
        let lock = worker.join(".git").join("index.lock");
        fs::write(&lock, "").expect("failed to write stale index lock");
        set_file_mtime_epoch(&lock);

        let commit = checkpoint_parallel_host_queue_changes(&worker, "trunk", &logger)
            .expect("stale index lock should be repaired before queue sync")
            .expect("queue sync should create a commit");

        assert!(!commit.is_empty());
        assert!(!lock.exists(), "stale index lock should be removed");
        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        let log = run_git_in(&worker, ["log", "--format=%s", "-1"]);
        assert_eq!(log.trim(), "worker: parallel host queue sync");
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn host_queue_state_files_skip_missing_untracked_docs() {
        let repo = unique_temp_dir("parallel-host-queue-files");
        init_git_repo(&repo);
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), "# plan\n").expect("failed to write plan");
        fs::write(repo.join("COMPLETED.md"), "# completed\n").expect("failed to write completed");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md", "COMPLETED.md"]);
        run_git_in(&repo, ["commit", "-m", "queue docs"]);
        fs::remove_file(repo.join("COMPLETED.md")).expect("failed to remove completed");

        let files = host_queue_state_files_for_repo(&repo);
        assert!(files.contains(&"IMPLEMENTATION_PLAN.md"));
        assert!(files.contains(&"COMPLETED.md"));
        assert!(!files.contains(&"WORKLIST.md"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn prepare_lane_landing_recovery_rebases_cleanly_when_possible() {
        let (root, remote, upstream, _worker) =
            init_remote_and_clones("parallel-landing-recovery-clean", "main");
        let lane = root.join("lane-clean");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);
        run_git_in(&lane, ["remote", "rename", "origin", "canonical"]);

        let base_commit = git_output(&lane, ["rev-parse", "HEAD"]);
        fs::write(lane.join("lane.txt"), "lane change\n").expect("failed to write lane file");
        run_git_in(&lane, ["add", "lane.txt"]);
        run_git_in(&lane, ["commit", "-m", "lane change"]);

        fs::write(upstream.join("main.txt"), "main change\n").expect("failed to write main file");
        run_git_in(&upstream, ["add", "main.txt"]);
        run_git_in(&upstream, ["commit", "-m", "main change"]);
        run_git_in(&upstream, ["push", "origin", "main"]);
        let remote_head = git_output(&upstream, ["rev-parse", "HEAD"]);

        let mut assignment = ActiveLaneAssignment {
            lane_index: 1,
            attempts: 1,
            task: LoopTask {
                id: "TASK-CLEAN".to_string(),
                title: "clean recovery".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [ ] `TASK-CLEAN` clean recovery\n".to_string(),
            },
            resumed: false,
            lane_root: root.join("lane-clean-root"),
            lane_repo_root: lane.clone(),
            base_commit: base_commit.clone(),
            stdout_log_path: root.join("lane-clean.stdout.log"),
            stderr_log_path: root.join("lane-clean.stderr.log"),
            worker_pid_path: root.join("lane-clean.worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };

        let prep = prepare_lane_landing_recovery(
            &mut assignment,
            "main",
            &base_commit,
            "git cherry-pick failed",
        )
        .expect("landing recovery should prepare");
        assert_eq!(prep, LaneLandingRecoveryPrep::RebasedCleanly);
        assert_eq!(assignment.base_commit, remote_head);
        assert_eq!(run_git_in(&lane, ["status", "--short"]), "");
        assert!(!lane_repo_has_active_cherry_pick(&lane));
        let log = run_git_in(&lane, ["log", "--format=%s", "-2"]);
        assert_eq!(log, "lane change\nmain change\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn cherry_pick_lane_range_treats_empty_tree_diff_as_already_applied() {
        let (root, remote, _upstream, worker) =
            init_remote_and_clones("parallel-empty-lane-commit", "main");
        let lane = root.join("lane-empty");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);

        let base_commit = git_output(&lane, ["rev-parse", "HEAD"]);
        run_git_in(
            &lane,
            ["commit", "--allow-empty", "-m", "verification-only marker"],
        );
        let lane_head = git_output(&lane, ["rev-parse", "HEAD"]);
        run_git_in(
            &worker,
            [
                "fetch",
                lane.to_str().expect("lane path should be utf-8"),
                lane_head.as_str(),
            ],
        );

        cherry_pick_lane_range(
            &worker,
            &base_commit,
            "FETCH_HEAD",
            CherryPickFailurePolicy::Abort,
        )
        .expect("empty tree-diff lane commit should be treated as already applied");

        assert_eq!(git_output(&worker, ["rev-parse", "HEAD"]), base_commit);
        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        assert!(!lane_repo_has_active_cherry_pick(&worker));

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn prepare_lane_landing_recovery_leaves_conflict_for_worker() {
        let (root, remote, upstream, _worker) =
            init_remote_and_clones("parallel-landing-recovery-conflict", "main");
        let lane = root.join("lane-conflict");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);
        run_git_in(&lane, ["remote", "rename", "origin", "canonical"]);

        let base_commit = git_output(&lane, ["rev-parse", "HEAD"]);
        fs::write(lane.join("shared.txt"), "lane version\n").expect("failed to write lane file");
        run_git_in(&lane, ["add", "shared.txt"]);
        run_git_in(&lane, ["commit", "-m", "lane conflict"]);

        fs::write(upstream.join("shared.txt"), "main version\n")
            .expect("failed to write upstream file");
        run_git_in(&upstream, ["add", "shared.txt"]);
        run_git_in(&upstream, ["commit", "-m", "main conflict"]);
        run_git_in(&upstream, ["push", "origin", "main"]);
        let remote_head = git_output(&upstream, ["rev-parse", "HEAD"]);

        let mut assignment = ActiveLaneAssignment {
            lane_index: 2,
            attempts: 1,
            task: LoopTask {
                id: "TASK-CONFLICT".to_string(),
                title: "conflict recovery".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [ ] `TASK-CONFLICT` conflict recovery\n".to_string(),
            },
            resumed: false,
            lane_root: root.join("lane-conflict-root"),
            lane_repo_root: lane.clone(),
            base_commit: base_commit.clone(),
            stdout_log_path: root.join("lane-conflict.stdout.log"),
            stderr_log_path: root.join("lane-conflict.stderr.log"),
            worker_pid_path: root.join("lane-conflict.worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };

        let prep = prepare_lane_landing_recovery(
            &mut assignment,
            "main",
            &base_commit,
            "git cherry-pick failed",
        )
        .expect("landing recovery should prepare");
        let note = match prep {
            LaneLandingRecoveryPrep::NeedsWorkerResolution(note) => note,
            other => panic!("expected worker-resolution prep, got {other:?}"),
        };
        assert_eq!(assignment.base_commit, remote_head);
        assert!(lane_repo_has_active_cherry_pick(&lane));
        let status = run_git_in(&lane, ["status", "--short"]);
        assert!(status.contains("shared.txt"));
        assert!(lane_repo_status_summary(&lane).contains("cherry-pick recovery"));
        assert!(note.contains("landing-recovery mode"));
        assert!(note.contains("cherry-pick --continue"));

        let resumed = lane_repo_recovery_note(&lane, "main", status.trim());
        assert!(resumed.contains("in-progress landing-recovery cherry-pick"));
        assert!(resumed.contains("shared.txt"));

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn audit_parallel_completion_drift_warns_without_demoting_plan() {
        let repo = unique_temp_dir("parallel-drift-audit");
        let run_root = unique_temp_dir("parallel-drift-audit-run");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let updated = audit_parallel_completion_drift(
            &repo,
            "main",
            &fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should exist"),
            &logger,
        )
        .expect("drift audit should succeed");

        assert_eq!(updated, plan);
        let persisted =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should persist");
        assert_eq!(persisted, plan);
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).unwrap_or_default();
        assert!(
            triage.contains("TASK-001") && triage.contains("Completed Tasks With Drift"),
            "receipt drift should be report-only, not scheduler work"
        );
        let live_log = fs::read_to_string(run_root.join("live.log"))
            .expect("receipt repair should write host log");
        assert!(live_log.contains("left IMPLEMENTATION_PLAN.md unchanged"));
    }

    #[test]
    fn audit_parallel_completion_drift_logs_only_changed_triage() {
        let repo = unique_temp_dir("parallel-drift-audit-stable-log");
        let run_root = unique_temp_dir("parallel-drift-audit-stable-log-run");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let updated = audit_parallel_completion_drift(&repo, "main", plan, &logger)
            .expect("first drift audit should succeed");
        assert_eq!(updated, plan);
        let first_log =
            fs::read_to_string(run_root.join("live.log")).expect("first audit should log drift");
        assert!(first_log.contains("left IMPLEMENTATION_PLAN.md unchanged"));

        audit_parallel_completion_drift(&repo, "main", &updated, &logger)
            .expect("second drift audit should succeed");
        let second_log =
            fs::read_to_string(run_root.join("live.log")).expect("second audit should keep log");
        assert_eq!(
            second_log, first_log,
            "unchanged receipt drift should stay visible in RECEIPTS-DRIFT.md without appending another fresh host warning"
        );

        assert!(
            receipt_drift_status_summary(&repo)
                .is_some_and(|summary| summary.contains("1 completed task(s)")),
            "completed receipt drift should remain status noise, not scheduler work"
        );
    }

    #[test]
    fn audit_parallel_completion_drift_backfills_safe_legacy_receipt_footer() {
        let (_root, _remote, repo, _worker) =
            init_remote_and_clones("parallel-drift-backfill", "trunk");
        let run_root = unique_temp_dir("parallel-drift-backfill-run");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Verification: `cargo test task_001`\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        fs::write(repo.join("REVIEW.md"), "## `TASK-001`\n\nComplete.\n")
            .expect("failed to write review");
        let receipt_dir = repo.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("failed to create receipt dir");
        fs::write(
            receipt_dir.join("TASK-001.json"),
            r#"{"commands":[{"command":"cargo test task_001","exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write legacy receipt");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"]);
        run_git_in(&repo, ["commit", "-m", "completed task"]);
        run_git_in(&repo, ["push", "origin", "trunk"]);
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let updated = audit_parallel_completion_drift(&repo, "trunk", plan, &logger)
            .expect("drift audit should backfill receipt footer");

        assert_eq!(updated, plan);
        assert!(!repo.join("RECEIPTS-DRIFT.md").exists());
        let log = git_output(&repo, ["log", "-1", "--format=%B"]);
        assert!(log.contains("Auto-Verification-Receipt-Task: TASK-001"));
        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("backfill should write host log");
        assert!(live_log.contains("receipt-backfill: footerized 1 completed task receipt(s)"));
    }

    #[test]
    fn repair_parallel_canonical_before_dispatch_ignores_receipt_json_staging() {
        let repo = unique_temp_dir("parallel-ignore-receipt-json");
        let run_root = unique_temp_dir("parallel-ignore-receipt-json-run");
        init_git_repo(&repo);
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        run_git_in(&repo, ["branch", "-M", "trunk"]);
        fs::write(repo.join("README.md"), "# repo\n").expect("failed to write README");
        run_git_in(&repo, ["add", "README.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);
        let receipt_dir = repo.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("failed to create receipt dir");
        fs::write(receipt_dir.join("TASK-001.json"), "{}\n").expect("failed to write receipt");
        let before = git_output(&repo, ["rev-parse", "HEAD"]);
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        repair_parallel_canonical_before_dispatch(&repo, "trunk", &logger)
            .expect("receipt JSON staging should not force a checkpoint");

        let after = git_output(&repo, ["rev-parse", "HEAD"]);
        assert_eq!(after, before);
        let status = git_output(&repo, ["status", "--short", "--untracked-files=all"]);
        assert!(status.contains(".auto/symphony/verification-receipts/TASK-001.json"));
    }

    #[test]
    fn audit_parallel_completion_drift_reports_closeout_candidates_without_promoting_plan() {
        let repo = unique_temp_dir("parallel-closeout-audit");
        let run_root = unique_temp_dir("parallel-closeout-audit-run");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [~] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        fs::write(repo.join("REVIEW.md"), "## `TASK-001`\n\nComplete.\n")
            .expect("failed to write review");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let updated = audit_parallel_completion_drift(
            &repo,
            "main",
            &fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should exist"),
            &logger,
        )
        .expect("drift audit should succeed");

        assert!(updated.starts_with("- [x] `TASK-001`"));
        let persisted =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should persist");
        assert_eq!(persisted, updated);
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).unwrap_or_default();
        assert!(
            triage.is_empty() || triage.contains("No repo-local receipt drift detected."),
            "closeout should not leave actionable receipt drift"
        );
        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("closeout should write host log");
        assert!(live_log.contains("receipt-closeout: closed 1 partial task(s)"));
    }
}
