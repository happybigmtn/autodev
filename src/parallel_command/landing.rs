use super::*;

pub(crate) const LANDING_REBASE_RETRY_LIMIT: usize = 5;
pub(crate) const LANDING_PUSH_RETRY_LIMIT: usize = 5;

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
    enforce_review_input_quarantine_before_dispatch(repo_root)?;
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
    enforce_review_input_quarantine_before_dispatch(repo_root)?;
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
    refuse_unsealed_task_completion_checkpoint(repo_root)?;
    let mut add_args = vec!["add".to_string(), "--all".to_string(), "--".to_string()];
    add_args.extend(dirty_paths.iter().cloned());
    run_git(repo_root, add_args.iter().map(|arg| arg.as_str()))?;
    let allowed_paths = dirty_paths.iter().map(String::as_str).collect::<Vec<_>>();
    refuse_worktree_paths_outside(repo_root, &allowed_paths, "parallel dispatch checkpoint")?;
    let staged = git_stdout(repo_root, ["diff", "--cached", "--name-only"])?;
    if staged.trim().is_empty() {
        return Ok(None);
    }
    let message = format!("{}: {message_suffix}", repo_name(repo_root));
    let commit = commit_staged_checkpoint_cas(repo_root, target_branch, &message)?;
    if let Err(err) = push_branch_with_remote_sync(repo_root, target_branch) {
        bail!(
            "created checkpoint commit {} but failed to sync/push: {err}",
            commit
        );
    }
    Ok(Some(commit))
}

/// Recover a process crash that left a candidate `[x]` in the worktree or
/// index before the receipt-bearing closeout commit reached `HEAD`.
///
/// Generic checkpoints refuse these transitions. At the next `auto parallel`
/// startup, deterministically demote them to `[~]`, stage the safe state, and
/// retain a gate hold so the normal lane pipeline re-runs every final gate.
pub(crate) fn recover_unsealed_task_completion_transitions(
    repo_root: &Path,
) -> Result<Vec<String>> {
    let worktree_plan = read_loop_plan(repo_root)?;
    let unsealed = unsealed_task_completion_ids(repo_root)?;
    if unsealed.is_empty() {
        return Ok(Vec::new());
    }

    let mut updated_plan = worktree_plan;
    for task_id in &unsealed {
        let matching = parse_loop_plan(&updated_plan)
            .tasks
            .into_iter()
            .filter(|task| &task.id == task_id)
            .collect::<Vec<_>>();
        let [task] = matching.as_slice() else {
            bail!(
                "cannot recover unsealed completion for `{task_id}`: expected exactly one worktree plan row, found {}",
                matching.len()
            );
        };
        updated_plan = update_reconciled_task_completion_in_plan_text(
            &updated_plan,
            task,
            LoopTaskStatus::Partial,
        );
        record_gate_hold(
            repo_root,
            task_id,
            "recovered unsealed Done transition after interrupted host closeout",
        )?;
    }
    atomic_write(
        &repo_root.join("IMPLEMENTATION_PLAN.md"),
        updated_plan.as_bytes(),
    )?;
    run_git(repo_root, ["add", "IMPLEMENTATION_PLAN.md"])?;
    Ok(unsealed)
}

pub(crate) fn checkpoint_parallel_host_queue_changes(
    repo_root: &Path,
    target_branch: &str,
    parallel_logger: &ParallelEventLogger,
) -> Result<Option<String>> {
    enforce_review_input_quarantine_before_dispatch(repo_root)?;
    repair_stale_git_index_lock(repo_root, parallel_logger, "before host queue sync")?;
    let derived_files = run_after_plan_update_hook(repo_root)?;
    let mut queue_files = host_queue_state_files_for_repo(repo_root)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    queue_files.extend(derived_files);
    queue_files.sort();
    queue_files.dedup();
    if queue_files.is_empty() {
        return Ok(None);
    }

    let mut status_args = vec!["status", "--short", "--"];
    status_args.extend(queue_files.iter().map(String::as_str));
    let status = git_stdout(repo_root, status_args)?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    refuse_unsealed_task_completion_checkpoint(repo_root)?;
    let mut add_args = vec!["add", "--all", "--"];
    add_args.extend(queue_files.iter().map(String::as_str));
    run_git(repo_root, add_args)?;
    let allowed_paths = queue_files.iter().map(String::as_str).collect::<Vec<_>>();
    refuse_worktree_paths_outside(repo_root, &allowed_paths, "parallel host queue checkpoint")?;
    let message = format!("{}: parallel host queue sync", repo_name(repo_root));
    let commit = commit_staged_checkpoint_cas(repo_root, target_branch, &message)?;
    let short_commit = git_stdout(repo_root, ["rev-parse", "--short", &commit])?;
    let short_commit = short_commit.trim();
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        parallel_logger.info(format!(
            "remote sync: rebased onto origin/{} after host queue sync",
            target_branch
        ));
    }
    parallel_logger.info(format!(
        "host sync:  committed queue-state changes at {short_commit}"
    ));
    Ok(Some(short_commit.to_string()))
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

/// Local drift re-verify is on unless `AUTO_PARALLEL_DRIFT_REVERIFY=0`.
/// When completion evidence for a `[x]` row goes stale for a locally
/// repairable reason (receipt/plan/artifact freshness), re-running the row's
/// declared verification commands can refresh its proof while the accepted
/// queue-truth policy preserves the completed row.
fn drift_local_reverify_enabled() -> bool {
    std::env::var("AUTO_PARALLEL_DRIFT_REVERIFY")
        .map(|value| value.trim() != "0")
        .unwrap_or(true)
}

/// Wall-clock budget for the whole local drift re-verify sweep within one audit
/// invocation (`AUTO_PARALLEL_DRIFT_REVERIFY_BUDGET_SECS`, default 900). Local
/// re-verification re-runs a task's real test commands, so a large stale set
/// could otherwise turn one audit into a serial test marathon that starves the
/// run. Once the budget is spent, remaining completed rows stay `[x]` and their
/// proof gaps remain explicit in `RECEIPTS-DRIFT.md`; fully-evidenced partials
/// remain manual closeout candidates. `0` disables local re-execution.
fn drift_reverify_budget() -> Duration {
    let secs = std::env::var("AUTO_PARALLEL_DRIFT_REVERIFY_BUDGET_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(900);
    Duration::from_secs(secs)
}

/// Forced full re-verify override. When `AUTO_PARALLEL_FORCE_FULL_REVERIFY` is
/// set to a non-empty, non-`0` value, the per-task owned-inputs gate is bypassed
/// and every `[x]` row is re-verified regardless of its fingerprint (a
/// deliberate full audit).
fn force_full_reverify_enabled() -> bool {
    std::env::var("AUTO_PARALLEL_FORCE_FULL_REVERIFY")
        .map(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty() && trimmed != "0"
        })
        .unwrap_or(false)
}

/// A task opts an expensive verification out of periodic drift sweeps with a
/// discoverable `[sweep-excluded]` marker anywhere in its plan row (conventionally
/// in the `Verification:` block). Once `[x]` with a valid receipt it is never
/// re-run by a sweep unless its own owned inputs change or a forced full
/// re-verify is requested.
fn task_is_sweep_excluded(task_markdown: &str) -> bool {
    task_markdown
        .to_ascii_lowercase()
        .contains("[sweep-excluded]")
}

/// Per-task decision from the owned-inputs gate for a completed `[x]` row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnedInputsDecision {
    /// Owned inputs definitively changed (or an error must be treated as change)
    /// — force re-verification even if the receipt still looks fresh.
    ForceReverify,
    /// Fingerprints do not require a re-run, but the receipt must still pass
    /// normal identity, content, provenance, artifact, and review inspection.
    FallThrough,
}

/// Decide how a completed `[x]` task should be treated by the drift sweep, using
/// the stamped-vs-recomputed owned-inputs fingerprint.
///
/// - `forced`: bypass everything and re-verify (`ForceReverify`).
/// - `sweep_excluded`: a match, legacy receipt, or hash error falls through to
///   full evidence inspection; only a definitive mismatch forces a re-run.
/// - Otherwise: match ⇒ inspect without forcing a re-run; mismatch or
///   hash-error-with-a-stamp ⇒ re-verify; no stamp (legacy) ⇒ inspect.
fn decide_owned_inputs(
    forced: bool,
    sweep_excluded: bool,
    _has_receipt_footer: bool,
    stored_fp: Option<&str>,
    current_fp: Option<&str>,
) -> OwnedInputsDecision {
    if forced {
        return OwnedInputsDecision::ForceReverify;
    }
    match (stored_fp, current_fp) {
        (Some(stored), Some(current)) if stored == current => OwnedInputsDecision::FallThrough,
        (Some(_), Some(_)) => OwnedInputsDecision::ForceReverify, // own inputs changed
        (Some(_), None) => {
            // Stamped, but we could not recompute (git/hash error).
            if sweep_excluded {
                OwnedInputsDecision::FallThrough
            } else {
                OwnedInputsDecision::ForceReverify // conservative: treat as changed
            }
        }
        (None, _) => OwnedInputsDecision::FallThrough,
    }
}

/// Resolve `(has_receipt_footer, stored_fingerprint)` for a task from the most
/// recent committed verification-receipt footer.
fn stored_owned_inputs(
    footers: &[VerificationReceiptFooter],
    task_id: &str,
) -> (bool, Option<String>) {
    match footers.iter().find(|footer| footer.task_id == task_id) {
        Some(footer) => (true, footer_task_owned_inputs(footer)),
        None => (false, None),
    }
}

pub(crate) async fn audit_parallel_completion_drift(
    repo_root: &Path,
    _target_branch: &str,
    plan_text: &str,
    parallel_logger: &ParallelEventLogger,
) -> Result<(String, bool)> {
    let snapshot = parse_loop_plan(plan_text);
    // Parse the plan once for owned-inputs fingerprinting (task contracts, Owns
    // paths, dependencies, declared artifacts) and read committed receipt
    // footers once so the per-task gate below does not rescan git history per row.
    let all_plan_tasks = parse_shared_tasks(plan_text);
    let receipt_footers = git_verification_receipt_footers(repo_root);
    let forced_full_reverify = force_full_reverify_enabled();
    let mut completed_drift = Vec::new();
    let mut locally_refreshed_done = Vec::new();
    let mut locally_refreshed_partial = Vec::new();
    let mut manual_closeout_candidates = Vec::new();
    let reverify_budget = drift_reverify_budget();
    let mut reverify_spent = Duration::ZERO;
    let mut reverify_deferred: Vec<String> = Vec::new();

    for task in snapshot
        .tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Done)
    {
        // Per-task owned-inputs gate: narrow re-verification from the global
        // tree signal to this task's own inputs. When the stamped fingerprint
        // still matches, inspect the receipt without forcing a re-run; when it
        // changed, force re-verification even if the footer otherwise looks
        // fresh. Fingerprints are an optimization hint, never authority.
        let sweep_excluded = task_is_sweep_excluded(&task.markdown);
        let (has_receipt_footer, stored_fp) = stored_owned_inputs(&receipt_footers, &task.id);
        let current_fp =
            compute_task_owned_inputs_fingerprint(repo_root, &task.id, &all_plan_tasks);
        let decision = decide_owned_inputs(
            forced_full_reverify,
            sweep_excluded,
            has_receipt_footer,
            stored_fp.as_deref(),
            current_fp.as_deref(),
        );
        let reverify_active = drift_local_reverify_enabled() && !reverify_budget.is_zero();
        let must_reverify = decision == OwnedInputsDecision::ForceReverify;
        let unchanged_owned_inputs = (!must_reverify)
            .then_some((stored_fp.as_deref(), current_fp.as_deref()))
            .and_then(|(stored, current)| match (stored, current) {
                (Some(stored), Some(current)) if stored == current => Some(current),
                _ => None,
            });
        let mut evidence = inspect_task_completion_evidence_with_owned_inputs(
            repo_root,
            &task.id,
            &task.markdown,
            unchanged_owned_inputs,
        );
        if !must_reverify && evidence.is_fully_evidenced() {
            continue;
        }
        if reverify_active
            && (must_reverify
                || assess_task_completion_gap(&task.markdown, &evidence).kind
                    == CompletionGapKind::LocalRepairable)
        {
            if reverify_spent >= reverify_budget {
                // Budget spent: preserve queue truth and report the stale proof
                // in triage for a later sweep.
                reverify_deferred.push(task.id.clone());
            } else {
                parallel_logger.info(format!(
                    "drift-reverify: `{}` completion evidence went stale ({}); re-running its verification locally without changing queue status",
                    task.id,
                    evidence.missing_reasons().join("; ")
                ));
                let started = Instant::now();
                let outcome = run_guarded_lane_verify_gate(
                    repo_root,
                    &task.id,
                    &task.markdown,
                    "completion-drift reverify",
                    true,
                )
                .await?;
                reverify_spent += started.elapsed();
                if let LaneVerifyOutcome::AllPassed = outcome {
                    let mut refreshed =
                        inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
                    if !refreshed.has_review_handoff {
                        ensure_host_review_handoff(repo_root, &task.id, &[], &refreshed)?;
                        refreshed =
                            inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
                    }
                    if refreshed.is_fully_evidenced() {
                        locally_refreshed_done.push(task.id.clone());
                        continue;
                    }
                    evidence = refreshed;
                }
            }
        }
        let mut reasons = evidence.missing_reasons();
        if reasons.is_empty() && must_reverify {
            // A changed task-owned fingerprint is itself actionable drift even
            // when the older receipt remains content-valid.
            reasons.push(
                "task-owned inputs changed since the receipt was stamped and host re-verification did not refresh the proof"
                    .to_string(),
            );
        }
        let entry = ReceiptDriftTriageEntry {
            task_id: task.id.clone(),
            title: task.title.clone(),
            status: LoopTaskStatus::Done,
            reasons,
        };
        completed_drift.push(entry);
    }

    for task in snapshot
        .tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Partial)
    {
        let mut evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
        // Refresh local receipt evidence when host re-execution of the declared
        // verification passes, while leaving the row `[~]` until the workspace
        // and independent-review gates run through normal dispatch. This covers
        // BOTH already-fully-evidenced partials AND
        // partials whose only gap is locally repairable (a stale or missing
        // receipt is exactly why a previously-demoted row is here): the gate
        // run re-stamps a fresh receipt, and we synthesize any missing REVIEW
        // handoff, then re-check. A row with a genuinely failing test, or with
        // external/live steps (`Skipped`), stays `[~]` — which correctly
        // protects intentionally-partial rows like the fleet gate.
        let gap = assess_task_completion_gap(&task.markdown, &evidence);
        let repairable =
            evidence.is_fully_evidenced() || gap.kind == CompletionGapKind::LocalRepairable;
        if drift_local_reverify_enabled() && !reverify_budget.is_zero() && repairable {
            if reverify_spent >= reverify_budget {
                // Budget spent: leave the row [~] (honest — no false promotion).
                reverify_deferred.push(task.id.clone());
            } else {
                let started = Instant::now();
                let outcome = run_guarded_lane_verify_gate(
                    repo_root,
                    &task.id,
                    &task.markdown,
                    "partial completion-drift reverify",
                    true,
                )
                .await?;
                reverify_spent += started.elapsed();
                if let LaneVerifyOutcome::AllPassed = outcome {
                    let mut refreshed =
                        inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
                    if !refreshed.has_review_handoff {
                        ensure_host_review_handoff(repo_root, &task.id, &[], &refreshed)?;
                        refreshed =
                            inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
                    }
                    if refreshed.is_fully_evidenced() {
                        locally_refreshed_partial.push(task.id.clone());
                    }
                    evidence = refreshed;
                }
            }
        }
        // Rows that look locally complete are reported as manual closeout
        // candidates until normal dispatch runs the remaining definition-of-done
        // gates; genuinely-incomplete or external-gated partials stay silent.
        if !evidence.is_fully_evidenced() {
            continue;
        }
        manual_closeout_candidates.push(ReceiptDriftTriageEntry {
            task_id: task.id.clone(),
            title: task.title.clone(),
            status: LoopTaskStatus::Partial,
            reasons: vec![
                "repo-local evidence appears complete, but definition-of-done gates must re-run before [x]".to_string(),
            ],
        });
    }

    let triage_changed = if !completed_drift.is_empty()
        || !manual_closeout_candidates.is_empty()
        || repo_root.join("RECEIPTS-DRIFT.md").exists()
    {
        write_receipts_drift_triage(
            repo_root,
            completed_drift.as_slice(),
            manual_closeout_candidates.as_slice(),
        )?
    } else {
        false
    };
    // Only report the sweep duration when it actually consumed wall time worth
    // noting; an instant no-op sweep would otherwise append a fresh host-log
    // line on every idempotent re-audit.
    if reverify_spent >= Duration::from_secs(1) {
        parallel_logger.info(format!(
            "drift-reverify: local sweep spent {}s of {}s budget",
            reverify_spent.as_secs(),
            reverify_budget.as_secs()
        ));
    }
    if !reverify_deferred.is_empty() {
        parallel_logger.warn(format!(
            "drift-reverify: budget exhausted after {}s; deferred {} task(s) to next audit ({})",
            reverify_spent.as_secs(),
            reverify_deferred.len(),
            reverify_deferred.join(", ")
        ));
    }
    if !locally_refreshed_done.is_empty() {
        parallel_logger.info(format!(
            "drift-reverify: refreshed receipt evidence for {} completed task(s) while preserving [x] queue status ({})",
            locally_refreshed_done.len(),
            locally_refreshed_done.join(", ")
        ));
    }
    if !locally_refreshed_partial.is_empty() {
        parallel_logger.info(format!(
            "drift-reverify: refreshed receipt evidence for {} partial task(s), but left [~] pending workspace and independent-review gates ({})",
            locally_refreshed_partial.len(),
            locally_refreshed_partial.join(", ")
        ));
    }
    if !manual_closeout_candidates.is_empty() {
        parallel_logger.info(format!(
            "receipt-closeout: found {} partial task(s) with repo-local evidence, but left [~] pending definition-of-done gates ({})",
            manual_closeout_candidates.len(),
            manual_closeout_candidates
                .iter()
                .map(|entry| entry.task_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if triage_changed && !completed_drift.is_empty() {
        parallel_logger.warn(format!(
            "warning: repo-local completion evidence drifted for {} completed task(s); wrote RECEIPTS-DRIFT.md without changing IMPLEMENTATION_PLAN.md ({})",
            completed_drift.len(),
            completed_drift
                .iter()
                .map(|entry| entry.task_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // exhaustive = the sweep finished without deferring any row for budget.
    // Only an exhaustive sweep may be cached as "nothing left to check".
    Ok((plan_text.to_string(), reverify_deferred.is_empty()))
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
            rendered.push_str(&format!(
                "  - Reason: {}\n",
                stable_receipt_drift_reason(reason)
            ));
        }
    }
    rendered
}

/// Remove only the volatile *current* commit identity from generated triage.
///
/// A receipt mismatch remains actionable because the recorded proof commit is
/// retained. Embedding the current `HEAD`, however, makes committing the report
/// change the report again on the next audit, creating an infinite queue-sync
/// loop. Other diagnostics remain byte-for-byte unchanged.
fn stable_receipt_drift_reason(reason: &str) -> String {
    const MARKER: &str = " is not current HEAD `";
    let Some(marker_start) = reason.find(MARKER) else {
        return reason.to_string();
    };
    let hash_start = marker_start + MARKER.len();
    let Some(hash_end_offset) = reason[hash_start..].find('`') else {
        return reason.to_string();
    };
    let hash_end = hash_start + hash_end_offset;
    let candidate = &reason[hash_start..hash_end];
    if !(7..=64).contains(&candidate.len())
        || !candidate.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return reason.to_string();
    }

    format!(
        "{} is not current HEAD{}",
        &reason[..marker_start],
        &reason[hash_end + 1..]
    )
}

pub(crate) async fn land_parallel_lane_result(
    repo_root: &Path,
    target_branch: &str,
    assignment: &mut ActiveLaneAssignment,
    review_config: &LaneReviewConfig,
) -> Result<LaneLandingOutcome> {
    let mut auto_repaired = false;
    let mut rebase_retries = 0usize;
    let (final_lane_head, final_range_base, canonical_review_range) = loop {
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
        let mut canonical_review_range = None;
        if !git_ref_is_ancestor(repo_root, "FETCH_HEAD", "HEAD")? {
            let pre_landing_head = git_stdout(repo_root, ["rev-parse", "--verify", "HEAD"])?;
            let pre_landing_head = pre_landing_head.trim().to_string();
            if let Err(err) = cherry_pick_lane_range(
                repo_root,
                &range_base,
                "FETCH_HEAD",
                CherryPickFailurePolicy::Abort,
            ) {
                if landing_error_suggests_dirty_canonical_worktree(&err)
                    && try_auto_checkpoint_canonical_for_landing(
                        repo_root,
                        target_branch,
                        assignment,
                        "before retrying lane landing against local canonical changes",
                    )?
                {
                    continue;
                }
                if rebase_retries >= LANDING_REBASE_RETRY_LIMIT {
                    return Ok(LaneLandingOutcome::DivergenceExhausted {
                        detail: format!(
                            "landing-divergence: lane-{} `{}` still could not land after {} fresh canonical rebase retries: {err:#}",
                            assignment.lane_index,
                            assignment.task.id,
                            LANDING_REBASE_RETRY_LIMIT,
                        ),
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
                        rebase_retries += 1;
                        auto_repaired = true;
                        let event = format!(
                            "landing-rebase-retry: rebased committed lane work onto fresh canonical HEAD {} and retrying landing ({}/{})",
                            assignment.base_commit,
                            rebase_retries,
                            LANDING_REBASE_RETRY_LIMIT
                        );
                        eprintln!(
                            "lane-{} `{}`: {event}",
                            assignment.lane_index, assignment.task.id
                        );
                        append_lane_host_event(
                            &assignment.stdout_log_path,
                            assignment.lane_index,
                            &assignment.task.id,
                            &event,
                        );
                        continue;
                    }
                    LaneLandingRecoveryPrep::NeedsWorkerResolution {
                        recovery_note,
                        conflict_paths,
                    } => {
                        return Ok(LaneLandingOutcome::NeedsRecovery {
                            recovery_note,
                            conflict_paths,
                        });
                    }
                }
            } else {
                let post_landing_head = git_stdout(repo_root, ["rev-parse", "--verify", "HEAD"])?;
                canonical_review_range = Some(LaneReviewRange {
                    base: pre_landing_head,
                    head: post_landing_head.trim().to_string(),
                });
            }
        }
        break (lane_head, range_base, canonical_review_range);
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
    if let Err(err) = propagate_lane_receipts(
        &assignment.lane_repo_root,
        repo_root,
        &assignment.task.id,
        &assignment.task.markdown,
    ) {
        eprintln!(
            "warning: failed propagating lane-{} receipts for `{}`: {err:#}",
            assignment.lane_index, assignment.task.id
        );
    }
    let mut completion_status =
        reconcile_parallel_landed_task(repo_root, assignment, &changed_files)?;
    if completion_status == LoopTaskStatus::Done {
        assignment.task.status = LoopTaskStatus::Done;
    } else if completion_status == LoopTaskStatus::Partial {
        assignment.task.status = LoopTaskStatus::Partial;
    }
    // Inline receipt-gap repair (2026-07-10): if the ONLY thing holding this
    // task at `[~]` is a locally-repairable verification gap — a MISSING
    // receipt (worker skipped the wrapper) OR a STALE receipt (a concurrent
    // lane changed a declared artifact so its pinned hash drifted) — run the
    // host verify gate NOW. It re-runs the full declared set at canonical HEAD
    // (the same `[x]` bar, not a weakened subset) and rewrites a fresh receipt
    // on pass; a re-reconcile then promotes to `[x]` in THIS landing.
    //
    // Measured on the live runners: the dominant rework mode was NOT missing
    // receipts (0) but receipt STALENESS from cross-lane contention on shared
    // monolith files (e.g. crates/rsociety-tui/src/lib.rs appears in 47 tasks'
    // Owns) — 48 hash-mismatch demotions, 19 of which queued a fresh model lane
    // and several looping the same task 2-3x. Keying on the LocalRepairable gap
    // classification (the same signal the drift audit uses to decide
    // re-verification) moves that re-verification inline to landing time, so a
    // stale-but-still-passing task closes as `[x]` immediately instead of
    // demoting and burning a follow-up model lane.
    if completion_status == LoopTaskStatus::Partial && verify_gate_enabled() {
        let evidence = inspect_task_completion_evidence(
            repo_root,
            &assignment.task.id,
            &assignment.task.markdown,
        );
        let repairable_verification_gap = evidence.has_review_handoff
            && repo_root.join("scripts/run-task-verification.sh").is_file()
            && evidence.missing_completion_artifacts.is_empty()
            && evidence.unresolved_audit_findings.is_empty()
            && assess_task_completion_gap(&assignment.task.markdown, &evidence).kind
                == CompletionGapKind::LocalRepairable;
        if repairable_verification_gap {
            let outcome = run_guarded_lane_verify_gate(
                repo_root,
                &assignment.task.id,
                &assignment.task.markdown,
                "inline landing receipt repair",
                true,
            )
            .await?;
            if outcome == LaneVerifyOutcome::AllPassed {
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    "host-reexec-verify(inline): receipt gap repaired at landing; no follow-up lane",
                );
                completion_status =
                    reconcile_parallel_landed_task(repo_root, assignment, &changed_files)?;
                assignment.task.status = completion_status;
            }
        }
    }
    // Definition of done: a task may stay `[x]` only if each finalization gate
    // produces a positive pass on the current integrated tree. Once any gate
    // holds it at `[~]`, later gates are skipped for this landing.
    completion_status = apply_definition_of_done_gates(
        repo_root,
        target_branch,
        assignment,
        &changed_files,
        canonical_review_range.as_ref(),
        completion_status,
        review_config,
    )
    .await?;
    if completion_status == LoopTaskStatus::Done {
        record_gate_hold(
            repo_root,
            &assignment.task.id,
            "all gates passed; durable closeout commit is still pending",
        )?;
    }
    let staged_queue_updates = repo_has_staged_queue_updates(repo_root)?;
    if staged_queue_updates || completion_status == LoopTaskStatus::Done {
        let (message, allow_empty) = if staged_queue_updates {
            (
                format!(
                    "{}: {} queue sync",
                    repo_name(repo_root),
                    assignment.task.id
                ),
                false,
            )
        } else {
            (
                format!(
                    "{}: {} receipt footer backfill",
                    repo_name(repo_root),
                    assignment.task.id
                ),
                true,
            )
        };
        let closeout = commit_task_closeout(
            repo_root,
            &assignment.task.id,
            completion_status,
            &message,
            allow_empty,
        );
        if let Err(err) = closeout {
            if completion_status != LoopTaskStatus::Done {
                return Err(err);
            }
            let reason = format!("durable lane closeout failed: {err:#}");
            record_gate_hold(repo_root, &assignment.task.id, &reason)?;
            if let Err(review_err) =
                append_lane_verify_failure(repo_root, &assignment.task.id, &reason)
            {
                eprintln!(
                    "warning: failed recording closeout failure for `{}` in REVIEW.md: {review_err:#}",
                    assignment.task.id
                );
            } else {
                run_git(repo_root, ["add", "REVIEW.md"])?;
            }
            persist_failed_gate_demotion(
                repo_root,
                assignment,
                LoopTaskStatus::Done,
                "durable lane closeout failure",
            )
            .with_context(|| {
                format!(
                    "failed rolling back `{}` to Partial after closeout error: {err:#}",
                    assignment.task.id
                )
            })?;
            completion_status = LoopTaskStatus::Partial;
            assignment.task.status = LoopTaskStatus::Partial;
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("closeout-failed: rolled task back to [~] before continuing: {err:#}"),
            );
            if repo_has_staged_queue_updates(repo_root)? {
                let partial_message = format!(
                    "{}: {} queue sync",
                    repo_name(repo_root),
                    assignment.task.id
                );
                commit_task_closeout(
                    repo_root,
                    &assignment.task.id,
                    LoopTaskStatus::Partial,
                    &partial_message,
                    false,
                )?;
            }
            auto_repaired = true;
        } else if completion_status == LoopTaskStatus::Done {
            clear_gate_hold(repo_root, &assignment.task.id)?;
        }
    }
    match push_parallel_landing_with_divergence_retries(repo_root, target_branch, assignment) {
        Ok(true) => println!("remote sync: rebased onto origin/{}", target_branch),
        Ok(false) => {}
        Err(err) if landing_error_suggests_retryable_divergence(&err) => {
            return Ok(LaneLandingOutcome::DivergenceExhausted {
                detail: format!(
                    "landing-divergence: lane-{} `{}` landed locally but remote synchronization still diverged after {} fresh fetch/rebase retries: {err:#}",
                    assignment.lane_index,
                    assignment.task.id,
                    LANDING_PUSH_RETRY_LIMIT,
                ),
            });
        }
        Err(err) => return Err(err),
    }
    Ok(LaneLandingOutcome::Landed {
        auto_repaired,
        completion_status,
    })
}

/// Orchestrator-side glue for the independent diff-review gate.
///
/// Runs the bounded review (see [`run_lane_review_gate`]) and maps
/// its outcome onto the lane's landing status WITHOUT touching the committed
/// work that already landed:
///
/// - Gate disabled (`AUTO_PARALLEL_REVIEW=0`) -> hold `[~]`.
/// - `Clean` -> return `incoming_status` unchanged; the lane lands `[x]`.
/// - `FindingsKeepPartial` -> append the structured findings to `REVIEW.md`
///   (staged for the closeout commit), demote a `Done` task to `Partial` in the
///   plan, stamp the lane closeout log, and return `Partial`. An already-Partial
///   task stays Partial; findings are still recorded.
/// - `SkippedFailOpen` -> record the reason, stamp `review_skipped`, and keep
///   the task `[~]`.
///
/// Write/git errors are logged, but the landing status stays conservative so a
/// skipped review can never become `[x]`.
#[cfg(test)]
async fn apply_lane_review_gate(
    repo_root: &Path,
    target_branch: &str,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
    review_config: &LaneReviewConfig,
) -> Result<LoopTaskStatus> {
    apply_lane_review_gate_in_transaction(
        repo_root,
        target_branch,
        assignment,
        changed_files,
        incoming_status,
        review_config,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn apply_lane_review_gate_in_transaction(
    repo_root: &Path,
    target_branch: &str,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
    review_config: &LaneReviewConfig,
    review_range: Option<&LaneReviewRange>,
    transaction: Option<&ArmedCanonicalGateTransaction>,
) -> Result<LoopTaskStatus> {
    if !review_gate_enabled() {
        return apply_lane_review_outcome(
            repo_root,
            assignment,
            incoming_status,
            LaneReviewOutcome::SkippedFailOpen {
                reason: "AUTO_PARALLEL_REVIEW=0 disabled a mandatory definition-of-done gate"
                    .to_string(),
            },
        );
    }
    let outcome = run_lane_review_gate_for_range(
        repo_root,
        target_branch,
        assignment,
        changed_files,
        review_range,
        review_config,
    )
    .await;
    if let Some(transaction) = transaction {
        revalidate_canonical_gate_transaction(
            repo_root,
            transaction,
            "independent review subprocess",
        )?;
    }
    if let LaneReviewOutcome::InputMutationFatal { reason } = &outcome {
        bail!(
            "independent reviewer mutated canonical landing inputs for `{}`; refusing closeout or remote push: {reason}",
            assignment.task.id
        );
    }
    apply_lane_review_outcome(repo_root, assignment, incoming_status, outcome)
}

async fn apply_definition_of_done_gates(
    repo_root: &Path,
    target_branch: &str,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    review_range: Option<&LaneReviewRange>,
    mut status: LoopTaskStatus,
    review_config: &LaneReviewConfig,
) -> Result<LoopTaskStatus> {
    if status != LoopTaskStatus::Done {
        return Ok(status);
    }
    let transaction =
        arm_canonical_gate_transaction(repo_root, &assignment.task.id, "definition-of-done")?;
    let gate_result = async {
        status = apply_lane_verify_gate_in_transaction(
            repo_root,
            assignment,
            status,
            Some(&transaction),
        )
        .await?;
        if status == LoopTaskStatus::Done {
            status = apply_workspace_test_gate_in_transaction(
                repo_root,
                assignment,
                changed_files,
                status,
                Some(&transaction),
            )
            .await?;
        }
        if status == LoopTaskStatus::Done {
            status = apply_lane_review_gate_in_transaction(
                repo_root,
                target_branch,
                assignment,
                changed_files,
                status,
                review_config,
                review_range,
                Some(&transaction),
            )
            .await?;
        }
        if status == LoopTaskStatus::Done {
            clear_gate_hold(repo_root, &assignment.task.id)?;
        }
        Result::<LoopTaskStatus>::Ok(status)
    }
    .await;
    match gate_result {
        Ok(status) => {
            clear_canonical_gate_transaction(repo_root, &transaction)?;
            Ok(status)
        }
        Err(err) => Err(err),
    }
}

fn task_status_in_plan_text(plan_text: &str, task_id: &str) -> Result<LoopTaskStatus> {
    let matching = parse_loop_plan(plan_text)
        .tasks
        .into_iter()
        .filter(|task| task.id == task_id)
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [task] => Ok(task.status),
        [] => bail!("IMPLEMENTATION_PLAN.md has no row for `{task_id}`"),
        _ => bail!("IMPLEMENTATION_PLAN.md has duplicate rows for `{task_id}`"),
    }
}

/// Prove that both views a closeout commit can consume carry the intended task
/// status. Checking only the worktree is insufficient: a transient `git add`
/// failure can leave an older `[x]` in the index while the worktree says `[~]`.
fn require_task_status_persisted(
    repo_root: &Path,
    task_id: &str,
    expected: LoopTaskStatus,
) -> Result<()> {
    let worktree = read_loop_plan(repo_root)?;
    let worktree_status = task_status_in_plan_text(&worktree, task_id)?;
    let indexed = git_stdout(repo_root, ["show", ":IMPLEMENTATION_PLAN.md"])
        .context("failed to read the indexed IMPLEMENTATION_PLAN.md")?;
    let indexed_status = task_status_in_plan_text(&indexed, task_id)?;
    if worktree_status != expected || indexed_status != expected {
        bail!(
            "refusing closeout for `{task_id}`: expected {:?} in both IMPLEMENTATION_PLAN.md views, found worktree {:?} and index {:?}",
            expected,
            worktree_status,
            indexed_status
        );
    }
    Ok(())
}

fn persist_failed_gate_demotion(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    incoming_status: LoopTaskStatus,
    gate_label: &str,
) -> Result<()> {
    assignment.task.status = LoopTaskStatus::Partial;
    if incoming_status != LoopTaskStatus::Done {
        return Ok(());
    }
    if update_reconciled_task_completion_in_plan(
        repo_root,
        &assignment.task,
        LoopTaskStatus::Partial,
    )? {
        run_git(repo_root, ["add", "IMPLEMENTATION_PLAN.md"]).with_context(|| {
            format!(
                "failed staging IMPLEMENTATION_PLAN.md after {gate_label} demote for `{}`",
                assignment.task.id
            )
        })?;
    }
    require_task_status_persisted(repo_root, &assignment.task.id, LoopTaskStatus::Partial)
}

fn demote_task_for_failed_gate(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    incoming_status: LoopTaskStatus,
    gate_label: &str,
) {
    if let Err(err) =
        persist_failed_gate_demotion(repo_root, assignment, incoming_status, gate_label)
    {
        eprintln!(
            "warning: failed persisting `{}` as [~] after {gate_label}; closeout will be refused: {err:#}",
            assignment.task.id
        );
    }
}

/// Apply a [`LaneReviewOutcome`] to the lane's landing status. Synchronous and
/// side-effecting (writes `REVIEW.md`, demotes the plan row, stamps the lane
/// closeout log). Split out of
/// [`apply_lane_review_gate`] so tests can drive the demote/append/stamp wiring
/// with a stubbed outcome instead of calling real codex.
pub(crate) fn apply_lane_review_outcome(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    incoming_status: LoopTaskStatus,
    outcome: LaneReviewOutcome,
) -> Result<LoopTaskStatus> {
    match outcome {
        LaneReviewOutcome::Clean => {
            match append_lane_review_clearance(repo_root, &assignment.task.id) {
                Ok(true) => {
                    if let Err(err) = run_git(repo_root, ["add", "REVIEW.md"]) {
                        eprintln!(
                            "warning: failed staging REVIEW.md after standing-review clearance for `{}`: {err:#}",
                            assignment.task.id
                        );
                        record_gate_hold(
                            repo_root,
                            &assignment.task.id,
                            "standing review clearance was not staged",
                        )?;
                        demote_task_for_failed_gate(
                            repo_root,
                            assignment,
                            incoming_status,
                            "standing-review clearance persistence failure",
                        );
                        append_lane_host_event(
                            &assignment.stdout_log_path,
                            assignment.lane_index,
                            &assignment.task.id,
                            "independent-review: CLEAN report received but standing-review clearance could not be staged; task held [~]",
                        );
                        return Ok(LoopTaskStatus::Partial);
                    }
                }
                Ok(false) => {}
                Err(err) => {
                    eprintln!(
                        "warning: failed appending standing-review clearance for `{}`: {err:#}",
                        assignment.task.id
                    );
                    record_gate_hold(
                        repo_root,
                        &assignment.task.id,
                        "standing review clearance could not be persisted",
                    )?;
                    demote_task_for_failed_gate(
                        repo_root,
                        assignment,
                        incoming_status,
                        "standing-review clearance persistence failure",
                    );
                    append_lane_host_event(
                        &assignment.stdout_log_path,
                        assignment.lane_index,
                        &assignment.task.id,
                        "independent-review: CLEAN report received but standing-review clearance could not be persisted; task held [~]",
                    );
                    return Ok(LoopTaskStatus::Partial);
                }
            }
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                "independent-review: clean (no actionable findings)",
            );
            Ok(incoming_status)
        }
        LaneReviewOutcome::FindingsKeepPartial { findings_summary } => {
            // Hold so evidence-only promotion can't re-promote past these findings.
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "independent review findings",
            )?;
            append_lane_review_findings(repo_root, &assignment.task.id, &findings_summary)
                .with_context(|| {
                    format!(
                        "failed persisting tracked independent-review hold for `{}`",
                        assignment.task.id
                    )
                })?;
            run_git(repo_root, ["add", "REVIEW.md"])?;
            // Hold the task at [~] so it re-dispatches until a clean-review diff.
            demote_task_for_failed_gate(
                repo_root,
                assignment,
                incoming_status,
                "independent-review findings",
            );
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                "independent-review: actionable findings recorded to REVIEW.md; task held [~] for re-dispatch",
            );
            Ok(LoopTaskStatus::Partial)
        }
        LaneReviewOutcome::SkippedFailOpen { reason } => {
            eprintln!(
                "warning: independent-review gate skipped for `{}`; keeping task [~]: {reason}",
                assignment.task.id
            );
            record_gate_hold(repo_root, &assignment.task.id, "independent review skipped")?;
            append_lane_review_findings(
                repo_root,
                &assignment.task.id,
                &format!("Independent review gate skipped before finalization: {reason}"),
            )
            .with_context(|| {
                format!(
                    "failed persisting tracked independent-review skip for `{}`",
                    assignment.task.id
                )
            })?;
            run_git(repo_root, ["add", "REVIEW.md"])?;
            demote_task_for_failed_gate(
                repo_root,
                assignment,
                incoming_status,
                "independent-review skip",
            );
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("review_skipped: {reason}"),
            );
            Ok(LoopTaskStatus::Partial)
        }
        LaneReviewOutcome::InputMutationFatal { reason } => {
            bail!(
                "independent reviewer mutated canonical landing inputs for `{}`; refusing closeout or remote push: {reason}",
                assignment.task.id
            )
        }
    }
}

/// Orchestrator-side glue for the host re-execution verify gate (see
/// [`super::verify_gate`]). Runs the bounded re-execution and maps its
/// outcome onto the lane's landing status WITHOUT touching the committed work:
///
/// - Gate disabled (`AUTO_PARALLEL_VERIFY_LANDINGS=0`) -> hold `[~]`.
/// - `AllPassed` -> `incoming_status` unchanged; the lane lands `[x]`.
/// - `Failed` -> FAIL-CLOSED: append the failing command + output tail to
///   `REVIEW.md` (staged for the closeout), demote a `Done` task to `Partial` in
///   the plan, stamp the lane log, and return `Partial`. The task re-dispatches
///   until the host's own re-run is green.
/// - `Skipped` -> FAIL-CLOSED for finalization: stamp `verify_skipped`, record
///   the reason in `REVIEW.md`, and keep the task `[~]`.
#[cfg(test)]
async fn apply_lane_verify_gate(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    incoming_status: LoopTaskStatus,
) -> Result<LoopTaskStatus> {
    apply_lane_verify_gate_in_transaction(repo_root, assignment, incoming_status, None).await
}

async fn run_guarded_lane_verify_gate(
    repo_root: &Path,
    task_id: &str,
    task_markdown: &str,
    gate_label: &str,
    refresh_completion_evidence: bool,
) -> Result<LaneVerifyOutcome> {
    if refresh_completion_evidence {
        if let Err(err) = clear_verified_source_attestation(repo_root, task_id) {
            return Ok(LaneVerifyOutcome::Skipped {
                reason: format!("could not clear prior host verified-source attestation: {err:#}"),
            });
        }
    }
    let transaction = arm_canonical_gate_transaction(repo_root, task_id, gate_label)?;
    let mut outcome = run_lane_verify_gate_in_canonical_transaction(
        repo_root,
        task_id,
        task_markdown,
        &transaction,
        gate_label,
    )
    .await?;
    if refresh_completion_evidence && outcome == LaneVerifyOutcome::AllPassed {
        let attestation = propagate_lane_receipts(repo_root, repo_root, task_id, task_markdown)
            .and_then(|()| record_verified_source_attestation(repo_root, task_id));
        if let Err(err) = attestation {
            outcome = LaneVerifyOutcome::Skipped {
                reason: format!(
                    "commands passed, but durable host source attestation could not be recorded: {err:#}"
                ),
            };
        }
    }
    clear_canonical_gate_transaction(repo_root, &transaction)?;
    Ok(outcome)
}

async fn run_lane_verify_gate_in_canonical_transaction(
    repo_root: &Path,
    task_id: &str,
    task_markdown: &str,
    transaction: &ArmedCanonicalGateTransaction,
    stage: &str,
) -> Result<LaneVerifyOutcome> {
    let snapshot = capture_canonical_gate_subprocess_snapshot(repo_root)?;
    let outcome = run_lane_verify_gate(repo_root, task_id, task_markdown).await;
    revalidate_canonical_gate_subprocess_snapshot(repo_root, transaction, &snapshot, stage)?;
    Ok(outcome)
}

async fn apply_lane_verify_gate_in_transaction(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    incoming_status: LoopTaskStatus,
    transaction: Option<&ArmedCanonicalGateTransaction>,
) -> Result<LoopTaskStatus> {
    if !verify_gate_enabled() {
        return apply_lane_verify_outcome(
            repo_root,
            assignment,
            incoming_status,
            LaneVerifyOutcome::Skipped {
                reason:
                    "AUTO_PARALLEL_VERIFY_LANDINGS=0 disabled a mandatory definition-of-done gate"
                        .to_string(),
            },
        );
    }
    if let Err(err) = clear_verified_source_attestation(repo_root, &assignment.task.id) {
        return apply_lane_verify_outcome(
            repo_root,
            assignment,
            incoming_status,
            LaneVerifyOutcome::Skipped {
                reason: format!("could not clear prior host verified-source attestation: {err:#}"),
            },
        );
    }
    let mut outcome = if let Some(transaction) = transaction {
        run_lane_verify_gate_in_canonical_transaction(
            repo_root,
            &assignment.task.id,
            &assignment.task.markdown,
            transaction,
            "host verification subprocess",
        )
        .await?
    } else {
        run_lane_verify_gate(repo_root, &assignment.task.id, &assignment.task.markdown).await
    };
    if outcome == LaneVerifyOutcome::AllPassed {
        let attestation = propagate_lane_receipts(
            repo_root,
            repo_root,
            &assignment.task.id,
            &assignment.task.markdown,
        )
        .and_then(|()| record_verified_source_attestation(repo_root, &assignment.task.id));
        if let Err(err) = attestation {
            outcome = LaneVerifyOutcome::Skipped {
                reason: format!(
                    "commands passed, but durable host source attestation could not be recorded: {err:#}"
                ),
            };
        }
    }
    apply_lane_verify_outcome(repo_root, assignment, incoming_status, outcome)
}

/// Apply a [`LaneVerifyOutcome`] to the lane's landing status. Synchronous and
/// side-effecting. Split out so tests can drive the
/// demote/append/stamp wiring with a stubbed outcome instead of spawning builds.
pub(crate) fn apply_lane_verify_outcome(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    incoming_status: LoopTaskStatus,
    outcome: LaneVerifyOutcome,
) -> Result<LoopTaskStatus> {
    match outcome {
        LaneVerifyOutcome::AllPassed => {
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                "host-reexec-verify: declared verification re-passed at canonical HEAD",
            );
            Ok(incoming_status)
        }
        LaneVerifyOutcome::Failed { detail } => {
            // Hold the task so evidence-only promotion can't undo this demotion.
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "host re-execution verification failed",
            )?;
            append_lane_verify_failure(repo_root, &assignment.task.id, &detail).with_context(
                || {
                    format!(
                        "failed persisting tracked host verification hold for `{}`",
                        assignment.task.id
                    )
                },
            )?;
            run_git(repo_root, ["add", "REVIEW.md"])?;
            // Hold the task at [~] so it re-dispatches until the host's re-run is green.
            demote_task_for_failed_gate(
                repo_root,
                assignment,
                incoming_status,
                "host-reexec-verify failure",
            );
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                "host-reexec-verify: a declared verification command FAILED at canonical HEAD; task held [~] for re-dispatch",
            );
            Ok(LoopTaskStatus::Partial)
        }
        LaneVerifyOutcome::Skipped { reason } => {
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "host re-execution verification skipped",
            )?;
            append_lane_verify_failure(
                repo_root,
                &assignment.task.id,
                &format!("host re-execution verification skipped before finalization: {reason}"),
            )
            .with_context(|| {
                format!(
                    "failed persisting tracked host verification skip for `{}`",
                    assignment.task.id
                )
            })?;
            run_git(repo_root, ["add", "REVIEW.md"])?;
            demote_task_for_failed_gate(
                repo_root,
                assignment,
                incoming_status,
                "host-reexec-verify skip",
            );
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("verify_skipped: {reason}"),
            );
            Ok(LoopTaskStatus::Partial)
        }
    }
}

