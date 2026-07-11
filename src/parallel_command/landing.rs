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

/// Local drift re-verify is on unless `AUTO_PARALLEL_DRIFT_REVERIFY=0`.
/// When completion evidence for a `[x]` row goes stale for a locally
/// repairable reason (receipt/plan/artifact freshness), re-running the row's
/// declared verification commands on the host is strictly cheaper than
/// demoting the row and re-dispatching it to a model-backed lane.
fn drift_local_reverify_enabled() -> bool {
    std::env::var("AUTO_PARALLEL_DRIFT_REVERIFY")
        .map(|value| value.trim() != "0")
        .unwrap_or(true)
}

/// Wall-clock budget for the whole local drift re-verify sweep within one audit
/// invocation (`AUTO_PARALLEL_DRIFT_REVERIFY_BUDGET_SECS`, default 900). Local
/// re-verification re-runs a task's real test commands, so a large stale set
/// could otherwise turn one audit into a serial test marathon that starves the
/// run. Once the budget is spent, remaining stale rows take the honest path
/// (Done -> demote to [~]; fully-evidenced Partial -> manual closeout) — a stale
/// row never silently stays [x] without either fresh receipts or demotion. `0`
/// disables the sweep entirely, reproducing pre-re-verify behavior.
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
    task_markdown.to_ascii_lowercase().contains("[sweep-excluded]")
}

/// Per-task decision from the owned-inputs gate for a completed `[x]` row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnedInputsDecision {
    /// Owned inputs are provably unchanged since the receipt was stamped —
    /// trust the receipt and skip inspection/re-verification entirely.
    SkipTrusted,
    /// Owned inputs definitively changed (or an error must be treated as change)
    /// — force re-verification even if the receipt still looks fresh.
    ForceReverify,
    /// No stamped fingerprint (legacy receipt) or nothing conclusive — fall back
    /// to the pre-existing evidence-freshness behavior.
    FallThrough,
}

