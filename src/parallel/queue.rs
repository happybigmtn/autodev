fn checkpoint_parallel_host_queue_changes(
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
    // Plan-integrity guard (Change #9): refuse the queue-sync commit if
    // its IMPLEMENTATION_PLAN.md diff demotes a completed task row. The
    // orchestrator catches the structured error and routes to a conflict
    // broker instead of silently losing receipts.
    if let Err(err) = assert_no_plan_demotion(repo_root, "HEAD") {
        let _ = run_git(repo_root, ["reset", "HEAD", "--", "IMPLEMENTATION_PLAN.md"]);
        let _ = run_git(repo_root, ["checkout", "--", "IMPLEMENTATION_PLAN.md"]);
        parallel_logger.warn(format!("{err:#}"));
        return Err(err);
    }
    let message = format!("{}: parallel host queue sync", repo_name(repo_root));
    run_git(repo_root, ["commit", "-m", &message])?;
    // Receipts rehash (Change #9): compute a fresh anchor over the
    // queue-state paths and amend the commit footer so the receipt is
    // commit-atomic. Older footers remain readable.
    if let Err(err) = receipts_rehash_amend(
        repo_root,
        &queue_files
            .iter()
            .map(|s| PathBuf::from(*s))
            .collect::<Vec<_>>(),
    ) {
        parallel_logger.warn(format!(
            "receipts-rehash amend skipped after host queue sync: {err:#}"
        ));
    }
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

fn try_checkpoint_parallel_host_queue_changes(
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

fn host_queue_state_files_for_repo(repo_root: &Path) -> Vec<&'static str> {
    HOST_QUEUE_STATE_FILES
        .into_iter()
        .filter(|relative| repo_path_exists_or_is_tracked(repo_root, relative))
        .collect()
}

fn repo_path_exists_or_is_tracked(repo_root: &Path, relative: &str) -> bool {
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

fn inspect_lane_repo_progress_or_shelve(
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

fn shelve_lane_after_host_failure(
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

fn inspect_loop_plan(repo_root: &Path) -> Result<LoopPlanSnapshot> {
    let plan = read_loop_plan(repo_root)?;
    Ok(parse_loop_plan(&plan))
}

#[derive(Debug, Eq, PartialEq)]
struct ReceiptDriftTriageEntry {
    task_id: String,
    title: String,
    status: LoopTaskStatus,
    reasons: Vec<String>,
}

fn audit_parallel_completion_drift(
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

fn backfill_completed_legacy_receipt_footer(
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

fn write_receipts_drift_triage(
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

fn render_receipts_drift_triage(
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

fn render_receipts_drift_entry(entry: &ReceiptDriftTriageEntry) -> String {
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

async fn refresh_parallel_plan(
    repo_root: &Path,
    target_branch: &str,
    linear_tracker: &mut Option<LinearTracker>,
    linear_auto_sync_state: &mut LinearAutoSyncState,
    parallel_logger: &ParallelEventLogger,
) -> Result<LoopPlanSnapshot> {
    let mut plan_text = read_loop_plan(repo_root)?;
    plan_text =
        audit_parallel_completion_drift(repo_root, target_branch, &plan_text, parallel_logger)?;
    if let Some(tracker) = linear_tracker.as_mut() {
        if let Err(err) = tracker.refresh_if_plan_changed(&plan_text).await {
            if !maybe_disable_linear_auto_sync_for_run(
                &err,
                linear_auto_sync_state,
                parallel_logger,
                "refreshing the Linear task cache from the updated plan",
            ) {
                parallel_logger.warn(format!(
                    "warning: failed to refresh Linear task cache from updated plan: {err:#}"
                ));
            }
        } else if !linear_auto_sync_state.is_disabled()
            && tracker.should_attempt_auto_sync(&plan_text)
        {
            let drift = tracker.coverage_drift(&plan_text);
            if !drift.is_empty() {
                let mut reasons = Vec::new();
                if !drift.missing_task_ids.is_empty() {
                    reasons.push(format!("missing {}", drift.missing_task_ids.join(", ")));
                }
                if !drift.stale_task_ids.is_empty() {
                    reasons.push(format!("stale {}", drift.stale_task_ids.join(", ")));
                }
                if !drift.terminal_task_ids.is_empty() {
                    reasons.push(format!("terminal {}", drift.terminal_task_ids.join(", ")));
                }
                if !drift.completed_active_task_ids.is_empty() {
                    reasons.push(format!(
                        "completed-active {}",
                        drift.completed_active_task_ids.join(", ")
                    ));
                }
                parallel_logger.info(format!(
                    "linear drift: {}. running `auto symphony sync --no-ai-planner` before dispatch",
                    reasons.join(" | ")
                ));
                tracker.mark_auto_sync_attempt(&plan_text);
                if let Err(err) = run_sync(SymphonySyncArgs {
                    repo_root: Some(repo_root.to_path_buf()),
                    project_slug: None,
                    todo_state: "Todo".to_string(),
                    planner_model: "gpt-5.5".to_string(),
                    planner_reasoning_effort: "high".to_string(),
                    codex_bin: PathBuf::from("codex"),
                    no_ai_planner: true,
                })
                .await
                {
                    if !maybe_disable_linear_auto_sync_for_run(
                        &err,
                        linear_auto_sync_state,
                        parallel_logger,
                        "automatic `auto symphony sync --no-ai-planner`",
                    ) {
                        parallel_logger.warn(format!(
                            "warning: automatic `auto symphony sync --no-ai-planner` failed; continuing without refreshed Linear coverage: {err:#}"
                        ));
                    }
                } else {
                    plan_text = read_loop_plan(repo_root)?;
                    if let Err(err) = tracker.refresh_after_sync(&plan_text).await {
                        if !maybe_disable_linear_auto_sync_for_run(
                            &err,
                            linear_auto_sync_state,
                            parallel_logger,
                            "refreshing the Linear cache after automatic sync",
                        ) {
                            parallel_logger.warn(format!(
                                "warning: failed refreshing Linear cache after automatic sync: {err:#}"
                            ));
                        }
                    } else {
                        parallel_logger.info(
                            "linear:      automatic `auto symphony sync --no-ai-planner` completed",
                        );
                    }
                }
            }
        }
    }
    Ok(parse_loop_plan(&plan_text))
}

async fn refresh_parallel_plan_or_last_good(
    repo_root: &Path,
    target_branch: &str,
    linear_tracker: &mut Option<LinearTracker>,
    linear_auto_sync_state: &mut LinearAutoSyncState,
    last_good_plan: &LoopPlanSnapshot,
    parallel_logger: &ParallelEventLogger,
) -> Result<LoopPlanSnapshot> {
    match refresh_parallel_plan(
        repo_root,
        target_branch,
        linear_tracker,
        linear_auto_sync_state,
        parallel_logger,
    )
    .await
    {
        Ok(plan) => Ok(plan),
        Err(err) => {
            parallel_logger.warn(format!(
                "warning: failed to refresh IMPLEMENTATION_PLAN.md; failing closed instead of reusing the last good queue snapshot: {err:#}"
            ));
            let _ = last_good_plan;
            Err(err.context("failed to refresh current queue snapshot; auto parallel fails closed outside explicit recovery paths"))
        }
    }
}

fn is_linear_usage_limit_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("usage_limit_exceeded")
        || message.contains("usage limit exceeded")
        || message.contains("exceeded the free issue limit")
        || message.contains("\"activeissuecount\"")
}

fn maybe_disable_linear_auto_sync_for_run(
    err: &anyhow::Error,
    linear_auto_sync_state: &mut LinearAutoSyncState,
    parallel_logger: &ParallelEventLogger,
    context: &str,
) -> bool {
    if !is_linear_usage_limit_error(err) {
        return false;
    }

    let warning = format!(
        "warning: {context} hit Linear workspace usage limits; disabling further automatic Linear sync for this run and continuing from IMPLEMENTATION_PLAN.md only: {err:#}"
    );
    if linear_auto_sync_state.disable_for_run(warning.clone()) {
        parallel_logger.warn(warning);
    }
    true
}