/// Definition-of-done workspace gate. In the default `baseline` mode it demotes
/// a task ONLY when the task introduced a NEW regression vs the run's
/// best-observed baseline (a previously-passing test now failing, or a
/// previously-compiling crate the task touched now failing to compile) — a
/// pre-existing failure elsewhere in the shared workspace no longer blocks every
/// task's promotion. `AUTO_PARALLEL_WORKSPACE_GATE_MODE=strict` restores the
/// legacy "whole workspace must be green" bar.
#[cfg(test)]
async fn apply_workspace_test_gate(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
) -> Result<LoopTaskStatus> {
    apply_workspace_test_gate_in_transaction(
        repo_root,
        assignment,
        changed_files,
        incoming_status,
        None,
    )
    .await
}

async fn apply_workspace_test_gate_in_transaction(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
    transaction: Option<&ArmedCanonicalGateTransaction>,
) -> Result<LoopTaskStatus> {
    apply_workspace_test_gate_mode_in_transaction(
        repo_root,
        assignment,
        changed_files,
        incoming_status,
        transaction,
        workspace_gate_mode(),
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn apply_workspace_test_gate_mode_in_transaction(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
    transaction: Option<&ArmedCanonicalGateTransaction>,
    mode: WorkspaceGateMode,
    cargo_bin: Option<PathBuf>,
) -> Result<LoopTaskStatus> {
    match mode {
        WorkspaceGateMode::Strict => {
            let probe = run_workspace_probe_in_canonical_transaction(
                repo_root,
                transaction,
                "strict workspace test subprocess",
                cargo_bin,
            )
            .await?;
            let outcome = workspace_test_outcome_from_probe(probe);
            apply_workspace_test_outcome(repo_root, assignment, incoming_status, outcome)
        }
        WorkspaceGateMode::Baseline => {
            apply_workspace_baseline_gate_in_transaction_with_cargo(
                repo_root,
                assignment,
                changed_files,
                incoming_status,
                transaction,
                cargo_bin,
            )
            .await
        }
    }
}

/// Derive the run root (`<run_root>/lanes/lane-N` -> `<run_root>`) from a lane
/// root, but ONLY when the path actually has the canonical `lanes/lane-*` shape.
/// Test fixtures use ad-hoc lane roots; returning `None` there makes the gate
/// fail closed instead of writing a stray baseline file into a shared temp
/// directory.
fn workspace_baseline_run_root(lane_root: &Path) -> Option<PathBuf> {
    let lanes = lane_root.parent()?;
    if lanes.file_name()?.to_str()? != "lanes" {
        return None;
    }
    Some(lanes.parent()?.to_path_buf())
}

#[cfg(test)]
async fn apply_workspace_baseline_gate(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
) -> Result<LoopTaskStatus> {
    apply_workspace_baseline_gate_in_transaction_with_cargo(
        repo_root,
        assignment,
        changed_files,
        incoming_status,
        None,
        None,
    )
    .await
}

async fn apply_workspace_baseline_gate_in_transaction_with_cargo(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
    transaction: Option<&ArmedCanonicalGateTransaction>,
    cargo_bin: Option<PathBuf>,
) -> Result<LoopTaskStatus> {
    let probe = run_workspace_probe_in_canonical_transaction(
        repo_root,
        transaction,
        "workspace baseline subprocess",
        cargo_bin,
    )
    .await?;
    let obs = match probe {
        WorkspaceProbe::NotApplicable { reason } => {
            // Non-Rust repo: nothing to check. Pass-through (never demote every
            // task in a Python/TS repo to [~]).
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("workspace-baseline: gate not applicable ({reason}); pass-through"),
            );
            return Ok(incoming_status);
        }
        WorkspaceProbe::Skipped { reason } => {
            return apply_workspace_test_outcome(
                repo_root,
                assignment,
                incoming_status,
                WorkspaceTestOutcome::Skipped {
                    reason: format!("workspace baseline probe skipped: {reason}"),
                },
            );
        }
        WorkspaceProbe::Ran(obs) => obs,
    };

    let Some(run_root) = workspace_baseline_run_root(&assignment.lane_root) else {
        return apply_workspace_test_outcome(
            repo_root,
            assignment,
            incoming_status,
            WorkspaceTestOutcome::Skipped {
                reason:
                    "workspace baseline has no lane run root; cannot prove the comparison policy"
                        .to_string(),
            },
        );
    };

    let baseline = load_workspace_baseline(&run_root);
    if !baseline.captured
        && (!obs.compiled || !obs.failing_tests.is_empty() || !obs.broken_crates.is_empty())
    {
        return apply_workspace_test_outcome(
            repo_root,
            assignment,
            incoming_status,
            WorkspaceTestOutcome::Skipped {
                reason: format!(
                    "workspace baseline was not captured before lane execution, and the current post-landing tree is not fully green; refusing to learn a tolerated baseline from this red tree\n{}",
                    summarize_workspace_failure(&obs)
                ),
            },
        );
    }
    let baseline_note = format!(
        "workspace-baseline: baseline had {} pre-existing failing test(s), {} broken crate(s); best-observed {} passing test(s), {} compiled crate(s)",
        baseline.baseline_failing_tests.len(),
        baseline.baseline_broken_crates.len(),
        baseline.ever_passed_tests.len(),
        baseline.ever_compiled_crates.len(),
    );

    // Only pay for `cargo metadata` attribution when a blocking regression
    // actually exists; the clean path stays cheap. The default STRICT gate blocks
    // on any NEW deterministic (non-environmental) failure REGARDLESS of lane
    // scope; the legacy path downgrades out-of-lane-scope regressions to advisory.
    let strict = workspace_strict_baseline_enabled();
    let decision = if strict {
        let env_patterns = env_failure_patterns();
        if strict_workspace_has_blocking(&baseline, &obs, &env_patterns) {
            let touched = touched_workspace_crates(repo_root, changed_files);
            classify_workspace_regressions_strict(&baseline, &obs, &touched, &env_patterns)
        } else {
            // No blocking regression, but still classify to surface tolerated
            // environmental failures without paying for `cargo metadata`.
            classify_workspace_regressions_strict(&baseline, &obs, &BTreeSet::new(), &env_patterns)
        }
    } else if has_candidate_regression(&baseline, &obs) {
        let touched = touched_workspace_crates(repo_root, changed_files);
        classify_workspace_regressions(&baseline, &obs, &touched)
    } else {
        WorkspaceRegressionDecision::default()
    };

    // Advance the best-observed baseline (monotonic) and persist BEFORE acting,
    // so a demote/redispatch cycle still records this run's passes/compiles.
    let mut advanced = baseline;
    advance_workspace_baseline(&mut advanced, &obs);
    save_workspace_baseline(&run_root, &advanced);

    // Environmental failures tolerated by pattern are surfaced (not swallowed) so
    // a mis-scoped pattern silently absorbing a real regression is auditable.
    if !decision.tolerated_environmental.is_empty() {
        append_lane_host_event(
            &assignment.stdout_log_path,
            assignment.lane_index,
            &assignment.task.id,
            &format!(
                "workspace-baseline: tolerated {} environmental failure(s) by pattern (never block): {}",
                decision.tolerated_environmental.len(),
                decision
                    .tolerated_environmental
                    .iter()
                    .take(25)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }

    // Nonblocking (another lane's) regressions are always surfaced for operators.
    // Only the legacy lane-scoped path produces these; the strict gate blocks
    // them instead.
    for note in &decision.nonblocking {
        append_lane_host_event(
            &assignment.stdout_log_path,
            assignment.lane_index,
            &assignment.task.id,
            &format!("workspace-baseline: NEW regression not attributed to this task: {note}"),
        );
    }

    if decision.is_blocked() {
        let scope_note = if strict {
            "NEW deterministic regression(s) in the workspace (strict baseline gate blocks REGARDLESS of lane scope — a green row must mean a green workspace):"
        } else {
            "NEW regression(s) introduced by this task:"
        };
        let detail = format!(
            "{baseline_note}\n\n{scope_note}\n- {}",
            decision.blocking.join("\n- ")
        );
        record_gate_hold(
            repo_root,
            &assignment.task.id,
            "workspace baseline regression",
        )?;
        append_lane_workspace_test_failure(
            repo_root,
            &assignment.task.id,
            "workspace baseline gate: workspace carries a NEW deterministic regression vs best-observed baseline",
            &detail,
        )
        .with_context(|| {
            format!(
                "failed persisting tracked workspace-baseline hold for `{}`",
                assignment.task.id
            )
        })?;
        run_git(repo_root, ["add", "REVIEW.md"])?;
        demote_task_for_failed_gate(
            repo_root,
            assignment,
            incoming_status,
            "workspace-baseline regression",
        );
        append_lane_host_event(
            &assignment.stdout_log_path,
            assignment.lane_index,
            &assignment.task.id,
            &format!(
                "workspace-baseline: NEW deterministic regression(s) present; held [~]{}: {}",
                if strict {
                    " (strict gate, lane-agnostic)"
                } else {
                    ""
                },
                decision.blocking.join("; ")
            ),
        );
        return Ok(LoopTaskStatus::Partial);
    }

    let promote_reason = if obs.compiled && obs.failing_tests.is_empty() {
        format!("{baseline_note}; workspace fully green")
    } else if !obs.compiled {
        format!(
            "{baseline_note}; workspace does not compile but the break(s) are pre-existing/another lane's ({}); no NEW regression from this task",
            obs.broken_crates
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!(
            "{baseline_note}; {} failing test(s) remain but all are pre-existing baseline failures; no NEW regression from this task",
            obs.failing_tests.len()
        )
    };
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &format!("workspace-baseline: promoted [x] — {promote_reason}"),
    );
    Ok(incoming_status)
}

async fn run_workspace_probe_in_canonical_transaction(
    repo_root: &Path,
    transaction: Option<&ArmedCanonicalGateTransaction>,
    stage: &str,
    cargo_bin: Option<PathBuf>,
) -> Result<WorkspaceProbe> {
    let snapshot = transaction
        .map(|_| capture_canonical_gate_subprocess_snapshot(repo_root))
        .transpose()?;
    let probe = run_workspace_probe_with_cargo(repo_root, cargo_bin).await;
    if let (Some(transaction), Some(snapshot)) = (transaction, snapshot.as_ref()) {
        revalidate_canonical_gate_subprocess_snapshot(repo_root, transaction, snapshot, stage)?;
    }
    Ok(probe)
}

/// Capture the run's pre-existing workspace failure/compile baseline ONCE, at run
/// start before any lane lands, so a regression introduced by the very first
/// landing cannot be silently absorbed into the baseline. Skipped in strict mode,
/// on non-Rust repos, and on resume (a baseline is already persisted). Bounded by
/// the same timeout as the gate; a timeout/skip just defers to lazy capture at
/// the first landing.
async fn run_guarded_workspace_probe(
    repo_root: &Path,
    task_id: &str,
    gate_label: &str,
) -> Result<WorkspaceProbe> {
    let transaction = arm_canonical_gate_transaction(repo_root, task_id, gate_label)?;
    let probe = run_workspace_probe_in_canonical_transaction(
        repo_root,
        Some(&transaction),
        gate_label,
        None,
    )
    .await?;
    clear_canonical_gate_transaction(repo_root, &transaction)?;
    Ok(probe)
}

pub(crate) async fn maybe_capture_workspace_baseline(
    repo_root: &Path,
    run_root: &Path,
    parallel_logger: &ParallelEventLogger,
) {
    if matches!(workspace_gate_mode(), WorkspaceGateMode::Strict) {
        return;
    }
    if !repo_root.join("Cargo.toml").is_file() {
        return;
    }
    let existing = load_workspace_baseline(run_root);
    if existing.captured {
        // Recapture-on-drift: when a run RESTARTS on a materially-advanced HEAD,
        // refresh the tolerated snapshot instead of blindly reusing a possibly
        // days-stale baseline. Safe by construction — recapture never folds a
        // non-environmental red into the tolerated set (it surfaces it), so it
        // cannot re-open the hole by absorbing a regression that landed while the
        // process was down.
        if workspace_strict_baseline_enabled() {
            let current_head = current_repo_head(repo_root);
            let drifted = matches!(
                (existing.head_at_capture.as_deref(), current_head.as_deref()),
                (Some(prev), Some(now)) if prev != now
            );
            if drifted {
                let probe = match run_guarded_workspace_probe(
                    repo_root,
                    "workspace-baseline",
                    "workspace baseline recapture",
                )
                .await
                {
                    Ok(probe) => probe,
                    Err(err) => {
                        parallel_logger.warn(format!(
                            "workspace-baseline: canonical integrity failed during recapture: \
                             {err:#}"
                        ));
                        return;
                    }
                };
                if let (Some(now), WorkspaceProbe::Ran(obs)) = (current_head.as_deref(), probe) {
                    let env_patterns = env_failure_patterns();
                    let recapture =
                        recapture_workspace_baseline_on_drift(&existing, &obs, &env_patterns, now);
                    save_workspace_baseline(run_root, &recapture.baseline);
                    parallel_logger.info(format!(
                        "workspace-baseline: HEAD advanced since capture ({} -> {}); recaptured on drift ({} newly-tolerated environmental failure(s))",
                        existing.head_at_capture.as_deref().unwrap_or("?"),
                        now,
                        recapture.newly_tolerated_environmental.len(),
                    ));
                    if !recapture.surfaced_nonenvironmental.is_empty() {
                        parallel_logger.warn(format!(
                            "workspace-baseline: recapture surfaced {} NEW non-environmental failing test(s) at the advanced HEAD — these are NOT tolerated and WILL block landings until fixed or classified environmental (AUTO_WORKSPACE_ENV_FAILURE_PATTERNS): {}",
                            recapture.surfaced_nonenvironmental.len(),
                            recapture
                                .surfaced_nonenvironmental
                                .iter()
                                .take(25)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", "),
                        ));
                    }
                    if let Some(diag) = workspace_compile_block_diagnostic(&recapture.baseline) {
                        parallel_logger.warn(format!("workspace-compile-block: {diag}"));
                    }
                    return;
                }
                parallel_logger.warn(
                    "workspace-baseline: HEAD advanced but recapture probe was skipped; reusing persisted baseline",
                );
            }
        }
        parallel_logger.info(format!(
            "workspace-baseline: reusing persisted baseline ({} pre-existing failing test(s), {} broken crate(s); best-observed {} passing / {} compiled)",
            existing.baseline_failing_tests.len(),
            existing.baseline_broken_crates.len(),
            existing.ever_passed_tests.len(),
            existing.ever_compiled_crates.len(),
        ));
        if let Some(diag) = workspace_compile_block_diagnostic(&existing) {
            parallel_logger.warn(format!("workspace-compile-block: {diag}"));
        }
        return;
    }
    parallel_logger
        .info("workspace-baseline: capturing pre-existing workspace baseline at run start (one-time; `cargo test --workspace --no-fail-fast`)...");
    let probe = match run_guarded_workspace_probe(
        repo_root,
        "workspace-baseline",
        "workspace baseline capture",
    )
    .await
    {
        Ok(probe) => probe,
        Err(err) => {
            parallel_logger.warn(format!(
                "workspace-baseline: canonical integrity failed during baseline capture: {err:#}"
            ));
            return;
        }
    };
    match probe {
        WorkspaceProbe::Ran(obs) => {
            let mut baseline = WorkspaceBaseline::default();
            advance_workspace_baseline(&mut baseline, &obs);
            // Stamp the capture HEAD so a later restart on an advanced HEAD can
            // recapture-on-drift instead of reusing a stale snapshot.
            baseline.head_at_capture = current_repo_head(repo_root);
            save_workspace_baseline(run_root, &baseline);
            parallel_logger.info(format!(
                "workspace-baseline: captured — compiles={}, {} pre-existing failing test(s), {} broken crate(s), {} passing test(s) recorded as best-observed",
                obs.compiled,
                obs.failing_tests.len(),
                obs.broken_crates.len(),
                obs.passing_tests.len(),
            ));
            if let Some(diag) = workspace_compile_block_diagnostic(&baseline) {
                parallel_logger.warn(format!("workspace-compile-block: {diag}"));
            }
        }
        WorkspaceProbe::NotApplicable { reason } => {
            parallel_logger.info(format!(
                "workspace-baseline: gate not applicable at run start ({reason})"
            ));
        }
        WorkspaceProbe::Skipped { reason } => {
            parallel_logger.warn(format!(
                "workspace-baseline: could not capture baseline at run start ({reason}); post-landing probes will remain held unless the workspace is fully green"
            ));
        }
    }
}

/// Map a task's changed files to the normalized names of the workspace crates
/// they live in (the task's compile/test blast radius), via `cargo metadata`.
/// Only invoked when a candidate regression exists, keeping metadata off the
/// clean landing path.
fn touched_workspace_crates(repo_root: &Path, changed_files: &[String]) -> BTreeSet<String> {
    let members = workspace_member_dirs(repo_root);
    let mut touched = BTreeSet::new();
    for file in changed_files {
        let abs = repo_root.join(file);
        let mut best: Option<(usize, &str)> = None;
        for (dir, name) in &members {
            if abs.starts_with(dir) {
                let len = dir.as_os_str().len();
                if best.map(|(best_len, _)| len > best_len).unwrap_or(true) {
                    best = Some((len, name.as_str()));
                }
            }
        }
        if let Some((_, name)) = best {
            touched.insert(name.to_string());
        }
    }
    touched
}

/// `(crate_dir, normalized_crate_name)` for each workspace member, from
/// `cargo metadata --no-deps`. Returns empty on any error (attribution then
/// treats all regressions as un-attributed/nonblocking — conservative for
/// throughput, and the own-verify gate still guards the task's own crate).
fn workspace_member_dirs(repo_root: &Path) -> Vec<(PathBuf, String)> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repo_root)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    #[derive(Deserialize)]
    struct Metadata {
        packages: Vec<Package>,
    }
    #[derive(Deserialize)]
    struct Package {
        name: String,
        manifest_path: String,
    }
    let Ok(metadata) = serde_json::from_slice::<Metadata>(&output.stdout) else {
        return Vec::new();
    };
    metadata
        .packages
        .into_iter()
        .filter_map(|pkg| {
            Path::new(&pkg.manifest_path)
                .parent()
                .map(|dir| (dir.to_path_buf(), normalize_crate_name(&pkg.name)))
        })
        .collect()
}

