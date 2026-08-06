use super::*;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LinearAutoSyncState {
    pub(crate) disabled_reason: Option<String>,
}

impl LinearAutoSyncState {
    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    pub(crate) fn disable_for_run(&mut self, reason: impl Into<String>) -> bool {
        if self.disabled_reason.is_some() {
            return false;
        }
        self.disabled_reason = Some(reason.into());
        true
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ParallelEventLogger {
    pub(crate) live_log_path: PathBuf,
}

impl ParallelEventLogger {
    pub(crate) fn new(run_root: &Path) -> Result<Self> {
        let live_log_path = run_root.join("live.log");
        fs::write(&live_log_path, b"")
            .with_context(|| format!("failed to initialize {}", live_log_path.display()))?;
        Ok(Self { live_log_path })
    }

    pub(crate) fn run_root(&self) -> PathBuf {
        self.live_log_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub(crate) fn info(&self, message: impl AsRef<str>) {
        let message = message.as_ref();
        println!("{message}");
        if let Err(err) = self.append(message) {
            eprintln!("warning: failed writing parallel live log: {err:#}");
        }
    }

    pub(crate) fn warn(&self, message: impl AsRef<str>) {
        let message = message.as_ref();
        eprintln!("{message}");
        if let Err(err) = self.append(message) {
            eprintln!("warning: failed writing parallel live log: {err:#}");
        }
    }

    pub(crate) fn append(&self, message: &str) -> Result<()> {
        let normalized = normalize_parallel_live_log_message(message);
        if normalized.is_empty() {
            return Ok(());
        }
        let redacted = redact_parallel_live_log_message(&normalized);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.live_log_path)
            .with_context(|| format!("failed to open {}", self.live_log_path.display()))?;
        writeln!(file, "{redacted}")
            .with_context(|| format!("failed to append {}", self.live_log_path.display()))
    }
}

/// Resolve the one-time startup workspace barrier only after at least one
/// isolated worker has been dispatched. Keeping this tiny sequencing primitive
/// separate makes the safety ordering executable in tests: dispatch happens
/// first, baseline completion second, and canonical host work may resume only
/// after this future returns.
async fn resolve_startup_workspace_baseline_after_dispatch<F>(
    pending: &mut bool,
    workers_active: bool,
    capture: F,
) -> Result<()>
where
    F: std::future::Future<Output = Result<()>>,
{
    if *pending && workers_active {
        capture.await?;
        *pending = false;
    }
    Ok(())
}

pub(crate) fn append_lane_host_event(
    log_path: &Path,
    lane_index: usize,
    task_id: &str,
    message: &str,
) {
    let rendered = format!(
        "[auto parallel host lane-{lane_index} {task_id}] {message}",
        lane_index = lane_index,
        task_id = task_id,
        message = message.trim()
    );
    if let Err(err) = append_lane_log_line(log_path, &rendered) {
        eprintln!(
            "warning: failed appending lane host event to {}: {err:#}",
            log_path.display()
        );
    }
}

pub(crate) fn append_lane_log_line(log_path: &Path, line: &str) -> Result<()> {
    if line.trim().is_empty() {
        return Ok(());
    }
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("failed to append {}", log_path.display()))
}

#[derive(Deserialize, Serialize)]
struct LaneHostPendingMarker {
    version: u32,
    phase: String,
    run_id: String,
    task_id: String,
    lane: usize,
    attempt: usize,
}

fn publish_lane_host_pending_marker(
    lane_root: &Path,
    lane: usize,
    task_id: &str,
    attempt: usize,
) -> Result<()> {
    let run_root = lane_root
        .parent()
        .and_then(Path::parent)
        .context("lane root is not nested under a parallel run root")?;
    let run_id = current_parallel_run_id(run_root)
        .context("parallel run has no current run id for host-pending publication")?;
    if lane_run_id(lane_root).as_deref() != Some(run_id.as_str()) {
        bail!("lane run id does not match the current parallel run")
    }
    let marker = LaneHostPendingMarker {
        version: LANE_HOST_PENDING_VERSION,
        phase: "awaiting_host".to_string(),
        run_id,
        task_id: task_id.to_string(),
        lane,
        attempt,
    };
    let bytes = serde_json::to_vec_pretty(&marker).context("serialize host-pending marker")?;
    atomic_write(&lane_root.join(LANE_HOST_PENDING_FILE), &bytes)
        .context("persist host-pending marker")
}

fn clear_lane_host_pending_marker(lane_root: &Path) -> Result<()> {
    let path = lane_root.join(LANE_HOST_PENDING_FILE);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn publish_lane_host_pending_marker_best_effort(
    lane_root: &Path,
    lane: usize,
    task_id: &str,
    attempt: usize,
) {
    if let Err(err) = publish_lane_host_pending_marker(lane_root, lane, task_id, attempt) {
        eprintln!(
            "warning: failed publishing host-pending state for lane-{lane} `{task_id}`: {err:#}"
        );
    }
}

struct LaneHostPendingGuard {
    lane_root: PathBuf,
    task_id: String,
    attempt: usize,
}

impl LaneHostPendingGuard {
    fn new(assignment: &ActiveLaneAssignment) -> Self {
        Self {
            lane_root: assignment.lane_root.clone(),
            task_id: assignment.task.id.clone(),
            attempt: assignment.attempts,
        }
    }
}

impl Drop for LaneHostPendingGuard {
    fn drop(&mut self) {
        let path = self.lane_root.join(LANE_HOST_PENDING_FILE);
        let Ok(bytes) = fs::read(&path) else {
            return;
        };
        let Ok(marker) = serde_json::from_slice::<LaneHostPendingMarker>(&bytes) else {
            return;
        };
        if marker.task_id == self.task_id && marker.attempt == self.attempt {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) fn append_idle_status_to_free_lanes(
    run_root: &Path,
    max_concurrent_workers: usize,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
    summary: &str,
) {
    for lane_index in 1..=max_concurrent_workers {
        if active_lanes.contains_key(&lane_index) {
            continue;
        }
        let lane_root = run_root.join("lanes").join(format!("lane-{lane_index}"));
        append_lane_host_event(
            &lane_root.join("stdout.log"),
            lane_index,
            "[idle]",
            &format!("idle: {summary}"),
        );
    }
}

fn partition_ready_tasks_with_operator_closeout(
    ready: Vec<LoopTask>,
    mut operator_has_closeout_evidence: impl FnMut(&LoopTask) -> bool,
) -> (Vec<LoopTask>, Vec<LoopTask>) {
    ready
        .into_iter()
        .partition(|task| is_operator_task(task) && !operator_has_closeout_evidence(task))
}

fn operator_task_has_closeout_evidence(repo_root: &Path, task: &LoopTask) -> bool {
    if !is_operator_task(task) {
        return false;
    }
    let mut evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
    // A review handoff is produced by the ordinary closeout pipeline. The
    // operator must already have supplied every immutable input: a current
    // passing receipt, declared artifacts, and clean owned-audit scope.
    evidence.has_review_handoff = true;
    evidence.unresolved_review_findings.clear();
    evidence.is_ready_for_definition_of_done_gates()
}

fn partition_ready_tasks_for_worker_dispatch(
    repo_root: &Path,
    ready: Vec<LoopTask>,
) -> (Vec<LoopTask>, Vec<LoopTask>) {
    partition_ready_tasks_with_operator_closeout(ready, |task| {
        operator_task_has_closeout_evidence(repo_root, task)
    })
}

/// Restore the completion transaction to a dependency-blocking Partial state
/// after any lane-processing error that may have happened after reconciliation
/// wrote or staged a candidate Done row.
///
/// Other lanes can remain active while the host processes one completed lane.
/// The scheduler must therefore recover synchronously in the error arm, before
/// its next queue refresh can observe the candidate row and dispatch dependents.
/// Any recovery or post-recovery assertion failure aborts the host loop.
fn recover_failed_parallel_lane_completion(
    repo_root: &Path,
    task_id: &str,
    remaining_active_lanes: usize,
    parallel_logger: &ParallelEventLogger,
    failure_context: &str,
) -> Result<Vec<String>> {
    let recovered = recover_unsealed_task_completion_transitions(repo_root).with_context(|| {
        format!(
                "failed to recover candidate completion for `{task_id}` after {failure_context}; \
                 refusing to continue scheduling with {remaining_active_lanes} other active lane(s)"
            )
    })?;
    refuse_unsealed_task_completion_checkpoint(repo_root).with_context(|| {
        format!(
            "candidate completion for `{task_id}` remained unsealed after recovery from \
             {failure_context}; refusing to continue scheduling with {remaining_active_lanes} \
             other active lane(s)"
        )
    })?;
    if !recovered.is_empty() {
        parallel_logger.warn(format!(
            "fail-closed recovery: demoted unsealed candidate completion(s) to [~] before \
             continuing with {remaining_active_lanes} other active lane(s) after `{task_id}` \
             failed: {}",
            recovered.join(", ")
        ));
    }
    Ok(recovered)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_parallel_loop(
    repo_root: &Path,
    args: &ParallelArgs,
    target_branch: &str,
    prompt_template: &str,
    run_root: &Path,
    worker_env: &LoopWorkerEnv,
    linear_tracker: &mut Option<LinearTracker>,
    parallel_logger: &ParallelEventLogger,
) -> Result<()> {
    let harness = if args.claude { "Claude" } else { "Codex" };
    repair_parallel_canonical_before_dispatch(repo_root, target_branch, parallel_logger)?;
    let mut join_set = JoinSet::<LaneAttemptResult>::new();
    let mut active_lanes = BTreeMap::<usize, ActiveLaneAssignment>::new();
    let mut active_tasks = BTreeSet::<String>::new();
    // Restore per-run scheduling bookkeeping from a prior (possibly crashed)
    // invocation on the same run_root. Every restored entry is re-pruned against
    // the freshly-read plan by the `retain` calls at the top of the main loop, so
    // a stale ledger can never resurrect a Done/spec-changed task; this only
    // preserves shelve/defer decisions and retry budgets across a restart so the
    // resumed host doesn't reset them and re-thrash through the same failures.
    let mut restored_run_state = load_parallel_run_state(run_root);
    let current_head = current_repo_head(repo_root);
    let auto_cleared = auto_clear_shelved_on_head_change(
        &mut restored_run_state,
        current_head.as_deref(),
        autoclear_shelved_disabled(),
    );
    if auto_cleared > 0 {
        parallel_logger.info(format!(
            "resume: HEAD advanced since the last run; auto-retrying {auto_cleared} shelved/deferred task(s) whose transient blocker plausibly resolved (set AUTO_PARALLEL_AUTOCLEAR_SHELVED=0 to disable)"
        ));
    }
    let auto_unshelved =
        auto_unshelve_landing_divergence_tasks(repo_root, &mut restored_run_state, parallel_logger)
            .await;
    if auto_unshelved > 0 {
        parallel_logger.info(format!(
            "resume: auto-unshelved {auto_unshelved} landing-divergence task(s) after current-HEAD verification passed"
        ));
    }
    let (mut shelved_tasks, mut shelved_task_details) =
        split_shelved_task_state(restored_run_state.shelved_tasks);
    let mut attempted_partial_followups = restored_run_state.attempted_partial_followups;
    let mut deferred_partial_tasks = restored_run_state.deferred_partial_tasks;
    let mut unblock_attempt_counts = restored_run_state.unblock_attempt_counts;
    // Cache the last-persisted run-state JSON so the ~5s main loop only rewrites
    // the ledger when its serialized value actually changed (not every idle tick).
    let mut last_persisted_run_state: Option<String> = None;
    // Per-task quota ride-out budget (F1 single-account gap). Transient to this
    // run: tracks cumulative wait + wait count so a session-quota exhaustion is
    // ridden out (bounded) instead of shelving the run, without ever hot-looping.
    let mut quota_ride_out: BTreeMap<String, QuotaRideOutState> = BTreeMap::new();
    if !shelved_tasks.is_empty()
        || !deferred_partial_tasks.is_empty()
        || !unblock_attempt_counts.is_empty()
        || !attempted_partial_followups.is_empty()
    {
        parallel_logger.info(format!(
            "resume: restored run-state ledger (shelved: {}, deferred: {}, unblock-counts: {}, followups: {})",
            shelved_tasks.len(),
            deferred_partial_tasks.len(),
            unblock_attempt_counts.len(),
            attempted_partial_followups.len()
        ));
    }
    let max_autonomous_unblock_attempts = autonomous_unblock_attempt_limit(args.max_retries);
    let mut linear_auto_sync_state = LinearAutoSyncState::default();
    let mut landed = 0usize;
    let mut plan = refresh_parallel_plan(
        repo_root,
        target_branch,
        linear_tracker,
        &mut linear_auto_sync_state,
        parallel_logger,
    )
    .await?;
    let preflight_report = run_parallel_preflight(repo_root, &plan, run_root, parallel_logger)?;
    // Defer the expensive initial workspace baseline until after the first
    // workers have been dispatched. Workers run in isolated worktrees, so their
    // implementation time can overlap this canonical read-only probe. The main
    // loop resolves this barrier before it joins/harvests any worker, and thus
    // before any canonical cherry-pick, queue checkpoint, or landing can occur.
    let mut startup_workspace_baseline_pending = true;
    let lane_config = LaneRunConfig::new(args, worker_env, preflight_report.prompt_clause());
    let review_config = LaneReviewConfig::from_run_config(&args.model, &args.codex_bin);
    try_checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger);
    wait_for_live_resume_workers(run_root, parallel_logger).await?;
    let mut resumable_lanes = discover_resume_candidates(
        repo_root,
        run_root,
        target_branch,
        &lane_config,
        &plan,
        parallel_logger,
    )?;
    // A completed resumable lane can be harvested immediately below, before the
    // normal dispatch loop gets a chance to resolve the startup barrier. Keep
    // resume semantics conservative: capture first, then permit harvesting.
    if !resumable_lanes.is_empty() {
        maybe_capture_workspace_baseline(repo_root, run_root, parallel_logger).await?;
        startup_workspace_baseline_pending = false;
    }
    landed += harvest_resumable_lane_results(
        repo_root,
        target_branch,
        &mut resumable_lanes,
        &mut attempted_partial_followups,
        &mut deferred_partial_tasks,
        linear_tracker,
        parallel_logger,
        &review_config,
    )
    .await?;
    plan = refresh_parallel_plan_or_last_good(
        repo_root,
        target_branch,
        linear_tracker,
        &mut linear_auto_sync_state,
        &plan,
        parallel_logger,
    )
    .await?;
    try_checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger);
    let mut rediscovered_lanes = discover_resume_candidates(
        repo_root,
        run_root,
        target_branch,
        &lane_config,
        &plan,
        parallel_logger,
    )?;
    preserve_resume_recovery_notes(&mut rediscovered_lanes, &resumable_lanes);
    resumable_lanes = rediscovered_lanes;
    let mut last_idle_summary = None::<String>;

    loop {
        nudge_lingering_committed_lanes(&mut active_lanes);
        if active_lanes.is_empty() {
            repair_parallel_canonical_before_dispatch(repo_root, target_branch, parallel_logger)?;
        }
        plan = refresh_parallel_plan_or_last_good(
            repo_root,
            target_branch,
            linear_tracker,
            &mut linear_auto_sync_state,
            &plan,
            parallel_logger,
        )
        .await?;
        try_checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger);
        shelved_tasks.retain(|task_id, markdown| {
            plan.tasks
                .iter()
                .find(|task| task.id == *task_id)
                .is_some_and(|task| task.markdown == *markdown)
        });
        shelved_task_details.retain(|task_id, details| {
            shelved_tasks
                .get(task_id)
                .is_some_and(|markdown| markdown == &details.markdown)
        });
        attempted_partial_followups.retain(|task_id, _count| {
            plan.tasks
                .iter()
                .find(|task| task.id == *task_id)
                .is_some_and(|task| task.status == LoopTaskStatus::Partial)
        });
        deferred_partial_tasks.retain(|task_id| {
            plan.tasks
                .iter()
                .find(|task| task.id == *task_id)
                .is_some_and(|task| task.status == LoopTaskStatus::Partial)
        });
        unblock_attempt_counts.retain(|task_id, _| {
            plan.tasks
                .iter()
                .find(|task| task.id == *task_id)
                .is_some_and(|task| task.status != LoopTaskStatus::Done)
        });
        // Persist the just-pruned bookkeeping so a crash/restart resumes it.
        // Record the current HEAD so the NEXT run can auto-recover these shelved
        // tasks if a fix lands (advances HEAD) between runs. Only writes when the
        // serialized ledger actually changed, so idle ticks don't churn the disk.
        save_parallel_run_state_if_changed(
            &mut last_persisted_run_state,
            run_root,
            &shelved_tasks,
            &shelved_task_details,
            &deferred_partial_tasks,
            &unblock_attempt_counts,
            &attempted_partial_followups,
            current_repo_head(repo_root).as_deref(),
        );

        if args
            .max_iterations
            .is_some_and(|limit| landed >= limit && active_lanes.is_empty())
        {
            println!(
                "reached max iterations: {}",
                args.max_iterations.unwrap_or_default()
            );
            break;
        }

        loop {
            let available_slots = args
                .max_concurrent_workers
                .saturating_sub(active_lanes.len());
            if available_slots == 0 {
                break;
            }
            let remaining_budget = args
                .max_iterations
                .map(|limit| limit.saturating_sub(landed + active_lanes.len()))
                .unwrap_or(usize::MAX);
            if remaining_budget == 0 {
                break;
            }

            let queue = plan.queue_snapshot();
            if queue.pending_ids.is_empty() {
                break;
            }

            // Gate-held partials failed a real gate; their landed code is known
            // to not pass, so dependents built on them would rework. Hold those
            // dependents until the hold clears (task lands cleanly).
            let gate_held = gate_held_task_ids(repo_root)
                .context("failed to read durable gate holds before dependency dispatch")?;
            let ready = prioritize_ready_parallel_tasks(
                repo_root,
                ready_parallel_tasks_with_gate_holds(
                    &plan,
                    &active_tasks,
                    &shelved_tasks,
                    &deferred_partial_tasks,
                    &gate_held,
                ),
            );
            if ready.is_empty() {
                if let Some(candidate) = next_parallel_unblock_candidate(
                    &plan,
                    &active_tasks,
                    &shelved_tasks,
                    &deferred_partial_tasks,
                    &resumable_lanes,
                    &unblock_attempt_counts,
                    max_autonomous_unblock_attempts,
                ) {
                    let (lane_index, resume_candidate) = if let Some((
                        lane_index,
                        candidate_resume,
                    )) = take_resume_candidate_for_task(
                        &mut resumable_lanes,
                        &candidate.task.id,
                        &active_lanes,
                    ) {
                        (lane_index, Some(candidate_resume))
                    } else {
                        (
                            next_free_lane_index(args.max_concurrent_workers, &active_lanes)
                                .context("failed to find a free loop lane for unblock recovery")?,
                            None,
                        )
                    };
                    let attempt_count = unblock_attempt_counts
                        .entry(candidate.task.id.clone())
                        .or_insert(0);
                    *attempt_count += 1;
                    parallel_logger.info(format!(
                        "unblock:     lane-{} -> {} [{} attempt {}/{}] because the normal ready queue is empty; downstream: {}",
                        lane_index,
                        candidate.task.id,
                        candidate.kind.label(),
                        *attempt_count,
                        max_autonomous_unblock_attempts,
                        if candidate.downstream.is_empty() {
                            "none".to_string()
                        } else {
                            candidate.downstream.join(", ")
                        }
                    ));
                    match candidate.kind {
                        ParallelUnblockCandidateKind::ShelvedResume => {
                            shelved_tasks.remove(&candidate.task.id);
                        }
                        ParallelUnblockCandidateKind::DeferredPartialCloseout => {
                            deferred_partial_tasks.remove(&candidate.task.id);
                        }
                    }
                    let mut assignment = match prepare_parallel_lane_assignment_with_fallback(
                        repo_root,
                        run_root,
                        target_branch,
                        &lane_config,
                        lane_index,
                        candidate.task.clone(),
                        resume_candidate,
                    ) {
                        Ok(assignment) => assignment,
                        Err(err) => {
                            parallel_logger.warn(format!(
                                "warning: failed preparing lane-{} for unblock task `{}`; keeping it parked for this run: {err:#}",
                                lane_index,
                                candidate.task.id
                            ));
                            match candidate.kind {
                                ParallelUnblockCandidateKind::ShelvedResume => {
                                    shelved_tasks.insert(
                                        candidate.task.id.clone(),
                                        candidate.task.markdown.clone(),
                                    );
                                }
                                ParallelUnblockCandidateKind::DeferredPartialCloseout => {
                                    deferred_partial_tasks.insert(candidate.task.id.clone());
                                }
                            }
                            continue;
                        }
                    };
                    attach_partial_follow_up_note(
                        repo_root,
                        &mut assignment,
                        &attempted_partial_followups,
                    );
                    prepend_host_recovery_note(
                        &mut assignment,
                        &render_parallel_unblock_note(&candidate),
                    );
                    if let Err(err) = spawn_parallel_lane_attempt(
                        &mut join_set,
                        &lane_config,
                        prompt_template,
                        &plan,
                        &mut assignment,
                        target_branch,
                    ) {
                        parallel_logger.warn(format!(
                            "warning: failed starting unblock lane-{} `{}`; keeping it parked for this run: {err:#}",
                            assignment.lane_index, assignment.task.id
                        ));
                        match candidate.kind {
                            ParallelUnblockCandidateKind::ShelvedResume => {
                                shelved_tasks.insert(
                                    candidate.task.id.clone(),
                                    candidate.task.markdown.clone(),
                                );
                            }
                            ParallelUnblockCandidateKind::DeferredPartialCloseout => {
                                deferred_partial_tasks.insert(candidate.task.id.clone());
                            }
                        }
                        continue;
                    }
                    if candidate.kind == ParallelUnblockCandidateKind::ShelvedResume {
                        // This is now a genuine fresh attempt. Do not let the
                        // prior landing classification leak into a later,
                        // unrelated worker/gate shelf outcome.
                        shelved_task_details.remove(&candidate.task.id);
                    }
                    active_tasks.insert(assignment.task.id.clone());
                    active_lanes.insert(assignment.lane_index, assignment);
                    last_idle_summary = None;
                    continue;
                }
                if active_lanes.len() < args.max_concurrent_workers {
                    let idle_summary = describe_parallel_idle_state(
                        &plan,
                        &active_tasks,
                        &shelved_tasks,
                        &deferred_partial_tasks,
                    );
                    if last_idle_summary.as_deref() != Some(idle_summary.as_str()) {
                        parallel_logger.info(format!(
                            "idle:        {} of {} lanes active; {}",
                            active_lanes.len(),
                            args.max_concurrent_workers,
                            idle_summary
                        ));
                        append_idle_status_to_free_lanes(
                            run_root,
                            args.max_concurrent_workers,
                            &active_lanes,
                            &idle_summary,
                        );
                        last_idle_summary = Some(idle_summary);
                    }
                }
                break;
            }
            let (operator_ready, worker_ready) =
                partition_ready_tasks_for_worker_dispatch(repo_root, ready);
            if !operator_ready.is_empty() {
                match write_operator_actions_for_ready_tasks(run_root, &operator_ready) {
                    Ok(path) => parallel_logger.info(format!(
                        "operator-queue: {} item(s) require operator action before code lanes can unblock; see {}",
                        operator_ready.len(),
                        path.display()
                    )),
                    Err(err) => parallel_logger.warn(format!(
                        "warning: failed writing operator action queue: {err:#}"
                    )),
                }
            } else {
                clear_stale_operator_actions(run_root, parallel_logger);
            }
            if worker_ready.is_empty() {
                let message = format!(
                    "no dependency-ready worker tasks remain; operator queue: {}",
                    operator_ready
                        .iter()
                        .map(|task| task.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                parallel_logger.info(&message);
                break;
            }

            let task = worker_ready[0].clone();

            // Pre-dispatch self-heal: a `[~]` (partial) task whose canonical
            // evidence is already complete does not need a worker. This happens
            // when its verification receipt became valid only after HEAD
            // advanced past the receipt commit (the freshness gate skips the
            // plan-hash / command checks once the receipt is an ancestor of
            // HEAD). Promote it host-side and re-evaluate the ready set, instead
            // of burning a full xhigh worker session that would exit
            // clean-no-commit and be shelved for the post-exit / end-of-run
            // recovery to reclaim. Genuine-partial tasks fail the same
            // `is_fully_evidenced()` gate and fall through to normal dispatch.
            if task.status == LoopTaskStatus::Partial {
                // Once the first startup worker is live, do not let a later
                // slot's host-side evidence promotion mutate the canonical
                // queue before the baseline barrier has resolved. Break out,
                // capture/revalidate the baseline, then reconsider this task on
                // the next outer iteration.
                if startup_workspace_baseline_pending && !active_lanes.is_empty() {
                    break;
                }
                match try_promote_partial_before_dispatch(
                    repo_root,
                    target_branch,
                    &task.id,
                    &task.markdown,
                    parallel_logger,
                ) {
                    Ok(true) => {
                        plan = refresh_parallel_plan_or_last_good(
                            repo_root,
                            target_branch,
                            linear_tracker,
                            &mut linear_auto_sync_state,
                            &plan,
                            parallel_logger,
                        )
                        .await?;
                        last_idle_summary = None;
                        continue;
                    }
                    Ok(false) => {}
                    Err(err) => {
                        parallel_logger.warn(format!(
                            "warning: pre-dispatch evidence check for `{}` failed; dispatching a worker instead: {err:#}",
                            task.id
                        ));
                    }
                }
            }

            let (lane_index, resume_candidate) = if let Some((lane_index, candidate)) =
                take_resume_candidate_for_task(&mut resumable_lanes, &task.id, &active_lanes)
            {
                (lane_index, Some(candidate))
            } else {
                (
                    next_free_lane_index(args.max_concurrent_workers, &active_lanes)
                        .context("failed to find a free loop lane")?,
                    None,
                )
            };
            let mut assignment = match prepare_parallel_lane_assignment_with_fallback(
                repo_root,
                run_root,
                target_branch,
                &lane_config,
                lane_index,
                task.clone(),
                resume_candidate,
            ) {
                Ok(assignment) => assignment,
                Err(err) => {
                    parallel_logger.warn(format!(
                        "warning: failed preparing lane-{} for `{}`; shelving for the rest of this run: {err:#}",
                        lane_index,
                        task.id
                    ));
                    shelved_tasks.insert(task.id.clone(), task.markdown.clone());
                    continue;
                }
            };
            attach_partial_follow_up_note(repo_root, &mut assignment, &attempted_partial_followups);
            if is_operator_task(&assignment.task) {
                prepend_host_recovery_note(
                    &mut assignment,
                    "The operator action is already represented by current canonical artifacts and a passing receipt. Treat this as verification-only closeout: do not repeat the external action, change its captured inputs, or invent provenance. Inspect the existing evidence and return AUTO_ALREADY_COMPLETE when it satisfies the task contract.",
                );
            }
            if let Err(err) = spawn_parallel_lane_attempt(
                &mut join_set,
                &lane_config,
                prompt_template,
                &plan,
                &mut assignment,
                target_branch,
            ) {
                parallel_logger.warn(format!(
                    "warning: failed starting lane-{} for `{}`; shelving for the rest of this run: {err:#}",
                    assignment.lane_index, assignment.task.id
                ));
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            if let Some(tracker) = linear_tracker.as_mut() {
                if let Err(err) = tracker.note_dispatch(&assignment.task.id).await {
                    eprintln!(
                        "warning: failed to move `{}` to in-progress in Linear: {err:#}",
                        assignment.task.id
                    );
                }
            }
            parallel_logger.info(format!(
                "dispatch:    [{}] lane-{} -> {} {}{}",
                classify_task_execution_kind(&assignment.task),
                lane_index,
                assignment.task.id,
                assignment.task.title,
                if assignment.resumed { " [resume]" } else { "" }
            ));
            let dispatch_message = if assignment.resumed {
                format!("dispatch: resumed `{}`", assignment.task.title)
            } else {
                format!("dispatch: started `{}`", assignment.task.title)
            };
            append_lane_host_event(
                &assignment.stdout_log_path,
                lane_index,
                &assignment.task.id,
                &dispatch_message,
            );
            active_tasks.insert(assignment.task.id.clone());
            active_lanes.insert(lane_index, assignment);
            last_idle_summary = None;
        }

        // Hard startup barrier. At least one isolated worker is now running, so
        // its implementation time overlaps the expensive baseline probe. While
        // this await is in progress the host performs no canonical repair,
        // refresh, checkpoint, join, cherry-pick, or landing. The guarded probe
        // itself snapshots and revalidates exact canonical state.
        resolve_startup_workspace_baseline_after_dispatch(
            &mut startup_workspace_baseline_pending,
            !active_lanes.is_empty(),
            maybe_capture_workspace_baseline(repo_root, run_root, parallel_logger),
        )
        .await?;

        if active_lanes.is_empty() {
            let queue = plan.queue_snapshot();
            if queue.pending_ids.is_empty() {
                if queue.blocked_ids.is_empty() {
                    parallel_logger.info("no unfinished `- [ ]` / `- [~]` tasks remain; stopping.");
                    // A completed plan does not prove every lane artifact is
                    // disposable: a stale/done lane can still contain dirty or
                    // unlanded work. Keep the ledger as the prune interlock until
                    // the same full lane inventory proof used by every other
                    // graceful terminal path succeeds.
                    let lanes_disposable = terminal_lane_repos_are_disposable(
                        repo_root,
                        run_root,
                        target_branch,
                        parallel_logger,
                    );
                    if clear_parallel_run_state_if_terminally_empty(
                        run_root,
                        lanes_disposable,
                        &shelved_tasks,
                        &deferred_partial_tasks,
                        &unblock_attempt_counts,
                        &attempted_partial_followups,
                    ) {
                        parallel_logger.info(
                            "run-state: cleared terminally empty ledger; disposable lane artifacts are now eligible for safe pruning",
                        );
                        // Drop the workspace baseline only with the ledger, so
                        // a preserved recovery run retains its original snapshot.
                        clear_workspace_baseline(run_root);
                    }
                } else {
                    parallel_logger.info(format!(
                        "all remaining tasks are blocked `[!]`; stopping. blocked: {}",
                        queue.blocked_ids.join(", ")
                    ));
                    let lanes_disposable = terminal_lane_repos_are_disposable(
                        repo_root,
                        run_root,
                        target_branch,
                        parallel_logger,
                    );
                    if clear_parallel_run_state_if_terminally_empty(
                        run_root,
                        lanes_disposable,
                        &shelved_tasks,
                        &deferred_partial_tasks,
                        &unblock_attempt_counts,
                        &attempted_partial_followups,
                    ) {
                        parallel_logger.info(
                            "run-state: cleared terminally empty ledger; disposable lane artifacts are now eligible for safe pruning",
                        );
                    }
                }
                break;
            }

            let recovered = recover_shelved_tasks_from_canonical_evidence(
                repo_root,
                target_branch,
                &mut shelved_tasks,
                parallel_logger,
            )?;
            if recovered > 0 {
                plan = refresh_parallel_plan_or_last_good(
                    repo_root,
                    target_branch,
                    linear_tracker,
                    &mut linear_auto_sync_state,
                    &plan,
                    parallel_logger,
                )
                .await?;
                last_idle_summary = None;
                continue;
            }

            // Before the terminal stop, surface the single most common real cause
            // of "nothing dispatchable while unfinished work remains": a workspace
            // that has never compiled this run. Without this, the stop message
            // above reads as an inscrutable scheduler giving up, when the true
            // fix is a broken build (e.g. a swept/missing `include_str!` fixture).
            if let Some(diag) =
                workspace_compile_block_diagnostic(&load_workspace_baseline(run_root))
            {
                parallel_logger.warn(format!("workspace-compile-block: {diag}"));
            }
            parallel_logger.info(no_dependency_ready_stop_message(
                &plan,
                &active_tasks,
                &queue,
                &shelved_tasks,
                &deferred_partial_tasks,
                &unblock_attempt_counts,
                max_autonomous_unblock_attempts,
            ));
            let lanes_disposable = terminal_lane_repos_are_disposable(
                repo_root,
                run_root,
                target_branch,
                parallel_logger,
            );
            if clear_parallel_run_state_if_terminally_empty(
                run_root,
                lanes_disposable,
                &shelved_tasks,
                &deferred_partial_tasks,
                &unblock_attempt_counts,
                &attempted_partial_followups,
            ) {
                parallel_logger.info(
                    "run-state: cleared terminally empty ledger; disposable lane artifacts are now eligible for safe pruning",
                );
            }
            break;
        }

        let joined = match tokio::time::timeout(LANE_POLL_INTERVAL, join_set.join_next()).await {
            Ok(Some(result)) => result,
            Ok(None) => {
                parallel_logger.warn(
                    "warning: parallel lane join set became empty while active lanes remained; stopping this host run so unfinished lane repos can be resumed safely on the next launch",
                );
                break;
            }
            Err(_) => continue,
        };
        let lane_result = match joined {
            Ok(lane_result) => lane_result,
            Err(err) => {
                parallel_logger.warn(format!(
                    "warning: parallel lane task panicked; stopping this host run so unfinished lane repos can be resumed safely on the next launch: {err}"
                ));
                break;
            }
        };
        let Some(mut assignment) = active_lanes.remove(&lane_result.lane_index) else {
            parallel_logger.warn(format!(
                "warning: missing active state for lane-{} after a worker completed; rebuilding active task bookkeeping and dropping the result",
                lane_result.lane_index
            ));
            rebuild_active_tasks(&mut active_tasks, &active_lanes);
            continue;
        };
        // Keep the durable marker present throughout host processing (including
        // a long canonical gate), then remove only this exact attempt's marker.
        // A fast retry cannot be erased by an older attempt's guard.
        let _host_pending_guard = LaneHostPendingGuard::new(&assignment);
        active_tasks.remove(&assignment.task.id);

        if let Some(violation) =
            detect_forbidden_worker_remote_git_command(&assignment.stdout_log_path)?
        {
            parallel_logger.warn(format!(
                "policy:      lane-{} `{}` attempted forbidden remote git command `{}`; shelving for the rest of this run. see {}",
                assignment.lane_index,
                assignment.task.id,
                violation.command,
                assignment.stdout_log_path.display()
            ));
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!(
                    "shelved: worker attempted forbidden remote git command `{}`; host owns remote sync",
                    violation.command
                ),
            );
            shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
            continue;
        }

        // Quota ride-out (F1 single-account gap): if this lane failed purely
        // because the Codex/Claude account is session-quota exhausted — not a
        // genuine task failure — and produced no landable work, wait out the
        // session reset and re-dispatch the SAME task instead of shelving the
        // run or burning a task-failure retry. Bounded + env-gated; falls
        // through to the normal handling below when it does not apply.
        if let Some(sleep_for) = maybe_lane_quota_ride_out(
            &assignment,
            &lane_config,
            &lane_result,
            &mut quota_ride_out,
            parallel_logger,
        )
        .await
        {
            tokio::time::sleep(sleep_for).await;
            // A quota wait is not a task failure: don't consume a retry attempt
            // (spawn_parallel_lane_attempt re-increments) and don't thread a
            // failure recovery note into the next prompt.
            assignment.attempts = assignment.attempts.saturating_sub(1);
            assignment.host_recovery_note = None;
            let plan_for_prompt = refresh_parallel_plan_or_last_good(
                repo_root,
                target_branch,
                linear_tracker,
                &mut linear_auto_sync_state,
                &plan,
                parallel_logger,
            )
            .await?;
            if let Err(err) = spawn_parallel_lane_attempt(
                &mut join_set,
                &lane_config,
                prompt_template,
                &plan_for_prompt,
                &mut assignment,
                target_branch,
            ) {
                parallel_logger.warn(format!(
                    "warning: failed re-dispatching lane-{} `{}` after a quota ride-out; shelving for the rest of this run: {err:#}",
                    assignment.lane_index, assignment.task.id
                ));
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            active_tasks.insert(assignment.task.id.clone());
            active_lanes.insert(assignment.lane_index, assignment);
            continue;
        }

        if let Some(error) = lane_result.error {
            eprintln!(
                "warning: lane-{} `{}` failed before producing an exit status; shelving for the rest of this run: {}",
                assignment.lane_index, assignment.task.id, error
            );
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("shelved: host failure before exit status: {error}"),
            );
            shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
            continue;
        }

        let Some(exit_status) = lane_result.exit_status else {
            shelve_lane_after_host_failure(
                &assignment,
                parallel_logger,
                &mut shelved_tasks,
                "lane attempt completed without an exit status or error",
            );
            continue;
        };

        if !exit_status.success() {
            let Some(progress) = inspect_lane_repo_progress_or_shelve(
                &assignment,
                parallel_logger,
                &mut shelved_tasks,
                "failed inspecting lane repo after a non-zero worker exit",
            ) else {
                continue;
            };
            match progress {
                LaneRepoProgress::NewCommits => {
                    match land_parallel_lane_result(
                        repo_root,
                        target_branch,
                        &mut assignment,
                        &review_config,
                    )
                    .await
                    {
                        Ok(LaneLandingOutcome::Landed {
                            auto_repaired,
                            completion_status,
                        }) => {
                            if completion_status == LoopTaskStatus::Done {
                                if let Some(tracker) = linear_tracker.as_mut() {
                                    if let Err(err) = tracker.note_done(&assignment.task.id).await {
                                        eprintln!(
                                            "warning: failed to archive `{}` in Linear: {err:#}",
                                            assignment.task.id
                                        );
                                    }
                                }
                            };
                            landed += 1;
                            let status_suffix = completion_status_suffix(
                                &assignment.task.id,
                                completion_status,
                                &mut attempted_partial_followups,
                                &mut deferred_partial_tasks,
                            );
                            if completion_status == LoopTaskStatus::Done {
                                unblock_attempt_counts.remove(&assignment.task.id);
                            }
                            let result_label = if auto_repaired {
                                "landed-with-host-repair-after-nonzero"
                            } else if completion_status == LoopTaskStatus::Partial {
                                "landed-partial-after-nonzero"
                            } else {
                                "landed-after-nonzero"
                            };
                            parallel_logger.info(format!(
                                "{result_label}: [{}] {} via lane-{}{} (total landed: {})",
                                classify_task_execution_kind(&assignment.task),
                                assignment.task.id,
                                assignment.lane_index,
                                status_suffix,
                                landed
                            ));
                            append_lane_host_event(
                                &assignment.stdout_log_path,
                                assignment.lane_index,
                                &assignment.task.id,
                                if auto_repaired {
                                    if completion_status == LoopTaskStatus::Partial {
                                        "landed-with-host-repair-after-nonzero: task remains [~] until local evidence is complete"
                                    } else {
                                        "landed-with-host-repair-after-nonzero: host harvested committed work"
                                    }
                                } else if completion_status == LoopTaskStatus::Partial {
                                    "landed-partial-after-nonzero: task remains [~] until local evidence is complete"
                                } else {
                                    "landed-after-nonzero: host harvested committed work"
                                },
                            );
                            last_idle_summary = None;
                            continue;
                        }
                        Ok(LaneLandingOutcome::NeedsRecovery {
                            recovery_note,
                            conflict_paths,
                        }) => {
                            match try_spawn_lane_recovery_attempt(
                                &mut join_set,
                                &lane_config,
                                prompt_template,
                                &plan,
                                &mut assignment,
                                target_branch,
                                args.max_retries,
                                parallel_logger,
                                "failed to land committed work after a non-zero worker exit",
                                recovery_note,
                            ) {
                                Ok(true) => {
                                    active_tasks.insert(assignment.task.id.clone());
                                    active_lanes.insert(assignment.lane_index, assignment);
                                    continue;
                                }
                                Ok(false) => {
                                    parallel_logger.warn(format!(
                                        "warning: failed landing lane-{} `{}` after non-zero worker exit and no recovery attempts remain; conflict paths: {}",
                                        assignment.lane_index,
                                        assignment.task.id,
                                        if conflict_paths.is_empty() {
                                            "unknown".to_string()
                                        } else {
                                            conflict_paths.join(", ")
                                        }
                                    ));
                                    if let Err(salvage_err) = write_parallel_salvage_record(
                                        &assignment,
                                        "host exhausted landing-recovery attempts after a non-zero worker exit",
                                    ) {
                                        parallel_logger.warn(format!(
                                            "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                            assignment.lane_index, assignment.task.id
                                        ));
                                    }
                                }
                                Err(retry_err) => {
                                    parallel_logger.warn(format!(
                                        "warning: failed restarting lane-{} `{}` after landing failure: {retry_err:#}; conflict paths: {}",
                                        assignment.lane_index,
                                        assignment.task.id,
                                        if conflict_paths.is_empty() {
                                            "unknown".to_string()
                                        } else {
                                            conflict_paths.join(", ")
                                        }
                                    ));
                                }
                            }
                            shelved_task_details.insert(
                                assignment.task.id.clone(),
                                ShelvedTaskDetails {
                                    markdown: assignment.task.markdown.clone(),
                                    failure_reason: ShelvedTaskFailureReason::LandingConflict,
                                    conflict_paths,
                                    detail: Some(
                                        "committed lane work conflicts with current canonical hunks"
                                            .to_string(),
                                    ),
                                },
                            );
                            shelved_tasks.insert(
                                assignment.task.id.clone(),
                                assignment.task.markdown.clone(),
                            );
                            continue;
                        }
                        Ok(LaneLandingOutcome::DivergenceExhausted { detail }) => {
                            parallel_logger.warn(format!(
                                "warning: failed landing lane-{} `{}` after non-zero worker exit and bounded fresh-HEAD retries; shelving as landing-divergence: {}",
                                assignment.lane_index, assignment.task.id, detail
                            ));
                            if let Err(salvage_err) =
                                write_parallel_salvage_record(&assignment, &detail)
                            {
                                parallel_logger.warn(format!(
                                    "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                    assignment.lane_index, assignment.task.id
                                ));
                            }
                            shelved_task_details.insert(
                                assignment.task.id.clone(),
                                ShelvedTaskDetails {
                                    markdown: assignment.task.markdown.clone(),
                                    failure_reason: ShelvedTaskFailureReason::LandingDivergence,
                                    conflict_paths: Vec::new(),
                                    detail: Some(detail),
                                },
                            );
                            shelved_tasks.insert(
                                assignment.task.id.clone(),
                                assignment.task.markdown.clone(),
                            );
                            continue;
                        }
                        Err(err) => {
                            let review_input_integrity_fatal =
                                landing_error_is_review_input_integrity_fatal(&err);
                            if review_input_integrity_fatal
                                && landing_error_has_unpersisted_review_quarantine(&err)
                            {
                                return Err(err).context(
                                    "independent reviewer mutated canonical inputs and durable quarantine persistence failed; preserving the restart-visible unsealed completion interlock",
                                );
                            }
                            recover_failed_parallel_lane_completion(
                                repo_root,
                                &assignment.task.id,
                                active_lanes.len(),
                                parallel_logger,
                                &format!("landing error after non-zero worker exit: {err:#}"),
                            )?;
                            if review_input_integrity_fatal {
                                return Err(err).context(
                                    "independent reviewer mutated canonical inputs; aborting the host loop before any checkpoint, dispatch, or push",
                                );
                            }
                            parallel_logger.warn(format!(
                                "warning: failed landing lane-{} `{}` after non-zero worker exit and no recovery attempts remain: {err:#}",
                                assignment.lane_index, assignment.task.id
                            ));
                            if let Err(salvage_err) =
                                write_parallel_salvage_record(&assignment, &format!("{err:#}"))
                            {
                                parallel_logger.warn(format!(
                                    "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                    assignment.lane_index, assignment.task.id
                                ));
                            }
                            let conflict_paths = conflict_paths_from_landing_error(&err);
                            if !conflict_paths.is_empty() {
                                shelved_task_details.insert(
                                    assignment.task.id.clone(),
                                    ShelvedTaskDetails {
                                        markdown: assignment.task.markdown.clone(),
                                        failure_reason: ShelvedTaskFailureReason::LandingConflict,
                                        conflict_paths,
                                        detail: Some(format!("{err:#}")),
                                    },
                                );
                            } else {
                                shelved_task_details.remove(&assignment.task.id);
                            }
                            shelved_tasks.insert(
                                assignment.task.id.clone(),
                                assignment.task.markdown.clone(),
                            );
                            continue;
                        }
                    }
                }
                LaneRepoProgress::Dirty(_)
                | LaneRepoProgress::NewCommitsWithDirty(_)
                | LaneRepoProgress::None => {}
            }
            if let Some(reason) = detect_lane_environment_blocker(&assignment) {
                let recovery_note = environment_blocker_recovery_note(
                    &reason,
                    &lane_config.preflight_prompt_clause,
                );
                match try_spawn_lane_recovery_attempt(
                    &mut join_set,
                    &lane_config,
                    prompt_template,
                    &plan,
                    &mut assignment,
                    target_branch,
                    args.max_retries,
                    parallel_logger,
                    "hit an external environment blocker",
                    recovery_note,
                ) {
                    Ok(true) => {
                        active_tasks.insert(assignment.task.id.clone());
                        active_lanes.insert(assignment.lane_index, assignment);
                        continue;
                    }
                    Ok(false) => {
                        parallel_logger.warn(format!(
                            "env-blocked: lane-{} `{}` exhausted retries after external blocker; shelving for the rest of this run: {}",
                            assignment.lane_index, assignment.task.id, reason
                        ));
                    }
                    Err(err) => {
                        parallel_logger.warn(format!(
                            "warning: failed restarting lane-{} `{}` after environment blocker: {err:#}; shelving for the rest of this run: {}",
                            assignment.lane_index, assignment.task.id, reason
                        ));
                    }
                }
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    &format!("env-blocked: {reason}"),
                );
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            let exit_code = exit_status.code().unwrap_or(-1);
            let is_futility = exit_code == FUTILITY_EXIT_MARKER;
            if assignment.attempts > args.max_retries {
                parallel_logger.warn(format!(
                    "warning: {} lane-{} (`{}`) exited with status {} after {} attempts; shelving for the rest of this run. see {}",
                    harness,
                    assignment.lane_index,
                    assignment.task.id,
                    if is_futility {
                        "futility".to_string()
                    } else {
                        exit_code.to_string()
                    },
                    assignment.attempts,
                    assignment.stderr_log_path.display()
                ));
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    &format!(
                        "shelved: worker exited {} after {} attempts",
                        if is_futility {
                            "with futility spiral".to_string()
                        } else {
                            format!("with code {exit_code}")
                        },
                        assignment.attempts
                    ),
                );
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }

            parallel_logger.info(format!(
                "warning: lane-{} `{}` exited non-zero ({}), retrying attempt {}/{}",
                assignment.lane_index,
                assignment.task.id,
                if is_futility {
                    "futility spiral".to_string()
                } else {
                    format!("code {exit_code}")
                },
                assignment.attempts,
                args.max_retries + 1
            ));
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!(
                    "retrying: worker exited {} on attempt {}/{}",
                    if is_futility {
                        "with futility spiral".to_string()
                    } else {
                        format!("with code {exit_code}")
                    },
                    assignment.attempts,
                    args.max_retries + 1
                ),
            );
            // Thread the terminal cause of THIS failed attempt into the next
            // prompt so the retry sees why it failed instead of re-running blind.
            assignment.host_recovery_note = Some(retry_failure_recovery_note(
                &assignment.lane_repo_root,
                &assignment.stdout_log_path,
                &assignment.stderr_log_path,
                exit_code,
                is_futility,
            ));
            let plan_for_prompt = refresh_parallel_plan_or_last_good(
                repo_root,
                target_branch,
                linear_tracker,
                &mut linear_auto_sync_state,
                &plan,
                parallel_logger,
            )
            .await?;
            if let Err(err) = spawn_parallel_lane_attempt(
                &mut join_set,
                &lane_config,
                prompt_template,
                &plan_for_prompt,
                &mut assignment,
                target_branch,
            ) {
                parallel_logger.warn(format!(
                    "warning: failed restarting lane-{} `{}`; shelving for the rest of this run: {err:#}",
                    assignment.lane_index, assignment.task.id
                ));
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            active_tasks.insert(assignment.task.id.clone());
            active_lanes.insert(assignment.lane_index, assignment);
            continue;
        }

        let Some(progress) = inspect_lane_repo_progress_or_shelve(
            &assignment,
            parallel_logger,
            &mut shelved_tasks,
            "failed inspecting lane repo after a successful worker exit",
        ) else {
            continue;
        };
        match progress {
            LaneRepoProgress::Dirty(status) | LaneRepoProgress::NewCommitsWithDirty(status) => {
                let recovery_note =
                    lane_repo_recovery_note(&assignment.lane_repo_root, target_branch, &status);
                match try_spawn_lane_recovery_attempt(
                    &mut join_set,
                    &lane_config,
                    prompt_template,
                    &plan,
                    &mut assignment,
                    target_branch,
                    args.max_retries,
                    parallel_logger,
                    "exited cleanly but left a dirty worktree",
                    recovery_note,
                ) {
                    Ok(true) => {
                        active_tasks.insert(assignment.task.id.clone());
                        active_lanes.insert(assignment.lane_index, assignment);
                        continue;
                    }
                    Ok(false) => {
                        parallel_logger.warn(format!(
                            "warning: parallel lane-{} (`{}`) exited cleanly but left uncommitted changes and no recovery attempts remain; shelving for the rest of this run:\n{}",
                            assignment.lane_index,
                            assignment.task.id,
                            status
                        ));
                    }
                    Err(err) => {
                        parallel_logger.warn(format!(
                            "warning: failed restarting lane-{} `{}` for dirty-worktree recovery: {err:#}; shelving for the rest of this run:\n{}",
                            assignment.lane_index, assignment.task.id, status
                        ));
                    }
                }
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    "shelved: worker exited cleanly but left uncommitted changes",
                );
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            LaneRepoProgress::None => {
                match reconcile_parallel_clean_no_commit(
                    repo_root,
                    target_branch,
                    &mut assignment,
                    parallel_logger,
                    &review_config,
                )
                .await
                {
                    Ok(true) => {
                        if push_parallel_clean_no_commit_closeout(
                            repo_root,
                            target_branch,
                            &assignment,
                        )? {
                            parallel_logger.info(format!(
                                "remote sync: rebased onto origin/{} after clean-no-commit closeout",
                                target_branch
                            ));
                        }
                        if let Some(tracker) = linear_tracker.as_mut() {
                            if let Err(err) = tracker.note_done(&assignment.task.id).await {
                                eprintln!(
                                    "warning: failed to archive `{}` in Linear: {err:#}",
                                    assignment.task.id
                                );
                            }
                        }
                        landed += 1;
                        attempted_partial_followups.remove(&assignment.task.id);
                        deferred_partial_tasks.remove(&assignment.task.id);
                        unblock_attempt_counts.remove(&assignment.task.id);
                        parallel_logger.info(format!(
                            "self-heal:   [{}] {} closed from canonical evidence after lane-{} exited cleanly without a commit (total landed: {})",
                            classify_task_execution_kind(&assignment.task),
                            assignment.task.id,
                            assignment.lane_index,
                            landed
                        ));
                        append_lane_host_event(
                            &assignment.stdout_log_path,
                            assignment.lane_index,
                            &assignment.task.id,
                            "self-heal: worker exited cleanly without a commit, but canonical review/receipt/artifact evidence is complete; host reconciled the task",
                        );
                        last_idle_summary = None;
                        continue;
                    }
                    Ok(false) => {}
                    Err(err) => {
                        let review_input_integrity_fatal =
                            landing_error_is_review_input_integrity_fatal(&err);
                        if review_input_integrity_fatal
                            && landing_error_has_unpersisted_review_quarantine(&err)
                        {
                            return Err(err).context(
                                "independent reviewer mutated canonical inputs and durable quarantine persistence failed; preserving the restart-visible unsealed completion interlock",
                            );
                        }
                        recover_failed_parallel_lane_completion(
                            repo_root,
                            &assignment.task.id,
                            active_lanes.len(),
                            parallel_logger,
                            &format!("clean-no-commit reconciliation error: {err:#}"),
                        )?;
                        if review_input_integrity_fatal {
                            return Err(err).context(
                                "independent reviewer mutated canonical inputs; aborting the host loop before any checkpoint, dispatch, or push",
                            );
                        }
                        parallel_logger.warn(format!(
                            "warning: failed checking canonical evidence for clean no-commit lane-{} `{}`: {err:#}",
                            assignment.lane_index, assignment.task.id
                        ));
                    }
                }
                parallel_logger.warn(format!(
                    "warning: parallel lane-{} (`{}`) exited cleanly without producing a local commit; shelving for the rest of this run. see {}",
                    assignment.lane_index,
                    assignment.task.id,
                    assignment.stderr_log_path.display()
                ));
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    "shelved: worker exited cleanly without producing a local commit",
                );
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            LaneRepoProgress::NewCommits => {
                match land_parallel_lane_result(
                    repo_root,
                    target_branch,
                    &mut assignment,
                    &review_config,
                )
                .await
                {
                    Ok(LaneLandingOutcome::Landed {
                        auto_repaired,
                        completion_status,
                    }) => {
                        if completion_status == LoopTaskStatus::Done {
                            if let Some(tracker) = linear_tracker.as_mut() {
                                if let Err(err) = tracker.note_done(&assignment.task.id).await {
                                    eprintln!(
                                        "warning: failed to archive `{}` in Linear: {err:#}",
                                        assignment.task.id
                                    );
                                }
                            }
                        }
                        landed += 1;
                        let status_suffix = completion_status_suffix(
                            &assignment.task.id,
                            completion_status,
                            &mut attempted_partial_followups,
                            &mut deferred_partial_tasks,
                        );
                        if completion_status == LoopTaskStatus::Done {
                            unblock_attempt_counts.remove(&assignment.task.id);
                        }
                        let result_label = if auto_repaired {
                            "landed-with-host-repair"
                        } else if completion_status == LoopTaskStatus::Partial {
                            "landed-partial"
                        } else {
                            "landed-clean"
                        };
                        parallel_logger.info(format!(
                            "{result_label}: [{}] {} via lane-{}{} (total landed: {})",
                            classify_task_execution_kind(&assignment.task),
                            assignment.task.id,
                            assignment.lane_index,
                            status_suffix,
                            landed
                        ));
                        append_lane_host_event(
                            &assignment.stdout_log_path,
                            assignment.lane_index,
                            &assignment.task.id,
                            if auto_repaired {
                                if completion_status == LoopTaskStatus::Partial {
                                    "landed-with-host-repair: task remains [~] until local evidence is complete"
                                } else {
                                    "landed-with-host-repair: host harvested committed work"
                                }
                            } else if completion_status == LoopTaskStatus::Partial {
                                "landed-partial: task remains [~] until local evidence is complete"
                            } else {
                                "landed-clean: host harvested committed work"
                            },
                        );
                        last_idle_summary = None;
                    }
                    Ok(LaneLandingOutcome::NeedsRecovery {
                        recovery_note,
                        conflict_paths,
                    }) => {
                        match try_spawn_lane_recovery_attempt(
                            &mut join_set,
                            &lane_config,
                            prompt_template,
                            &plan,
                            &mut assignment,
                            target_branch,
                            args.max_retries,
                            parallel_logger,
                            "failed to land committed work",
                            recovery_note,
                        ) {
                            Ok(true) => {
                                active_tasks.insert(assignment.task.id.clone());
                                active_lanes.insert(assignment.lane_index, assignment);
                                continue;
                            }
                            Ok(false) => {
                                let conflict_suffix = if conflict_paths.is_empty() {
                                    "unknown".to_string()
                                } else {
                                    conflict_paths.join(", ")
                                };
                                parallel_logger.warn(format!(
                                    "warning: failed landing lane-{} `{}` and no recovery attempts remain; conflict paths: {}",
                                    assignment.lane_index, assignment.task.id, conflict_suffix
                                ));
                                if let Err(salvage_err) = write_parallel_salvage_record(
                                    &assignment,
                                    "host exhausted landing-recovery attempts",
                                ) {
                                    parallel_logger.warn(format!(
                                        "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                        assignment.lane_index, assignment.task.id
                                    ));
                                }
                            }
                            Err(retry_err) => {
                                parallel_logger.warn(format!(
                                    "warning: failed restarting lane-{} `{}` after landing failure: {retry_err:#}; conflict paths: {}",
                                    assignment.lane_index,
                                    assignment.task.id,
                                    if conflict_paths.is_empty() {
                                        "unknown".to_string()
                                    } else {
                                        conflict_paths.join(", ")
                                    }
                                ));
                            }
                        }
                        shelved_task_details.insert(
                            assignment.task.id.clone(),
                            ShelvedTaskDetails {
                                markdown: assignment.task.markdown.clone(),
                                failure_reason: ShelvedTaskFailureReason::LandingConflict,
                                conflict_paths,
                                detail: Some(
                                    "committed lane work conflicts with current canonical hunks"
                                        .to_string(),
                                ),
                            },
                        );
                        shelved_tasks
                            .insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                        continue;
                    }
                    Ok(LaneLandingOutcome::DivergenceExhausted { detail }) => {
                        parallel_logger.warn(format!(
                            "warning: failed landing lane-{} `{}` after {} bounded fresh-HEAD retries; shelving as landing-divergence: {}",
                            assignment.lane_index,
                            assignment.task.id,
                            LANDING_REBASE_RETRY_LIMIT,
                            detail
                        ));
                        if let Err(salvage_err) =
                            write_parallel_salvage_record(&assignment, &detail)
                        {
                            parallel_logger.warn(format!(
                                "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                assignment.lane_index, assignment.task.id
                            ));
                        }
                        shelved_task_details.insert(
                            assignment.task.id.clone(),
                            ShelvedTaskDetails {
                                markdown: assignment.task.markdown.clone(),
                                failure_reason: ShelvedTaskFailureReason::LandingDivergence,
                                conflict_paths: Vec::new(),
                                detail: Some(detail),
                            },
                        );
                        shelved_tasks
                            .insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                        continue;
                    }
                    Err(err) => {
                        let review_input_integrity_fatal =
                            landing_error_is_review_input_integrity_fatal(&err);
                        if review_input_integrity_fatal
                            && landing_error_has_unpersisted_review_quarantine(&err)
                        {
                            return Err(err).context(
                                "independent reviewer mutated canonical inputs and durable quarantine persistence failed; preserving the restart-visible unsealed completion interlock",
                            );
                        }
                        recover_failed_parallel_lane_completion(
                            repo_root,
                            &assignment.task.id,
                            active_lanes.len(),
                            parallel_logger,
                            &format!("landing error: {err:#}"),
                        )?;
                        if review_input_integrity_fatal {
                            return Err(err).context(
                                "independent reviewer mutated canonical inputs; aborting the host loop before any checkpoint, dispatch, or push",
                            );
                        }
                        parallel_logger.warn(format!(
                            "warning: failed landing lane-{} `{}` and no recovery attempts remain; shelving for the rest of this run: {err:#}",
                            assignment.lane_index, assignment.task.id
                        ));
                        if let Err(salvage_err) =
                            write_parallel_salvage_record(&assignment, &format!("{err:#}"))
                        {
                            parallel_logger.warn(format!(
                                "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                assignment.lane_index, assignment.task.id
                            ));
                        }
                        let conflict_paths = conflict_paths_from_landing_error(&err);
                        if !conflict_paths.is_empty() {
                            shelved_task_details.insert(
                                assignment.task.id.clone(),
                                ShelvedTaskDetails {
                                    markdown: assignment.task.markdown.clone(),
                                    failure_reason: ShelvedTaskFailureReason::LandingConflict,
                                    conflict_paths,
                                    detail: Some(format!("{err:#}")),
                                },
                            );
                        } else {
                            shelved_task_details.remove(&assignment.task.id);
                        }
                        shelved_tasks
                            .insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                        continue;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Fingerprint of the source and normalized task contracts a drift-reverify
/// sweep could depend on. Host-owned queue commits must not invalidate a sweep:
/// they change HEAD and REVIEW/triage files, but do not change verified source.
/// The shared source-state collector still hashes tracked, staged, modified,
/// and untracked source contents and fails closed on collection errors.
fn drift_sweep_input_fingerprint(repo_root: &Path) -> Option<String> {
    current_dirty_state_fingerprint(repo_root)
}

pub(crate) async fn refresh_parallel_plan(
    repo_root: &Path,
    target_branch: &str,
    linear_tracker: &mut Option<LinearTracker>,
    linear_auto_sync_state: &mut LinearAutoSyncState,
    parallel_logger: &ParallelEventLogger,
) -> Result<LoopPlanSnapshot> {
    let mut plan_text = read_loop_plan(repo_root)?;
    // Content-addressed sweep skip (2026-07-10 radical harness fix, all repos):
    // the drift-reverify sweep re-runs every row's declared verification
    // defensively on every refresh and every restart, which for slow
    // (Java-oracle / cargo) verification burned 1500s+ per sweep and deferred
    // real work. A cross-task regression can only exist if some verification
    // input changed; the fingerprint below (HEAD + full working-tree status =
    // source + plan + receipts) changes iff any such input changed. When it
    // matches the last EXHAUSTIVE sweep, re-running would reproduce the same
    // result, so the sweep is skipped and lanes dispatch immediately.
    let run_root = parallel_logger.run_root();
    let sweep_fp = drift_sweep_input_fingerprint(repo_root);
    let already_swept = sweep_fp
        .as_deref()
        .zip(load_completed_sweep_fingerprint(&run_root).as_deref())
        .map(|(now, last)| now == last)
        .unwrap_or(false);
    // The skipped branch runs on every idle host refresh. The persisted sweep
    // fingerprint is already the evidence that no work is needed; do not log
    // the same no-op decision every poll and bury actionable worker events.
    if !already_swept {
        let (audited, exhaustive) =
            audit_parallel_completion_drift(repo_root, target_branch, &plan_text, parallel_logger)
                .await?;
        plan_text = audited;
        // Only cache when the sweep verified every row (no budget defer), and
        // recompute the fingerprint AFTER the audit's own receipt/plan writes.
        if exhaustive {
            if let Some(fp) = drift_sweep_input_fingerprint(repo_root) {
                save_completed_sweep_fingerprint(&run_root, &fp);
            }
        } else {
            clear_completed_sweep_fingerprint(&run_root);
        }
    }
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
                    planner_model: "gpt-5.6-sol".to_string(),
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

pub(crate) async fn refresh_parallel_plan_or_last_good(
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

pub(crate) fn is_linear_usage_limit_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("usage_limit_exceeded")
        || message.contains("usage limit exceeded")
        || message.contains("exceeded the free issue limit")
        || message.contains("\"activeissuecount\"")
}

pub(crate) fn maybe_disable_linear_auto_sync_for_run(
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

pub(crate) fn normalize_parallel_live_log_message(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

pub(crate) fn redact_parallel_live_log_message(message: &str) -> String {
    static BEARER_RE: OnceLock<Regex> = OnceLock::new();
    static ASSIGNMENT_RE: OnceLock<Regex> = OnceLock::new();

    let bearer_re = BEARER_RE.get_or_init(|| {
        Regex::new(r"(?i)(authorization:\s*bearer\s+)([^\s]+)")
            .expect("valid bearer-token redaction regex")
    });
    let assignment_re = ASSIGNMENT_RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b([A-Z0-9_]*(?:TOKEN|SECRET|PASSWORD|PASS|API_KEY|PRIVATE_KEY|ACCESS_KEY))=([^\s]+)",
        )
        .expect("valid env-assignment redaction regex")
    });

    let redacted = bearer_re.replace_all(message, "$1[REDACTED]");
    assignment_re
        .replace_all(&redacted, "$1=[REDACTED]")
        .into_owned()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ForbiddenWorkerRemoteGitCommand {
    command: String,
    verb: String,
}

fn detect_forbidden_worker_remote_git_command(
    rendered_stdout_log_path: &Path,
) -> Result<Option<ForbiddenWorkerRemoteGitCommand>> {
    if !rendered_stdout_log_path.exists() {
        return Ok(None);
    }
    let log_text = fs::read_to_string(rendered_stdout_log_path)
        .with_context(|| format!("failed to read {}", rendered_stdout_log_path.display()))?;
    Ok(detect_forbidden_worker_remote_git_command_in_rendered_log(
        &log_text,
    ))
}

fn detect_forbidden_worker_remote_git_command_in_rendered_log(
    log_text: &str,
) -> Option<ForbiddenWorkerRemoteGitCommand> {
    let mut in_command = false;
    let mut command_lines = Vec::new();

    for raw_line in log_text.lines() {
        let sanitized = strip_ansi_codes(raw_line);
        let line = strip_auto_stream_prefix(&sanitized);
        let trimmed = line.trim();

        if trimmed == "command:" {
            if let Some(violation) =
                detect_forbidden_worker_remote_git_command_lines(&command_lines)
            {
                return Some(violation);
            }
            command_lines.clear();
            in_command = true;
            continue;
        }

        if !in_command {
            continue;
        }

        if line.chars().next().is_some_and(char::is_whitespace) {
            command_lines.push(line.trim().to_string());
            continue;
        }

        if let Some(violation) = detect_forbidden_worker_remote_git_command_lines(&command_lines) {
            return Some(violation);
        }
        command_lines.clear();
        in_command = false;
    }

    detect_forbidden_worker_remote_git_command_lines(&command_lines)
}

fn detect_forbidden_worker_remote_git_command_lines(
    command_lines: &[String],
) -> Option<ForbiddenWorkerRemoteGitCommand> {
    let command = command_lines.join(" ");
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    forbidden_remote_git_verb(command).map(|verb| ForbiddenWorkerRemoteGitCommand {
        command: command.to_string(),
        verb,
    })
}

fn terminal_lane_repos_are_disposable(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    parallel_logger: &ParallelEventLogger,
) -> bool {
    match parallel_lane_repos_are_disposable(repo_root, run_root, target_branch) {
        Ok(disposable) => disposable,
        Err(err) => {
            parallel_logger.warn(format!(
                "warning: preserving terminal run-state ledger because lane disposal proof failed: {err:#}"
            ));
            false
        }
    }
}

fn forbidden_remote_git_verb(command: &str) -> Option<String> {
    forbidden_remote_git_verb_with_depth(command, 0)
}

fn forbidden_remote_git_verb_with_depth(command: &str, depth: usize) -> Option<String> {
    if depth > 4 {
        return None;
    }
    let tokens = shell_tokens(command);
    if tokens.is_empty() {
        return None;
    }

    for (index, token) in tokens.iter().enumerate() {
        if !is_shell_invocation(token) {
            continue;
        }
        let mut cursor = index + 1;
        while cursor < tokens.len() {
            let token = tokens[cursor].as_str();
            if matches!(token, "-c" | "-lc") {
                if let Some(script) = tokens.get(cursor + 1) {
                    if let Some(verb) = forbidden_remote_git_verb_with_depth(script, depth + 1) {
                        return Some(verb);
                    }
                }
                break;
            }
            cursor += 1;
        }
    }

    let mut start = 0usize;
    while start < tokens.len() {
        while start < tokens.len() && is_shell_separator(&tokens[start]) {
            start += 1;
        }
        let mut end = start;
        while end < tokens.len() && !is_shell_separator(&tokens[end]) {
            end += 1;
        }
        if start < end {
            if let Some(verb) = forbidden_remote_git_verb_in_segment(&tokens[start..end]) {
                return Some(verb);
            }
        }
        start = end + 1;
    }

    None
}

fn forbidden_remote_git_verb_in_segment(tokens: &[String]) -> Option<String> {
    let mut cursor = 0usize;
    while cursor < tokens.len() {
        match tokens[cursor].as_str() {
            "sudo" | "command" | "time" => cursor += 1,
            "env" | "/usr/bin/env" => {
                cursor += 1;
                while cursor < tokens.len() && looks_like_env_assignment(&tokens[cursor]) {
                    cursor += 1;
                }
            }
            _ => break,
        }
    }
    if cursor >= tokens.len() || !is_git_invocation(&tokens[cursor]) {
        return None;
    }

    cursor += 1;
    while cursor < tokens.len() {
        let token = tokens[cursor].as_str();
        if matches!(
            token,
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace"
        ) {
            cursor += 2;
            continue;
        }
        if token.starts_with("--git-dir=")
            || token.starts_with("--work-tree=")
            || token.starts_with("--namespace=")
        {
            cursor += 1;
            continue;
        }
        if token.starts_with('-') {
            cursor += 1;
            continue;
        }
        break;
    }

    tokens.get(cursor).and_then(|verb| {
        let verb = verb.as_str();
        if matches!(verb, "push" | "pull" | "fetch" | "rebase") {
            Some(verb.to_string())
        } else {
            None
        }
    })
}

fn shell_tokens(command: &str) -> Vec<String> {
    shlex::split(command).unwrap_or_else(|| {
        command
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
    })
}

fn looks_like_env_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(key, _)| {
        !key.is_empty()
            && key
                .chars()
                .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    })
}

fn is_shell_separator(token: &str) -> bool {
    matches!(token, "&&" | "||" | ";" | "|")
}

fn is_shell_invocation(token: &str) -> bool {
    command_basename(token).is_some_and(|name| matches!(name, "sh" | "bash" | "zsh" | "dash"))
}

fn is_git_invocation(token: &str) -> bool {
    command_basename(token).is_some_and(|name| name == "git")
}

fn command_basename(token: &str) -> Option<&str> {
    token.rsplit('/').next().filter(|name| !name.is_empty())
}

fn strip_auto_stream_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some((_, after)) = rest.split_once("] ") {
            return after;
        }
    }
    line
}

fn strip_ansi_codes(input: &str) -> String {
    static ANSI_RE: OnceLock<Regex> = OnceLock::new();
    ANSI_RE
        .get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("valid ansi regex"))
        .replace_all(input, "")
        .into_owned()
}

/// Per-task quota ride-out budget (see `maybe_lane_quota_ride_out`).
#[derive(Default)]
struct QuotaRideOutState {
    waited: Duration,
    waits: u32,
}

/// Decide whether a failed lane should ride out a Codex/Claude session-quota
/// window and re-dispatch the SAME task, instead of shelving / burning a
/// task-failure retry (the F1 single-account gap: the only live account is
/// over-quota, the router selects it anyway, and the exec fails with a
/// usage-limit signature that the plain non-zero-exit path would treat as a
/// task failure).
///
/// Returns `Some(sleep)` only when ALL of: the lane failed, the failure carries
/// a real usage-limit signature (log tail via `quota_patterns`, or a
/// `run_with_quota` all-accounts bail), the lane produced no landable work,
/// live usage confirms every account is session-exhausted with a concrete reset
/// horizon, and that reset fits the bounded per-task budget. Otherwise `None` —
/// the caller falls through to the normal retry/shelve path. Env-gate:
/// `AUTO_QUOTA_BACKOFF_MAX_SECS=0` disables it entirely.
async fn maybe_lane_quota_ride_out(
    assignment: &ActiveLaneAssignment,
    lane_config: &LaneRunConfig,
    lane_result: &LaneAttemptResult,
    ledger: &mut BTreeMap<String, QuotaRideOutState>,
    parallel_logger: &ParallelEventLogger,
) -> Option<Duration> {
    let provider = if lane_config.claude {
        crate::quota_config::Provider::Claude
    } else {
        crate::quota_config::Provider::Codex
    };
    if !crate::quota_exec::is_quota_available(provider) {
        return None;
    }

    // Only an actual failure can be a quota exhaustion.
    let failed =
        lane_result.error.is_some() || lane_result.exit_status.is_some_and(|s| !s.success());
    if !failed {
        return None;
    }

    // Cheap signature gate first (log tail + bail-string), before any git /
    // network IO, so healthy runs and ordinary failures pay almost nothing.
    let verdict = crate::quota_exec::lane_output_quota_verdict(
        provider,
        Some(&assignment.stdout_log_path),
        Some(&assignment.stderr_log_path),
    );
    let bail_is_quota = lane_result
        .error
        .as_deref()
        .is_some_and(crate::quota_exec::error_text_is_quota_exhaustion);
    let signature_exhausted =
        matches!(verdict, crate::quota_patterns::QuotaVerdict::Exhausted) || bail_is_quota;
    if !signature_exhausted {
        return None;
    }

    // Never clobber committed/dirty work: if the worker produced anything
    // landable before hitting the wall, let the normal landing path harvest it.
    match inspect_lane_repo_progress(&assignment.lane_repo_root, &assignment.base_commit) {
        Ok(LaneRepoProgress::None) => {}
        _ => return None,
    }

    // Confirm against live usage and get the reset horizon. `None` (usage
    // unknown / disabled / all-invalid) means we do NOT wait — a missing reset
    // horizon can never cause a hot-loop.
    let (all_exhausted, soonest) = crate::quota_exec::probe_session_exhaustion(provider).await?;
    let cap = crate::quota_exec::lane_quota_backoff_cap();
    let entry = ledger.entry(assignment.task.id.clone()).or_default();
    let sleep_for = crate::quota_exec::lane_quota_backoff_decision(
        cap,
        entry.waited,
        entry.waits,
        signature_exhausted,
        all_exhausted,
        soonest,
    )?;
    entry.waited = entry.waited.saturating_add(sleep_for);
    entry.waits += 1;

    let message = format!(
        "quota-ride-out: lane-{} `{}` {provider} session-quota exhausted; waiting {}s for the session reset then re-dispatching the SAME task (wait {}/{}, budget {}s/{}s). Set AUTO_QUOTA_BACKOFF_MAX_SECS=0 to disable, or higher to ride out longer windows.",
        assignment.lane_index,
        assignment.task.id,
        sleep_for.as_secs(),
        entry.waits,
        crate::quota_exec::LANE_QUOTA_MAX_WAITS_PER_TASK,
        entry.waited.as_secs(),
        cap.as_secs(),
    );
    parallel_logger.info(&message);
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &message,
    );
    Some(sleep_for)
}

pub(crate) fn spawn_parallel_lane_attempt(
    join_set: &mut JoinSet<LaneAttemptResult>,
    lane_config: &LaneRunConfig,
    prompt_template: &str,
    plan: &LoopPlanSnapshot,
    assignment: &mut ActiveLaneAssignment,
    target_branch: &str,
) -> Result<()> {
    clear_lane_host_pending_marker(&assignment.lane_root)?;
    assignment.attempts += 1;
    assignment.clean_commit_since = None;
    assignment.terminate_requested_at = None;
    refresh_assignment_task_from_plan(plan, assignment);
    let full_prompt = build_parallel_lane_prompt(
        prompt_template,
        plan,
        &assignment.task,
        target_branch,
        &lane_config.cargo_target_prompt_clause,
        &lane_config.preflight_prompt_clause,
        assignment.host_recovery_note.as_deref(),
    );
    let prompt_path = assignment.lane_root.join(format!(
        "{}-attempt-{:02}-prompt.md",
        assignment.task.id, assignment.attempts
    ));
    let repo_root = assignment.lane_repo_root.clone();
    let stderr_log_path = assignment.stderr_log_path.clone();
    let stdout_log_path = assignment.stdout_log_path.clone();
    let worker_pid_path = assignment.worker_pid_path.clone();
    let extra_env = lane_config.env_for_lane(&assignment.lane_root);
    let lane_index = assignment.lane_index;
    let task_id = assignment.task.id.clone();
    let lane_root = assignment.lane_root.clone();
    let attempt = assignment.attempts;
    let effort = lane_config.effective_reasoning_effort(
        assignment.task.estimated_scope.as_deref(),
        assignment.attempts,
    );
    if effort != lane_config.reasoning_effort {
        eprintln!(
            "effort-routing: {} scope {} attempt {} -> {} (ceiling {})",
            task_id,
            assignment.task.estimated_scope.as_deref().unwrap_or("?"),
            assignment.attempts,
            effort,
            lane_config.reasoning_effort
        );
    }
    let lane_config = lane_config.clone();

    join_set.spawn(async move {
        if let Err(err) = atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))
        {
            publish_lane_host_pending_marker_best_effort(&lane_root, lane_index, &task_id, attempt);
            return LaneAttemptResult {
                lane_index,
                exit_status: None,
                error: Some(format!("{err:#}")),
            };
        }
        let context_label = format!("auto parallel lane-{lane_index} {task_id}");
        let exit_status = if lane_config.claude {
            run_claude_exec_with_env(
                &repo_root,
                &full_prompt,
                &lane_config.model,
                &effort,
                lane_config.max_turns,
                &stderr_log_path,
                Some(&stdout_log_path),
                &context_label,
                &extra_env,
                Some(&worker_pid_path),
                None,
            )
            .await
        } else {
            run_codex_exec_with_env(
                &repo_root,
                &full_prompt,
                &lane_config.model,
                &effort,
                &lane_config.codex_bin,
                &stderr_log_path,
                Some(&stdout_log_path),
                &context_label,
                &extra_env,
                Some(&worker_pid_path),
                None,
            )
            .await
        };
        let result = match exit_status {
            Ok(exit_status) => LaneAttemptResult {
                lane_index,
                exit_status: Some(exit_status),
                error: None,
            },
            Err(err) => LaneAttemptResult {
                lane_index,
                exit_status: None,
                error: Some(format!("{err:#}")),
            },
        };
        // Publish immediately before JoinSet can expose this completed result
        // to the host. Cancellation/panic does not create a false queue item.
        publish_lane_host_pending_marker_best_effort(&lane_root, lane_index, &task_id, attempt);
        result
    });
    Ok(())
}

pub(crate) fn nudge_lingering_committed_lanes(
    active_lanes: &mut BTreeMap<usize, ActiveLaneAssignment>,
) {
    for assignment in active_lanes.values_mut() {
        let progress = match inspect_lane_repo_progress(
            &assignment.lane_repo_root,
            &assignment.base_commit,
        ) {
            Ok(progress) => progress,
            Err(err) => {
                eprintln!(
                    "warning: failed inspecting lane-{} `{}` while checking for harvestable commits: {err:#}",
                    assignment.lane_index, assignment.task.id
                );
                assignment.clean_commit_since = None;
                assignment.terminate_requested_at = None;
                continue;
            }
        };
        match progress {
            LaneRepoProgress::NewCommits => {
                let identity = match read_worker_pid_identity(&assignment.worker_pid_path) {
                    Ok(identity) => identity,
                    Err(err) => {
                        eprintln!(
                            "warning: failed reading worker pid for lane-{} `{}`: {err:#}",
                            assignment.lane_index, assignment.task.id
                        );
                        assignment.clean_commit_since = None;
                        assignment.terminate_requested_at = None;
                        continue;
                    }
                };
                let Some(identity) = identity else {
                    assignment.clean_commit_since = None;
                    assignment.terminate_requested_at = None;
                    continue;
                };
                let pid = identity.pid();

                let commit_since = assignment
                    .clean_commit_since
                    .get_or_insert_with(Instant::now);
                if let Some(requested_at) = assignment.terminate_requested_at {
                    if requested_at.elapsed() >= CLEAN_COMMIT_KILL_GRACE {
                        match signal_worker_identity(
                            &assignment.worker_pid_path,
                            &identity,
                            "KILL",
                        ) {
                            Err(err) => eprintln!(
                                "warning: failed sending SIGKILL to lingering worker pid {} for lane-{} `{}`: {err:#}",
                                pid, assignment.lane_index, assignment.task.id
                            ),
                            Ok(false) => eprintln!(
                                "warning: skipped SIGKILL for lane-{} `{}` because worker pid {} no longer owns the same identity-bound lease",
                                assignment.lane_index, assignment.task.id, pid
                            ),
                            Ok(true) => {
                                println!(
                                    "harvest:     lane-{} `{}` still lingered after clean commit; sent SIGKILL to pid {}",
                                    assignment.lane_index, assignment.task.id, pid
                                );
                                append_lane_host_event(
                                    &assignment.stdout_log_path,
                                    assignment.lane_index,
                                    &assignment.task.id,
                                    &format!(
                                        "harvest: sent SIGKILL to lingering worker pid {pid} after clean commit"
                                    ),
                                );
                            }
                        }
                        assignment.terminate_requested_at = None;
                    }
                    continue;
                }

                if !clean_commit_harvest_ready(
                    commit_since.elapsed(),
                    path_modified_elapsed(&assignment.stdout_log_path)
                        .ok()
                        .flatten(),
                ) {
                    continue;
                }

                match signal_worker_identity(&assignment.worker_pid_path, &identity, "TERM") {
                    Err(err) => {
                        eprintln!(
                                "warning: failed sending SIGTERM to lingering worker pid {} for lane-{} `{}`: {err:#}",
                                pid, assignment.lane_index, assignment.task.id
                            );
                        assignment.terminate_requested_at = None;
                    }
                    Ok(false) => {
                        eprintln!(
                                "warning: skipped SIGTERM for lane-{} `{}` because worker pid {} no longer owns the same identity-bound lease",
                                assignment.lane_index, assignment.task.id, pid
                            );
                        assignment.clean_commit_since = None;
                        assignment.terminate_requested_at = None;
                    }
                    Ok(true) => {
                        println!(
                                "harvest:     lane-{} `{}` has a clean local commit; sent SIGTERM to lingering pid {}",
                                assignment.lane_index, assignment.task.id, pid
                            );
                        append_lane_host_event(
                                &assignment.stdout_log_path,
                                assignment.lane_index,
                                &assignment.task.id,
                                &format!(
                                    "harvest: sent SIGTERM to lingering worker pid {pid} after clean commit"
                                ),
                            );
                        assignment.terminate_requested_at = Some(Instant::now());
                    }
                }
            }
            LaneRepoProgress::Dirty(_)
            | LaneRepoProgress::NewCommitsWithDirty(_)
            | LaneRepoProgress::None => {
                assignment.clean_commit_since = None;
                assignment.terminate_requested_at = None;
            }
        }
    }
}

pub(crate) fn clean_commit_harvest_ready(
    clean_commit_elapsed: Duration,
    last_output_elapsed: Option<Duration>,
) -> bool {
    clean_commit_elapsed >= CLEAN_COMMIT_GRACE
        && last_output_elapsed.is_none_or(|elapsed| elapsed >= CLEAN_COMMIT_QUIET_GRACE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::sync::{Arc, Mutex};
    use std::time::UNIX_EPOCH;
    use tokio::sync::Notify;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    fn ready_task(id: &str, lane_kind: LaneKind, markdown: &str) -> LoopTask {
        LoopTask {
            id: id.to_string(),
            title: format!("{id} title"),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind,
            markdown: markdown.to_string(),
        }
    }

    fn init_parallel_scheduler_repo(label: &str, plan: &str) -> PathBuf {
        let repo = unique_temp_dir(label);
        fs::create_dir_all(&repo).expect("failed to create scheduler repo");
        run_git(&repo, ["init"]).expect("failed to initialize scheduler repo");
        run_git(&repo, ["config", "user.name", "autodev tests"])
            .expect("failed to configure git user");
        run_git(&repo, ["config", "user.email", "autodev@example.com"])
            .expect("failed to configure git email");
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write seed plan");
        run_git(&repo, ["add", "IMPLEMENTATION_PLAN.md"]).expect("failed to stage seed plan");
        run_git(&repo, ["commit", "-m", "seed scheduler plan"])
            .expect("failed to commit seed plan");
        repo
    }

    #[test]
    fn host_pending_marker_is_run_bound_and_attempt_owned() {
        let run_root = unique_temp_dir("parallel-host-pending-marker");
        let lane_root = run_root.join("lanes/lane-2");
        fs::create_dir_all(&lane_root).expect("create lane root");
        fs::write(run_root.join(CURRENT_RUN_ID_FILE), "run-current\n")
            .expect("write current run id");
        fs::write(lane_root.join(LANE_RUN_ID_FILE), "run-current\n").expect("write lane run id");

        publish_lane_host_pending_marker(&lane_root, 2, "TASK-WAIT", 1).expect("publish marker");
        let marker: LaneHostPendingMarker = serde_json::from_slice(
            &fs::read(lane_root.join(LANE_HOST_PENDING_FILE)).expect("read marker"),
        )
        .expect("parse marker");
        assert_eq!(marker.run_id, "run-current");
        assert_eq!(marker.task_id, "TASK-WAIT");
        assert_eq!(marker.attempt, 1);

        let older_guard = LaneHostPendingGuard {
            lane_root: lane_root.clone(),
            task_id: "TASK-WAIT".to_string(),
            attempt: 1,
        };
        publish_lane_host_pending_marker(&lane_root, 2, "TASK-WAIT", 2)
            .expect("publish replacement marker");
        drop(older_guard);
        assert!(
            lane_root.join(LANE_HOST_PENDING_FILE).exists(),
            "older attempt must not clear a replacement marker"
        );

        let current_guard = LaneHostPendingGuard {
            lane_root: lane_root.clone(),
            task_id: "TASK-WAIT".to_string(),
            attempt: 2,
        };
        drop(current_guard);
        assert!(!lane_root.join(LANE_HOST_PENDING_FILE).exists());
        fs::remove_dir_all(run_root).expect("remove run root");
    }

    #[tokio::test]
    async fn startup_workspace_baseline_begins_after_worker_dispatch_and_before_harvest() {
        let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        events.lock().expect("lock events").push("worker-started");
        let capture_events = Arc::clone(&events);
        let mut pending = true;

        resolve_startup_workspace_baseline_after_dispatch(&mut pending, true, async move {
            capture_events
                .lock()
                .expect("lock capture events")
                .push("baseline-started");
            tokio::task::yield_now().await;
            capture_events
                .lock()
                .expect("lock capture events")
                .push("baseline-completed");
            Ok(())
        })
        .await
        .expect("resolve startup baseline");
        events.lock().expect("lock events").push("harvest-allowed");

        assert_eq!(
            *events.lock().expect("lock final events"),
            vec![
                "worker-started",
                "baseline-started",
                "baseline-completed",
                "harvest-allowed"
            ]
        );
        assert!(!pending);
    }

    #[tokio::test]
    async fn startup_workspace_barrier_blocks_canonical_queue_mutation_until_completion() {
        let original = "- [ ] `TASK-A` Wait behind startup baseline\n  Dependencies: none\n";
        let repo = init_parallel_scheduler_repo("startup-baseline-queue-barrier", original);
        let original_head = current_repo_head(&repo).expect("read original HEAD");
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let capture_entered = Arc::clone(&entered);
        let capture_release = Arc::clone(&release);
        let mut pending = true;
        {
            let barrier =
                resolve_startup_workspace_baseline_after_dispatch(&mut pending, true, async move {
                    capture_entered.notify_one();
                    capture_release.notified().await;
                    Ok(())
                });
            tokio::pin!(barrier);

            tokio::select! {
                result = &mut barrier => panic!("startup barrier completed before the probe was released: {result:?}"),
                _ = entered.notified() => {}
            }
            assert_eq!(
                fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("read frozen plan"),
                original
            );
            assert_eq!(
                current_repo_head(&repo).as_deref(),
                Some(original_head.as_str())
            );

            release.notify_one();
            barrier.await.expect("resolve startup barrier");
        }
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            original.replace("- [ ]", "- [~]"),
        )
        .expect("mutate queue after barrier");
        assert!(!pending);
        assert!(fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md"))
            .expect("read updated plan")
            .contains("- [~]"));

        fs::remove_dir_all(repo).expect("clean startup barrier fixture");
    }

    #[test]
    fn drift_sweep_fingerprint_ignores_host_queue_commits_but_tracks_source() {
        let repo = init_parallel_scheduler_repo(
            "parallel-drift-source-fingerprint",
            "- [ ] `TASK-A` Verify source\n  Dependencies: none\n",
        );
        fs::write(repo.join("source.rs"), "pub fn value() -> u8 { 1 }\n")
            .expect("failed to write source fixture");
        run_git(&repo, ["add", "source.rs"]).expect("failed to stage source fixture");
        run_git(&repo, ["commit", "-m", "seed source fixture"])
            .expect("failed to commit source fixture");
        let before = drift_sweep_input_fingerprint(&repo).expect("baseline fingerprint");

        fs::write(repo.join("REVIEW.md"), "# REVIEW\n\nhost queue update\n")
            .expect("failed to write host review fixture");
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-A` Verify source\n  Dependencies: none\n",
        )
        .expect("failed to mark queue item done");
        run_git(&repo, ["add", "REVIEW.md", "IMPLEMENTATION_PLAN.md"])
            .expect("failed to stage host queue update");
        run_git(&repo, ["commit", "-m", "host queue sync"])
            .expect("failed to commit host queue update");
        let after_queue = drift_sweep_input_fingerprint(&repo).expect("queue-only fingerprint");
        assert_eq!(
            before, after_queue,
            "host queue commits must not trigger an exhaustive drift reverify"
        );

        fs::write(repo.join("source.rs"), "pub fn value() -> u8 { 2 }\n")
            .expect("failed to change source fixture");
        let after_source =
            drift_sweep_input_fingerprint(&repo).expect("changed-source fingerprint");
        assert_ne!(
            after_queue, after_source,
            "source changes must trigger an exhaustive drift reverify"
        );

        fs::remove_dir_all(repo).expect("failed to remove fingerprint fixture");
    }

    #[test]
    fn post_stage_landing_error_with_second_lane_recovers_before_dependent_dispatch() {
        let partial_plan = "\
# IMPLEMENTATION_PLAN

- [~] `TASK-A` Candidate closeout
  Dependencies: none

- [ ] `TASK-B` Already active second lane
  Dependencies: none

- [ ] `TASK-C` Must wait for candidate closeout
  Dependencies: `TASK-A`
";
        let repo = init_parallel_scheduler_repo("parallel-post-stage-failure", partial_plan);
        let run_root = unique_temp_dir("parallel-post-stage-failure-run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("failed to initialize logger");

        // Inject the exact failed-landing state: lane A wrote and staged its
        // candidate Done row, then host processing failed while lane B was
        // still active.
        let candidate_done = partial_plan.replacen(
            "- [~] `TASK-A` Candidate closeout",
            "- [x] `TASK-A` Candidate closeout",
            1,
        );
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), candidate_done)
            .expect("failed to write candidate Done plan");
        run_git(&repo, ["add", "IMPLEMENTATION_PLAN.md"])
            .expect("failed to stage candidate Done plan");

        let recovered = recover_failed_parallel_lane_completion(
            &repo,
            "TASK-A",
            1,
            &logger,
            "injected failure after candidate Done was staged",
        )
        .expect("scheduler must recover before continuing with lane B");

        assert_eq!(recovered, vec!["TASK-A".to_string()]);
        let worktree = fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md"))
            .expect("failed to read recovered worktree plan");
        let indexed = git_stdout(&repo, ["show", ":IMPLEMENTATION_PLAN.md"])
            .expect("failed to read recovered index plan");
        assert!(worktree.contains("- [~] `TASK-A` Candidate closeout"));
        assert!(indexed.contains("- [~] `TASK-A` Candidate closeout"));

        let plan = parse_loop_plan(&worktree);
        let active_tasks = BTreeSet::from(["TASK-B".to_string()]);
        let shelved_tasks = BTreeMap::from([(
            "TASK-A".to_string(),
            plan.task("TASK-A")
                .expect("TASK-A should remain in plan")
                .markdown
                .clone(),
        )]);
        let ready = ready_parallel_tasks_with_gate_holds(
            &plan,
            &active_tasks,
            &shelved_tasks,
            &BTreeSet::new(),
            &gate_held_task_ids(&repo).expect("gate holds should be readable"),
        );
        assert!(
            ready.iter().all(|task| task.id != "TASK-C"),
            "dependent TASK-C must not dispatch while lane B remains active: {ready:#?}"
        );

        fs::remove_dir_all(repo).expect("failed to clean scheduler repo");
        fs::remove_dir_all(run_root).expect("failed to clean run root");
    }

    #[test]
    fn evidence_tasks_dispatch_through_worker_pipeline_while_operator_tasks_stay_host_queued() {
        let ready = vec![
            ready_task("CODE", LaneKind::Code, "- [ ] `CODE` Code work\n"),
            ready_task(
                "EVIDENCE",
                LaneKind::Evidence,
                "- [ ] `EVIDENCE` Collect evidence\n",
            ),
            ready_task(
                "VERIFY-ONLY",
                LaneKind::Code,
                "- [ ] `VERIFY-ONLY` Re-run proof\n  Scope boundary: verification only; do not modify code\n  Acceptance criteria: receipt exists\n",
            ),
            ready_task(
                "OPERATOR",
                LaneKind::Operator,
                "- [ ] `OPERATOR` Deposit funds\n",
            ),
        ];

        let (operator, worker) =
            partition_ready_tasks_with_operator_closeout(ready.clone(), |_| false);

        assert_eq!(
            operator
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["OPERATOR"]
        );
        assert_eq!(
            worker
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["CODE", "EVIDENCE", "VERIFY-ONLY"],
            "all non-operator tasks must enter normal dispatch so clean-no-commit can run every definition-of-done gate"
        );

        let (operator, worker) =
            partition_ready_tasks_with_operator_closeout(ready, |task| task.id == "OPERATOR");
        assert!(operator.is_empty());
        assert_eq!(
            worker
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["CODE", "EVIDENCE", "VERIFY-ONLY", "OPERATOR"],
            "an operator row with canonical receipt/artifact evidence must enter verification-only closeout"
        );
    }

    #[test]
    fn operator_with_valid_canonical_receipt_enters_closeout_worker_queue() {
        use crate::completion_artifacts::normalized_plan_hash_bytes;
        use sha2::{Digest as _, Sha256};

        let plan_text = "\
- [ ] `OPERATOR` Capture external identity
  Lane kind: operator
  Verification: `bash scripts/verify-operator.sh`
  Completion artifacts: `evidence/identity.json`
  Dependencies: none
";
        let repo = init_parallel_scheduler_repo("operator-receipt-closeout", plan_text);
        fs::create_dir_all(repo.join("scripts")).expect("create scripts directory");
        fs::create_dir_all(repo.join("evidence")).expect("create evidence directory");
        fs::write(repo.join(".gitignore"), ".auto/\n").expect("write ignore file");
        fs::write(repo.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("write receipt wrapper");
        fs::write(
            repo.join("scripts/verify-operator.sh"),
            "#!/bin/sh\nexit 0\n",
        )
        .expect("write task verifier");
        fs::write(repo.join("evidence/identity.json"), "identity\n")
            .expect("write identity artifact");
        run_git(
            &repo,
            [
                "add",
                ".gitignore",
                "scripts/run-task-verification.sh",
                "scripts/verify-operator.sh",
                "evidence/identity.json",
            ],
        )
        .expect("stage operator evidence fixtures");
        run_git(&repo, ["commit", "-m", "capture operator evidence"])
            .expect("commit operator evidence fixtures");

        let receipt_dir = repo.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("create receipt directory");
        let commit = git_stdout(&repo, ["rev-parse", "HEAD"])
            .expect("read HEAD")
            .trim()
            .to_string();
        let dirty = current_dirty_state_fingerprint(&repo).expect("compute dirty fingerprint");
        let plan_hash = normalized_plan_hash_bytes(plan_text.as_bytes());
        let artifact_hash = format!("{:x}", Sha256::digest(b"identity\n"));
        fs::write(
            receipt_dir.join("OPERATOR.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "task_id": "OPERATOR",
                "commit": commit,
                "dirty_state": {"fingerprint": dirty, "entries": []},
                "plan_hash": plan_hash,
                "commands": [{
                    "command": "bash scripts/verify-operator.sh",
                    "argv": ["bash", "scripts/verify-operator.sh"],
                    "expected_argv": ["bash", "scripts/verify-operator.sh"],
                    "exit_code": 0,
                    "status": "passed"
                }],
                "declared_artifacts": [{
                    "path": "evidence/identity.json",
                    "sha256": artifact_hash
                }]
            }))
            .expect("serialize receipt"),
        )
        .expect("write receipt");

        let task = parse_loop_plan(plan_text)
            .task("OPERATOR")
            .expect("operator task should parse")
            .clone();
        let evidence = inspect_task_completion_evidence(&repo, &task.id, &task.markdown);
        assert!(
            operator_task_has_closeout_evidence(&repo, &task),
            "operator evidence should be closeout-ready: {:?}",
            evidence.missing_reasons()
        );
        let (operator, worker) =
            partition_ready_tasks_for_worker_dispatch(&repo, vec![task.clone()]);
        assert!(operator.is_empty());
        assert_eq!(
            worker
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["OPERATOR"]
        );

        fs::write(repo.join("evidence/identity.json"), "drifted\n")
            .expect("mutate identity artifact");
        assert!(!operator_task_has_closeout_evidence(&repo, &task));
        fs::remove_dir_all(repo).expect("clean operator closeout fixture");
    }

    #[test]
    fn linear_usage_limit_error_detection_matches_linear_graphql_payloads() {
        let usage_limit = anyhow!(
            "Linear GraphQL returned errors: [{{\"extensions\":{{\"code\":\"USAGE_LIMIT_EXCEEDED\",\"meta\":{{\"usageMetric\":\"activeIssueCount\"}}}},\"message\":\"usage limit exceeded\"}}]"
        );
        let unrelated = anyhow!("Linear project `demo` not found");

        assert!(is_linear_usage_limit_error(&usage_limit));
        assert!(!is_linear_usage_limit_error(&unrelated));
    }

    #[test]
    fn linear_usage_limit_disables_auto_sync_for_the_rest_of_the_run() {
        let run_root = unique_temp_dir("parallel-linear-usage-limit");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("failed to create logger");
        let err = anyhow!(
            "Linear GraphQL returned errors: [{{\"extensions\":{{\"code\":\"USAGE_LIMIT_EXCEEDED\",\"meta\":{{\"usageMetric\":\"activeIssueCount\"}}}},\"message\":\"You've exceeded the free issue limit for this workspace.\"}}]"
        );
        let mut state = LinearAutoSyncState::default();

        assert!(maybe_disable_linear_auto_sync_for_run(
            &err,
            &mut state,
            &logger,
            "automatic `auto symphony sync --no-ai-planner`",
        ));
        assert!(state.is_disabled());
        assert!(maybe_disable_linear_auto_sync_for_run(
            &err,
            &mut state,
            &logger,
            "automatic `auto symphony sync --no-ai-planner`",
        ));

        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("failed to read live log");
        assert_eq!(
            live_log
                .matches("disabling further automatic Linear sync for this run")
                .count(),
            1
        );

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn rendered_worker_log_detects_forbidden_remote_git_command() {
        let violation = detect_forbidden_worker_remote_git_command_in_rendered_log(
            "[auto parallel] command:\n[auto parallel]   /bin/bash -lc 'git push origin pilot'\n[auto parallel] exit: code 128\n",
        )
        .expect("expected forbidden command");

        assert_eq!(violation.verb, "push");
        assert_eq!(violation.command, "/bin/bash -lc 'git push origin pilot'");
    }

    #[test]
    fn rendered_worker_log_ignores_searches_for_forbidden_git_text() {
        let violation = detect_forbidden_worker_remote_git_command_in_rendered_log(
            "command:\n  /bin/bash -lc 'rg -n \"git push|git fetch\" src README.md'\nresult: no matches\n",
        );

        assert_eq!(violation, None);
    }

    #[test]
    fn rendered_worker_log_detects_git_global_option_remote_command() {
        let violation = detect_forbidden_worker_remote_git_command_in_rendered_log(
            "command:\n  /usr/bin/git -C ../repo fetch origin\nresult: fetched\n",
        )
        .expect("expected forbidden command");

        assert_eq!(violation.verb, "fetch");
        assert_eq!(violation.command, "/usr/bin/git -C ../repo fetch origin");
    }
}