/// Decide how a completed `[x]` task should be treated by the drift sweep, using
/// the stamped-vs-recomputed owned-inputs fingerprint.
///
/// - `forced`: bypass everything and re-verify (`ForceReverify`).
/// - `sweep_excluded` + a valid receipt: never re-run on mere staleness/legacy/
///   hash-error; only a definitive fingerprint mismatch (own inputs changed)
///   triggers re-verification.
/// - Otherwise: match ⇒ trust; mismatch or hash-error-with-a-stamp ⇒ re-verify;
///   no stamp (legacy) ⇒ fall back.
fn decide_owned_inputs(
    forced: bool,
    sweep_excluded: bool,
    has_receipt_footer: bool,
    stored_fp: Option<&str>,
    current_fp: Option<&str>,
) -> OwnedInputsDecision {
    if forced {
        return OwnedInputsDecision::ForceReverify;
    }
    match (stored_fp, current_fp) {
        (Some(stored), Some(current)) if stored == current => OwnedInputsDecision::SkipTrusted,
        (Some(_), Some(_)) => OwnedInputsDecision::ForceReverify, // own inputs changed
        (Some(_), None) => {
            // Stamped, but we could not recompute (git/hash error).
            if sweep_excluded {
                OwnedInputsDecision::SkipTrusted // trust the valid receipt
            } else {
                OwnedInputsDecision::ForceReverify // conservative: treat as changed
            }
        }
        (None, _) => {
            // Legacy receipt (no stamped fingerprint).
            if sweep_excluded && has_receipt_footer {
                OwnedInputsDecision::SkipTrusted
            } else {
                OwnedInputsDecision::FallThrough
            }
        }
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
    target_branch: &str,
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
    let mut owned_inputs_trusted: Vec<String> = Vec::new();
    let mut updated_plan_text = plan_text.to_string();
    let mut completed_drift = Vec::new();
    let mut backfilled_receipts = Vec::new();
    let mut locally_reverified = Vec::new();
    let mut locally_repromoted = Vec::new();
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
        // still matches, trust the receipt without re-running; when it changed,
        // force re-verification even if the footer otherwise looks fresh (this
        // closes the legacy gap where footer/ancestor receipts skip whole-tree
        // freshness and could miss source drift outside declared artifacts).
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
        if decision == OwnedInputsDecision::SkipTrusted {
            owned_inputs_trusted.push(task.id.clone());
            continue;
        }
        // A changed fingerprint only forces a demote path when the local
        // re-verify sweep is actually active; with the sweep disabled we never
        // demote an otherwise-fresh [x] row (there is nothing to re-prove it
        // with), preserving pre-sweep behavior.
        let reverify_active = drift_local_reverify_enabled() && !reverify_budget.is_zero();
        let must_reverify = decision == OwnedInputsDecision::ForceReverify;
        let must_reverify_effective = must_reverify && reverify_active;

        let evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
        if !must_reverify_effective && evidence.is_fully_evidenced() {
            continue;
        }
        if !must_reverify_effective
            && backfill_completed_legacy_receipt_footer(repo_root, task, &evidence)?
        {
            let refreshed = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
            if refreshed.is_fully_evidenced() {
                backfilled_receipts.push(task.id.clone());
                continue;
            }
        }
        if reverify_active
            && (must_reverify
                || assess_task_completion_gap(&task.markdown, &evidence).kind
                    == CompletionGapKind::LocalRepairable)
        {
            if reverify_spent >= reverify_budget {
                // Budget spent: fall through to the honest demote path below.
                reverify_deferred.push(task.id.clone());
            } else {
                parallel_logger.info(format!(
                    "drift-reverify: `{}` completion evidence went stale ({}); re-running its verification locally before demoting",
                    task.id,
                    evidence.missing_reasons().join("; ")
                ));
                let started = Instant::now();
                let outcome = run_lane_verify_gate(repo_root, &task.id, &task.markdown).await;
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
                        locally_reverified.push(task.id.clone());
                        continue;
                    }
                }
            }
        }
        let mut reasons = evidence.missing_reasons();
        if reasons.is_empty() && must_reverify {
            // A fingerprint-forced demote of a row whose receipt still looked
            // fresh: its own inputs changed and host re-verification did not
            // re-prove [x]. Record a legible reason.
            reasons.push(
                "task-owned inputs changed since the receipt was stamped and host re-verification did not re-prove [x]"
                    .to_string(),
            );
        }
        let entry = ReceiptDriftTriageEntry {
            task_id: task.id.clone(),
            title: task.title.clone(),
            status: LoopTaskStatus::Partial,
            reasons,
        };
        updated_plan_text = update_reconciled_task_completion_in_plan_text(
            &updated_plan_text,
            task,
            LoopTaskStatus::Partial,
        );
        completed_drift.push(entry);
    }

    for task in snapshot
        .tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Partial)
    {
        let evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
        // Restore a `[~]` row to `[x]` when the definition-of-done gate — host
        // re-execution of the declared verification via `run_lane_verify_gate`
        // — passes. This covers BOTH already-fully-evidenced partials AND
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
                let outcome = run_lane_verify_gate(repo_root, &task.id, &task.markdown).await;
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
                        updated_plan_text = update_reconciled_task_completion_in_plan_text(
                            &updated_plan_text,
                            task,
                            LoopTaskStatus::Done,
                        );
                        locally_repromoted.push(task.id.clone());
                        continue;
                    }
                }
            }
        }
        // Only rows that look complete but couldn't be auto-restored are
        // reported as manual closeout candidates; genuinely-incomplete or
        // external-gated partials are left silently at `[~]`.
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

    if updated_plan_text != plan_text {
        let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
        atomic_write(&plan_path, updated_plan_text.as_bytes())
            .with_context(|| format!("failed to write {}", plan_path.display()))?;
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
    if !owned_inputs_trusted.is_empty() {
        parallel_logger.info(format!(
            "drift-reverify: trusted {} completed task(s) via unchanged owned-inputs fingerprint; skipped re-verification ({})",
            owned_inputs_trusted.len(),
            owned_inputs_trusted.join(", ")
        ));
    }
    if !locally_reverified.is_empty() {
        parallel_logger.info(format!(
            "drift-reverify: locally re-proven {} completed task(s) without demotion ({})",
            locally_reverified.len(),
            locally_reverified.join(", ")
        ));
    }
    if !locally_repromoted.is_empty() {
        parallel_logger.info(format!(
            "drift-reverify: restored {} partial task(s) to [x] after host re-execution passed ({})",
            locally_repromoted.len(),
            locally_repromoted.join(", ")
        ));
    }
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
            "warning: repo-local completion evidence drifted for {} completed task(s); wrote RECEIPTS-DRIFT.md and demoted IMPLEMENTATION_PLAN.md rows to [~] ({})",
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
    Ok((updated_plan_text, reverify_deferred.is_empty()))
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

pub(crate) async fn land_parallel_lane_result(
    repo_root: &Path,
    target_branch: &str,
    assignment: &mut ActiveLaneAssignment,
    review_config: &LaneReviewConfig,
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
        let evidence =
            inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
        let repairable_verification_gap = evidence.has_review_handoff
            && repo_root.join("scripts/run-task-verification.sh").is_file()
            && evidence.missing_completion_artifacts.is_empty()
            && evidence.unresolved_audit_findings.is_empty()
            && assess_task_completion_gap(&assignment.task.markdown, &evidence).kind
                == CompletionGapKind::LocalRepairable;
        if repairable_verification_gap {
            let outcome = run_lane_verify_gate(
                repo_root,
                &assignment.task.id,
                &assignment.task.markdown,
            )
            .await;
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
    if completion_status == LoopTaskStatus::Done {
        completion_status = apply_lane_verify_gate(repo_root, assignment, completion_status).await;
    }
    if completion_status == LoopTaskStatus::Done {
        completion_status =
            apply_workspace_test_gate(repo_root, assignment, &changed_files, completion_status)
                .await;
    }
    if completion_status == LoopTaskStatus::Done {
        completion_status = apply_lane_review_gate(
            repo_root,
            target_branch,
            assignment,
            &changed_files,
            completion_status,
            review_config,
        )
        .await;
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
async fn apply_lane_review_gate(
    repo_root: &Path,
    target_branch: &str,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
    review_config: &LaneReviewConfig,
) -> LoopTaskStatus {
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
    // Reaching this point means the host just re-ran the task's declared
    // verification and `cargo test --workspace` successfully at canonical HEAD.
    let current_tree_verification_passed = true;
    let outcome = run_lane_review_gate(
        repo_root,
        target_branch,
        assignment,
        changed_files,
        current_tree_verification_passed,
        review_config,
    )
    .await;
    apply_lane_review_outcome(repo_root, assignment, incoming_status, outcome)
}

fn demote_task_for_failed_gate(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    incoming_status: LoopTaskStatus,
    gate_label: &str,
) {
    assignment.task.status = LoopTaskStatus::Partial;
    if incoming_status != LoopTaskStatus::Done {
        return;
    }
    match update_reconciled_task_completion_in_plan(
        repo_root,
        &assignment.task,
        LoopTaskStatus::Partial,
    ) {
        Ok(true) => {
            if let Err(err) = run_git(repo_root, ["add", "IMPLEMENTATION_PLAN.md"]) {
                eprintln!(
                    "warning: failed staging IMPLEMENTATION_PLAN.md after {gate_label} demote for `{}`: {err:#}",
                    assignment.task.id
                );
            }
        }
        Ok(false) => {}
        Err(err) => {
            eprintln!(
                "warning: failed demoting `{}` to [~] after {gate_label}: {err:#}",
                assignment.task.id
            );
        }
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
) -> LoopTaskStatus {
    match outcome {
        LaneReviewOutcome::Clean => {
            // Clean diff review satisfies any prior review hold on this task.
            clear_gate_hold(repo_root, &assignment.task.id);
            match append_lane_review_clearance(repo_root, &assignment.task.id) {
                Ok(true) => {
                    if let Err(err) = run_git(repo_root, ["add", "REVIEW.md"]) {
                        eprintln!(
                            "warning: failed staging REVIEW.md after standing-review clearance for `{}`: {err:#}",
                            assignment.task.id
                        );
                    }
                }
                Ok(false) => {}
                Err(err) => {
                    eprintln!(
                        "warning: failed appending standing-review clearance for `{}`: {err:#}",
                        assignment.task.id
                    );
                }
            }
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                "independent-review: clean (no actionable findings)",
            );
            incoming_status
        }
        LaneReviewOutcome::FindingsKeepPartial { findings_summary } => {
            // Hold so evidence-only promotion can't re-promote past these findings.
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "independent review findings",
            );
            // Record findings for the next pass. Best-effort: a failure here
            // must not block landing the committed work.
            if let Err(err) =
                append_lane_review_findings(repo_root, &assignment.task.id, &findings_summary)
            {
                eprintln!(
                    "warning: failed appending independent-review findings for `{}`: {err:#}",
                    assignment.task.id
                );
            } else if let Err(err) = run_git(repo_root, ["add", "REVIEW.md"]) {
                eprintln!(
                    "warning: failed staging REVIEW.md after independent-review findings for `{}`: {err:#}",
                    assignment.task.id
                );
            }
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
            LoopTaskStatus::Partial
        }
        LaneReviewOutcome::SkippedFailOpen { reason } => {
            eprintln!(
                "warning: independent-review gate skipped for `{}`; keeping task [~]: {reason}",
                assignment.task.id
            );
            record_gate_hold(repo_root, &assignment.task.id, "independent review skipped");
            if let Err(err) = append_lane_review_findings(
                repo_root,
                &assignment.task.id,
                &format!("Independent review gate skipped before finalization: {reason}"),
            ) {
                eprintln!(
                    "warning: failed appending independent-review skip for `{}`: {err:#}",
                    assignment.task.id
                );
            } else if let Err(err) = run_git(repo_root, ["add", "REVIEW.md"]) {
                eprintln!(
                    "warning: failed staging REVIEW.md after independent-review skip for `{}`: {err:#}",
                    assignment.task.id
                );
            }
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
            LoopTaskStatus::Partial
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
async fn apply_lane_verify_gate(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    incoming_status: LoopTaskStatus,
) -> LoopTaskStatus {
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
    let outcome =
        run_lane_verify_gate(repo_root, &assignment.task.id, &assignment.task.markdown).await;
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
) -> LoopTaskStatus {
    match outcome {
        LaneVerifyOutcome::AllPassed => {
            // Host produced its own green: any prior gate hold is satisfied.
            clear_gate_hold(repo_root, &assignment.task.id);
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                "host-reexec-verify: declared verification re-passed at canonical HEAD",
            );
            incoming_status
        }
        LaneVerifyOutcome::Failed { detail } => {
            // Hold the task so evidence-only promotion can't undo this demotion.
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "host re-execution verification failed",
            );
            // Record why for the next pass. Best-effort: never block landing the
            // committed work.
            if let Err(err) = append_lane_verify_failure(repo_root, &assignment.task.id, &detail) {
                eprintln!(
                    "warning: failed appending host-reexec-verify failure for `{}`: {err:#}",
                    assignment.task.id
                );
            } else if let Err(err) = run_git(repo_root, ["add", "REVIEW.md"]) {
                eprintln!(
                    "warning: failed staging REVIEW.md after host-reexec-verify failure for `{}`: {err:#}",
                    assignment.task.id
                );
            }
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
            LoopTaskStatus::Partial
        }
        LaneVerifyOutcome::Skipped { reason } => {
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "host re-execution verification skipped",
            );
            if let Err(err) = append_lane_verify_failure(
                repo_root,
                &assignment.task.id,
                &format!("host re-execution verification skipped before finalization: {reason}"),
            ) {
                eprintln!(
                    "warning: failed appending host-reexec-verify skip for `{}`: {err:#}",
                    assignment.task.id
                );
            } else if let Err(err) = run_git(repo_root, ["add", "REVIEW.md"]) {
                eprintln!(
                    "warning: failed staging REVIEW.md after host-reexec-verify skip for `{}`: {err:#}",
                    assignment.task.id
                );
            }
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
            LoopTaskStatus::Partial
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
async fn apply_workspace_test_gate(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
) -> LoopTaskStatus {
    match workspace_gate_mode() {
        WorkspaceGateMode::Strict => {
            let outcome = run_workspace_test_gate(repo_root).await;
            apply_workspace_test_outcome(repo_root, assignment, incoming_status, outcome)
        }
        WorkspaceGateMode::Baseline => {
            apply_workspace_baseline_gate(repo_root, assignment, changed_files, incoming_status)
                .await
        }
    }
}

/// Derive the run root (`<run_root>/lanes/lane-N` -> `<run_root>`) from a lane
/// root, but ONLY when the path actually has the canonical `lanes/lane-*` shape.
/// Test fixtures use ad-hoc lane roots; returning `None` there makes the gate
/// fail open (pass-through) instead of writing a stray baseline file into a
/// shared temp directory.
fn workspace_baseline_run_root(lane_root: &Path) -> Option<PathBuf> {
    let lanes = lane_root.parent()?;
    if lanes.file_name()?.to_str()? != "lanes" {
        return None;
    }
    Some(lanes.parent()?.to_path_buf())
}

async fn apply_workspace_baseline_gate(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    changed_files: &[String],
    incoming_status: LoopTaskStatus,
) -> LoopTaskStatus {
    let obs = match run_workspace_probe(repo_root).await {
        WorkspaceProbe::Skipped { reason } if reason.contains("not applicable") => {
            // Non-Rust repo: nothing to check. Pass-through (never demote every
            // task in a Python/TS repo to [~]).
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("workspace-baseline: gate not applicable ({reason}); pass-through"),
            );
            return incoming_status;
        }
        WorkspaceProbe::Skipped { reason } => {
            // Infra-level skip (timeout / spawn error / ambiguous non-zero). The
            // per-task verify gate already produced a positive at canonical HEAD,
            // so a slow-or-broken workspace probe must NOT hold this task hostage.
            // Baseline mode fails OPEN here by design (logged for review).
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!(
                    "workspace-baseline: probe skipped ({reason}); pass-through (own verification already re-passed)"
                ),
            );
            return incoming_status;
        }
        WorkspaceProbe::Ran(obs) => obs,
    };

    let Some(run_root) = workspace_baseline_run_root(&assignment.lane_root) else {
        append_lane_host_event(
            &assignment.stdout_log_path,
            assignment.lane_index,
            &assignment.task.id,
            "workspace-baseline: no lane run root; cannot compare against baseline; pass-through",
        );
        return incoming_status;
    };

    let baseline = load_workspace_baseline(&run_root);
    let baseline_note = format!(
        "workspace-baseline: baseline had {} pre-existing failing test(s), {} broken crate(s); best-observed {} passing test(s), {} compiled crate(s)",
        baseline.baseline_failing_tests.len(),
        baseline.baseline_broken_crates.len(),
        baseline.ever_passed_tests.len(),
        baseline.ever_compiled_crates.len(),
    );

    // Only pay for `cargo metadata` attribution when a candidate regression
    // actually exists; the clean path stays cheap.
    let decision = if has_candidate_regression(&baseline, &obs) {
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

    // Nonblocking (another lane's) regressions are always surfaced for operators.
    for note in &decision.nonblocking {
        append_lane_host_event(
            &assignment.stdout_log_path,
            assignment.lane_index,
            &assignment.task.id,
            &format!("workspace-baseline: NEW regression not attributed to this task: {note}"),
        );
    }

    if decision.is_blocked() {
        let detail = format!(
            "{baseline_note}\n\nNEW regression(s) introduced by this task:\n- {}",
            decision.blocking.join("\n- ")
        );
        record_gate_hold(
            repo_root,
            &assignment.task.id,
            "workspace baseline regression",
        );
        if let Err(err) = append_lane_workspace_test_failure(
            repo_root,
            &assignment.task.id,
            "workspace baseline gate: task introduced a NEW regression vs best-observed baseline",
            &detail,
        ) {
            eprintln!(
                "warning: failed appending workspace-baseline failure for `{}`: {err:#}",
                assignment.task.id
            );
        } else if let Err(err) = run_git(repo_root, ["add", "REVIEW.md"]) {
            eprintln!(
                "warning: failed staging REVIEW.md after workspace-baseline failure for `{}`: {err:#}",
                assignment.task.id
            );
        }
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
                "workspace-baseline: task introduced NEW regression(s); held [~]: {}",
                decision.blocking.join("; ")
            ),
        );
        return LoopTaskStatus::Partial;
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
    incoming_status
}

/// Capture the run's pre-existing workspace failure/compile baseline ONCE, at run
/// start before any lane lands, so a regression introduced by the very first
/// landing cannot be silently absorbed into the baseline. Skipped in strict mode,
/// on non-Rust repos, and on resume (a baseline is already persisted). Bounded by
/// the same timeout as the gate; a timeout/skip just defers to lazy capture at
/// the first landing.
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
    match run_workspace_probe(repo_root).await {
        WorkspaceProbe::Ran(obs) => {
            let mut baseline = WorkspaceBaseline::default();
            advance_workspace_baseline(&mut baseline, &obs);
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
        WorkspaceProbe::Skipped { reason } => {
            parallel_logger.warn(format!(
                "workspace-baseline: could not capture baseline at run start ({reason}); will capture lazily at first landing"
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
) -> LoopTaskStatus {
    match outcome {
        WorkspaceTestOutcome::Passed => {
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                "workspace-test: `cargo test --workspace` passed at canonical HEAD",
            );
            incoming_status
        }
        WorkspaceTestOutcome::Failed { detail } => {
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "workspace cargo test failed",
            );
            if let Err(err) = append_lane_workspace_test_failure(
                repo_root,
                &assignment.task.id,
                "workspace cargo test failed before finalization",
                &detail,
            ) {
                eprintln!(
                    "warning: failed appending workspace-test failure for `{}`: {err:#}",
                    assignment.task.id
                );
            } else if let Err(err) = run_git(repo_root, ["add", "REVIEW.md"]) {
                eprintln!(
                    "warning: failed staging REVIEW.md after workspace-test failure for `{}`: {err:#}",
                    assignment.task.id
                );
            }
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
            LoopTaskStatus::Partial
        }
        WorkspaceTestOutcome::Skipped { reason } if reason.contains("not applicable") => {
            // Non-Rust repo: the gate has nothing to check. Treat as pass-through
            // rather than demoting every task in Python/TS repos to [~] forever.
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("workspace-test: gate not applicable ({reason}); pass-through"),
            );
            incoming_status
        }
        WorkspaceTestOutcome::Skipped { reason } => {
            record_gate_hold(
                repo_root,
                &assignment.task.id,
                "workspace cargo test skipped",
            );
            if let Err(err) = append_lane_workspace_test_failure(
                repo_root,
                &assignment.task.id,
                "workspace cargo test skipped before finalization",
                &reason,
            ) {
                eprintln!(
                    "warning: failed appending workspace-test skip for `{}`: {err:#}",
                    assignment.task.id
                );
            } else if let Err(err) = run_git(repo_root, ["add", "REVIEW.md"]) {
                eprintln!(
                    "warning: failed staging REVIEW.md after workspace-test skip for `{}`: {err:#}",
                    assignment.task.id
                );
            }
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
            LoopTaskStatus::Partial
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
    _target_branch: &str,
    assignment: &ActiveLaneAssignment,
    _parallel_logger: &ParallelEventLogger,
) -> Result<bool> {
    write_clean_no_commit_verdict(
        assignment,
        "needs-human-triage",
        "lane exited cleanly without a local commit; canonical evidence will be inspected before shelving",
    )?;
    // A task a host gate demoted must not be promoted from stale evidence even
    // if the worker exits clean-no-commit believing it is already done.
    if task_is_gate_held(repo_root, &assignment.task.id) {
        write_clean_no_commit_verdict(
            assignment,
            "gate-held",
            "a host gate (verify/review) demoted this task; it must be re-worked and re-verified, not promoted from existing evidence",
        )?;
        return Ok(false);
    }
    propagate_lane_receipts(&assignment.lane_repo_root, repo_root, &assignment.task.id)?;
    let mut evidence_before =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    evidence_before.has_review_handoff = true;
    if !evidence_before.is_ready_for_definition_of_done_gates() {
        return Ok(false);
    }

    let mut task = assignment.task.clone();
    let completion_status = reconcile_parallel_landed_task_state(repo_root, &mut task, &[])?;
    write_clean_no_commit_verdict(
        assignment,
        match completion_status {
            LoopTaskStatus::Done => "done",
            LoopTaskStatus::Partial => "landed-unverified",
            _ => "needs-human-triage",
        },
        "canonical evidence is complete; reconciled the task through the host definition-of-done path without requiring a new worker commit",
    )?;

    Ok(true)
}

pub(crate) fn reconcile_ready_evidence_task_from_canonical_evidence(
    repo_root: &Path,
    task: &LoopTask,
) -> Result<Option<LoopTaskStatus>> {
    // A task a host gate demoted must not be promoted from stale evidence. It
    // needs new work or a fresh verification pass, not a queue-state shortcut.
    if task_is_gate_held(repo_root, &task.id) {
        return Ok(None);
    }
    let mut evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
    evidence.has_review_handoff = true;
    if !evidence.is_fully_evidenced() {
        return Ok(None);
    }

    let mut task = task.clone();
    reconcile_parallel_landed_task_state(repo_root, &mut task, &[]).map(Some)
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

pub(crate) fn record_gate_hold(repo_root: &Path, task_id: &str, reason: &str) {
    let path = gate_hold_path(repo_root, task_id);
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            eprintln!("warning: failed creating gate-hold dir for `{task_id}`: {err:#}");
            return;
        }
    }
    if let Err(err) = std::fs::write(&path, reason) {
        eprintln!("warning: failed recording gate hold for `{task_id}`: {err:#}");
    }
}

pub(crate) fn clear_gate_hold(repo_root: &Path, task_id: &str) {
    let path = gate_hold_path(repo_root, task_id);
    if path.exists() {
        if let Err(err) = std::fs::remove_file(&path) {
            eprintln!("warning: failed clearing gate hold for `{task_id}`: {err:#}");
        }
    }
}

pub(crate) fn task_is_gate_held(repo_root: &Path, task_id: &str) -> bool {
    gate_hold_path(repo_root, task_id).exists()
}

/// Whether a gate-HELD Partial (`[~]`) blocks its dependents from dispatching
/// (default on; `AUTO_PARALLEL_GATE_HOLD_DEPS=0` restores the legacy behavior
/// where every Partial satisfies a dependency regardless of a durable gate hold).
pub(crate) fn gate_hold_blocks_dependents_enabled() -> bool {
    std::env::var("AUTO_PARALLEL_GATE_HOLD_DEPS")
        .map(|value| value.trim() != "0")
        .unwrap_or(true)
}

/// Task ids currently carrying a durable gate hold. A gate hold is recorded only
/// on a REAL gate failure (host re-verification failed, workspace regression, or
/// unresolved review findings) and cleared when the task lands cleanly, so its
/// presence means the task's landed code is known to not pass a gate. Returns an
/// empty set when the feature is disabled, the hold directory is absent, or it is
/// unreadable — the caller then falls back to treating every Partial as resolved.
pub(crate) fn gate_held_task_ids(repo_root: &Path) -> BTreeSet<String> {
    if !gate_hold_blocks_dependents_enabled() {
        return BTreeSet::new();
    }
    let dir = repo_root.join(".auto/parallel/gate-holds");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".hold"))
                .map(|id| id.to_string())
        })
        .collect()
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
    if task_is_gate_held(repo_root, task_id) {
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
        [
            "clean",
            "-nd",
            "--",
            ".auto/symphony/verification-receipts",
        ],
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

        propagate_lane_receipts(&lane, &canonical, "TASK-1").expect("propagate should succeed");

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
            repo.join("REVIEW.md"),
            "# Review\n\n## `TASK-1`\n- Source: test handoff.\n",
        )
        .expect("write review");
        git_ok(&repo, ["add", "."]);
        git_ok(&repo, ["commit", "-m", "seed"]);
        let head_before = git_output(&repo, ["rev-parse", "HEAD"]);

        // ...but a host gate has demoted it. Evidence-only promotion must refuse,
        // so the verify/review gate's demotion isn't silently undone.
        record_gate_hold(&repo, "TASK-1", "host re-execution verification failed");
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
        clear_gate_hold(&repo, "TASK-1");
        assert!(!task_is_gate_held(&repo, "TASK-1"));
        let promoted = promote_task_from_canonical_evidence_no_push(&repo, "TASK-1", task_markdown)
            .expect("promotion should not error after clear");
        assert!(
            !promoted,
            "task still must not promote from evidence after clearing hold"
        );

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[test]
    fn clean_no_commit_reconciles_fully_evidenced_pending_task() {
        let repo = unique_temp_dir("parallel-clean-no-commit-evidenced");
        init_git_repo(&repo);
        let task_markdown = "- [ ] `TASK-006` Evidence already landed\nVerification:\n  - `cargo test -p demo task_006`\nDependencies: none\n";
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("# Plan\n\n{task_markdown}"),
        )
        .expect("write plan");
        fs::create_dir_all(repo.join("scripts")).expect("create scripts");
        fs::write(repo.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("write wrapper");
        fs::write(
            repo.join("REVIEW.md"),
            "# Review\n\n## `TASK-006`\n- Source: test handoff.\n- Remaining blockers: none.\n",
        )
        .expect("write review");
        run_git_in(&repo, ["add", "."]);
        run_git_in(&repo, ["commit", "-m", "seed task"]);
        let receipt_commit = git_output(&repo, ["rev-parse", "HEAD"]);
        fs::create_dir_all(repo.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::write(
            repo.join(".auto/symphony/verification-receipts/TASK-006.json"),
            format!(
                r#"{{"commit":"{receipt_commit}","commands":[{{"command":"cargo test -p demo task_006","exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write receipt");
        run_git_in(
            &repo,
            ["add", ".auto/symphony/verification-receipts/TASK-006.json"],
        );
        run_git_in(&repo, ["commit", "-m", "seed receipt"]);

        let lane_root = repo.join("lane-clean-no-commit");
        fs::create_dir_all(&lane_root).expect("create lane root");
        let run_root = repo.join("parallel-run");
        fs::create_dir_all(&run_root).expect("create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let assignment = ActiveLaneAssignment {
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

        let reconciled = reconcile_parallel_clean_no_commit(&repo, "main", &assignment, &logger)
            .expect("clean no-commit reconciliation should succeed");

        assert!(reconciled, "fully evidenced pending task should reconcile");
        let plan = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(
            plan.contains("- [x] `TASK-006` Evidence already landed"),
            "plan should advance to [x], got:\n{plan}"
        );
        let staged = run_git_in(&repo, ["diff", "--cached", "--name-only"]);
        assert!(
            staged.contains("IMPLEMENTATION_PLAN.md"),
            "queue update should be staged for host sync: {staged}"
        );
        let verdict = fs::read_to_string(lane_root.join("clean-no-commit-verdict.json"))
            .expect("read verdict");
        assert!(verdict.contains("\"verdict\": \"done\""), "{verdict}");

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[test]
    fn clean_no_commit_reconciles_from_lane_local_receipt() {
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
        fs::write(repo.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("write wrapper");
        fs::write(
            repo.join("REVIEW.md"),
            "# Review\n\n## `TASK-007`: host re-execution verification failed\n- Source: stale host gate.\n- The host re-ran this task's declared verification command(s) at canonical HEAD and one FAILED.\n\n```\nprevious stale failure\n```\n\n## `TASK-007`\n- Source: test handoff.\n- Remaining blockers: none.\n",
        )
        .expect("write review");
        run_git_in(&repo, ["add", "."]);
        run_git_in(&repo, ["commit", "-m", "seed task"]);
        let base_commit = git_output(&repo, ["rev-parse", "HEAD"]);

        let lane_repo = repo.join("lane-repo");
        fs::create_dir_all(lane_repo.join(".auto/symphony/verification-receipts"))
            .expect("create lane receipt dir");
        fs::write(
            lane_repo.join(".auto/symphony/verification-receipts/TASK-007.json"),
            format!(
                r#"{{"commit":"{base_commit}","commands":[{{"command":"cargo test -p demo task_007","expected_argv":["cargo","test","-p","demo","task_007"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write lane receipt");

        let lane_root = repo.join("lane-clean-no-commit");
        fs::create_dir_all(&lane_root).expect("create lane root");
        let run_root = repo.join("parallel-run");
        fs::create_dir_all(&run_root).expect("create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let assignment = ActiveLaneAssignment {
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

        let reconciled = reconcile_parallel_clean_no_commit(&repo, "main", &assignment, &logger)
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
        let receipt_commit = git_output(&repo, ["rev-parse", "HEAD"]);
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
        fs::write(
            repo.join(".auto/symphony/verification-receipts/TASK-004.json"),
            format!(
                r#"{{"commit":"{receipt_commit}","commands":[{{"command":"cargo test -p demo task_004","exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("failed to write receipt");
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
                "scripts/run-task-verification.sh",
                ".auto/symphony/verification-receipts/TASK-004.json",
            ],
        );
        run_git_in(&repo, ["commit", "-m", "seed task"]);

        let mut task = LoopTask {
            id: "TASK-004".to_string(),
            title: "Clear standing finding".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-004` Clear standing finding\nVerification:\n  - `cargo test -p demo task_004`\nDependencies: none\n".to_string(),
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

    #[tokio::test]
    async fn audit_parallel_completion_drift_demotes_completed_rows() {
        let repo = unique_temp_dir("parallel-drift-audit");
        let run_root = unique_temp_dir("parallel-drift-audit-run");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        // A row with no receipts and no runnable verification demotes whether or
        // not the budget is spent — the budgeted re-verify path finds nothing to
        // run and falls through honestly. (Budget parsing is covered directly by
        // `drift_reverify_budget_parses_env`.)
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
            triage.contains("TASK-001") && triage.contains("Completed Tasks With Drift"),
            "receipt drift should stay visible in triage"
        );
        let live_log = fs::read_to_string(run_root.join("live.log"))
            .expect("receipt repair should write host log");
        assert!(live_log.contains("demoted IMPLEMENTATION_PLAN.md rows to [~]"));
    }

    #[tokio::test]
    async fn audit_parallel_completion_drift_logs_only_changed_triage() {
        let repo = unique_temp_dir("parallel-drift-audit-stable-log");
        let run_root = unique_temp_dir("parallel-drift-audit-stable-log-run");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let (updated, _) = audit_parallel_completion_drift(&repo, "main", plan, &logger)
            .await
            .expect("first drift audit should succeed");
        assert!(updated.starts_with("- [~] `TASK-001`"), "{updated}");
        let first_log =
            fs::read_to_string(run_root.join("live.log")).expect("first audit should log drift");
        assert!(first_log.contains("demoted IMPLEMENTATION_PLAN.md rows to [~]"));

        let _ = audit_parallel_completion_drift(&repo, "main", &updated, &logger)
            .await
            .expect("second drift audit should succeed");
        let second_log =
            fs::read_to_string(run_root.join("live.log")).expect("second audit should keep log");
        assert_eq!(
            second_log, first_log,
            "unchanged receipt drift should stay visible in RECEIPTS-DRIFT.md without appending another fresh host warning"
        );

        let drift_summary = receipt_drift_status_summary(&repo);
        assert!(
            drift_summary
                .as_deref()
                .is_none_or(|summary| !summary.contains("completed task(s)")),
            "completed receipt drift should be cleared once the row is demoted: {drift_summary:?}"
        );
    }

    #[tokio::test]
    async fn audit_parallel_completion_drift_backfills_safe_legacy_receipt_footer() {
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

        let (updated, _) = audit_parallel_completion_drift(&repo, "trunk", plan, &logger)
            .await
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
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [~] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        fs::write(repo.join("REVIEW.md"), "## `TASK-001`\n\nComplete.\n")
            .expect("failed to write review");
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
            lane_root: root.join("lane-review-root"),
            lane_repo_root: root.join("lane-review-repo"),
            base_commit: "0000000000000000000000000000000000000000".to_string(),
            stdout_log_path: root.join("lane-review.stdout.log"),
            stderr_log_path: root.join("lane-review.stderr.log"),
            worker_pid_path: root.join("lane-review.worker.pid"),
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
        git_ok(&root, ["add", "."]);
        git_ok(&root, ["commit", "-q", "-m", "seed passing current tree"]);

        let mut assignment =
            review_gate_assignment_with_markdown(&root, task_id, title, task_markdown);
        let review_config = LaneReviewConfig {
            model: "unused".to_string(),
            reasoning_effort: "unused".to_string(),
            codex_bin: PathBuf::from("/bin/false"),
        };

        let mut status =
            super::apply_lane_verify_gate(&root, &mut assignment, LoopTaskStatus::Done).await;
        assert_eq!(status, LoopTaskStatus::Done);
        status = super::apply_workspace_test_gate(&root, &mut assignment, &[], status).await;
        assert_eq!(status, LoopTaskStatus::Done);
        status = super::apply_lane_review_gate(
            &root,
            "main",
            &mut assignment,
            &[],
            status,
            &review_config,
        )
        .await;

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
        // Baseline gate on a fixture lane root (no `lanes/lane-*` shape) fails
        // open with a pass-through, leaving the incoming [x] intact.
        assert!(log.contains("workspace-baseline: no lane run root"));
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
        let status =
            super::apply_lane_verify_gate(&root, &mut assignment, LoopTaskStatus::Done).await;

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
        );
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
        );
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
        );
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
    fn verify_all_passed_outcome_keeps_done_and_stamps() {
        let root = unique_temp_dir("verify-gate-pass");
        init_git_repo(&root);
        let mut assignment =
            review_gate_assignment(&root, "TASK-VOK-1", "host re-run green lands done");
        let status = apply_lane_verify_outcome(
            &root,
            &mut assignment,
            LoopTaskStatus::Done,
            LaneVerifyOutcome::AllPassed,
        );
        assert_eq!(status, LoopTaskStatus::Done);
        assert_eq!(assignment.task.status, LoopTaskStatus::Done);
        let log = fs::read_to_string(&assignment.stdout_log_path).expect("closeout log");
        assert!(log.contains("host-reexec-verify: declared verification re-passed"));
        assert!(!root.join("REVIEW.md").exists());
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
        );
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
        );
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
        );
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
    fn owned_inputs_gate_trusts_matching_fingerprint() {
        // Stored == current owned-inputs fingerprint -> trust the receipt.
        assert_eq!(
            super::decide_owned_inputs(false, false, true, Some("abc"), Some("abc")),
            super::OwnedInputsDecision::SkipTrusted
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
    fn owned_inputs_gate_sweep_excluded_trusts_legacy_and_hash_error() {
        // A sweep-excluded task with a valid receipt is never re-run by a sweep
        // on mere legacy-ness or a hash error.
        assert_eq!(
            super::decide_owned_inputs(false, true, true, None, Some("def")),
            super::OwnedInputsDecision::SkipTrusted
        );
        assert_eq!(
            super::decide_owned_inputs(false, true, true, Some("abc"), None),
            super::OwnedInputsDecision::SkipTrusted
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
        assert!(!super::task_is_sweep_excluded("- [x] `T1` x\n  Verification:\n    - `cargo test`\n"));
    }
}