pub(crate) fn apply_workspace_test_outcome(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    incoming_status: LoopTaskStatus,
    outcome: WorkspaceTestOutcome,
) -> Result<LoopTaskStatus> {
    match outcome {
        WorkspaceTestOutcome::Passed => {
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                "workspace-test: `cargo test --workspace` passed at canonical HEAD",
            );
            Ok(incoming_status)
        }
        WorkspaceTestOutcome::Failed { detail } => {
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "workspace cargo test failed",
            )?;
            append_lane_workspace_test_failure(
                repo_root,
                &assignment.task.id,
                "workspace cargo test failed before finalization",
                &detail,
            )
            .with_context(|| {
                format!(
                    "failed persisting tracked workspace-test hold for `{}`",
                    assignment.task.id
                )
            })?;
            run_git(repo_root, ["add", "REVIEW.md"])?;
            demote_task_for_failed_gate(
                repo_root,
                assignment,
                incoming_status,
                "workspace-test failure",
            );
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                "workspace-test: `cargo test --workspace` FAILED at canonical HEAD; task held [~]",
            );
            Ok(LoopTaskStatus::Partial)
        }
        WorkspaceTestOutcome::NotApplicable { reason } => {
            // Non-Rust repo: the gate has nothing to check. Treat as pass-through
            // rather than demoting every task in Python/TS repos to [~] forever.
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("workspace-test: gate not applicable ({reason}); pass-through"),
            );
            Ok(incoming_status)
        }
        WorkspaceTestOutcome::Skipped { reason } => {
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "workspace cargo test skipped",
            )?;
            append_lane_workspace_test_failure(
                repo_root,
                &assignment.task.id,
                "workspace cargo test skipped before finalization",
                &reason,
            )
            .with_context(|| {
                format!(
                    "failed persisting tracked workspace-test skip for `{}`",
                    assignment.task.id
                )
            })?;
            run_git(repo_root, ["add", "REVIEW.md"])?;
            demote_task_for_failed_gate(
                repo_root,
                assignment,
                incoming_status,
                "workspace-test skip",
            );
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("workspace_test_skipped: {reason}"),
            );
            Ok(LoopTaskStatus::Partial)
        }
    }
}

/// Append a host re-execution failure note to `REVIEW.md` so the next worker
/// sees exactly which declared verification command failed at canonical HEAD.
fn append_lane_verify_failure(repo_root: &Path, task_id: &str, detail: &str) -> Result<()> {
    let review_path = repo_root.join("REVIEW.md");
    let mut review_text = if review_path.exists() {
        std::fs::read_to_string(&review_path)?
    } else {
        "# REVIEW\n\nAwaiting auto review:\n".to_string()
    };
    if !review_text.ends_with('\n') {
        review_text.push('\n');
    }
    review_text.push_str(&format!(
        "\n## `{task_id}`: host re-execution verification failed\n\
- Source: auto parallel host re-execution verify gate (held at `[~]`).\n\
- The host re-ran this task's declared verification command(s) at canonical HEAD and one FAILED.\n  Fix the failure, then the task re-dispatches until the host's own re-run is green.\n\n\
```\n{}\n```\n",
        detail.trim()
    ));
    atomic_write(&review_path, review_text.as_bytes())?;
    Ok(())
}

fn append_lane_workspace_test_failure(
    repo_root: &Path,
    task_id: &str,
    title: &str,
    detail: &str,
) -> Result<()> {
    let review_path = repo_root.join("REVIEW.md");
    let mut review_text = if review_path.exists() {
        std::fs::read_to_string(&review_path)?
    } else {
        "# REVIEW\n\nAwaiting auto review:\n".to_string()
    };
    if !review_text.ends_with('\n') {
        review_text.push('\n');
    }
    review_text.push_str(&format!(
        "\n## `{task_id}`: workspace cargo test failed\n\
- Source: auto parallel workspace definition-of-done gate (held at `[~]`).\n\
- The host requires `cargo test --workspace` to pass on the current tree before any task can be marked `[x]`.\n\
- Gate result: {title}.\n\n\
```\n{}\n```\n",
        detail.trim()
    ));
    atomic_write(&review_path, review_text.as_bytes())?;
    Ok(())
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
    task_markdown: &str,
) -> Result<()> {
    let receipt_rel = std::path::PathBuf::from(".auto/symphony/verification-receipts")
        .join(format!("{task_id}.json"));
    let src_receipt = lane_repo_root.join(&receipt_rel);
    let dst_receipt = canonical_root.join(&receipt_rel);
    if src_receipt.is_file() {
        let canonical_receipt_before = std::fs::read(&dst_receipt).ok();
        let evidence = inspect_task_completion_evidence(canonical_root, task_id, task_markdown);
        let expected_commands = verification_plan(task_markdown).executable_commands;
        let declared_artifacts = evidence.declared_completion_artifacts;
        let canonical_receipt_was_valid =
            canonical_receipt_before.as_ref().is_some_and(|previous| {
                std::str::from_utf8(previous).is_ok_and(|text| {
                    matches!(
                        direct_verification_receipt_problem(
                            canonical_root,
                            &dst_receipt,
                            text,
                            &expected_commands,
                            &declared_artifacts,
                        ),
                        Ok(None)
                    )
                })
            });
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
        if let Some(fp) = current_dirty_state_fingerprint(canonical_root) {
            if let Some(obj) = value.as_object_mut() {
                let mut dirty = serde_json::Map::new();
                dirty.insert("fingerprint".to_string(), serde_json::Value::String(fp));
                obj.insert("dirty_state".to_string(), serde_json::Value::Object(dirty));
            }
        }
        // Refresh `plan_hash` to canonical's current IMPLEMENTATION_PLAN.md, the
        // same way `commit` and `dirty_state` are refreshed above. The host
        // rewrites `commit` to canonical HEAD, which ACTIVATES the receipt
        // freshness gate's plan-hash comparison (it only runs when the receipt
        // commit is current — see `verification_receipt_freshness_problem` in
        // completion_artifacts/receipt.rs). The hash is taken over the plan with
        // task-status checkbox markers normalized out (see
        // `normalized_plan_hash_bytes`), so a checkbox flip no longer drifts it;
        // this refresh now only matters when the canonical plan's SPEC content
        // (not its statuses) changed between worker verification and landing.
        // Genuine spec drift is still caught by the declared-artifact hash
        // checks, the verification-command checks, and the diff-review gate.
        if let Ok(plan_bytes) = std::fs::read(canonical_root.join("IMPLEMENTATION_PLAN.md")) {
            let plan_hash = crate::completion_artifacts::normalized_plan_hash_bytes(&plan_bytes);
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "plan_hash".to_string(),
                    serde_json::Value::String(plan_hash),
                );
            }
        }
        let pretty = serde_json::to_string_pretty(&value)
            .context("failed to re-serialize symphony receipt for canonical write")?;
        let candidate = pretty + "\n";
        let candidate_problem = direct_verification_receipt_problem(
            canonical_root,
            &dst_receipt,
            &candidate,
            &expected_commands,
            &declared_artifacts,
        )?;
        if canonical_receipt_was_valid && candidate_problem.is_some() {
            eprintln!(
                "warning: refusing to replace valid canonical receipt for `{task_id}` with degraded lane evidence: {}",
                candidate_problem.unwrap_or_default()
            );
        } else {
            atomic_write(&dst_receipt, candidate.as_bytes()).with_context(|| {
                format!(
                    "failed to write canonical symphony receipt {}",
                    dst_receipt.display()
                )
            })?;
        }
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

pub(crate) async fn reconcile_parallel_clean_no_commit(
    repo_root: &Path,
    target_branch: &str,
    assignment: &mut ActiveLaneAssignment,
    parallel_logger: &ParallelEventLogger,
    review_config: &LaneReviewConfig,
) -> Result<bool> {
    write_clean_no_commit_verdict(
        assignment,
        "needs-human-triage",
        "lane exited cleanly without a local commit; canonical evidence will be inspected before shelving",
    )?;
    // A prior hold is not completion evidence, but it is also not a permanent
    // liveness barrier: a concurrent canonical fix may already have resolved
    // the finding. Preserve the hold while re-running every current-tree gate;
    // only the complete gate pipeline may clear it.
    if task_is_gate_held(repo_root, &assignment.task.id)? {
        parallel_logger.info(format!(
            "clean-no-commit: lane-{} `{}` is gate-held; starting full current-tree revalidation",
            assignment.lane_index, assignment.task.id
        ));
        append_lane_host_event(
            &assignment.stdout_log_path,
            assignment.lane_index,
            &assignment.task.id,
            "clean-no-commit: existing gate hold retained while full revalidation runs",
        );
    }
    propagate_lane_receipts(
        &assignment.lane_repo_root,
        repo_root,
        &assignment.task.id,
        &assignment.task.markdown,
    )?;
    let evidence_before =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    // A clean no-commit lane is specifically a request to re-adjudicate the
    // canonical tree. Missing/stale receipt evidence and standing review
    // findings are repair inputs for the verify/review gates below, not reasons
    // to skip those gates. Only immutable completion artifacts and owned audit
    // constraints must already be satisfied.
    let mut immutable_evidence = evidence_before.clone();
    immutable_evidence.has_review_handoff = true;
    immutable_evidence.unresolved_review_findings.clear();
    immutable_evidence.verification_receipt_present = true;
    immutable_evidence.verification_receipt_status = None;
    if !immutable_evidence.is_ready_for_definition_of_done_gates() {
        let missing = immutable_evidence.missing_reasons().join("; ");
        let reason =
            format!("canonical evidence is not ready for definition-of-done gates: {missing}");
        write_clean_no_commit_verdict(assignment, "evidence-incomplete", &reason)?;
        parallel_logger.warn(format!(
            "clean-no-commit: lane-{} `{}` cannot reconcile: {reason}",
            assignment.lane_index, assignment.task.id
        ));
        append_lane_host_event(
            &assignment.stdout_log_path,
            assignment.lane_index,
            &assignment.task.id,
            &format!("clean-no-commit evidence incomplete: {missing}"),
        );
        return Ok(false);
    }

    let mut review_evidence = evidence_before;
    review_evidence.has_review_handoff = true;
    if ensure_host_review_handoff(repo_root, &assignment.task.id, &[], &review_evidence)? {
        run_git(repo_root, ["add", "REVIEW.md"])?;
    }
    // Gate a virtual Done transition while the persisted queue row remains
    // Pending/Partial. A crash at any point before every gate passes therefore
    // cannot leave a generic startup checkpoint able to commit an unearned [x].
    let mut completion_status = LoopTaskStatus::Done;
    completion_status = apply_definition_of_done_gates(
        repo_root,
        target_branch,
        assignment,
        &[],
        None,
        completion_status,
        review_config,
    )
    .await?;

    if completion_status != LoopTaskStatus::Done {
        let hold_reason = std::fs::read_to_string(gate_hold_path(repo_root, &assignment.task.id))
            .unwrap_or_else(|_| "a definition-of-done gate did not pass".to_string());
        let review_marker = format!("## `{}`:", assignment.task.id);
        let review_detail = std::fs::read_to_string(repo_root.join("REVIEW.md"))
            .ok()
            .and_then(|review| {
                let (_, tail) = review.rsplit_once(&review_marker)?;
                let body = tail.split("\n## ").next().unwrap_or(tail).trim();
                Some(format!("; {review_marker} {body}"))
            })
            .unwrap_or_default();
        let reason = format!("{hold_reason}{review_detail}");
        write_clean_no_commit_verdict(assignment, "landed-unverified", &reason)?;
        parallel_logger.warn(format!(
            "clean-no-commit: lane-{} `{}` remains landed-unverified: {reason}",
            assignment.lane_index, assignment.task.id
        ));
        return Ok(false);
    }

    // Verification can mutate only repo-local receipt state while review can
    // mutate host-owned queue files. Rebind the receipt's mutable canonical
    // metadata after all gates, then require the ordinary evidence inspector
    // to accept it. A direct command pass without a wrapper/receipt is not
    // durable completion authority.
    propagate_lane_receipts(
        repo_root,
        repo_root,
        &assignment.task.id,
        &assignment.task.markdown,
    )?;
    let final_evidence =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    if !final_evidence.is_fully_evidenced() {
        let missing = final_evidence.missing_reasons().join("; ");
        let reason = format!("post-gate completion evidence is not fresh and durable: {missing}");
        let demoted = apply_lane_verify_outcome(
            repo_root,
            assignment,
            LoopTaskStatus::Done,
            LaneVerifyOutcome::Skipped {
                reason: reason.clone(),
            },
        )?;
        debug_assert_eq!(demoted, LoopTaskStatus::Partial);
        write_clean_no_commit_verdict(assignment, "landed-unverified", &reason)?;
        parallel_logger.warn(format!(
            "clean-no-commit: lane-{} `{}` remains landed-unverified: {reason}",
            assignment.lane_index, assignment.task.id
        ));
        return Ok(false);
    }

    record_gate_hold(
        repo_root,
        &assignment.task.id,
        "all gates passed; durable closeout commit is still pending",
    )?;
    assignment.task.status = LoopTaskStatus::Done;

    // Full gates are necessary but not sufficient for durable completion:
    // persist the [x]/review state in a host-authored commit carrying the
    // verification receipt. If the queue was already committed, use the
    // provenance-constrained empty backfill form instead.
    let closeout_result = (|| -> Result<()> {
        let plan_updated = update_reconciled_task_completion_in_plan(
            repo_root,
            &assignment.task,
            LoopTaskStatus::Done,
        )?;
        if plan_updated {
            run_git(repo_root, ["add", "IMPLEMENTATION_PLAN.md"])?;
        }
        let staged = git_stdout(repo_root, ["diff", "--cached", "--name-only"])?;
        let staged_paths = staged
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .collect::<Vec<_>>();
        let allowed_queue_paths = host_queue_state_files_for_repo(repo_root);
        if let Some(path) = staged_paths
            .iter()
            .find(|path| !allowed_queue_paths.contains(path))
        {
            bail!(
                "clean-no-commit closeout for `{}` has non-host queue path `{path}` staged; refusing to mint a durable verification footer",
                assignment.task.id
            );
        }
        let (message, allow_empty) = if staged_paths.is_empty() {
            (
                format!(
                    "{}: {} receipt footer backfill",
                    repo_name(repo_root),
                    assignment.task.id
                ),
                true,
            )
        } else {
            (
                format!(
                    "{}: {} queue sync",
                    repo_name(repo_root),
                    assignment.task.id
                ),
                false,
            )
        };
        commit_task_closeout(
            repo_root,
            &assignment.task.id,
            LoopTaskStatus::Done,
            &message,
            allow_empty,
        )
    })();
    if let Err(err) = closeout_result {
        let reason = format!("durable clean-no-commit closeout failed: {err:#}");
        record_gate_hold(repo_root, &assignment.task.id, &reason)?;
        persist_failed_gate_demotion(
            repo_root,
            assignment,
            LoopTaskStatus::Done,
            "durable clean-no-commit closeout failure",
        )
        .with_context(|| {
            format!(
                "failed rolling back `{}` to Partial after closeout error: {err:#}",
                assignment.task.id
            )
        })?;
        write_clean_no_commit_verdict(assignment, "landed-unverified", &reason)?;
        parallel_logger.warn(format!(
            "clean-no-commit: lane-{} `{}` remains landed-unverified: {reason}",
            assignment.lane_index, assignment.task.id
        ));
        return Ok(false);
    }
    clear_gate_hold(repo_root, &assignment.task.id)?;

    write_clean_no_commit_verdict(
        assignment,
        "done",
        "canonical evidence and current-tree verification, workspace, and independent-review gates all passed; host persisted a durable closeout receipt without requiring a new worker commit",
    )?;

    Ok(true)
}

/// Host-local run state marking a task as "held" by a host gate (verify or
/// review) because it FAILED that gate and must be re-worked + re-verified
/// before it can be `[x]` again. Lives under `.auto/` (gitignored), so it never
/// commits and is naturally scoped to the canonical checkout.
///
/// This guards a subtle interaction: a task a gate just demoted to `[~]` still
/// has a present-and-fresh worker receipt + review handoff, so it still passes
/// `is_fully_evidenced()`. Without this hold, the evidence-only promotion paths
/// (pre-dispatch self-heal + end-of-run recovery + clean-no-commit reconcile)
/// would re-promote it to `[x]` from that stale evidence on the very next pass,
/// silently undoing the demotion. The hold blocks evidence-only promotion until
/// the task lands cleanly through the full pipeline (which clears it).
fn gate_hold_path(repo_root: &Path, task_id: &str) -> PathBuf {
    repo_root
        .join(".auto/parallel/gate-holds")
        .join(format!("{task_id}.hold"))
}

pub(crate) fn record_gate_hold(repo_root: &Path, task_id: &str, reason: &str) -> Result<()> {
    let path = gate_hold_path(repo_root, task_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed creating gate-hold directory for `{task_id}`"))?;
    }
    atomic_write(&path, reason.as_bytes())
        .with_context(|| format!("failed recording mandatory gate hold for `{task_id}`"))
}

pub(crate) fn clear_gate_hold(repo_root: &Path, task_id: &str) -> Result<()> {
    let path = gate_hold_path(repo_root, task_id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed clearing gate hold for `{task_id}`")),
    }
}

pub(crate) fn task_is_gate_held(repo_root: &Path, task_id: &str) -> Result<bool> {
    Ok(gate_held_task_ids(repo_root)?.contains(task_id))
}

/// Task ids currently carrying a durable gate hold. A gate hold is recorded only
/// on a REAL gate failure (host re-verification failed, workspace regression, or
/// unresolved review findings) and cleared when the task lands cleanly. Tracked
/// REVIEW.md findings are unioned with local holds so a clean clone preserves
/// the dependency block. Any unreadable hold/review state returns an error;
/// callers must abort scheduling rather than treating unknown state as clear.
pub(crate) fn gate_held_task_ids(repo_root: &Path) -> Result<BTreeSet<String>> {
    let dir = repo_root.join(".auto/parallel/gate-holds");
    let mut held = BTreeSet::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.with_context(|| {
                    format!(
                        "failed reading mandatory gate-hold entry in {}",
                        dir.display()
                    )
                })?;
                let name = entry.file_name();
                let name = name.to_str().with_context(|| {
                    format!("gate-hold filename in {} was not UTF-8", dir.display())
                })?;
                if let Some(task_id) = name.strip_suffix(".hold") {
                    held.insert(task_id.to_string());
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed reading mandatory gate holds at {}", dir.display())
            })
        }
    }

    let review_path = repo_root.join("REVIEW.md");
    let review = match std::fs::read_to_string(&review_path) {
        Ok(review) => review,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed reading tracked review holds at {}",
                    review_path.display()
                )
            })
        }
    };
    let plan = read_loop_plan(repo_root)?;
    for task in parse_loop_plan(&plan)
        .tasks
        .into_iter()
        .filter(|task| task.status == LoopTaskStatus::Partial)
    {
        if !unresolved_review_findings_for_task(&review, &task.id).is_empty() {
            held.insert(task.id);
        }
    }
    Ok(held)
}

/// Legacy evidence-only promotion hook. The definition of done now requires
/// current-tree task verification, workspace tests, and standing-review
/// clearance, so canonical evidence alone can no longer flip `[~]` to `[x]`.
/// Returns `false` after inspecting evidence so callers fall through to a real
/// worker/gate pass.
pub(crate) fn promote_task_from_canonical_evidence_no_push(
    repo_root: &Path,
    task_id: &str,
    markdown: &str,
) -> Result<bool> {
    // A task a host gate demoted must NOT be promoted from its (still-present)
    // stale evidence — it has to be re-worked and re-verified first.
    if task_is_gate_held(repo_root, task_id)? {
        return Ok(false);
    }
    let evidence = inspect_task_completion_evidence(repo_root, task_id, markdown);
    if !evidence.is_fully_evidenced() {
        return Ok(false);
    }
    Ok(false)
}

/// Pre-dispatch evidence check for legacy callers. This used to promote
/// already-evidenced `[~]` rows to `[x]`, but the definition of done now
/// requires current-tree task verification, workspace tests, and review
/// clearance. Evidence-only paths therefore fall through to a real worker/gate
/// pass.
pub(crate) fn try_promote_partial_before_dispatch(
    repo_root: &Path,
    target_branch: &str,
    task_id: &str,
    markdown: &str,
    parallel_logger: &ParallelEventLogger,
) -> Result<bool> {
    if !promote_task_from_canonical_evidence_no_push(repo_root, task_id, markdown)? {
        return Ok(false);
    }
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        parallel_logger.info(format!(
            "remote sync: rebased onto origin/{} after pre-dispatch evidence promotion",
            target_branch
        ));
    }
    parallel_logger.info(format!(
        "self-heal: promoted already-evidenced partial `{}` to done before dispatch (no worker needed)",
        task_id
    ));
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
        if promote_task_from_canonical_evidence_no_push(repo_root, &task_id, &markdown)? {
            recovered.push(task_id);
        }
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

/// Re-examine only shelves explicitly classified as landing divergence. These
/// tasks were not rejected by a completion gate, so a passing host re-execution
/// at the current canonical HEAD is sufficient to remove the stale scheduling
/// poison. Conflict and legacy/gate-failure shelves are deliberately ignored.
pub(crate) async fn auto_unshelve_landing_divergence_tasks(
    repo_root: &Path,
    state: &mut ParallelRunState,
    parallel_logger: &ParallelEventLogger,
) -> usize {
    let candidates = state
        .shelved_tasks
        .iter()
        .filter_map(|(task_id, entry)| {
            entry.details().and_then(|details| {
                (details.failure_reason == ShelvedTaskFailureReason::LandingDivergence)
                    .then(|| (task_id.clone(), details.markdown.clone()))
            })
        })
        .collect::<Vec<_>>();
    let current_head = current_repo_head(repo_root).unwrap_or_else(|| "unknown".to_string());
    let mut recovered = 0usize;

    for (task_id, markdown) in candidates {
        parallel_logger.info(format!(
            "resume: re-verifying landing-divergence shelf `{task_id}` against canonical HEAD {current_head}"
        ));
        match run_guarded_lane_verify_gate(
            repo_root,
            &task_id,
            &markdown,
            "landing-divergence unshelve reverify",
            false,
        )
        .await
        {
            Err(err) => {
                parallel_logger.warn(format!(
                    "resume: keeping landing-divergence shelf `{task_id}` because canonical gate \
                     integrity could not be proved: {err:#}"
                ));
            }
            Ok(LaneVerifyOutcome::AllPassed) => {
                state.shelved_tasks.remove(&task_id);
                state.unblock_attempt_counts.remove(&task_id);
                state.attempted_partial_followups.remove(&task_id);
                recovered += 1;
                parallel_logger.info(format!(
                    "auto-unshelve: `{task_id}` was shelved only for landing-divergence; current-HEAD verification passed, so it is dependency-ready again"
                ));
            }
            Ok(LaneVerifyOutcome::Failed { detail }) => {
                parallel_logger.warn(format!(
                    "resume: keeping landing-divergence shelf `{task_id}` because current-HEAD verification failed: {detail}"
                ));
            }
            Ok(LaneVerifyOutcome::Skipped { reason }) => {
                parallel_logger.warn(format!(
                    "resume: keeping landing-divergence shelf `{task_id}` because current-HEAD verification could not produce a pass: {reason}"
                ));
            }
        }
    }

    recovered
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
    let mut task = assignment.task.clone();
    reconcile_parallel_landed_task_state(repo_root, &mut task, changed_files)
}

pub(crate) fn reconcile_parallel_landed_task_state(
    repo_root: &Path,
    task: &mut LoopTask,
    changed_files: &[String],
) -> Result<LoopTaskStatus> {
    let evidence_before = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
    let mut review_evidence = evidence_before.clone();
    review_evidence.has_review_handoff = true;
    let review_added =
        ensure_host_review_handoff(repo_root, &task.id, changed_files, &review_evidence)?;
    let evidence_after = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
    let completion_status = if evidence_after.is_ready_for_definition_of_done_gates() {
        LoopTaskStatus::Done
    } else {
        LoopTaskStatus::Partial
    };

    task.status = completion_status;
    let plan_updated =
        update_reconciled_task_completion_in_plan(repo_root, task, completion_status)?;
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
    expected_status: LoopTaskStatus,
    message: &str,
    allow_empty: bool,
) -> Result<()> {
    enforce_review_input_quarantine_before_dispatch(repo_root)?;
    let derived_files = run_after_plan_update_hook(repo_root)?;
    require_task_status_persisted(repo_root, task_id, expected_status)?;
    if expected_status == LoopTaskStatus::Done {
        refuse_unsealed_task_completion_transitions_except(repo_root, task_id)?;
    } else {
        refuse_unsealed_task_completion_checkpoint(repo_root)?;
    }
    let mut queue_files = host_queue_state_files_for_repo(repo_root)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    queue_files.extend(derived_files);
    queue_files.sort();
    queue_files.dedup();
    let allowed_paths = queue_files.iter().map(String::as_str).collect::<Vec<_>>();
    refuse_worktree_paths_outside(repo_root, &allowed_paths, "task closeout")?;
    let validated_tree = (expected_status == LoopTaskStatus::Done)
        .then(|| {
            capture_validated_task_closeout_tree(repo_root, task_id, &allowed_paths, allow_empty)
        })
        .transpose()?;
    let footer = verification_receipt_commit_footer(repo_root, task_id)?;
    if expected_status == LoopTaskStatus::Done && footer.is_none() {
        bail!(
            "refusing Done closeout for `{task_id}` without a durable verification receipt footer"
        );
    }
    let exact_message = match footer {
        Some(footer) => format!("{message}\n\n{footer}"),
        None => message.to_string(),
    };
    if expected_status == LoopTaskStatus::Done {
        commit_validated_task_closeout_tree_cas(
            repo_root,
            validated_tree.context("Done closeout lost its validated candidate tree")?,
            &exact_message,
        )?;
    } else {
        commit_staged_queue_checkpoint_cas(repo_root, &exact_message, allow_empty)?;
    }
    Ok(())
}

/// Run the repository-owned derived-state refresh after the host changes
/// IMPLEMENTATION_PLAN.md. The optional hook prints one tracked repository path
/// per line; only those paths are staged and admitted to the queue closeout
/// authority set. This keeps hash-bound manifests consistent without granting
/// the hook permission to sweep arbitrary source changes into a host commit.
pub(crate) fn run_after_plan_update_hook(repo_root: &Path) -> Result<Vec<String>> {
    let hook = repo_root.join("scripts/autodev-after-plan-update.sh");
    if !hook.is_file() {
        return Ok(Vec::new());
    }
    let plan_diff = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["diff", "--quiet", "HEAD", "--", "IMPLEMENTATION_PLAN.md"])
        .output()
        .with_context(|| format!("failed checking plan changes in {}", repo_root.display()))?;
    match plan_diff.status.code() {
        Some(0) => return Ok(Vec::new()),
        Some(1) => {}
        _ => bail!(
            "failed checking whether IMPLEMENTATION_PLAN.md changed: {}",
            String::from_utf8_lossy(&plan_diff.stderr).trim()
        ),
    }

    let output = Command::new("bash")
        .arg(&hook)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("failed to launch {}", hook.display()))?;
    if !output.status.success() {
        bail!(
            "post-plan-update hook {} failed with status {}:\n{}\n{}",
            hook.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8(output.stdout)
        .context("post-plan-update hook output was not valid UTF-8")?;
    let mut paths = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    for path in &paths {
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            bail!("post-plan-update hook reported unsafe path `{path}`");
        }
        let tracked = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["ls-files", "--error-unmatch", "--", path])
            .output()
            .with_context(|| format!("failed checking hook output path `{path}`"))?;
        if !tracked.status.success() {
            bail!("post-plan-update hook reported untracked path `{path}`");
        }
    }
    if !paths.is_empty() {
        let mut add_args = vec!["add", "-u", "--"];
        add_args.extend(paths.iter().map(String::as_str));
        run_git(repo_root, add_args)?;
    }
    Ok(paths)
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
        Err(err) => Ok(LaneLandingRecoveryPrep::NeedsWorkerResolution {
            recovery_note: prepared_landing_recovery_note(
                target_branch,
                landing_error,
                &format!("{err:#}"),
            ),
            conflict_paths: unmerged_conflict_paths(&assignment.lane_repo_root),
        }),
    }
}

pub(crate) fn cherry_pick_lane_range(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
    failure_policy: CherryPickFailurePolicy,
) -> Result<()> {
    if let Some(commit) =
        lane_range_reserved_verification_receipt_commit(repo_root, base_commit, head_ref)?
    {
        bail!(
            "refusing incoming lane commit {commit}: reserved verification receipt trailers may only be minted by canonical host closeout commits"
        );
    }
    let changed_files = lane_changed_files(repo_root, base_commit, head_ref)?;
    if let Some(path) = changed_files
        .iter()
        .find(|path| HOST_QUEUE_STATE_FILES.contains(&path.as_str()))
    {
        bail!("refusing incoming lane range that modifies host-owned queue path `{path}`");
    }
    if changed_files.is_empty() {
        return Ok(());
    }

    scrub_parallel_receipt_staging(repo_root)?;
    // A dirty canonical worktree (e.g. regenerated evidence/report artifacts left
    // by an out-of-band verification) makes `git cherry-pick` refuse with "local
    // changes would be overwritten by merge", which shelves the task even though
    // the lane commit is authoritative for the task range. With
    // AUTO_PARALLEL_LANDING_AUTOSTASH=1, stash the dirty state before the
    // cherry-pick; on success drop it (the lane output supersedes), on failure
    // restore it so nothing is lost.
    let autostash = landing_autostash_requested() && canonical_worktree_is_dirty(repo_root)?;
    if autostash {
        run_git(
            repo_root,
            [
                "stash",
                "push",
                "--include-untracked",
                "-m",
                "auto-parallel landing autostash",
            ],
        )?;
    }
    let range = format!("{base_commit}..{head_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cherry-pick")
        .arg(git_cherry_pick_empty_arg())
        .arg(&range)
        .output()
        .with_context(|| format!("failed to cherry-pick {range} in {}", repo_root.display()))?;
    if output.status.success() {
        if autostash {
            // Lane commit is authoritative for these paths; discard the stashed
            // pre-landing dirty state rather than re-conflicting on pop.
            let _ = run_git(repo_root, ["stash", "drop"]);
        }
        return Ok(());
    }

    if failure_policy == CherryPickFailurePolicy::Abort {
        let _ = run_git(repo_root, ["cherry-pick", "--abort"]);
    }
    if autostash {
        let _ = run_git(repo_root, ["stash", "pop"]);
    }
    bail!(
        "git cherry-pick failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

pub(crate) fn unmerged_conflict_paths(repo_root: &Path) -> Vec<String> {
    let Ok(paths) = git_stdout(repo_root, ["diff", "--name-only", "--diff-filter=U"]) else {
        return Vec::new();
    };
    let mut paths = paths
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

/// Remote branch movement can race the post-landing push even after the lane
/// commits were integrated locally. Each retry goes through
/// `push_branch_with_remote_sync`, which fetches/rebases onto a fresh remote
/// branch before pushing. Real rebase conflicts are not considered retryable.
pub(crate) fn push_parallel_landing_with_divergence_retries(
    repo_root: &Path,
    target_branch: &str,
    assignment: &ActiveLaneAssignment,
) -> Result<bool> {
    let mut retries = 0usize;
    loop {
        match push_branch_with_remote_sync(repo_root, target_branch) {
            Ok(synced) => return Ok(synced),
            Err(err)
                if landing_error_suggests_retryable_divergence(&err)
                    && retries < LANDING_PUSH_RETRY_LIMIT =>
            {
                retries += 1;
                let event = format!(
                    "landing-push-retry: canonical remote moved; fetched/rebased fresh HEAD and retrying push ({retries}/{LANDING_PUSH_RETRY_LIMIT}): {err:#}"
                );
                eprintln!(
                    "lane-{} `{}`: {event}",
                    assignment.lane_index, assignment.task.id
                );
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    &event,
                );
            }
            Err(err) => return Err(err),
        }
    }
}

pub(crate) fn landing_error_suggests_retryable_divergence(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    (message.contains("non-fast-forward")
        || message.contains("fetch first")
        || message.contains("failed to push some refs")
        || message.contains("cannot lock ref")
        || message.contains("stale info")
        || message.contains("failed to update ref"))
        && !message.contains("aborted conflicted rebase")
        && !message.contains("merge conflict")
        && !message.contains("could not apply")
}

/// Best-effort extraction for conflicts reported by a failed remote rebase.
/// The normal lane recovery path reads unmerged paths directly from Git; this
/// parser covers errors whose rebase was already aborted by the sync helper.
pub(crate) fn conflict_paths_from_landing_error(err: &anyhow::Error) -> Vec<String> {
    let message = format!("{err:#}");
    let mut paths = Vec::new();
    for line in message.lines() {
        let trimmed = line.trim();
        if let Some((_, path)) = trimmed.rsplit_once("Merge conflict in ") {
            let path = path.trim();
            if !path.is_empty() {
                paths.push(path.to_string());
            }
        } else if let Some(path) = trimmed.strip_prefix("CONFLICT ") {
            if let Some((_, path)) = path.rsplit_once(": ") {
                let path = path.trim();
                if !path.is_empty() {
                    paths.push(path.to_string());
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// Whether `AUTO_PARALLEL_LANDING_AUTOSTASH=1` is set (opt-in dirty-tree landing).
fn landing_autostash_requested() -> bool {
    std::env::var("AUTO_PARALLEL_LANDING_AUTOSTASH")
        .ok()
        .as_deref()
        == Some("1")
}

/// True when the canonical worktree has uncommitted (tracked or untracked) changes.
fn canonical_worktree_is_dirty(repo_root: &Path) -> Result<bool> {
    Ok(!git_stdout(repo_root, ["status", "--porcelain"])?
        .trim()
        .is_empty())
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
    // Log exactly what the `git clean` will remove (dry-run preview) BEFORE
    // deleting, so any swept file is diagnosable from the run log instead of
    // vanishing silently. This sweep is deliberately path-scoped to the
    // host-owned receipts dir; the preview makes that scope auditable and would
    // make an accidental over-broad deletion visible rather than mysterious.
    if let Ok(preview) = git_stdout(
        repo_root,
        ["clean", "-nd", "--", ".auto/symphony/verification-receipts"],
    ) {
        let removals: Vec<&str> = preview.lines().filter(|l| !l.trim().is_empty()).collect();
        if !removals.is_empty() {
            eprintln!(
                "parallel receipt-scrub: git clean will remove {} untracked path(s) under .auto/symphony/verification-receipts:\n{}",
                removals.len(),
                removals.join("\n")
            );
        }
    }
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
    use std::os::unix::fs::PermissionsExt;
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

    fn write_fake_clean_reviewer(root: &Path) -> PathBuf {
        let path = root.join("fake-codex-clean.sh");
        fs::write(
            &path,
            r#"#!/bin/sh
prompt=$(cat)
report=$(printf '%s\n' "$prompt" | sed -n 's/^Write your report to `\([^`]*\)` as markdown.*$/\1/p' | tail -n 1)
[ -n "$report" ] || exit 2
mkdir -p "$(dirname "$report")"
printf 'VERDICT: CLEAN\n\n## Summary\nCurrent canonical tree and standing findings were independently reviewed.\n' > "$report"
"#,
        )
        .expect("write fake reviewer");
        let mut permissions = fs::metadata(&path)
            .expect("stat fake reviewer")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod fake reviewer");
        path
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

    #[derive(Clone, Copy)]
    enum GateHeldDonePlanView {
        Worktree,
        Index,
    }

    fn init_gate_held_done_checkpoint_repo(
        name: &str,
        view: GateHeldDonePlanView,
    ) -> (PathBuf, PathBuf, String) {
        let (root, _remote, _upstream, worker) = init_remote_and_clones(name, "trunk");
        let task_id = "TASK-GATE-HELD".to_string();
        let partial_plan =
            format!("# IMPLEMENTATION_PLAN\n\n- [~] `{task_id}` gate-held checkpoint task\n");
        let done_plan =
            format!("# IMPLEMENTATION_PLAN\n\n- [x] `{task_id}` gate-held checkpoint task\n");

        fs::write(worker.join("IMPLEMENTATION_PLAN.md"), &partial_plan)
            .expect("failed to write partial plan");
        run_git_in(&worker, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&worker, ["commit", "-m", "seed partial plan"]);
        run_git_in(&worker, ["push", "origin", "trunk"]);
        record_gate_hold(&worker, &task_id, "host gate failed").expect("record gate hold");

        match view {
            GateHeldDonePlanView::Worktree => {
                fs::write(worker.join("IMPLEMENTATION_PLAN.md"), &done_plan)
                    .expect("failed to write done worktree plan");
            }
            GateHeldDonePlanView::Index => {
                fs::write(worker.join("IMPLEMENTATION_PLAN.md"), &done_plan)
                    .expect("failed to write done index plan");
                run_git_in(&worker, ["add", "IMPLEMENTATION_PLAN.md"]);
                fs::write(worker.join("IMPLEMENTATION_PLAN.md"), &partial_plan)
                    .expect("failed to restore partial worktree plan");
            }
        }

        (root, worker, task_id)
    }

    #[test]
    fn startup_recovery_demotes_unsealed_done_in_both_plan_views() {
        for (label, view) in [
            ("worktree", GateHeldDonePlanView::Worktree),
            ("index", GateHeldDonePlanView::Index),
        ] {
            let (root, worker, task_id) = init_gate_held_done_checkpoint_repo(
                &format!("parallel-recover-unsealed-{label}"),
                view,
            );
            clear_gate_hold(&worker, &task_id).expect("clear gate hold");
            let head = git_output(&worker, ["rev-parse", "HEAD"]);

            let recovered = recover_unsealed_task_completion_transitions(&worker)
                .expect("startup recovery should demote the unsealed transition");

            assert_eq!(recovered, vec![task_id.clone()]);
            let worktree =
                fs::read_to_string(worker.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
            let indexed = run_git_in(&worker, ["show", ":IMPLEMENTATION_PLAN.md"]);
            assert!(worktree.contains(&format!("- [~] `{task_id}`")));
            assert!(indexed.contains(&format!("- [~] `{task_id}`")));
            assert_eq!(head, git_output(&worker, ["rev-parse", "HEAD"]));
            assert!(
                super::gate_hold_path(&worker, &task_id).is_file(),
                "recovered task must remain held until full gates rerun"
            );

            fs::remove_dir_all(root).expect("cleanup");
        }
    }

    #[test]
    fn startup_recovery_demotes_same_id_completed_contract_mutation() {
        let repo = unique_temp_dir("parallel-recover-mutated-done-contract");
        init_git_repo(&repo);
        let original = "\
- [x] `TASK-CLOSED` Completed contract
  Verification: `cargo test original_contract`
  Dependencies: none
";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), original).expect("write original plan");
        git_ok(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&repo, ["commit", "-q", "-m", "seed completed contract"]);
        let head = git_output(&repo, ["rev-parse", "HEAD"]);

        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            original.replace("original_contract", "mutated_contract"),
        )
        .expect("write mutated completed contract");
        git_ok(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);

        let recovered = recover_unsealed_task_completion_transitions(&repo)
            .expect("same-id contract mutation should recover fail-closed");

        assert_eq!(recovered, vec!["TASK-CLOSED".to_string()]);
        let worktree = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        let indexed = run_git_in(&repo, ["show", ":IMPLEMENTATION_PLAN.md"]);
        assert!(worktree.contains("- [~] `TASK-CLOSED`"));
        assert!(indexed.contains("- [~] `TASK-CLOSED`"));
        assert!(worktree.contains("mutated_contract"));
        assert_eq!(head, git_output(&repo, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(repo).expect("cleanup");
    }

    #[test]
    fn tracked_review_hold_blocks_dependents_in_a_clean_clone() {
        let repo = unique_temp_dir("parallel-tracked-review-hold");
        init_git_repo(&repo);
        let plan = "\
# IMPLEMENTATION_PLAN

- [~] `TASK-A` Failed prerequisite
  Dependencies: none

- [ ] `TASK-C` Dependent must wait
  Dependencies: `TASK-A`
";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("write plan");
        fs::write(
            repo.join("REVIEW.md"),
            "# REVIEW\n\n## `TASK-A`: host re-execution verification failed\n\
- Source: host verification gate (held at `[~]`).\n\n\
Current-tree verification failed.\n",
        )
        .expect("write tracked hold");
        git_ok(&repo, ["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"]);
        git_ok(&repo, ["commit", "-q", "-m", "seed tracked gate hold"]);
        assert!(
            !repo.join(".auto/parallel/gate-holds").exists(),
            "fixture must model a clean clone without host-local state"
        );

        let held = gate_held_task_ids(&repo).expect("tracked holds should be readable");
        assert!(held.contains("TASK-A"), "{held:?}");
        let snapshot = parse_loop_plan(plan);
        let ready = snapshot.ready_tasks_with_gate_holds(&BTreeSet::new(), &held);
        assert!(
            ready.iter().all(|task| task.id != "TASK-C"),
            "tracked REVIEW hold must block TASK-C in a clean clone: {ready:#?}"
        );

        fs::remove_dir_all(repo).expect("cleanup");
    }

    #[test]
    fn unreadable_gate_hold_state_aborts_dependency_scheduling() {
        let repo = unique_temp_dir("parallel-unreadable-gate-hold");
        init_git_repo(&repo);
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-A` Failed prerequisite\n  Dependencies: none\n\n\
             - [ ] `TASK-C` Dependent must wait\n  Dependencies: `TASK-A`\n",
        )
        .expect("write plan");
        git_ok(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&repo, ["commit", "-q", "-m", "seed held dependency"]);
        fs::create_dir_all(repo.join(".auto/parallel")).expect("create parallel runtime");
        fs::write(repo.join(".auto/parallel/gate-holds"), "not a directory\n")
            .expect("replace hold directory with unreadable state");

        let write_error = record_gate_hold(&repo, "TASK-A", "verification failed")
            .expect_err("hold persistence failure must be explicit");
        assert!(
            format!("{write_error:#}").contains("gate-hold"),
            "{write_error:#}"
        );
        let read_error = gate_held_task_ids(&repo)
            .expect_err("scheduler must abort instead of treating unknown holds as clear");
        assert!(
            format!("{read_error:#}").contains("gate holds"),
            "{read_error:#}"
        );

        fs::remove_dir_all(repo).expect("cleanup");
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
    fn propagate_lane_receipts_refreshes_plan_hash_to_canonical() {
        fn sha256_hex_local(bytes: &[u8]) -> String {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(bytes))
        }

        let root = unique_temp_dir("parallel-propagate-plan-hash");
        let lane = root.join("lane");
        let canonical = root.join("canonical");
        init_git_repo(&lane);
        init_git_repo(&canonical);

        // Canonical carries the plan AFTER the host flipped TASK-1's checkbox on
        // landing — exactly the mutation that makes a worker's recorded full-file
        // plan hash stale through no fault of its own.
        let canonical_plan = "# Plan\n- [x] `TASK-1` done now\n";
        fs::write(canonical.join("IMPLEMENTATION_PLAN.md"), canonical_plan)
            .expect("failed to write canonical plan");
        git_ok(&canonical, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&canonical, ["commit", "-m", "canonical plan"]);

        // The lane recorded its receipt against the OLD plan text, so its
        // plan_hash can never match canonical's current hash.
        let receipt_dir = lane.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("failed to create lane receipt dir");
        let stale = serde_json::json!({
            "commit": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "plan_hash": sha256_hex_local(b"# Plan\n- [ ] `TASK-1` not yet\n"),
            "dirty_state": { "fingerprint": "stale" },
            "commands": [],
            "declared_artifacts": [],
        });
        fs::write(
            receipt_dir.join("TASK-1.json"),
            serde_json::to_string_pretty(&stale).expect("serialize stale receipt"),
        )
        .expect("failed to write lane receipt");

        propagate_lane_receipts(&lane, &canonical, "TASK-1", "- [x] `TASK-1` done now\n")
            .expect("propagate should succeed");

        let propagated: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(canonical.join(".auto/symphony/verification-receipts/TASK-1.json"))
                .expect("failed to read canonical receipt"),
        )
        .expect("failed to parse canonical receipt");

        // The refreshed hash is taken over the plan with status markers
        // normalized (`[x]` -> `[ ]`), so it equals the hash of the normalized
        // canonical plan, not the raw bytes.
        let normalized_canonical = canonical_plan.replace("[x]", "[ ]");
        assert_eq!(
            propagated["plan_hash"].as_str(),
            Some(sha256_hex_local(normalized_canonical.as_bytes()).as_str()),
            "plan_hash must be refreshed to canonical's normalized IMPLEMENTATION_PLAN.md"
        );
        // The existing commit refresh must still hold (the two must stay in sync,
        // since the plan-hash gate only runs once the commit reads as current).
        let head = git_output(&canonical, ["rev-parse", "HEAD"]);
        assert_eq!(propagated["commit"].as_str(), Some(head.as_str()));

        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn propagate_lane_receipts_does_not_replace_valid_canonical_evidence_with_degraded_lane_receipt(
    ) {
        let root = unique_temp_dir("parallel-propagate-preserve-valid");
        let lane = root.join("lane");
        let canonical = root.join("canonical");
        init_git_repo(&lane);
        init_git_repo(&canonical);

        let task_markdown = "- [~] `TASK-1` Preserve valid evidence\n\
Verification:\n\
  - `cargo test -p demo task_1`\n\
Dependencies: none\n";
        fs::write(
            canonical.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("write canonical plan");
        fs::create_dir_all(canonical.join("scripts")).expect("create scripts directory");
        fs::write(
            canonical.join("scripts/run-task-verification.sh"),
            "#!/bin/sh\n",
        )
        .expect("write verification wrapper");
        fs::write(canonical.join(".gitignore"), ".auto/\n").expect("write gitignore");
        git_ok(&canonical, ["add", "."]);
        git_ok(&canonical, ["commit", "-m", "seed canonical task"]);
        git_ok(
            &canonical,
            ["commit", "--allow-empty", "-m", "advance canonical head"],
        );

        let canonical_receipt_dir = canonical.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&canonical_receipt_dir).expect("create canonical receipt dir");
        let canonical_receipt = serde_json::json!({
            "task_id": "TASK-1",
            "commit": "stale-before-host-refresh",
            "commands": [{
                "command": "cargo test -p demo task_1",
                "argv": ["cargo", "test", "-p", "demo", "task_1"],
                "expected_argv": ["cargo", "test", "-p", "demo", "task_1"],
                "exit_code": 0,
                "status": "passed"
            }]
        });
        let canonical_receipt_path = canonical_receipt_dir.join("TASK-1.json");
        fs::write(
            &canonical_receipt_path,
            serde_json::to_string_pretty(&canonical_receipt).expect("serialize canonical receipt"),
        )
        .expect("write canonical receipt");
        propagate_lane_receipts(&canonical, &canonical, "TASK-1", task_markdown)
            .expect("canonical receipt metadata should refresh");
        let before = fs::read(&canonical_receipt_path).expect("read canonical receipt");

        let lane_receipt_dir = lane.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&lane_receipt_dir).expect("create lane receipt dir");
        fs::write(
            lane_receipt_dir.join("TASK-1.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "task_id": "TASK-1",
                "commit": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "commands": []
            }))
            .expect("serialize degraded lane receipt"),
        )
        .expect("write degraded lane receipt");

        propagate_lane_receipts(&lane, &canonical, "TASK-1", task_markdown)
            .expect("propagate should succeed");

        assert_eq!(
            fs::read(&canonical_receipt_path).expect("read receipt after propagation"),
            before,
            "a lane receipt that loses valid command evidence must not replace canonical proof"
        );

        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn promote_from_canonical_evidence_refuses_fully_evidenced_partial_without_gates() {
        let repo = unique_temp_dir("promote-evidence-done");
        init_git_repo(&repo);

        // Canonical evidence alone used to promote this already-evidenced
        // follow-up. The definition of done now requires current-tree gates, so
        // the evidence-only self-heal must leave it [~].
        let task_markdown = "- [~] `TASK-1` Already complete follow-up\n\nNo verification commands and no completion artifacts to chase.\n";
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("failed to write plan");
        fs::write(
            repo.join("REVIEW.md"),
            "# Review\n\n## `TASK-1`\n- Source: test handoff.\n- Remaining blockers: none.\n",
        )
        .expect("failed to write review");
        git_ok(&repo, ["add", "."]);
        git_ok(&repo, ["commit", "-m", "seed plan + review"]);
        let head_before = git_output(&repo, ["rev-parse", "HEAD"]);

        let promoted = promote_task_from_canonical_evidence_no_push(&repo, "TASK-1", task_markdown)
            .expect("promotion check should not error");
        assert!(
            !promoted,
            "fully-evidenced partial must not promote without DoD gates"
        );

        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(
            plan.contains("- [~] `TASK-1`"),
            "plan checkbox must stay [~], got:\n{plan}"
        );
        let head_after = git_output(&repo, ["rev-parse", "HEAD"]);
        assert_eq!(
            head_before, head_after,
            "no closeout commit should be created"
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn promote_from_canonical_evidence_refuses_gate_held_task() {
        let repo = unique_temp_dir("promote-evidence-gate-held");
        init_git_repo(&repo);

        // Same fully-evidenced shape as the promote-success test...
        let task_markdown = "- [~] `TASK-1` Demoted by a host gate\n\nNo verification commands.\n";
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(
            repo.join(".gitignore"),
            ".auto/\nlane-clean-no-commit/\nlane-repo/\nparallel-run/\n",
        )
        .expect("write gitignore");
        fs::write(
            repo.join("REVIEW.md"),
            "# Review\n\n## `TASK-1`\n- Source: test handoff.\n",
        )
        .expect("write review");
        git_ok(&repo, ["add", "."]);
        git_ok(&repo, ["commit", "-m", "seed"]);
        let head_before = git_output(&repo, ["rev-parse", "HEAD"]);

        // ...but a host gate has demoted it. Evidence-only promotion must refuse,
        // so the verify/review gate's demotion isn't silently undone.
        record_gate_hold(&repo, "TASK-1", "host re-execution verification failed")
            .expect("record hold");
        let promoted = promote_task_from_canonical_evidence_no_push(&repo, "TASK-1", task_markdown)
            .expect("promotion check should not error");
        assert!(
            !promoted,
            "gate-held task must not be promoted from evidence"
        );
        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(plan.contains("- [~] `TASK-1`"), "plan stays [~]: {plan}");
        assert_eq!(head_before, git_output(&repo, ["rev-parse", "HEAD"]));

        // Clearing the old local hold is still not enough; [x] requires the
        // current-tree verify, workspace-test, and review gates.
        clear_gate_hold(&repo, "TASK-1").expect("clear hold");
        assert!(!task_is_gate_held(&repo, "TASK-1").expect("read holds"));
        let promoted = promote_task_from_canonical_evidence_no_push(&repo, "TASK-1", task_markdown)
            .expect("promotion should not error after clear");
        assert!(
            !promoted,
            "task still must not promote from evidence after clearing hold"
        );

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[tokio::test]
    async fn clean_no_commit_reconciles_fully_evidenced_pending_task() {
        let repo = unique_temp_dir("parallel-clean-no-commit-evidenced");
        init_git_repo(&repo);
        let task_markdown = "- [ ] `TASK-006` Evidence already landed\nVerification:\n  - `cargo test -p demo task_006`\nDependencies: none\n";
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(
            repo.join(".gitignore"),
            ".auto/\nlane-clean-no-commit/\nlane-repo/\nparallel-run/\n",
        )
        .expect("write gitignore");
        fs::create_dir_all(repo.join("scripts")).expect("create scripts");
        let wrapper = repo.join("scripts/run-task-verification.sh");
        fs::write(&wrapper, "#!/bin/sh\nexit 0\n").expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        fs::write(
            repo.join("REVIEW.md"),
            "# Review\n\n## `TASK-006`\n- Source: test handoff.\n\
- Remaining blockers: none.\n\n\
## `TASK-006`: independent review findings\n\
- Source: stale test finding.\n\n\
1. Recheck the current tree before clearing this stale finding.\n",
        )
        .expect("write review");
        let fake_reviewer = write_fake_clean_reviewer(&repo);
        run_git_in(&repo, ["add", "."]);
        run_git_in(&repo, ["commit", "-m", "seed task"]);
        let receipt_commit = git_output(&repo, ["rev-parse", "HEAD"]);
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::write(
            repo.join(".auto/symphony/verification-receipts/TASK-006.json"),
            format!(
                r#"{{"task_id":"TASK-006","commit":"{receipt_commit}","commands":[{{"command":"cargo test -p demo task_006","argv":["cargo","test","-p","demo","task_006"],"expected_argv":["cargo","test","-p","demo","task_006"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write receipt");
        propagate_lane_receipts(&repo, &repo, "TASK-006", task_markdown)
            .expect("refresh canonical receipt metadata");

        let lane_root = repo.join("lane-clean-no-commit");
        fs::create_dir_all(&lane_root).expect("create lane root");
        let run_root = repo.join(".auto/parallel-run");
        fs::create_dir_all(&run_root).expect("create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let mut assignment = ActiveLaneAssignment {
            lane_index: 1,
            attempts: 1,
            task: LoopTask {
                id: "TASK-006".to_string(),
                title: "Evidence already landed".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: task_markdown.to_string(),
            },
            resumed: false,
            lane_root: lane_root.clone(),
            lane_repo_root: repo.join("lane-repo"),
            base_commit: receipt_commit,
            stdout_log_path: lane_root.join("stdout.log"),
            stderr_log_path: lane_root.join("stderr.log"),
            worker_pid_path: lane_root.join("worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };
        let review_config = LaneReviewConfig {
            model: "unused".to_string(),
            reasoning_effort: "unused".to_string(),
            codex_bin: fake_reviewer,
        };
        record_gate_hold(&repo, "TASK-006", "standing review must be re-adjudicated")
            .expect("record gate hold");

        let reconciled = reconcile_parallel_clean_no_commit(
            &repo,
            "main",
            &mut assignment,
            &logger,
            &review_config,
        )
        .await
        .expect("clean no-commit reconciliation should succeed");

        assert!(reconciled, "fully evidenced pending task should reconcile");
        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(
            plan.contains("- [x] `TASK-006` Evidence already landed"),
            "plan should advance to [x], got:\n{plan}"
        );
        let staged = run_git_in(&repo, ["diff", "--cached", "--name-only"]);
        assert!(
            staged.trim().is_empty(),
            "host closeout should commit queue state before reporting success: {staged}"
        );
        let closeout_message = run_git_in(&repo, ["log", "-1", "--pretty=%B"]);
        assert!(
            closeout_message
                .lines()
                .next()
                .is_some_and(|subject| subject.ends_with(": TASK-006 queue sync")),
            "closeout must use the host queue-sync subject: {closeout_message}"
        );
        assert!(
            closeout_message.contains("Auto-Verification-Receipt-Task: TASK-006"),
            "closeout must carry the durable task receipt: {closeout_message}"
        );
        let verdict = fs::read_to_string(lane_root.join("clean-no-commit-verdict.json"))
            .expect("read verdict");
        assert!(verdict.contains("\"verdict\": \"done\""), "{verdict}");
        assert!(
            !task_is_gate_held(&repo, "TASK-006").expect("read holds"),
            "the complete current-tree gate pipeline must clear the prior hold"
        );

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[tokio::test]
    async fn clean_no_commit_reconciles_from_lane_local_receipt() {
        let repo = unique_temp_dir("parallel-clean-no-commit-lane-receipt");
        init_git_repo(&repo);
        let task_markdown = "- [ ] `TASK-007` Already implemented\nVerification:\n  - `cargo test -p demo task_007`\nDependencies: none\n";
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(
            repo.join(".gitignore"),
            ".auto/\nlane-clean-no-commit/\nlane-repo/\nparallel-run/\n",
        )
        .expect("write gitignore");
        fs::create_dir_all(repo.join("scripts")).expect("create scripts");
        let wrapper = repo.join("scripts/run-task-verification.sh");
        fs::write(&wrapper, "#!/bin/sh\nexit 0\n").expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        fs::write(
            repo.join("REVIEW.md"),
            "# Review\n\n## `TASK-007`: host re-execution verification failed\n- Source: stale host gate.\n- The host re-ran this task's declared verification command(s) at canonical HEAD and one FAILED.\n\n```\nprevious stale failure\n```\n\n## `TASK-007`\n- Source: test handoff.\n- Remaining blockers: none.\n",
        )
        .expect("write review");
        let fake_reviewer = write_fake_clean_reviewer(&repo);
        run_git_in(&repo, ["add", "."]);
        run_git_in(&repo, ["commit", "-m", "seed task"]);
        let base_commit = git_output(&repo, ["rev-parse", "HEAD"]);

        let lane_repo = repo.join("lane-repo");
        fs::create_dir_all(lane_repo.join(".auto/symphony/verification-receipts"))
            .expect("create lane receipt dir");
        fs::write(
            lane_repo.join(".auto/symphony/verification-receipts/TASK-007.json"),
            format!(
                r#"{{"task_id":"TASK-007","commit":"{base_commit}","commands":[{{"command":"cargo test -p demo task_007","argv":["cargo","test","-p","demo","task_007"],"expected_argv":["cargo","test","-p","demo","task_007"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write lane receipt");

        let lane_root = repo.join("lane-clean-no-commit");
        fs::create_dir_all(&lane_root).expect("create lane root");
        let run_root = repo.join("parallel-run");
        fs::create_dir_all(&run_root).expect("create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let mut assignment = ActiveLaneAssignment {
            lane_index: 1,
            attempts: 1,
            task: LoopTask {
                id: "TASK-007".to_string(),
                title: "Already implemented".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: task_markdown.to_string(),
            },
            resumed: false,
            lane_root: lane_root.clone(),
            lane_repo_root: lane_repo,
            base_commit,
            stdout_log_path: lane_root.join("stdout.log"),
            stderr_log_path: lane_root.join("stderr.log"),
            worker_pid_path: lane_root.join("worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };
        let review_config = LaneReviewConfig {
            model: "unused".to_string(),
            reasoning_effort: "unused".to_string(),
            codex_bin: fake_reviewer,
        };

        let reconciled = reconcile_parallel_clean_no_commit(
            &repo,
            "main",
            &mut assignment,
            &logger,
            &review_config,
        )
        .await
        .expect("lane-local receipt should be propagated and reconciled");

        assert!(reconciled, "lane-local receipt should reconcile the task");
        assert!(
            repo.join(".auto/symphony/verification-receipts/TASK-007.json")
                .is_file(),
            "lane receipt should be copied to canonical"
        );
        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(
            plan.contains("- [x] `TASK-007` Already implemented"),
            "plan should advance to [x], got:\n{plan}"
        );
        let verdict = fs::read_to_string(lane_root.join("clean-no-commit-verdict.json"))
            .expect("read verdict");
        assert!(verdict.contains("\"verdict\": \"done\""), "{verdict}");

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[tokio::test]
    async fn clean_no_commit_direct_pass_without_receipt_stays_partial() {
        let repo = unique_temp_dir("parallel-clean-no-commit-no-receipt");
        init_git_repo(&repo);
        let task_markdown = "- [ ] `TASK-007A` Direct pass is not durable\n\
Verification:\n\
  - Run `bash -c true`\n\
Dependencies: none\n";
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(
            repo.join(".gitignore"),
            ".auto/\nlane-clean-no-commit/\nlane-repo/\nparallel-run/\n",
        )
        .expect("write gitignore");
        fs::write(
            repo.join("REVIEW.md"),
            "# Review\n\n## `TASK-007A`\n- Source: test handoff.\n- Remaining blockers: none.\n",
        )
        .expect("write review");
        run_git_in(&repo, ["add", "."]);
        run_git_in(&repo, ["commit", "-m", "seed task"]);
        let base_commit = git_output(&repo, ["rev-parse", "HEAD"]);

        let lane_root = repo.join("lane-clean-no-commit");
        fs::create_dir_all(&lane_root).expect("create lane root");
        let run_root = repo.join("parallel-run");
        fs::create_dir_all(&run_root).expect("create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("initialize logger");
        let mut assignment = ActiveLaneAssignment {
            lane_index: 2,
            attempts: 1,
            task: LoopTask {
                id: "TASK-007A".to_string(),
                title: "Direct pass is not durable".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: task_markdown.to_string(),
            },
            resumed: false,
            lane_root: lane_root.clone(),
            lane_repo_root: repo.join("lane-repo"),
            base_commit: base_commit.clone(),
            stdout_log_path: lane_root.join("stdout.log"),
            stderr_log_path: lane_root.join("stderr.log"),
            worker_pid_path: lane_root.join("worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };
        let review_config = LaneReviewConfig {
            model: "unused".to_string(),
            reasoning_effort: "unused".to_string(),
            codex_bin: write_fake_clean_reviewer(&repo),
        };

        let reconciled = reconcile_parallel_clean_no_commit(
            &repo,
            "main",
            &mut assignment,
            &logger,
            &review_config,
        )
        .await
        .expect("missing durable receipt should be a bounded hold");

        assert!(
            !reconciled,
            "a direct host command pass must not replace durable receipt evidence"
        );
        assert_eq!(base_commit, git_output(&repo, ["rev-parse", "HEAD"]));
        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(
            plan.contains("- [~] `TASK-007A` Direct pass is not durable"),
            "missing receipt must demote the task: {plan}"
        );
        let staged = run_git_in(&repo, ["diff", "--cached", "--name-only"]);
        assert!(
            staged.contains("IMPLEMENTATION_PLAN.md"),
            "the safe partial state must be staged: {staged}"
        );
        let verdict = fs::read_to_string(lane_root.join("clean-no-commit-verdict.json"))
            .expect("read verdict");
        assert!(
            verdict.contains("\"verdict\": \"landed-unverified\"") && verdict.contains("receipt"),
            "{verdict}"
        );

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[tokio::test]
    async fn clean_no_commit_does_not_close_when_current_tree_verification_fails() {
        let repo = unique_temp_dir("parallel-clean-no-commit-red-verify");
        init_git_repo(&repo);
        let task_markdown = "- [ ] `TASK-008` Evidence must be rechecked\n\
Verification:\n\
  - `cargo test -p package_that_does_not_exist`\n\
Dependencies: none\n";
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(
            repo.join(".gitignore"),
            ".auto/\nlane-clean-no-commit/\nlane-repo/\nparallel-run/\n",
        )
        .expect("write gitignore");
        fs::write(
            repo.join("REVIEW.md"),
            "# Review\n\n## `TASK-008`\n- Source: test handoff.\n- Remaining blockers: none.\n",
        )
        .expect("write review");
        fs::create_dir_all(repo.join("scripts")).expect("create scripts");
        let wrapper = repo.join("scripts/run-task-verification.sh");
        fs::write(
            &wrapper,
            "#!/bin/sh\nshift\nif [ \"$1\" = \"--\" ]; then shift; fi\nexec \"$@\"\n",
        )
        .expect("write verification wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        run_git_in(&repo, ["add", "."]);
        run_git_in(&repo, ["commit", "-m", "seed task"]);
        let receipt_commit = git_output(&repo, ["rev-parse", "HEAD"]);
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::write(
            repo.join(".auto/symphony/verification-receipts/TASK-008.json"),
            format!(
                r#"{{"task_id":"TASK-008","commit":"{receipt_commit}","commands":[{{"command":"cargo test -p package_that_does_not_exist","argv":["cargo","test","-p","package_that_does_not_exist"],"expected_argv":["cargo","test","-p","package_that_does_not_exist"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write receipt");
        propagate_lane_receipts(&repo, &repo, "TASK-008", task_markdown)
            .expect("refresh canonical receipt metadata");

        let lane_root = repo.join("lane-clean-no-commit");
        fs::create_dir_all(&lane_root).expect("create lane root");
        let run_root = repo.join("parallel-run");
        fs::create_dir_all(&run_root).expect("create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("initialize logger");
        let mut assignment = ActiveLaneAssignment {
            lane_index: 1,
            attempts: 1,
            task: LoopTask {
                id: "TASK-008".to_string(),
                title: "Evidence must be rechecked".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: task_markdown.to_string(),
            },
            resumed: false,
            lane_root: lane_root.clone(),
            lane_repo_root: repo.join("lane-repo"),
            base_commit: receipt_commit,
            stdout_log_path: lane_root.join("stdout.log"),
            stderr_log_path: lane_root.join("stderr.log"),
            worker_pid_path: lane_root.join("worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };
        let review_config = LaneReviewConfig {
            model: "unused".to_string(),
            reasoning_effort: "unused".to_string(),
            codex_bin: PathBuf::from("/bin/false"),
        };

        let reconciled = reconcile_parallel_clean_no_commit(
            &repo,
            "main",
            &mut assignment,
            &logger,
            &review_config,
        )
        .await
        .expect("clean no-commit reconciliation should be bounded");

        assert!(
            !reconciled,
            "receipt presence alone must not bypass current-tree verification"
        );
        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(
            plan.contains("- [~] `TASK-008` Evidence must be rechecked"),
            "failed verification must hold the task partial: {plan}"
        );
        let verdict = fs::read_to_string(lane_root.join("clean-no-commit-verdict.json"))
            .expect("read verdict");
        assert!(
            verdict.contains("\"verdict\": \"landed-unverified\""),
            "{verdict}"
        );
        assert!(
            verdict.contains("verification") && verdict.contains("package_that_does_not_exist"),
            "verdict must state the exact failed gate: {verdict}"
        );

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[tokio::test]
    async fn clean_no_commit_missing_receipt_runs_gates_and_records_exact_failure() {
        let repo = unique_temp_dir("parallel-clean-no-commit-missing-evidence");
        init_git_repo(&repo);
        let task_markdown = "- [ ] `TASK-009` Missing receipt proof\n\
Verification:\n\
  - `cargo test -p demo task_009`\n\
Dependencies: none\n";
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("write plan");
        git_ok(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&repo, ["commit", "-q", "-m", "seed missing receipt task"]);

        let mut assignment = review_gate_assignment_with_markdown(
            &repo,
            "TASK-009",
            "Missing receipt proof",
            task_markdown.to_string(),
        );
        assignment.task.status = LoopTaskStatus::Pending;
        fs::create_dir_all(&assignment.lane_root).expect("create lane root");
        let run_root = repo.join("parallel-run");
        fs::create_dir_all(&run_root).expect("create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("initialize logger");
        let review_config = LaneReviewConfig {
            model: "unused".to_string(),
            reasoning_effort: "unused".to_string(),
            codex_bin: PathBuf::from("/bin/false"),
        };

        let reconciled = reconcile_parallel_clean_no_commit(
            &repo,
            "main",
            &mut assignment,
            &logger,
            &review_config,
        )
        .await
        .expect("missing evidence should be classified");

        assert!(!reconciled);
        let verdict = fs::read_to_string(assignment.lane_root.join("clean-no-commit-verdict.json"))
            .expect("read verdict");
        assert!(
            verdict.contains("\"verdict\": \"landed-unverified\""),
            "{verdict}"
        );
        assert!(
            verdict.contains("host re-execution verification failed")
                && verdict.contains("cargo test -p demo task_009"),
            "verdict must name the failed repair gate and exact command: {verdict}"
        );
        let live_log = fs::read_to_string(run_root.join("live.log")).expect("read live log");
        assert!(
            live_log.contains("host re-execution verification failed"),
            "operator log must preserve the exact failed repair gate: {live_log}"
        );

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[test]
    fn promote_from_canonical_evidence_leaves_unevidenced_partial_untouched() {
        let repo = unique_temp_dir("promote-evidence-skip");
        init_git_repo(&repo);

        // Same task, but NO REVIEW.md handoff -> not fully evidenced. The host
        // must NOT promote it; it has to fall through to a real worker dispatch.
        let task_markdown = "- [~] `TASK-1` Genuinely partial\n\nStill needs work.\n";
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("failed to write plan");
        git_ok(&repo, ["add", "."]);
        git_ok(&repo, ["commit", "-m", "seed plan"]);
        let head_before = git_output(&repo, ["rev-parse", "HEAD"]);

        let promoted = promote_task_from_canonical_evidence_no_push(&repo, "TASK-1", task_markdown)
            .expect("promotion check should not error");
        assert!(
            !promoted,
            "task without a REVIEW handoff must not be promoted"
        );

        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(
            plan.contains("- [~] `TASK-1`"),
            "plan checkbox must stay [~], got:\n{plan}"
        );
        let head_after = git_output(&repo, ["rev-parse", "HEAD"]);
        assert_eq!(head_before, head_after, "no commit should be created");

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
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
    fn dispatch_checkpoint_refuses_prestaged_path_outside_declared_scope() {
        let repo = unique_temp_dir("parallel-dispatch-prestaged-outside");
        init_git_repo(&repo);
        fs::create_dir_all(repo.join("src")).expect("create src");
        fs::write(repo.join("README.md"), "# seed\n").expect("write seed README");
        git_ok(&repo, ["add", "README.md"]);
        git_ok(&repo, ["commit", "-q", "-m", "seed dispatch repo"]);
        fs::write(repo.join("src/prestaged.rs"), "pub fn injected() {}\n")
            .expect("write pre-staged source");
        git_ok(&repo, ["add", "src/prestaged.rs"]);
        fs::write(repo.join("README.md"), "# intended dispatch checkpoint\n")
            .expect("dirty intended path");
        let head = git_output(&repo, ["rev-parse", "HEAD"]);
        let branch = git_output(&repo, ["branch", "--show-current"]);

        let error = checkpoint_parallel_dispatch_paths(
            &repo,
            branch.trim(),
            &["README.md".to_string()],
            "auto parallel checkpoint",
        )
        .expect_err("dispatch checkpoint must refuse pre-staged paths outside its scope");
        let detail = format!("{error:#}");
        assert!(detail.contains("src/prestaged.rs"), "{detail}");
        assert_eq!(head, git_output(&repo, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(repo).expect("cleanup");
    }

    #[test]
    fn host_queue_checkpoint_refuses_prestaged_source_path() {
        let repo = unique_temp_dir("parallel-host-queue-prestaged-source");
        let run_root = unique_temp_dir("parallel-host-queue-prestaged-source-run");
        init_git_repo(&repo);
        fs::create_dir_all(repo.join("src")).expect("create src");
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n",
        )
        .expect("write plan");
        fs::write(repo.join("REVIEW.md"), "# REVIEW\n").expect("write review");
        git_ok(&repo, ["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"]);
        git_ok(&repo, ["commit", "-q", "-m", "seed host queue"]);
        fs::write(repo.join("src/injected.rs"), "pub fn injected() {}\n")
            .expect("write pre-staged source");
        git_ok(&repo, ["add", "src/injected.rs"]);
        fs::write(repo.join("REVIEW.md"), "# REVIEW\n\nqueue update\n").expect("dirty host queue");
        fs::create_dir_all(&run_root).expect("create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("create logger");
        let head = git_output(&repo, ["rev-parse", "HEAD"]);
        let branch = git_output(&repo, ["branch", "--show-current"]);

        let error = checkpoint_parallel_host_queue_changes(&repo, branch.trim(), &logger)
            .expect_err("host queue checkpoint must refuse pre-staged source");
        let detail = format!("{error:#}");
        assert!(detail.contains("src/injected.rs"), "{detail}");
        assert_eq!(head, git_output(&repo, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(repo).expect("cleanup");
        fs::remove_dir_all(run_root).expect("cleanup run root");
    }

    #[test]
    fn dispatch_checkpoint_refuses_gate_held_done_in_worktree_plan_view() {
        let (root, worker, task_id) = init_gate_held_done_checkpoint_repo(
            "parallel-dispatch-held-done-worktree",
            GateHeldDonePlanView::Worktree,
        );
        fs::write(worker.join("README.md"), "# dirty dispatch path\n")
            .expect("failed to dirty dispatch path");
        let head_before = run_git_in(&worker, ["rev-parse", "HEAD"]);

        let error = checkpoint_parallel_dispatch_paths(
            &worker,
            "trunk",
            &["README.md".to_string()],
            "auto parallel checkpoint",
        )
        .expect_err("a worktree [x] for a gate-held task must block dispatch checkpointing");

        let detail = format!("{error:#}");
        assert!(detail.contains(&task_id), "{detail}");
        assert!(detail.contains("worktree newly completed"), "{detail}");
        assert!(detail.contains("unsealed"), "{detail}");
        assert_eq!(head_before, run_git_in(&worker, ["rev-parse", "HEAD"]));
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn dispatch_checkpoint_refuses_gate_held_done_in_index_plan_view() {
        let (root, worker, task_id) = init_gate_held_done_checkpoint_repo(
            "parallel-dispatch-held-done-index",
            GateHeldDonePlanView::Index,
        );
        fs::write(worker.join("README.md"), "# dirty dispatch path\n")
            .expect("failed to dirty dispatch path");
        let head_before = run_git_in(&worker, ["rev-parse", "HEAD"]);

        let error = checkpoint_parallel_dispatch_paths(
            &worker,
            "trunk",
            &["README.md".to_string()],
            "auto parallel checkpoint",
        )
        .expect_err("an indexed [x] for a gate-held task must block dispatch checkpointing");

        let detail = format!("{error:#}");
        assert!(detail.contains(&task_id), "{detail}");
        assert!(detail.contains("index newly completed"), "{detail}");
        assert!(detail.contains("unsealed"), "{detail}");
        assert_eq!(head_before, run_git_in(&worker, ["rev-parse", "HEAD"]));
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
    fn host_queue_checkpoint_refuses_gate_held_done_in_worktree_plan_view() {
        let (root, worker, task_id) = init_gate_held_done_checkpoint_repo(
            "parallel-host-queue-held-done-worktree",
            GateHeldDonePlanView::Worktree,
        );
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let head_before = run_git_in(&worker, ["rev-parse", "HEAD"]);

        let error = checkpoint_parallel_host_queue_changes(&worker, "trunk", &logger)
            .expect_err("a worktree [x] for a gate-held task must block host queue checkpointing");

        let detail = format!("{error:#}");
        assert!(detail.contains(&task_id), "{detail}");
        assert!(detail.contains("worktree newly completed"), "{detail}");
        assert!(detail.contains("unsealed"), "{detail}");
        assert_eq!(head_before, run_git_in(&worker, ["rev-parse", "HEAD"]));
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn host_queue_checkpoint_refuses_gate_held_done_in_index_plan_view() {
        let (root, worker, task_id) = init_gate_held_done_checkpoint_repo(
            "parallel-host-queue-held-done-index",
            GateHeldDonePlanView::Index,
        );
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        fs::write(
            worker.join("REVIEW.md"),
            "# Review\n\nHost queue state still needs syncing.\n",
        )
        .expect("failed to dirty host queue state");
        let head_before = run_git_in(&worker, ["rev-parse", "HEAD"]);

        let error = checkpoint_parallel_host_queue_changes(&worker, "trunk", &logger)
            .expect_err("an indexed [x] for a gate-held task must block host queue checkpointing");

        let detail = format!("{error:#}");
        assert!(detail.contains(&task_id), "{detail}");
        assert!(detail.contains("index newly completed"), "{detail}");
        assert!(detail.contains("unsealed"), "{detail}");
        assert_eq!(head_before, run_git_in(&worker, ["rev-parse", "HEAD"]));
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
    fn landed_task_reconciliation_marks_partial_when_receipt_wrapper_is_missing() {
        let repo = unique_temp_dir("parallel-landed-task-missing-wrapper");
        init_git_repo(&repo);
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n- [ ] `TASK-003` Add event envelopes\nVerification:\n  - `cargo test -p ab-events`\nCompletion artifacts: `crates/ab-events/`\nDependencies: none\n",
        )
        .expect("failed to write plan");
        fs::create_dir_all(repo.join("crates/ab-events/src"))
            .expect("failed to create artifact dir");
        fs::write(
            repo.join("crates/ab-events/src/lib.rs"),
            "pub fn event() {}\n",
        )
        .expect("failed to write artifact");
        run_git_in(
            &repo,
            [
                "add",
                "IMPLEMENTATION_PLAN.md",
                "crates/ab-events/src/lib.rs",
            ],
        );
        run_git_in(&repo, ["commit", "-m", "seed task"]);

        let mut task = LoopTask {
            id: "TASK-003".to_string(),
            title: "Add event envelopes".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("M".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-003` Add event envelopes\nVerification:\n  - `cargo test -p ab-events`\nCompletion artifacts: `crates/ab-events/`\nDependencies: none\n".to_string(),
        };

        let status = reconcile_parallel_landed_task_state(
            &repo,
            &mut task,
            &["crates/ab-events/src/lib.rs".to_string()],
        )
        .expect("landing reconciliation should complete");

        assert_eq!(status, LoopTaskStatus::Partial);
        assert_eq!(task.status, LoopTaskStatus::Partial);
        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md"))
            .expect("plan should be readable");
        assert!(plan.contains("- [~] `TASK-003` Add event envelopes"));
        let review = fs::read_to_string(repo.join("REVIEW.md")).expect("review should be written");
        assert!(review.contains("`TASK-003`"));
        assert!(review.contains("crates/ab-events/src/lib.rs"));
        assert!(review.contains("missing scripts/run-task-verification.sh"));
        assert!(!review.contains("missing REVIEW.md handoff"));
        let staged = run_git_in(&repo, ["diff", "--cached", "--name-only"]);
        assert!(staged.contains("IMPLEMENTATION_PLAN.md"));
        assert!(staged.contains("REVIEW.md"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn landed_task_reconciliation_enters_gates_with_standing_review_findings() {
        let repo = unique_temp_dir("parallel-landed-task-standing-review");
        init_git_repo(&repo);
        fs::write(repo.join("README.md"), "# seed\n").expect("failed to write seed");
        run_git_in(&repo, ["add", "README.md"]);
        run_git_in(&repo, ["commit", "-m", "initial"]);
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n- [ ] `TASK-004` Clear standing finding\nVerification:\n  - `cargo test -p demo task_004`\nDependencies: none\n",
        )
        .expect("failed to write plan");
        fs::create_dir_all(repo.join("scripts")).expect("failed to create scripts dir");
        fs::write(repo.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(repo.join(".gitignore"), ".auto/\n").expect("write gitignore");
        fs::write(
            repo.join("REVIEW.md"),
            "# REVIEW\n\n## `TASK-004`: independent review findings\n- Source: auto parallel independent diff-review gate (held at `[~]`).\n\n1. `src/lib.rs`: stale standing finding to re-run.\n",
        )
        .expect("failed to write review");
        run_git_in(
            &repo,
            [
                "add",
                "IMPLEMENTATION_PLAN.md",
                "REVIEW.md",
                ".gitignore",
                "scripts/run-task-verification.sh",
            ],
        );
        run_git_in(&repo, ["commit", "-m", "seed task"]);
        let receipt_commit = git_output(&repo, ["rev-parse", "HEAD"]);
        fs::write(
            repo.join(".auto/symphony/verification-receipts/TASK-004.json"),
            format!(
                r#"{{"task_id":"TASK-004","commit":"{receipt_commit}","commands":[{{"command":"cargo test -p demo task_004","argv":["cargo","test","-p","demo","task_004"],"expected_argv":["cargo","test","-p","demo","task_004"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("failed to write receipt");
        let task_markdown = "- [ ] `TASK-004` Clear standing finding\nVerification:\n  - `cargo test -p demo task_004`\nDependencies: none\n";
        propagate_lane_receipts(&repo, &repo, "TASK-004", task_markdown)
            .expect("refresh canonical receipt metadata");

        let mut task = LoopTask {
            id: "TASK-004".to_string(),
            title: "Clear standing finding".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: task_markdown.to_string(),
        };

        let status =
            reconcile_parallel_landed_task_state(&repo, &mut task, &["src/lib.rs".to_string()])
                .expect("landing reconciliation should complete");

        assert_eq!(status, LoopTaskStatus::Done);
        assert_eq!(task.status, LoopTaskStatus::Done);
        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md"))
            .expect("plan should be readable");
        assert!(
            plan.contains("- [x] `TASK-004` Clear standing finding"),
            "ready tasks should enter final gates even when REVIEW.md still has a standing finding: {plan}"
        );

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
    fn landing_rebase_retry_budget_refreshes_canonical_head_five_times() {
        let (root, remote, upstream, _worker) =
            init_remote_and_clones("parallel-landing-recovery-five", "main");
        let lane = root.join("lane-five");
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
        fs::write(lane.join("lane.txt"), "task result\n").expect("write lane task file");
        run_git_in(&lane, ["add", "lane.txt"]);
        run_git_in(&lane, ["commit", "-m", "lane task"]);

        let mut assignment = ActiveLaneAssignment {
            lane_index: 1,
            attempts: 1,
            task: LoopTask {
                id: "TASK-FIVE".to_string(),
                title: "five fresh rebases".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [ ] `TASK-FIVE` five fresh rebases\n".to_string(),
            },
            resumed: false,
            lane_root: root.join("lane-five-root"),
            lane_repo_root: lane.clone(),
            base_commit,
            stdout_log_path: root.join("lane-five.stdout.log"),
            stderr_log_path: root.join("lane-five.stderr.log"),
            worker_pid_path: root.join("lane-five.worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };

        assert_eq!(LANDING_REBASE_RETRY_LIMIT, 5);
        for retry in 1..=LANDING_REBASE_RETRY_LIMIT {
            let canonical_file = format!("canonical-{retry}.txt");
            fs::write(
                upstream.join(&canonical_file),
                format!("canonical {retry}\n"),
            )
            .expect("write canonical file");
            run_git_in(&upstream, ["add", canonical_file.as_str()]);
            run_git_in(
                &upstream,
                ["commit", "-m", format!("canonical move {retry}").as_str()],
            );
            run_git_in(&upstream, ["push", "origin", "main"]);
            let fresh_head = git_output(&upstream, ["rev-parse", "HEAD"]);
            let range_base = assignment.base_commit.clone();

            let prep = prepare_lane_landing_recovery(
                &mut assignment,
                "main",
                &range_base,
                "canonical moved during landing",
            )
            .expect("fresh landing rebase should prepare");
            assert_eq!(prep, LaneLandingRecoveryPrep::RebasedCleanly);
            assert_eq!(assignment.base_commit, fresh_head);
            assert_eq!(
                fs::read_to_string(lane.join("lane.txt")).expect("read lane task file"),
                "task result\n"
            );
            assert_eq!(run_git_in(&lane, ["status", "--short"]), "");
        }

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
    fn cherry_pick_lane_range_rejects_reserved_verification_receipt_trailers() {
        let (root, remote, _upstream, worker) =
            init_remote_and_clones("parallel-reserved-receipt-footer", "main");
        let lane = root.join("lane-reserved-footer");
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
        fs::write(lane.join("forged.rs"), "pub fn forged() {}\n")
            .expect("write forged lane source");
        run_git_in(&lane, ["add", "forged.rs"]);
        run_git_in(
            &lane,
            [
                "commit",
                "-m",
                "lane result",
                "-m",
                "Auto-Verification-Receipt-Version: 1\nAuto-Verification-Receipt-Task: TASK-FORGED\nAuto-Verification-Receipt-JSON: Zm9yZ2Vk",
            ],
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
        let canonical_head = git_output(&worker, ["rev-parse", "HEAD"]);

        let error = cherry_pick_lane_range(
            &worker,
            &base_commit,
            "FETCH_HEAD",
            CherryPickFailurePolicy::Abort,
        )
        .expect_err("lane commits may not mint reserved host receipt trailers");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("reserved verification receipt"),
            "{rendered}"
        );
        assert_eq!(git_output(&worker, ["rev-parse", "HEAD"]), canonical_head);
        assert!(!worker.join("forged.rs").exists());
        assert!(!lane_repo_has_active_cherry_pick(&worker));

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn cherry_pick_lane_range_rejects_host_owned_queue_paths() {
        let (root, remote, _upstream, worker) =
            init_remote_and_clones("parallel-host-queue-lane-change", "main");
        let lane = root.join("lane-host-queue");
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
        fs::write(
            lane.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-FORGED` lane must not own queue state\n",
        )
        .expect("write forged queue state");
        run_git_in(&lane, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&lane, ["commit", "-m", "lane modifies host queue"]);
        let lane_head = git_output(&lane, ["rev-parse", "HEAD"]);
        run_git_in(
            &worker,
            [
                "fetch",
                lane.to_str().expect("lane path should be utf-8"),
                lane_head.as_str(),
            ],
        );
        let canonical_head = git_output(&worker, ["rev-parse", "HEAD"]);

        let error = cherry_pick_lane_range(
            &worker,
            &base_commit,
            "FETCH_HEAD",
            CherryPickFailurePolicy::Abort,
        )
        .expect_err("lane commits may not modify host queue state");

        assert!(
            format!("{error:#}").contains("host-owned queue path"),
            "{error:#}"
        );
        assert_eq!(git_output(&worker, ["rev-parse", "HEAD"]), canonical_head);
        assert!(!worker.join("IMPLEMENTATION_PLAN.md").exists());

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
        let (note, conflict_paths) = match prep {
            LaneLandingRecoveryPrep::NeedsWorkerResolution {
                recovery_note,
                conflict_paths,
            } => (recovery_note, conflict_paths),
            other => panic!("expected worker-resolution prep, got {other:?}"),
        };
        assert_eq!(assignment.base_commit, remote_head);
        assert!(lane_repo_has_active_cherry_pick(&lane));
        let status = run_git_in(&lane, ["status", "--short"]);
        assert!(status.contains("shared.txt"));
        assert!(lane_repo_status_summary(&lane).contains("cherry-pick recovery"));
        assert!(note.contains("landing-recovery mode"));
        assert!(note.contains("cherry-pick --continue"));
        assert_eq!(conflict_paths, vec!["shared.txt".to_string()]);

        let resumed = lane_repo_recovery_note(&lane, "main", status.trim());
        assert!(resumed.contains("in-progress landing-recovery cherry-pick"));
        assert!(resumed.contains("shared.txt"));

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[tokio::test]
    async fn startup_auto_unshelves_only_passing_landing_divergence_tasks() {
        let repo_root = unique_temp_dir("parallel-auto-unshelve-repo");
        let run_root = unique_temp_dir("parallel-auto-unshelve-run");
        init_git_repo(&repo_root);
        fs::write(repo_root.join("seed.txt"), "canonical seed\n").expect("write seed");
        fs::create_dir_all(&run_root).expect("create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let passing_markdown =
            "- [~] `TASK-PASS` pass\n\nVerification:\n- Run `bash -c true`\n".to_string();
        let failing_markdown =
            "- [~] `TASK-FAIL` fail\n\nVerification:\n- Run `bash -c false`\n".to_string();
        let conflict_markdown =
            "- [~] `TASK-CONFLICT` conflict\n\nVerification:\n- Run `bash -c true`\n".to_string();
        let legacy_markdown =
            "- [~] `TASK-GATE` gate failure\n\nVerification:\n- Run `bash -c true`\n".to_string();
        fs::write(
            repo_root.join("IMPLEMENTATION_PLAN.md"),
            format!(
                "{passing_markdown}\n{failing_markdown}\n{conflict_markdown}\n{legacy_markdown}"
            ),
        )
        .expect("write implementation plan");
        git_ok(&repo_root, ["add", "seed.txt", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&repo_root, ["commit", "-q", "-m", "seed canonical repo"]);
        let mut state = ParallelRunState::default();
        state.shelved_tasks.insert(
            "TASK-PASS".to_string(),
            ShelvedTaskState::Detailed(ShelvedTaskDetails {
                markdown: passing_markdown,
                failure_reason: ShelvedTaskFailureReason::LandingDivergence,
                conflict_paths: Vec::new(),
                detail: Some("canonical moved".to_string()),
            }),
        );
        state.shelved_tasks.insert(
            "TASK-FAIL".to_string(),
            ShelvedTaskState::Detailed(ShelvedTaskDetails {
                markdown: failing_markdown,
                failure_reason: ShelvedTaskFailureReason::LandingDivergence,
                conflict_paths: Vec::new(),
                detail: Some("canonical moved".to_string()),
            }),
        );
        state.shelved_tasks.insert(
            "TASK-CONFLICT".to_string(),
            ShelvedTaskState::Detailed(ShelvedTaskDetails {
                markdown: conflict_markdown,
                failure_reason: ShelvedTaskFailureReason::LandingConflict,
                conflict_paths: vec!["src/lib.rs".to_string()],
                detail: Some("same hunk".to_string()),
            }),
        );
        state.shelved_tasks.insert(
            "TASK-GATE".to_string(),
            ShelvedTaskState::Legacy(legacy_markdown),
        );
        state
            .unblock_attempt_counts
            .insert("TASK-PASS".to_string(), 3);
        state
            .attempted_partial_followups
            .insert("TASK-PASS".to_string(), 2);

        let recovered =
            auto_unshelve_landing_divergence_tasks(&repo_root, &mut state, &logger).await;

        assert_eq!(recovered, 1);
        assert!(!state.shelved_tasks.contains_key("TASK-PASS"));
        assert!(!state.unblock_attempt_counts.contains_key("TASK-PASS"));
        assert!(!state.attempted_partial_followups.contains_key("TASK-PASS"));
        assert!(state.shelved_tasks.contains_key("TASK-FAIL"));
        assert!(state.shelved_tasks.contains_key("TASK-CONFLICT"));
        assert!(state.shelved_tasks.contains_key("TASK-GATE"));
        let log = fs::read_to_string(run_root.join("live.log")).expect("read live log");
        assert!(log.contains("auto-unshelve: `TASK-PASS`"));
        assert!(log.contains("keeping landing-divergence shelf `TASK-FAIL`"));

        fs::remove_dir_all(&repo_root).expect("remove repo root");
        fs::remove_dir_all(&run_root).expect("remove run root");
    }

    #[test]
    fn drift_reverify_budget_parses_env() {
        use std::time::Duration;
        std::env::remove_var("AUTO_PARALLEL_DRIFT_REVERIFY_BUDGET_SECS");
        assert_eq!(super::drift_reverify_budget(), Duration::from_secs(900));
        std::env::set_var("AUTO_PARALLEL_DRIFT_REVERIFY_BUDGET_SECS", "120");
        assert_eq!(super::drift_reverify_budget(), Duration::from_secs(120));
        std::env::set_var("AUTO_PARALLEL_DRIFT_REVERIFY_BUDGET_SECS", "0");
        assert!(super::drift_reverify_budget().is_zero());
        std::env::remove_var("AUTO_PARALLEL_DRIFT_REVERIFY_BUDGET_SECS");
    }

    #[test]
    fn receipt_drift_triage_is_stable_across_current_head_changes() {
        let recorded = "421beaf8f49627bc9ef67353622bc05654535e4f";
        let entry_at_first_head = ReceiptDriftTriageEntry {
            task_id: "TASK-STABLE-TRIAGE".to_string(),
            title: "Keep generated triage idempotent".to_string(),
            status: LoopTaskStatus::Done,
            reasons: vec![format!(
                "stale verification receipt `receipt.json`: commit mismatch, recorded `{recorded}` is not current HEAD `12b7411c41a03b63f05ab1d0c95593a0ea4c692d`"
            )],
        };
        let entry_at_next_head = ReceiptDriftTriageEntry {
            task_id: "TASK-STABLE-TRIAGE".to_string(),
            title: "Keep generated triage idempotent".to_string(),
            status: LoopTaskStatus::Done,
            reasons: vec![format!(
                "stale verification receipt `receipt.json`: commit mismatch, recorded `{recorded}` is not current HEAD `4420c916f489bb0c408a8c898e4544531dfe512a`"
            )],
        };

        let first = render_receipts_drift_triage(&[entry_at_first_head], &[]);
        let next = render_receipts_drift_triage(&[entry_at_next_head], &[]);

        assert_eq!(
            first, next,
            "committing the triage report must not change its own generated body"
        );
        assert!(
            first.contains(recorded),
            "the recorded proof identity matters"
        );
        assert!(!first.contains("12b7411c41a03b63f05ab1d0c95593a0ea4c692d"));
        assert!(first.contains("is not current HEAD"));
    }

    #[tokio::test]
    async fn audit_parallel_completion_drift_preserves_completed_rows() {
        let repo = unique_temp_dir("parallel-drift-audit");
        let run_root = unique_temp_dir("parallel-drift-audit-run");
        init_git_repo(&repo);
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        git_ok(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&repo, ["commit", "-q", "-m", "seed completed plan"]);
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        // Missing evidence is triaged, but landed queue truth remains completed.
        let (updated, _) = audit_parallel_completion_drift(
            &repo,
            "main",
            &fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should exist"),
            &logger,
        )
        .await
        .expect("drift audit should succeed");

        assert_eq!(updated, plan);
        let persisted =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should persist");
        assert_eq!(persisted, updated);
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).unwrap_or_default();
        assert!(
            triage.contains("TASK-001") && triage.contains("Completed Tasks With Drift"),
            "receipt drift should stay visible in triage"
        );
        let live_log = fs::read_to_string(run_root.join("live.log"))
            .expect("receipt repair should write host log");
        assert!(live_log.contains("without changing IMPLEMENTATION_PLAN.md"));
    }

    #[tokio::test]
    async fn audit_parallel_completion_drift_logs_only_changed_triage() {
        let repo = unique_temp_dir("parallel-drift-audit-stable-log");
        let run_root = unique_temp_dir("parallel-drift-audit-stable-log-run");
        init_git_repo(&repo);
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        git_ok(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&repo, ["commit", "-q", "-m", "seed completed plan"]);
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let (updated, _) = audit_parallel_completion_drift(&repo, "main", plan, &logger)
            .await
            .expect("first drift audit should succeed");
        assert_eq!(updated, plan);
        let first_log =
            fs::read_to_string(run_root.join("live.log")).expect("first audit should log drift");
        assert!(first_log.contains("without changing IMPLEMENTATION_PLAN.md"));

        let _ = audit_parallel_completion_drift(&repo, "main", &updated, &logger)
            .await
            .expect("second drift audit should succeed");
        let second_log =
            fs::read_to_string(run_root.join("live.log")).expect("second audit should keep log");
        assert_eq!(
            second_log
                .matches("wrote RECEIPTS-DRIFT.md without changing IMPLEMENTATION_PLAN.md")
                .count(),
            1,
            "unchanged triage may retry local repair, but must not append another warning: first={first_log:?} second={second_log:?}"
        );

        let drift_summary = receipt_drift_status_summary(&repo);
        assert!(
            drift_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("completed task(s)")),
            "completed receipt drift should remain visible without changing queue truth: {drift_summary:?}"
        );
    }

    #[tokio::test]
    async fn audit_parallel_completion_drift_triages_legacy_receipt_without_demotion() {
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
            r#"{"task_id":"TASK-001","commands":[{"command":"cargo test task_001","argv":["cargo","test","task_001"],"expected_argv":["cargo","test","task_001"],"exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write legacy receipt");
        fs::create_dir_all(repo.join("scripts")).expect("create scripts");
        fs::write(
            repo.join("scripts/run-task-verification.sh"),
            "#!/bin/sh\nexit 0\n",
        )
        .expect("write wrapper");
        run_git_in(
            &repo,
            [
                "add",
                "IMPLEMENTATION_PLAN.md",
                "REVIEW.md",
                "scripts/run-task-verification.sh",
            ],
        );
        run_git_in(&repo, ["commit", "-m", "completed task"]);
        run_git_in(&repo, ["push", "origin", "trunk"]);
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let (updated, _) = audit_parallel_completion_drift(&repo, "trunk", plan, &logger)
            .await
            .expect("drift audit should triage legacy receipt without source attestation");

        assert_eq!(updated, plan, "receipt drift must not rewrite queue truth");
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md"))
            .expect("backfilled legacy receipt should remain in drift triage");
        assert!(
            triage.contains("TASK-001")
                && triage.contains("stale verification receipt")
                && triage.contains("missing current commit metadata"),
            "{triage}"
        );
        let log = git_output(&repo, ["log", "-1", "--format=%B"]);
        assert!(
            !log.contains("Auto-Verification-Receipt-Task: TASK-001"),
            "legacy JSON and a fake exit-zero wrapper must not mint durable authority: {log}"
        );
        let live_log = fs::read_to_string(run_root.join("live.log"))
            .expect("drift audit should write host log");
        assert!(live_log.contains("without changing IMPLEMENTATION_PLAN.md"));
    }

    #[tokio::test]
    async fn legacy_done_evidence_is_triaged_without_publishing_a_footer() {
        let repo = unique_temp_dir("parallel-drift-no-early-footer");
        let run_root = unique_temp_dir("parallel-drift-no-early-footer-run");
        init_git_repo(&repo);
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::create_dir_all(&run_root).expect("create run root");
        let task_id = "TASK-NO-EARLY-FOOTER";
        let command = "bash -c true";
        let plan = format!(
            "- [x] `{task_id}` Legacy evidence still needs current gates\n  Verification: `{command}`\n  Dependencies: none\n  Estimated scope: S\n"
        );
        fs::write(repo.join(".gitignore"), ".auto/\n").expect("write gitignore");
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), &plan).expect("write plan");
        fs::write(
            repo.join("REVIEW.md"),
            format!("## `{task_id}`\n\nExisting handoff.\n"),
        )
        .expect("write review");
        git_ok(
            &repo,
            ["add", ".gitignore", "IMPLEMENTATION_PLAN.md", "REVIEW.md"],
        );
        git_ok(&repo, ["commit", "-q", "-m", "seed completed legacy task"]);
        fs::write(
            repo.join(format!(
                ".auto/symphony/verification-receipts/{task_id}.json"
            )),
            format!(
                r#"{{"task_id":"{task_id}","commands":[{{"command":"{command}","argv":["bash","-c","true"],"expected_argv":["bash","-c","true"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write legacy receipt");
        propagate_lane_receipts(&repo, &repo, task_id, &plan)
            .expect("bind legacy receipt to current source");
        record_verified_source_attestation(&repo, task_id)
            .expect("record legacy source attestation");
        let evidence = inspect_task_completion_evidence(&repo, task_id, &plan);
        assert!(
            !evidence.is_fully_evidenced(),
            "without the verification wrapper, only an early footer could make this Done row appear trusted"
        );
        let head_before = git_output(&repo, ["rev-parse", "HEAD"]);
        let logger = ParallelEventLogger::new(&run_root).expect("initialize logger");

        let (audited, _) = audit_parallel_completion_drift(&repo, "main", &plan, &logger)
            .await
            .expect("legacy drift should be triaged without early footer publication");
        assert_eq!(audited, plan);
        assert_eq!(
            git_output(&repo, ["rev-parse", "HEAD"]),
            head_before,
            "the drift sweep must not publish a staging footer before current gates"
        );
        let latest_message = git_output(&repo, ["log", "-1", "--format=%B"]);
        assert!(
            !latest_message.contains("Auto-Verification-Receipt-Task:"),
            "{latest_message}"
        );

        let restart_plan =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read restart plan");
        let (restarted, _) = audit_parallel_completion_drift(&repo, "main", &restart_plan, &logger)
            .await
            .expect("restart should preserve completed queue truth");
        assert_eq!(restarted, plan);

        fs::remove_dir_all(&repo).expect("remove repo");
        fs::remove_dir_all(&run_root).expect("remove run root");
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
    fn repair_parallel_canonical_before_dispatch_ignores_run_artifact_roots() {
        let repo = unique_temp_dir("parallel-ignore-run-artifacts");
        let run_root = unique_temp_dir("parallel-ignore-run-artifacts-run");
        init_git_repo(&repo);
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        run_git_in(&repo, ["branch", "-M", "trunk"]);
        fs::write(repo.join("README.md"), "# repo\n").expect("failed to write README");
        run_git_in(&repo, ["add", "README.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);
        fs::create_dir_all(repo.join("steward")).expect("failed to create steward dir");
        fs::write(repo.join("steward").join("final-review.md"), "PASS\n")
            .expect("failed to write final review");
        fs::create_dir_all(repo.join("genesis")).expect("failed to create genesis dir");
        fs::write(repo.join("genesis").join("GBRAIN-CONTEXT.md"), "context\n")
            .expect("failed to write gbrain context");
        let before = git_output(&repo, ["rev-parse", "HEAD"]);
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        repair_parallel_canonical_before_dispatch(&repo, "trunk", &logger)
            .expect("run artifact roots should not force a checkpoint");

        let after = git_output(&repo, ["rev-parse", "HEAD"]);
        assert_eq!(after, before);
        let status = git_output(&repo, ["status", "--short", "--untracked-files=all"]);
        assert!(status.contains("steward/final-review.md"));
        assert!(status.contains("genesis/GBRAIN-CONTEXT.md"));
    }

    #[tokio::test]
    async fn audit_parallel_completion_drift_reports_closeout_candidates_without_promoting_plan() {
        let repo = unique_temp_dir("parallel-closeout-audit");
        let run_root = unique_temp_dir("parallel-closeout-audit-run");
        init_git_repo(&repo);
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [~] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        fs::write(repo.join("REVIEW.md"), "## `TASK-001`\n\nComplete.\n")
            .expect("failed to write review");
        git_ok(&repo, ["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"]);
        git_ok(&repo, ["commit", "-q", "-m", "seed partial closeout"]);
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let (updated, _) = audit_parallel_completion_drift(
            &repo,
            "main",
            &fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should exist"),
            &logger,
        )
        .await
        .expect("drift audit should succeed");

        assert!(updated.starts_with("- [~] `TASK-001`"), "{updated}");
        let persisted =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should persist");
        assert_eq!(persisted, updated);
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).unwrap_or_default();
        assert!(
            triage.contains("Manual Closeout Candidates") && triage.contains("TASK-001"),
            "closeout candidate should be reported without promotion: {triage}"
        );
        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("closeout should write host log");
        assert!(live_log.contains("left [~] pending definition-of-done gates"));
    }

    #[tokio::test]
    async fn drift_verify_refresh_never_promotes_partial_without_workspace_and_review_gates() {
        let repo = unique_temp_dir("parallel-drift-partial-needs-dod");
        let run_root = unique_temp_dir("parallel-drift-partial-needs-dod-run");
        init_git_repo(&repo);
        fs::create_dir_all(repo.join("scripts")).expect("create scripts dir");
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::create_dir_all(&run_root).expect("create run root");
        let task_id = "TASK-DOD-PARTIAL";
        let command = "bash -c true";
        let plan = format!(
            "- [~] `{task_id}` Already evidenced but not finalized\n  Verification: `{command}`\n  Dependencies: none\n  Estimated scope: S\n"
        );
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), &plan).expect("write plan");
        fs::write(
            repo.join("REVIEW.md"),
            format!("## `{task_id}`\n\nHost handoff is present.\n"),
        )
        .expect("write review");
        let wrapper = repo.join("scripts/run-task-verification.sh");
        fs::write(
            &wrapper,
            "#!/bin/sh\nshift\nif [ \"${1:-}\" = \"--\" ]; then shift; fi\nexec \"$@\"\n",
        )
        .expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        fs::write(
            repo.join(format!(
                ".auto/symphony/verification-receipts/{task_id}.json"
            )),
            format!(
                r#"{{"task_id":"{task_id}","commands":[{{"command":"{command}","argv":["bash","-c","true"],"expected_argv":["bash","-c","true"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write receipt");
        git_ok(
            &repo,
            [
                "add",
                "IMPLEMENTATION_PLAN.md",
                "REVIEW.md",
                "scripts/run-task-verification.sh",
            ],
        );
        git_ok(
            &repo,
            ["commit", "-q", "-m", "seed partial verification repo"],
        );
        let logger = ParallelEventLogger::new(&run_root).expect("initialize logger");

        let (updated, _) = audit_parallel_completion_drift(&repo, "main", &plan, &logger)
            .await
            .expect("drift audit should be bounded");

        assert!(
            updated.starts_with(&format!("- [~] `{task_id}`")),
            "verify-only receipt refresh must not bypass workspace and review gates: {updated}"
        );
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md"))
            .expect("partial closeout should stay visible");
        assert!(
            triage.contains("Manual Closeout Candidates") && triage.contains(task_id),
            "{triage}"
        );

        fs::remove_dir_all(&repo).expect("remove repo");
        fs::remove_dir_all(&run_root).expect("remove run root");
    }

    #[tokio::test]
    async fn drift_verify_refresh_preserves_done_queue_truth() {
        let repo = unique_temp_dir("parallel-drift-done-needs-dod");
        let run_root = unique_temp_dir("parallel-drift-done-needs-dod-run");
        init_git_repo(&repo);
        fs::create_dir_all(repo.join("scripts")).expect("create scripts dir");
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::create_dir_all(&run_root).expect("create run root");
        let task_id = "TASK-DOD-DONE";
        let command = "bash -c true";
        let plan = format!(
            "- [x] `{task_id}` Stale evidence requires complete reproof\n  Verification: `{command}`\n  Dependencies: none\n  Estimated scope: S\n"
        );
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), &plan).expect("write plan");
        fs::write(
            repo.join("REVIEW.md"),
            format!("## `{task_id}`\n\nExisting handoff.\n"),
        )
        .expect("write review");
        fs::write(
            repo.join(format!(
                ".auto/symphony/verification-receipts/{task_id}.json"
            )),
            format!(r#"{{"task_id":"{task_id}","commands":[]}}"#),
        )
        .expect("write stale receipt");
        let wrapper = repo.join("scripts/run-task-verification.sh");
        fs::write(
            &wrapper,
            format!(
                r#"#!/bin/sh
task="$1"
shift
if [ "${{1:-}}" = "--" ]; then shift; fi
printf '%s\n' '{{"task_id":"{task_id}","commands":[{{"command":"{command}","argv":["bash","-c","true"],"expected_argv":["bash","-c","true"],"exit_code":0,"status":"passed"}}]}}' > ".auto/symphony/verification-receipts/$task.json"
exec "$@"
"#
            ),
        )
        .expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        git_ok(
            &repo,
            [
                "add",
                "IMPLEMENTATION_PLAN.md",
                "REVIEW.md",
                "scripts/run-task-verification.sh",
            ],
        );
        git_ok(
            &repo,
            ["commit", "-q", "-m", "seed completed verification repo"],
        );
        let logger = ParallelEventLogger::new(&run_root).expect("initialize logger");

        let (updated, _) = audit_parallel_completion_drift(&repo, "main", &plan, &logger)
            .await
            .expect("drift audit should be bounded");

        assert_eq!(
            updated, plan,
            "receipt repair must preserve [x] queue truth"
        );
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).unwrap_or_default();
        assert!(
            !triage.contains(task_id),
            "successfully refreshed evidence should leave no drift entry: {triage}"
        );

        fs::remove_dir_all(&repo).expect("remove repo");
        fs::remove_dir_all(&run_root).expect("remove run root");
    }

    #[tokio::test]
    async fn done_drift_preserves_queue_truth_across_refresh_failure_and_restart() {
        let repo = unique_temp_dir("parallel-drift-refresh-crash");
        let run_root = unique_temp_dir("parallel-drift-refresh-crash-run");
        init_git_repo(&repo);
        fs::create_dir_all(repo.join("scripts")).expect("create scripts dir");
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::create_dir_all(&run_root).expect("create run root");
        let task_id = "TASK-REFRESH-CRASH";
        let command = "bash -c true";
        let plan = format!(
            "- [x] `{task_id}` Refresh preserves durable queue truth\n  Verification: `{command}`\n  Dependencies: none\n  Estimated scope: S\n"
        );
        fs::write(repo.join(".gitignore"), ".auto/\n").expect("write gitignore");
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), &plan).expect("write plan");
        fs::write(
            repo.join("REVIEW.md"),
            format!("## `{task_id}`\n\nExisting handoff.\n"),
        )
        .expect("write review");
        let wrapper = repo.join("scripts/run-task-verification.sh");
        fs::write(
            &wrapper,
            format!(
                r#"#!/bin/sh
task="$1"
shift
if [ "${{1:-}}" = "--" ]; then shift; fi
grep -F -- '- [x] `{task_id}`' IMPLEMENTATION_PLAN.md >/dev/null || exit 91
printf 'refresh observed durable done\n' > .auto/refresh-saw-done
printf '%s\n' '{{"task_id":"{task_id}","commands":[{{"command":"{command}","argv":["bash","-c","true"],"expected_argv":["bash","-c","true"],"exit_code":0,"status":"passed"}}]}}' > ".auto/symphony/verification-receipts/$task.json"
exec "$@"
"#
            ),
        )
        .expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        git_ok(
            &repo,
            [
                "add",
                ".gitignore",
                "IMPLEMENTATION_PLAN.md",
                "REVIEW.md",
                "scripts/run-task-verification.sh",
            ],
        );
        git_ok(&repo, ["commit", "-q", "-m", "seed completed drift task"]);
        fs::write(
            repo.join(format!(
                ".auto/symphony/verification-receipts/{task_id}.json"
            )),
            format!(r#"{{"task_id":"{task_id}","commands":[]}}"#),
        )
        .expect("write stale receipt");
        fs::create_dir(repo.join("RECEIPTS-DRIFT.md"))
            .expect("create deterministic post-refresh write failure");
        let logger = ParallelEventLogger::new(&run_root).expect("initialize logger");

        let error = audit_parallel_completion_drift(&repo, "main", &plan, &logger)
            .await
            .expect_err("post-refresh triage persistence should simulate a crash");
        assert!(
            format!("{error:#}").contains("RECEIPTS-DRIFT.md"),
            "{error:#}"
        );
        assert!(
            repo.join(".auto/refresh-saw-done").exists(),
            "verification must observe preserved [x] queue truth while refreshing evidence"
        );
        assert!(
            repo.join(format!(".auto/parallel/verified-source/{task_id}.json"))
                .exists(),
            "the simulated crash point must be after receipt and source-attestation refresh"
        );
        let persisted =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read persisted plan");
        assert_eq!(
            persisted, plan,
            "a triage write failure must not alter the plan"
        );

        fs::remove_dir(repo.join("RECEIPTS-DRIFT.md")).expect("clear crash fixture");
        let restarted_plan =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read restart plan");
        let (restarted, _) =
            audit_parallel_completion_drift(&repo, "main", &restarted_plan, &logger)
                .await
                .expect("restart should audit the durable Partial row");
        assert_eq!(restarted, plan, "restart must retain completed queue truth");
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).unwrap_or_default();
        assert!(
            !triage.contains(task_id),
            "the refreshed receipt should clear drift after restart: {triage}"
        );

        fs::remove_dir_all(&repo).expect("remove repo");
        fs::remove_dir_all(&run_root).expect("remove run root");
    }

    #[tokio::test]
    async fn matching_owned_inputs_survive_an_unrelated_later_commit() {
        let repo = unique_temp_dir("parallel-drift-unrelated-commit");
        let run_root = unique_temp_dir("parallel-drift-unrelated-commit-run");
        init_git_repo(&repo);
        fs::create_dir_all(repo.join("scripts")).expect("create scripts dir");
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::create_dir_all(&run_root).expect("create run root");
        let task_id = "TASK-UNCHANGED-INPUTS";
        let command = "bash -c true";
        let partial_plan = format!(
            "- [~] `{task_id}` Unrelated commits preserve proof\n  Verification: `{command}`\n  Dependencies: none\n  Estimated scope: S\n"
        );
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), &partial_plan).expect("write partial plan");
        fs::write(repo.join(".gitignore"), ".auto/\n").expect("write gitignore");
        fs::write(
            repo.join("REVIEW.md"),
            format!("## `{task_id}`\n\nInitial handoff.\n"),
        )
        .expect("write review");
        let wrapper = repo.join("scripts/run-task-verification.sh");
        fs::write(
            &wrapper,
            "#!/bin/sh\nshift\nif [ \"${1:-}\" = \"--\" ]; then shift; fi\nexec \"$@\"\n",
        )
        .expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        run_git_in(
            &repo,
            [
                "add",
                ".gitignore",
                "IMPLEMENTATION_PLAN.md",
                "REVIEW.md",
                "scripts/run-task-verification.sh",
            ],
        );
        run_git_in(&repo, ["commit", "-m", "seed partial task"]);

        fs::write(
            repo.join(format!(
                ".auto/symphony/verification-receipts/{task_id}.json"
            )),
            format!(
                r#"{{"task_id":"{task_id}","commands":[{{"command":"{command}","argv":["bash","-c","true"],"expected_argv":["bash","-c","true"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write receipt");
        propagate_lane_receipts(&repo, &repo, task_id, &partial_plan)
            .expect("bind receipt to seeded source");
        record_verified_source_attestation(&repo, task_id)
            .expect("attest the fixture's verified source state");

        let plan =
            partial_plan.replace(&format!("- [~] `{task_id}`"), &format!("- [x] `{task_id}`"));
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), &plan).expect("write done plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        commit_task_closeout(
            &repo,
            task_id,
            LoopTaskStatus::Done,
            &format!("{}: {task_id} queue sync", repo_name(&repo)),
            false,
        )
        .expect("commit completion with a host-stamped footer");

        fs::write(
            repo.join("unrelated.txt"),
            "This file is outside the task-owned inputs.\n",
        )
        .expect("write unrelated file");
        run_git_in(&repo, ["add", "unrelated.txt"]);
        run_git_in(&repo, ["commit", "-m", "add unrelated work"]);
        let logger = ParallelEventLogger::new(&run_root).expect("initialize logger");

        let (updated, _) = audit_parallel_completion_drift(&repo, "main", &plan, &logger)
            .await
            .expect("drift audit should trust unchanged task inputs");

        assert_eq!(updated, plan, "unrelated work must not demote the task");
        assert_eq!(
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read persisted plan"),
            plan,
            "the durable plan must remain Done"
        );
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).unwrap_or_default();
        assert!(
            !triage.contains(task_id),
            "unchanged task evidence must not enter drift triage: {triage}"
        );

        fs::remove_dir_all(&repo).expect("remove repo");
        fs::remove_dir_all(&run_root).expect("remove run root");
    }

    #[tokio::test]
    async fn matching_owned_inputs_never_hide_new_standing_review_finding() {
        let repo = unique_temp_dir("parallel-drift-trusted-review-finding");
        let run_root = unique_temp_dir("parallel-drift-trusted-review-finding-run");
        init_git_repo(&repo);
        fs::create_dir_all(repo.join("scripts")).expect("create scripts dir");
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::create_dir_all(&run_root).expect("create run root");
        let task_id = "TASK-TRUSTED-REVIEW";
        let command = "bash -c true";
        let partial_plan = format!(
            "- [~] `{task_id}` Matching inputs still honor review findings\n  Verification: `{command}`\n  Dependencies: none\n  Estimated scope: S\n"
        );
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), &partial_plan).expect("write partial plan");
        fs::write(repo.join(".gitignore"), ".auto/\n").expect("write gitignore");
        fs::write(
            repo.join("REVIEW.md"),
            format!("## `{task_id}`\n\nInitial handoff.\n"),
        )
        .expect("write initial review");
        let wrapper = repo.join("scripts/run-task-verification.sh");
        fs::write(
            &wrapper,
            "#!/bin/sh\nshift\nif [ \"${1:-}\" = \"--\" ]; then shift; fi\nexec \"$@\"\n",
        )
        .expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        run_git_in(
            &repo,
            [
                "add",
                ".gitignore",
                "IMPLEMENTATION_PLAN.md",
                "REVIEW.md",
                "scripts/run-task-verification.sh",
            ],
        );
        run_git_in(&repo, ["commit", "-m", "seed trusted partial task"]);

        fs::write(
            repo.join(format!(
                ".auto/symphony/verification-receipts/{task_id}.json"
            )),
            format!(
                r#"{{"task_id":"{task_id}","commands":[{{"command":"{command}","argv":["bash","-c","true"],"expected_argv":["bash","-c","true"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write receipt");
        propagate_lane_receipts(&repo, &repo, task_id, &partial_plan)
            .expect("bind receipt to clean seeded source");
        record_verified_source_attestation(&repo, task_id)
            .expect("attest the fixture's exact verified source state");

        let plan =
            partial_plan.replace(&format!("- [~] `{task_id}`"), &format!("- [x] `{task_id}`"));
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), &plan).expect("write done plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        commit_task_closeout(
            &repo,
            task_id,
            LoopTaskStatus::Done,
            &format!("{}: {task_id} queue sync", repo_name(&repo)),
            false,
        )
        .expect("commit trusted completion with a valid host footer");

        fs::write(
            repo.join("REVIEW.md"),
            format!(
                "## `{task_id}`\n\nInitial handoff.\n\n## `{task_id}`: independent review findings\n- Source: regression test.\n\n1. `src/risk.rs`: unresolved current-tree risk.\n"
            ),
        )
        .expect("append standing finding without changing owned inputs");
        let logger = ParallelEventLogger::new(&run_root).expect("initialize logger");

        let (updated, _) = audit_parallel_completion_drift(&repo, "main", &plan, &logger)
            .await
            .expect("drift audit should be bounded");

        assert_eq!(
            updated, plan,
            "a review finding is triage evidence, not authority to rewrite queue truth"
        );
        let triage =
            fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).expect("finding should be triaged");
        assert!(
            triage.contains(task_id) && triage.contains("unresolved REVIEW.md finding"),
            "{triage}"
        );
        let live_log = fs::read_to_string(run_root.join("live.log")).expect("read live log");
        assert!(
            !live_log.contains("trusted 1 completed task(s)"),
            "task with a standing finding must not take the trusted early return: {live_log}"
        );

        fs::remove_dir_all(&repo).expect("remove repo");
        fs::remove_dir_all(&run_root).expect("remove run root");
    }

    fn review_gate_assignment(
        root: &std::path::Path,
        task_id: &str,
        title: &str,
    ) -> ActiveLaneAssignment {
        review_gate_assignment_with_markdown(
            root,
            task_id,
            title,
            format!("- [ ] `{task_id}` {title}\n"),
        )
    }

    fn review_gate_assignment_with_markdown(
        root: &std::path::Path,
        task_id: &str,
        title: &str,
        markdown: String,
    ) -> ActiveLaneAssignment {
        ActiveLaneAssignment {
            lane_index: 3,
            attempts: 1,
            task: LoopTask {
                id: task_id.to_string(),
                title: title.to_string(),
                status: LoopTaskStatus::Done,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown,
            },
            resumed: false,
            lane_root: root.join(".auto/lane-review-root"),
            lane_repo_root: root.join(".auto/lane-review-repo"),
            base_commit: "0000000000000000000000000000000000000000".to_string(),
            stdout_log_path: root.join(".auto/lane-review.stdout.log"),
            stderr_log_path: root.join(".auto/lane-review.stderr.log"),
            worker_pid_path: root.join(".auto/lane-review.worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        }
    }

    fn seed_cargo_workspace_with_test(
        root: &std::path::Path,
        package_name: &str,
        test_name: &str,
        passes: bool,
    ) {
        fs::create_dir_all(root.join("src")).expect("create src dir");
        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nmembers = [\".\"]\n\n[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
            ),
        )
        .expect("write Cargo.toml");
        let assertion = if passes {
            ""
        } else {
            "panic!(\"current tree is still red\");"
        };
        fs::write(
            root.join("src/lib.rs"),
            format!(
                "#[cfg(test)]\nmod tests {{\n    #[test]\n    fn {test_name}() {{\n        {assertion}\n    }}\n}}\n"
            ),
        )
        .expect("write src/lib.rs");
    }

    fn write_fake_passing_workspace_cargo(root: &Path, mutation: &str) -> PathBuf {
        let path = root.join("fake-workspace-cargo.sh");
        fs::write(
            &path,
            format!("#!/usr/bin/env bash\nset -euo pipefail\n{mutation}\nexit 0\n"),
        )
        .expect("write fake workspace cargo");
        let mut permissions = fs::metadata(&path)
            .expect("stat fake workspace cargo")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod fake workspace cargo");
        path
    }

    #[tokio::test]
    async fn passing_host_verify_that_stages_queue_state_aborts_and_quarantines() {
        let root = unique_temp_dir("verify-queue-mutation");
        init_git_repo(&root);
        let task_id = "TASK-VERIFY-QUEUE-MUTATION";
        let title = "verify cannot mutate host queue state";
        let task_markdown = format!(
            "- [x] `{task_id}` {title}\nVerification:\n  - `bash -c true`\nDependencies: none\n"
        );
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!("# IMPLEMENTATION_PLAN\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(root.join("AGENTS.md"), "original authority\n").expect("write authority file");
        fs::create_dir_all(root.join("scripts")).expect("create scripts");
        let wrapper = root.join("scripts/run-task-verification.sh");
        fs::write(
            &wrapper,
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf 'mutated by verifier\\n' > AGENTS.md\ngit add AGENTS.md\nshift\nif [[ ${1:-} == \"--\" ]]; then shift; fi\nexec \"$@\"\n",
        )
        .expect("write mutating verification wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        git_ok(&root, ["add", "."]);
        git_ok(
            &root,
            ["commit", "-q", "-m", "seed verify mutation fixture"],
        );

        let mut assignment =
            review_gate_assignment_with_markdown(&root, task_id, title, task_markdown);
        let transaction = arm_canonical_gate_transaction(&root, task_id, "definition-of-done")
            .expect("arm outer source transaction");

        let error = super::apply_lane_verify_gate_in_transaction(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            Some(&transaction),
        )
        .await
        .expect_err("passing verifier queue mutation must abort before gate outcome handling");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(REVIEW_INPUT_MUTATION_FATAL_MARKER),
            "{rendered}"
        );
        assert!(
            fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md"))
                .expect("read plan")
                .contains(&format!("- [x] `{task_id}` {title}")),
            "fatal containment must run before any host demotion/closeout write"
        );
        let staged = run_git_in(&root, ["diff", "--cached", "--name-only"]);
        assert!(staged.lines().any(|path| path == "AGENTS.md"), "{staged}");
        let restart_error = enforce_review_input_quarantine_before_dispatch(&root)
            .expect_err("exact queue mutation must remain quarantined across restart");
        assert!(
            format!("{restart_error:#}").contains(REVIEW_INPUT_MUTATION_FATAL_MARKER),
            "{restart_error:#}"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[tokio::test]
    async fn passing_strict_workspace_that_stages_target_checkbox_aborts_and_quarantines() {
        let root = unique_temp_dir("strict-workspace-queue-mutation");
        init_git_repo(&root);
        let task_id = "TASK-STRICT-QUEUE-MUTATION";
        let title = "strict workspace cannot mutate target checkbox";
        let task_markdown = format!("- [x] `{task_id}` {title}\n");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!("# IMPLEMENTATION_PLAN\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("write manifest");
        git_ok(&root, ["add", "."]);
        git_ok(
            &root,
            ["commit", "-q", "-m", "seed strict mutation fixture"],
        );
        let cargo = write_fake_passing_workspace_cargo(
            &root,
            "sed -i 's/- \\[x\\]/- [~]/' IMPLEMENTATION_PLAN.md\ngit add IMPLEMENTATION_PLAN.md",
        );

        let mut assignment =
            review_gate_assignment_with_markdown(&root, task_id, title, task_markdown);
        let transaction = arm_canonical_gate_transaction(&root, task_id, "definition-of-done")
            .expect("arm outer source transaction");
        let error = super::apply_workspace_test_gate_mode_in_transaction(
            &root,
            &mut assignment,
            &[],
            LoopTaskStatus::Done,
            Some(&transaction),
            WorkspaceGateMode::Strict,
            Some(cargo),
        )
        .await
        .expect_err("passing strict workspace queue mutation must abort before outcome handling");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(REVIEW_INPUT_MUTATION_FATAL_MARKER),
            "{rendered}"
        );
        assert!(
            fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md"))
                .expect("read plan")
                .contains(&format!("- [~] `{task_id}` {title}")),
            "fixture must prove the target checkbox itself was not normalized"
        );
        let restart_error = enforce_review_input_quarantine_before_dispatch(&root)
            .expect_err("exact target-checkbox mutation must remain quarantined");
        assert!(
            format!("{restart_error:#}").contains(REVIEW_INPUT_MUTATION_FATAL_MARKER),
            "{restart_error:#}"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[tokio::test]
    async fn passing_baseline_workspace_that_stages_agents_aborts_and_quarantines() {
        let root = unique_temp_dir("baseline-workspace-queue-mutation");
        init_git_repo(&root);
        let task_id = "TASK-BASELINE-QUEUE-MUTATION";
        let title = "baseline workspace cannot mutate host authority";
        let task_markdown = format!("- [x] `{task_id}` {title}\n");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!("# IMPLEMENTATION_PLAN\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(root.join("AGENTS.md"), "original authority\n").expect("write authority");
        fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("write manifest");
        git_ok(&root, ["add", "."]);
        git_ok(
            &root,
            ["commit", "-q", "-m", "seed baseline mutation fixture"],
        );
        let cargo = write_fake_passing_workspace_cargo(
            &root,
            "printf 'mutated by workspace\\n' > AGENTS.md\ngit add AGENTS.md",
        );

        let mut assignment =
            review_gate_assignment_with_markdown(&root, task_id, title, task_markdown);
        let transaction = arm_canonical_gate_transaction(&root, task_id, "definition-of-done")
            .expect("arm outer source transaction");
        let error = super::apply_workspace_test_gate_mode_in_transaction(
            &root,
            &mut assignment,
            &[],
            LoopTaskStatus::Done,
            Some(&transaction),
            WorkspaceGateMode::Baseline,
            Some(cargo),
        )
        .await
        .expect_err("passing baseline workspace queue mutation must abort before baseline update");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(REVIEW_INPUT_MUTATION_FATAL_MARKER),
            "{rendered}"
        );
        assert_eq!(assignment.task.status, LoopTaskStatus::Done);
        let staged = run_git_in(&root, ["diff", "--cached", "--name-only"]);
        assert!(staged.lines().any(|path| path == "AGENTS.md"), "{staged}");
        let restart_error = enforce_review_input_quarantine_before_dispatch(&root)
            .expect_err("exact authority mutation must remain quarantined");
        assert!(
            format!("{restart_error:#}").contains(REVIEW_INPUT_MUTATION_FATAL_MARKER),
            "{restart_error:#}"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[tokio::test]
    async fn empty_diff_standing_review_clears_after_current_tree_gates_pass() {
        let root = unique_temp_dir("empty-diff-standing-review-pass");
        init_git_repo(&root);
        let package_name = "dod_fixture_pass";
        seed_cargo_workspace_with_test(&root, package_name, "task_dod_pass", true);
        let task_id = "TASK-DOD-PASS";
        let title = "standing finding already fixed";
        let task_markdown = format!(
            "- [x] `{task_id}` {title}\nVerification:\n  - `cargo test -q -p {package_name} tests::task_dod_pass -- --exact`\nDependencies: none\n"
        );
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!("# IMPLEMENTATION_PLAN\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(
            root.join("REVIEW.md"),
            format!(
                "# REVIEW\n\n## `{task_id}`: independent review findings\n- Source: auto parallel independent diff-review gate (held at `[~]`).\n\n1. `src/lib.rs`: old failure now fixed in the current tree.\n"
            ),
        )
        .expect("write review");
        fs::create_dir_all(root.join("scripts")).expect("create scripts");
        let wrapper = root.join("scripts/run-task-verification.sh");
        fs::write(
            &wrapper,
            format!(
                r#"#!/bin/sh
task="$1"
shift
if [ "${{1:-}}" = "--" ]; then shift; fi
mkdir -p .auto/symphony/verification-receipts
printf '%s\n' '{{"task_id":"{task_id}","commands":[{{"command":"cargo test -q -p {package_name} tests::task_dod_pass -- --exact","argv":["cargo","test","-q","-p","{package_name}","tests::task_dod_pass","--","--exact"],"expected_argv":["cargo","test","-q","-p","{package_name}","tests::task_dod_pass","--","--exact"],"exit_code":0,"status":"passed"}}]}}' > ".auto/symphony/verification-receipts/$task.json"
exec "$@"
"#
            ),
        )
        .expect("write verification wrapper");
        let mut wrapper_permissions = fs::metadata(&wrapper).expect("stat wrapper").permissions();
        wrapper_permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, wrapper_permissions).expect("chmod wrapper");
        let fake_reviewer = write_fake_clean_reviewer(&root);
        git_ok(&root, ["add", "."]);
        git_ok(&root, ["commit", "-q", "-m", "seed passing current tree"]);

        let mut assignment =
            review_gate_assignment_with_markdown(&root, task_id, title, task_markdown);
        assignment.lane_root = root.join("run/lanes/lane-3");
        fs::create_dir_all(&assignment.lane_root).expect("create canonical lane root");
        let review_config = LaneReviewConfig {
            model: "unused".to_string(),
            reasoning_effort: "unused".to_string(),
            codex_bin: fake_reviewer,
        };

        let mut status =
            super::apply_lane_verify_gate(&root, &mut assignment, LoopTaskStatus::Done)
                .await
                .expect("verify gate");
        assert_eq!(status, LoopTaskStatus::Done);
        status = super::apply_workspace_test_gate(&root, &mut assignment, &[], status)
            .await
            .expect("workspace gate");
        assert_eq!(status, LoopTaskStatus::Done);
        status = super::apply_lane_review_gate(
            &root,
            "main",
            &mut assignment,
            &[],
            status,
            &review_config,
        )
        .await
        .expect("review gate");

        assert_eq!(status, LoopTaskStatus::Done);
        assert_eq!(assignment.task.status, LoopTaskStatus::Done);
        let review = fs::read_to_string(root.join("REVIEW.md")).expect("read review");
        assert!(review.contains("standing review cleared"));
        assert!(
            unresolved_review_findings_for_task(&review, task_id).is_empty(),
            "clearance should supersede the old finding: {review}"
        );
        let staged = run_git_in(&root, ["diff", "--cached", "--name-only"]);
        assert!(staged.contains("REVIEW.md"), "REVIEW.md staged: {staged}");
        let log = fs::read_to_string(&assignment.stdout_log_path).expect("closeout log");
        assert!(log.contains("host-reexec-verify: declared verification re-passed"));
        assert!(log.contains("workspace-baseline: promoted [x]"));
        assert!(log.contains("independent-review: clean"));

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[tokio::test]
    async fn empty_diff_standing_review_stays_partial_when_current_tree_verify_fails() {
        let root = unique_temp_dir("empty-diff-standing-review-fail");
        init_git_repo(&root);
        let package_name = "dod_fixture_fail";
        seed_cargo_workspace_with_test(&root, package_name, "task_dod_fail", false);
        let task_id = "TASK-DOD-FAIL";
        let title = "standing finding still fails";
        let task_markdown = format!(
            "- [x] `{task_id}` {title}\nVerification:\n  - `cargo test -q -p {package_name} tests::task_dod_fail -- --exact`\nDependencies: none\n"
        );
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!("# IMPLEMENTATION_PLAN\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::write(
            root.join("REVIEW.md"),
            format!(
                "# REVIEW\n\n## `{task_id}`: independent review findings\n- Source: auto parallel independent diff-review gate (held at `[~]`).\n\n1. `src/lib.rs`: failure remains red in the current tree.\n"
            ),
        )
        .expect("write review");
        git_ok(&root, ["add", "."]);
        git_ok(&root, ["commit", "-q", "-m", "seed failing current tree"]);

        let mut assignment =
            review_gate_assignment_with_markdown(&root, task_id, title, task_markdown);
        let status = super::apply_lane_verify_gate(&root, &mut assignment, LoopTaskStatus::Done)
            .await
            .expect("verify gate");

        assert_eq!(status, LoopTaskStatus::Partial);
        assert_eq!(assignment.task.status, LoopTaskStatus::Partial);
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(
            plan.contains(&format!("- [~] `{task_id}` {title}")),
            "plan should stay partial when current-tree verification fails: {plan}"
        );
        let review = fs::read_to_string(root.join("REVIEW.md")).expect("read review");
        assert!(!review.contains("standing review cleared"));
        assert!(
            !unresolved_review_findings_for_task(&review, task_id).is_empty(),
            "standing finding must remain unresolved: {review}"
        );
        assert!(review.contains("host re-execution verification failed"));
        let staged = run_git_in(&root, ["diff", "--cached", "--name-only"]);
        assert!(staged.contains("REVIEW.md"), "REVIEW.md staged: {staged}");
        assert!(
            staged.contains("IMPLEMENTATION_PLAN.md"),
            "plan staged: {staged}"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[tokio::test]
    async fn workspace_baseline_missing_run_root_fails_closed() {
        let root = unique_temp_dir("workspace-baseline-missing-run-root");
        init_git_repo(&root);
        let task_id = "TASK-WORKSPACE-SKIP";
        let title = "workspace gate must be observed";
        seed_cargo_workspace_with_test(&root, "workspace_skip_fixture", "green", true);
        let task_markdown = format!("- [x] `{task_id}` {title}\nVerification:\n  - `cargo test`\n");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!("# IMPLEMENTATION_PLAN\n\n{task_markdown}"),
        )
        .expect("write plan");
        git_ok(&root, ["add", "."]);
        git_ok(&root, ["commit", "-q", "-m", "seed green workspace"]);

        let mut assignment =
            review_gate_assignment_with_markdown(&root, task_id, title, task_markdown);
        let status =
            super::apply_workspace_baseline_gate(&root, &mut assignment, &[], LoopTaskStatus::Done)
                .await
                .expect("workspace baseline gate");

        assert_eq!(
            status,
            LoopTaskStatus::Partial,
            "missing baseline run identity is a skipped gate, not a pass"
        );
        assert!(task_is_gate_held(&root, task_id).expect("read holds"));
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(
            plan.contains(&format!("- [~] `{task_id}` {title}")),
            "{plan}"
        );
        let review = fs::read_to_string(root.join("REVIEW.md")).expect("read review");
        assert!(review.contains("workspace cargo test skipped"), "{review}");
        assert!(review.contains("no lane run root"), "{review}");

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[tokio::test]
    async fn unavailable_prelanding_baseline_never_absorbs_unchanged_red_retry() {
        let root = unique_temp_dir("workspace-baseline-red-retry");
        init_git_repo(&root);
        let task_id = "TASK-WORKSPACE-RED-RETRY";
        let title = "red retry cannot become its own baseline";
        seed_cargo_workspace_with_test(&root, "workspace_red_retry", "still_red", false);
        let task_markdown = format!("- [x] `{task_id}` {title}\nVerification:\n  - `cargo test`\n");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!("# IMPLEMENTATION_PLAN\n\n{task_markdown}"),
        )
        .expect("write plan");
        git_ok(&root, ["add", "."]);
        git_ok(&root, ["commit", "-q", "-m", "seed red workspace"]);

        let run_root = root.join("run");
        let lane_root = run_root.join("lanes/lane-3");
        fs::create_dir_all(&lane_root).expect("create canonical lane root");
        let mut assignment =
            review_gate_assignment_with_markdown(&root, task_id, title, task_markdown);
        assignment.lane_root = lane_root;

        for attempt in 1..=2 {
            assignment.task.status = LoopTaskStatus::Done;
            let status = super::apply_workspace_baseline_gate(
                &root,
                &mut assignment,
                &[],
                LoopTaskStatus::Done,
            )
            .await
            .expect("workspace baseline gate");
            assert_eq!(
                status,
                LoopTaskStatus::Partial,
                "unchanged red attempt {attempt} must remain held"
            );
            assert!(
                !load_workspace_baseline(&run_root).captured,
                "a post-landing red tree must never become the trusted baseline"
            );
        }

        let review = fs::read_to_string(root.join("REVIEW.md")).expect("read review");
        assert!(
            review.contains("refusing to learn a tolerated baseline from this red tree"),
            "{review}"
        );
        assert!(task_is_gate_held(&root, task_id).expect("read holds"));

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn review_clean_outcome_keeps_done_and_stamps_clean() {
        let root = unique_temp_dir("review-gate-clean");
        init_git_repo(&root);
        let mut assignment =
            review_gate_assignment(&root, "TASK-CLEAN-1", "clean diff lands as done");
        let status = apply_lane_review_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            LaneReviewOutcome::Clean,
        )
        .expect("apply clean review");
        assert_eq!(status, LoopTaskStatus::Done);
        assert_eq!(assignment.task.status, LoopTaskStatus::Done);
        let log = fs::read_to_string(&assignment.stdout_log_path).expect("closeout log written");
        assert!(log.contains("independent-review: clean"));
        // Clean review must not create or touch REVIEW.md.
        assert!(!root.join("REVIEW.md").exists());
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn review_findings_outcome_demotes_done_and_appends_review_md() {
        let root = unique_temp_dir("review-gate-findings");
        init_git_repo(&root);
        // Seed a committed plan marking the task [x] so the demote has a row to flip.
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n- [x] `TASK-FND-1` findings should demote this\n",
        )
        .expect("write plan");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&root, ["commit", "-q", "-m", "seed plan"]);

        let mut assignment =
            review_gate_assignment(&root, "TASK-FND-1", "findings should demote this");
        let status = apply_lane_review_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            LaneReviewOutcome::FindingsKeepPartial {
                findings_summary: "1. `src/x.rs`: real bug introduced by this diff.".to_string(),
            },
        )
        .expect("apply review findings");
        // Status downgraded.
        assert_eq!(status, LoopTaskStatus::Partial);
        assert_eq!(assignment.task.status, LoopTaskStatus::Partial);
        // Plan row demoted [x] -> [~].
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).expect("plan readable");
        assert!(
            plan.contains("- [~] `TASK-FND-1` findings should demote this"),
            "plan should demote done row: {plan}"
        );
        // REVIEW.md got the findings under a marked block.
        let review = fs::read_to_string(root.join("REVIEW.md")).expect("review written");
        assert!(review.contains("## `TASK-FND-1`: independent review findings"));
        assert!(review.contains("real bug introduced by this diff"));
        // REVIEW.md + plan staged for the closeout commit.
        let staged = run_git_in(&root, ["diff", "--cached", "--name-only"]);
        assert!(staged.contains("REVIEW.md"), "REVIEW.md staged: {staged}");
        assert!(
            staged.contains("IMPLEMENTATION_PLAN.md"),
            "plan staged: {staged}"
        );
        // Closeout log stamped.
        let log = fs::read_to_string(&assignment.stdout_log_path).expect("closeout log");
        assert!(log.contains("independent-review: actionable findings recorded"));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn review_skipped_outcome_demotes_done_and_records_review_md() {
        let root = unique_temp_dir("review-gate-skipped");
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n- [x] `TASK-SKIP-1` review error stays partial\n",
        )
        .expect("write plan");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&root, ["commit", "-q", "-m", "seed plan"]);
        let mut assignment =
            review_gate_assignment(&root, "TASK-SKIP-1", "review error stays partial");
        let status = apply_lane_review_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            LaneReviewOutcome::SkippedFailOpen {
                reason: "review timed out after 900s".to_string(),
            },
        )
        .expect("apply skipped review");
        assert_eq!(status, LoopTaskStatus::Partial);
        assert_eq!(assignment.task.status, LoopTaskStatus::Partial);
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).expect("plan readable");
        assert!(
            plan.contains("- [~] `TASK-SKIP-1` review error stays partial"),
            "plan should demote done row: {plan}"
        );
        let review = fs::read_to_string(root.join("REVIEW.md")).expect("review written");
        assert!(review.contains("## `TASK-SKIP-1`: independent review findings"));
        assert!(review.contains("review timed out after 900s"));
        let log = fs::read_to_string(&assignment.stdout_log_path).expect("closeout log");
        assert!(log.contains("review_skipped: review timed out after 900s"));
        let staged = run_git_in(&root, ["diff", "--cached", "--name-only"]);
        assert!(staged.contains("REVIEW.md"), "REVIEW.md staged: {staged}");
        assert!(
            staged.contains("IMPLEMENTATION_PLAN.md"),
            "plan staged: {staged}"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn review_input_mutation_aborts_before_queue_closeout() {
        let root = unique_temp_dir("review-gate-input-mutation");
        init_git_repo(&root);
        let mut assignment =
            review_gate_assignment(&root, "TASK-MUTATION-1", "reviewer mutation is fatal");
        let error = apply_lane_review_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            LaneReviewOutcome::InputMutationFatal {
                reason: "reviewer moved canonical HEAD".to_string(),
            },
        )
        .expect_err("reviewer mutation must abort landing");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("refusing closeout or remote push"),
            "{rendered}"
        );
        assert!(
            !root.join("REVIEW.md").exists(),
            "fatal reviewer mutation must not be converted into a queue update"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn verify_all_passed_outcome_keeps_done_and_stamps() {
        let root = unique_temp_dir("verify-gate-pass");
        init_git_repo(&root);
        let mut assignment =
            review_gate_assignment(&root, "TASK-VOK-1", "host re-run green lands done");
        record_gate_hold(&root, "TASK-VOK-1", "later gate still unresolved").expect("record hold");
        let status = apply_lane_verify_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            LaneVerifyOutcome::AllPassed,
        )
        .expect("apply passing verification");
        assert_eq!(status, LoopTaskStatus::Done);
        assert_eq!(assignment.task.status, LoopTaskStatus::Done);
        let log = fs::read_to_string(&assignment.stdout_log_path).expect("closeout log");
        assert!(log.contains("host-reexec-verify: declared verification re-passed"));
        assert!(!root.join("REVIEW.md").exists());
        assert!(
            task_is_gate_held(&root, "TASK-VOK-1").expect("read holds"),
            "one green verify gate must not clear a hold owned by a later gate"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn verify_failed_outcome_demotes_done_and_records_failure() {
        let root = unique_temp_dir("verify-gate-fail");
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n- [x] `TASK-VFAIL-1` host re-run must demote this\n",
        )
        .expect("write plan");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&root, ["commit", "-q", "-m", "seed plan"]);

        let mut assignment =
            review_gate_assignment(&root, "TASK-VFAIL-1", "host re-run must demote this");
        let status = apply_lane_verify_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            LaneVerifyOutcome::Failed {
                detail: "`cargo test foo` exited with status 101\nthread 'foo' panicked"
                    .to_string(),
            },
        )
        .expect("apply failed verification");
        assert_eq!(status, LoopTaskStatus::Partial);
        assert_eq!(assignment.task.status, LoopTaskStatus::Partial);
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).expect("plan readable");
        assert!(
            plan.contains("- [~] `TASK-VFAIL-1` host re-run must demote this"),
            "plan should demote done row: {plan}"
        );
        let review = fs::read_to_string(root.join("REVIEW.md")).expect("review written");
        assert!(review.contains("## `TASK-VFAIL-1`: host re-execution verification failed"));
        assert!(review.contains("cargo test foo"));
        let staged = run_git_in(&root, ["diff", "--cached", "--name-only"]);
        assert!(staged.contains("REVIEW.md"), "REVIEW.md staged: {staged}");
        assert!(
            staged.contains("IMPLEMENTATION_PLAN.md"),
            "plan staged: {staged}"
        );
        let log = fs::read_to_string(&assignment.stdout_log_path).expect("closeout log");
        assert!(log.contains("host-reexec-verify: a declared verification command FAILED"));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn failed_gate_cannot_close_out_a_stale_staged_done_row() {
        let root = unique_temp_dir("verify-gate-stale-index");
        init_git_repo(&root);
        let task_id = "TASK-VFAIL-STALE";
        let title = "stale staged done must not commit";
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!("# IMPLEMENTATION_PLAN\n\n- [~] `{task_id}` {title}\n"),
        )
        .expect("write partial seed plan");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&root, ["commit", "-q", "-m", "seed partial plan"]);
        let head_before = git_output(&root, ["rev-parse", "HEAD"]);

        // Reconciliation has staged [x], but the subsequent host gate fails.
        // Hold the Git index lock across failure persistence. The gate must
        // return an error before closeout even if neither plan view can be
        // demoted from the dangerous staged [x].
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!("# IMPLEMENTATION_PLAN\n\n- [x] `{task_id}` {title}\n"),
        )
        .expect("write reconciled done plan");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        let index_lock = root.join(".git/index.lock");
        fs::write(&index_lock, "held by test\n").expect("hold index lock");

        let mut assignment = review_gate_assignment(&root, task_id, title);
        let gate_error = apply_lane_verify_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            LaneVerifyOutcome::Failed {
                detail: "current-tree verification failed".to_string(),
            },
        )
        .expect_err("an active index lock must fail closed during gate demotion");
        fs::remove_file(&index_lock).expect("release index lock");

        assert!(
            format!("{gate_error:#}").contains("index.lock"),
            "{gate_error:#}"
        );
        let worktree =
            fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).expect("read worktree plan");
        assert!(
            worktree.contains(&format!("- [x] `{task_id}` {title}")),
            "failed persistence may leave worktree Done, but must never close out: {worktree}"
        );
        let indexed = run_git_in(&root, ["show", ":IMPLEMENTATION_PLAN.md"]);
        assert!(
            indexed.contains(&format!("- [x] `{task_id}` {title}")),
            "test must retain the stale staged done row: {indexed}"
        );

        let error = commit_task_closeout(
            &root,
            task_id,
            LoopTaskStatus::Partial,
            "queue sync must fail",
            false,
        )
        .expect_err("closeout must refuse a stale staged [x]");
        let detail = format!("{error:#}");
        assert!(detail.contains("refusing closeout"), "{detail}");
        assert!(detail.contains("worktree Done"), "{detail}");
        assert!(detail.contains("index Done"), "{detail}");
        assert_eq!(head_before, git_output(&root, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn task_closeout_refuses_another_tasks_unsealed_done_transition() {
        let root = unique_temp_dir("closeout-cross-task-unsealed-done");
        init_git_repo(&root);
        fs::create_dir_all(root.join("scripts")).expect("create scripts");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("create receipts");
        fs::write(root.join(".gitignore"), ".auto/\n").expect("write gitignore");
        fs::write(
            root.join("scripts/run-task-verification.sh"),
            "#!/bin/sh\nexit 0\n",
        )
        .expect("write verification wrapper");
        let partial_plan = "\
# IMPLEMENTATION_PLAN

- [~] `TASK-A` First candidate
  Verification:
    - `cargo test task_a`
  Dependencies: none

- [~] `TASK-B` Footer owner
  Verification:
    - `cargo test task_b`
  Dependencies: none
";
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), partial_plan).expect("write partial plan");
        git_ok(
            &root,
            [
                "add",
                ".gitignore",
                "scripts/run-task-verification.sh",
                "IMPLEMENTATION_PLAN.md",
            ],
        );
        git_ok(&root, ["commit", "-q", "-m", "seed partial tasks"]);
        let head_before = git_output(&root, ["rev-parse", "HEAD"]);

        let both_done = partial_plan
            .replace("- [~] `TASK-A`", "- [x] `TASK-A`")
            .replace("- [~] `TASK-B`", "- [x] `TASK-B`");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), &both_done)
            .expect("write both candidate Done rows");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-B.json"),
            r#"{"task_id":"TASK-B","commands":[{"command":"cargo test task_b","argv":["cargo","test","task_b"],"expected_argv":["cargo","test","task_b"],"exit_code":0,"status":"passed"}]}"#,
        )
        .expect("write TASK-B receipt");
        let task_b = parse_loop_plan(&both_done)
            .task("TASK-B")
            .expect("TASK-B should parse")
            .clone();
        propagate_lane_receipts(&root, &root, "TASK-B", &task_b.markdown)
            .expect("bind receipt to current source");
        record_verified_source_attestation(&root, "TASK-B").expect("attest TASK-B current source");

        let error = commit_task_closeout(
            &root,
            "TASK-B",
            LoopTaskStatus::Done,
            "repo: TASK-B queue sync",
            false,
        )
        .expect_err("TASK-B closeout must not absorb TASK-A completion");
        let detail = format!("{error:#}");
        assert!(detail.contains("TASK-A"), "{detail}");
        assert!(
            detail.contains("unsealed") || detail.contains("other task"),
            "{detail}"
        );
        assert_eq!(head_before, git_output(&root, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn task_closeout_refuses_verified_but_uncommitted_source_state() {
        let root = unique_temp_dir("closeout-uncommitted-verified-source");
        init_git_repo(&root);
        fs::create_dir_all(root.join("scripts")).expect("create scripts");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("create receipts");
        fs::write(root.join(".gitignore"), ".auto/\n").expect("write gitignore");
        fs::write(
            root.join("scripts/run-task-verification.sh"),
            "#!/bin/sh\nexit 0\n",
        )
        .expect("write verification wrapper");
        let partial_plan = "\
# IMPLEMENTATION_PLAN

- [~] `TASK-B` Footer owner
  Verification:
    - `cargo test task_b`
  Dependencies: none
";
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), partial_plan).expect("write partial plan");
        git_ok(
            &root,
            [
                "add",
                ".gitignore",
                "scripts/run-task-verification.sh",
                "IMPLEMENTATION_PLAN.md",
            ],
        );
        git_ok(&root, ["commit", "-q", "-m", "seed partial task"]);
        let head_before = git_output(&root, ["rev-parse", "HEAD"]);

        let done_plan = partial_plan.replace("- [~] `TASK-B`", "- [x] `TASK-B`");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), &done_plan).expect("write candidate Done");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        fs::write(
            root.join("src/verifier_injected.rs"),
            "pub fn injected() {}\n",
        )
        .expect("write uncommitted verifier mutation");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-B.json"),
            r#"{"task_id":"TASK-B","commands":[{"command":"cargo test task_b","argv":["cargo","test","task_b"],"expected_argv":["cargo","test","task_b"],"exit_code":0,"status":"passed"}]}"#,
        )
        .expect("write TASK-B receipt");
        let task_b = parse_loop_plan(&done_plan)
            .task("TASK-B")
            .expect("TASK-B should parse")
            .clone();
        propagate_lane_receipts(&root, &root, "TASK-B", &task_b.markdown)
            .expect("bind receipt to dirty current source");
        record_verified_source_attestation(&root, "TASK-B").expect("attest dirty current source");

        let error = commit_task_closeout(
            &root,
            "TASK-B",
            LoopTaskStatus::Done,
            "repo: TASK-B queue sync",
            false,
        )
        .expect_err("closeout must refuse verified but uncommitted source");
        let detail = format!("{error:#}");
        assert!(detail.contains("src/verifier_injected.rs"), "{detail}");
        assert!(
            detail.contains("dirty") || detail.contains("outside"),
            "{detail}"
        );
        assert_eq!(head_before, git_output(&root, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn task_closeout_uses_exact_index_tree_without_running_commit_hooks() {
        let root = unique_temp_dir("closeout-isolated-hooks");
        init_git_repo(&root);
        let plan = "\
# IMPLEMENTATION_PLAN

- [~] `TASK-TARGET` Partial queue closeout
  Dependencies: none

- [ ] `TASK-OTHER` Hook must not complete this
  Dependencies: none
";
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), plan).expect("write plan");
        fs::write(root.join("REVIEW.md"), "# REVIEW\n").expect("write review");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"]);
        git_ok(&root, ["commit", "-q", "-m", "seed closeout"]);
        let parent = git_output(&root, ["rev-parse", "HEAD"]);

        let pre_commit = root.join(".git/hooks/pre-commit");
        fs::write(
            &pre_commit,
            "#!/bin/sh\n\
             mkdir -p src\n\
             printf '%s\\n' 'pub fn injected() {}' > src/hook_injected.rs\n\
             printf '%s\\n' ran > pre-commit-ran\n\
             git add src/hook_injected.rs pre-commit-ran\n",
        )
        .expect("write hostile pre-commit hook");
        let prepare_commit_msg = root.join(".git/hooks/prepare-commit-msg");
        fs::write(
            &prepare_commit_msg,
            "#!/bin/sh\n\
             sed 's/- \\[ \\] `TASK-OTHER`/- [x] `TASK-OTHER`/' \
             IMPLEMENTATION_PLAN.md > IMPLEMENTATION_PLAN.md.hook\n\
             mv IMPLEMENTATION_PLAN.md.hook IMPLEMENTATION_PLAN.md\n\
             printf '%s\\n' ran > prepare-commit-msg-ran\n\
             git add IMPLEMENTATION_PLAN.md prepare-commit-msg-ran\n",
        )
        .expect("write hostile prepare-commit-msg hook");
        for hook in [&pre_commit, &prepare_commit_msg] {
            let mut permissions = fs::metadata(hook).expect("stat hook").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(hook, permissions).expect("make hook executable");
        }

        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nPartial task remains held.\n",
        )
        .expect("update review");
        git_ok(&root, ["add", "REVIEW.md"]);
        let intended_tree = git_output(&root, ["write-tree"]);

        commit_task_closeout(
            &root,
            "TASK-TARGET",
            LoopTaskStatus::Partial,
            "repo: TASK-TARGET queue sync",
            false,
        )
        .expect("isolated task closeout should succeed");

        assert_eq!(git_output(&root, ["rev-parse", "HEAD^"]), parent);
        assert_eq!(
            git_output(&root, ["rev-parse", "HEAD^{tree}"]),
            intended_tree
        );
        assert_eq!(
            git_output(&root, ["log", "-1", "--format=%B"]),
            "repo: TASK-TARGET queue sync"
        );
        assert!(!root.join("src/hook_injected.rs").exists());
        assert!(!root.join("pre-commit-ran").exists());
        assert!(!root.join("prepare-commit-msg-ran").exists());
        let committed_plan = run_git_in(&root, ["show", "HEAD:IMPLEMENTATION_PLAN.md"]);
        assert!(committed_plan.contains("- [ ] `TASK-OTHER`"));
        assert!(!committed_plan.contains("- [x] `TASK-OTHER`"));

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn verify_skipped_outcome_demotes_done_and_records_failure() {
        let root = unique_temp_dir("verify-gate-skip");
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n- [x] `TASK-VSKIP-1` host re-run skipped demotes this\n",
        )
        .expect("write plan");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&root, ["commit", "-q", "-m", "seed plan"]);

        let mut assignment =
            review_gate_assignment(&root, "TASK-VSKIP-1", "host re-run skipped demotes this");
        let status = apply_lane_verify_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            LaneVerifyOutcome::Skipped {
                reason: "no host-reproducible verification commands".to_string(),
            },
        )
        .expect("apply skipped verification");
        assert_eq!(status, LoopTaskStatus::Partial);
        assert_eq!(assignment.task.status, LoopTaskStatus::Partial);
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).expect("plan readable");
        assert!(
            plan.contains("- [~] `TASK-VSKIP-1` host re-run skipped demotes this"),
            "plan should demote done row: {plan}"
        );
        let review = fs::read_to_string(root.join("REVIEW.md")).expect("review written");
        assert!(review.contains("host re-execution verification skipped"));
        let staged = run_git_in(&root, ["diff", "--cached", "--name-only"]);
        assert!(staged.contains("REVIEW.md"), "REVIEW.md staged: {staged}");
        assert!(
            staged.contains("IMPLEMENTATION_PLAN.md"),
            "plan staged: {staged}"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn workspace_test_failed_outcome_demotes_done_and_records_failure() {
        let root = unique_temp_dir("workspace-gate-fail");
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n- [x] `TASK-WFAIL-1` workspace red demotes this\n",
        )
        .expect("write plan");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&root, ["commit", "-q", "-m", "seed plan"]);

        let mut assignment =
            review_gate_assignment(&root, "TASK-WFAIL-1", "workspace red demotes this");
        let status = apply_workspace_test_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            WorkspaceTestOutcome::Failed {
                detail: "`cargo test --workspace` exited with status 101\ncompile error"
                    .to_string(),
            },
        )
        .expect("apply failed workspace test");
        assert_eq!(status, LoopTaskStatus::Partial);
        assert_eq!(assignment.task.status, LoopTaskStatus::Partial);
        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).expect("plan readable");
        assert!(
            plan.contains("- [~] `TASK-WFAIL-1` workspace red demotes this"),
            "plan should demote done row: {plan}"
        );
        let review = fs::read_to_string(root.join("REVIEW.md")).expect("review written");
        assert!(review.contains("## `TASK-WFAIL-1`: workspace cargo test failed"));
        assert!(review.contains("compile error"));
        let staged = run_git_in(&root, ["diff", "--cached", "--name-only"]);
        assert!(staged.contains("REVIEW.md"), "REVIEW.md staged: {staged}");
        assert!(
            staged.contains("IMPLEMENTATION_PLAN.md"),
            "plan staged: {staged}"
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn workspace_test_not_applicable_preserves_done_without_recording_failure() {
        let root = unique_temp_dir("workspace-gate-not-applicable");
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n- [x] `TASK-WNA-1` non-Rust workspace passes through\n",
        )
        .expect("write plan");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);
        git_ok(&root, ["commit", "-q", "-m", "seed plan"]);

        let mut assignment =
            review_gate_assignment(&root, "TASK-WNA-1", "non-Rust workspace passes through");
        let status = apply_workspace_test_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            WorkspaceTestOutcome::NotApplicable {
                reason: "no Cargo.toml found; workspace cargo test gate is not applicable"
                    .to_string(),
            },
        )
        .expect("apply non-applicable workspace test");

        assert_eq!(status, LoopTaskStatus::Done);
        assert_eq!(assignment.task.status, LoopTaskStatus::Done);
        assert!(!root.join("REVIEW.md").exists());
        let log = fs::read_to_string(&assignment.stdout_log_path).expect("closeout log");
        assert!(log.contains("workspace-test: gate not applicable"));
        assert!(log.contains("pass-through"));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn owned_inputs_gate_matching_fingerprint_still_inspects_receipt() {
        // Stored == current narrows re-verification, but never bypasses receipt
        // identity/content/provenance inspection.
        assert_eq!(
            super::decide_owned_inputs(false, false, true, Some("abc"), Some("abc")),
            super::OwnedInputsDecision::FallThrough
        );
    }

    #[test]
    fn owned_inputs_gate_reverifies_on_mismatch() {
        // Own inputs changed -> re-verify even though a receipt exists.
        assert_eq!(
            super::decide_owned_inputs(false, false, true, Some("abc"), Some("def")),
            super::OwnedInputsDecision::ForceReverify
        );
        // Mismatch still forces re-verify for a sweep-excluded task (own inputs
        // changed is the one exception that re-runs it).
        assert_eq!(
            super::decide_owned_inputs(false, true, true, Some("abc"), Some("def")),
            super::OwnedInputsDecision::ForceReverify
        );
    }

    #[test]
    fn owned_inputs_gate_forced_bypasses_fingerprints() {
        // Forced full re-verify ignores a matching fingerprint.
        assert_eq!(
            super::decide_owned_inputs(true, false, true, Some("abc"), Some("abc")),
            super::OwnedInputsDecision::ForceReverify
        );
        // ...and even a sweep-excluded task.
        assert_eq!(
            super::decide_owned_inputs(true, true, true, Some("abc"), Some("abc")),
            super::OwnedInputsDecision::ForceReverify
        );
    }

    #[test]
    fn owned_inputs_gate_legacy_receipt_falls_back() {
        // No stamped fingerprint -> pre-existing evidence-freshness behavior.
        assert_eq!(
            super::decide_owned_inputs(false, false, true, None, Some("def")),
            super::OwnedInputsDecision::FallThrough
        );
    }

    #[test]
    fn owned_inputs_gate_hash_error_forces_reverify() {
        // Stamped but not recomputable (git/hash error) -> conservative change.
        assert_eq!(
            super::decide_owned_inputs(false, false, true, Some("abc"), None),
            super::OwnedInputsDecision::ForceReverify
        );
    }

    #[test]
    fn owned_inputs_gate_sweep_excluded_still_inspects_legacy_and_hash_error_receipts() {
        // Sweep exclusion may avoid a re-run after full evidence inspection,
        // but it never turns raw footer fields into trusted completion.
        assert_eq!(
            super::decide_owned_inputs(false, true, true, None, Some("def")),
            super::OwnedInputsDecision::FallThrough
        );
        assert_eq!(
            super::decide_owned_inputs(false, true, true, Some("abc"), None),
            super::OwnedInputsDecision::FallThrough
        );
        // But a sweep-excluded row with NO receipt footer at all is not blindly
        // trusted — it falls through to normal evidence inspection.
        assert_eq!(
            super::decide_owned_inputs(false, true, false, None, Some("def")),
            super::OwnedInputsDecision::FallThrough
        );
    }

    #[test]
    fn sweep_excluded_marker_is_discoverable_in_markdown() {
        assert!(super::task_is_sweep_excluded(
            "- [x] `T1` x\n  Verification:\n    - `cargo test` [sweep-excluded]\n"
        ));
        assert!(!super::task_is_sweep_excluded(
            "- [x] `T1` x\n  Verification:\n    - `cargo test`\n"
        ));
    }

    #[test]
    fn partial_closeout_commits_reported_plan_derived_file() {
        let root = unique_temp_dir("plan-update-hook");
        init_git_repo(&root);
        fs::create_dir_all(root.join("scripts")).expect("create scripts");
        fs::write(
            root.join("scripts/autodev-after-plan-update.sh"),
            "#!/usr/bin/env bash\nset -euo pipefail\ncp IMPLEMENTATION_PLAN.md derived-plan.txt\nprintf '%s\\n' derived-plan.txt\n",
        )
        .expect("write hook");
        fs::write(root.join("derived-plan.txt"), "# old plan\n").expect("write derived");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# Plan\n\n- [ ] `TASK-HOOK` refresh derived plan\nDependencies: none\n",
        )
        .expect("write seed plan");
        git_ok(
            &root,
            [
                "add",
                "scripts/autodev-after-plan-update.sh",
                "derived-plan.txt",
                "IMPLEMENTATION_PLAN.md",
            ],
        );
        git_ok(&root, ["commit", "-q", "-m", "seed hook"]);

        let partial = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md"))
            .expect("read plan")
            .replace("- [ ] `TASK-HOOK`", "- [~] `TASK-HOOK`");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), &partial).expect("write partial plan");
        git_ok(&root, ["add", "IMPLEMENTATION_PLAN.md"]);

        commit_task_closeout(
            &root,
            "TASK-HOOK",
            LoopTaskStatus::Partial,
            "repo: TASK-HOOK queue sync",
            false,
        )
        .expect("partial closeout should include derived plan file");

        assert_eq!(
            fs::read_to_string(root.join("derived-plan.txt")).expect("read derived"),
            partial
        );
        let committed = run_git_in(&root, ["show", "--name-only", "--format=", "HEAD"]);
        assert!(committed.contains("IMPLEMENTATION_PLAN.md"), "{committed}");
        assert!(committed.contains("derived-plan.txt"), "{committed}");
        fs::remove_dir_all(&root).expect("cleanup");
    }
}
