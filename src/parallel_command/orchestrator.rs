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

pub(crate) async fn run_serial_loop(
    repo_root: &Path,
    reference_repos: &[PathBuf],
    args: &ParallelArgs,
    target_branch: &str,
    prompt_template: &str,
    run_root: &Path,
    worker_env: &LoopWorkerEnv,
) -> Result<()> {
    let stderr_log_path = run_root.join("stderr.log");
    let stdout_log_path = run_root.join("stdout.log");
    fs::write(&stdout_log_path, b"")
        .with_context(|| format!("failed to initialize {}", stdout_log_path.display()))?;
    let harness = if args.claude { "Claude" } else { "Codex" };
    let mut iteration = 0usize;
    let mut consecutive_failures = 0usize;

    loop {
        if args.max_iterations.is_some_and(|limit| iteration >= limit) {
            println!(
                "reached max iterations: {}",
                args.max_iterations.unwrap_or_default()
            );
            break;
        }

        let plan = inspect_loop_plan(repo_root)?;
        let queue = plan.queue_snapshot();
        if queue.pending_ids.is_empty() {
            if queue.blocked_ids.is_empty() {
                println!("no unfinished `- [ ]` / `- [~]` tasks remain; stopping.");
            } else {
                println!(
                    "all remaining tasks are blocked `[!]`; stopping. blocked: {}",
                    queue.blocked_ids.join(", ")
                );
            }
            break;
        }

        let ready = plan.ready_tasks(&BTreeSet::new());
        if ready.is_empty() {
            println!(
                "no dependency-ready `- [ ]` tasks remain; stopping. blocked: {}",
                if queue.blocked_ids.is_empty() {
                    "none".to_string()
                } else {
                    queue.blocked_ids.join(", ")
                }
            );
            break;
        }

        let current_task = ready[0].clone();
        println!("next task:   {}", current_task.id);
        if !queue.blocked_ids.is_empty() {
            println!("blocked:     {}", queue.blocked_ids.join(", "));
        }

        let full_prompt = build_iteration_prompt(
            prompt_template,
            &LoopQueueSnapshot {
                pending_ids: ready.iter().map(|task| task.id.clone()).collect(),
                blocked_ids: queue.blocked_ids.clone(),
            },
        );

        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("loop-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        println!("prompt log:  {}", prompt_path.display());

        let state_before = collect_tracked_repo_states(repo_root, reference_repos)?;
        println!();
        println!("running {harness} iteration {}", iteration + 1);

        let exit_status = if args.claude {
            run_claude_exec_with_env(
                repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                args.max_turns,
                &stderr_log_path,
                Some(&stdout_log_path),
                "auto parallel",
                &worker_env.extra_env,
                None,
                None,
            )
            .await?
        } else {
            run_codex_exec_with_env(
                repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                &args.codex_bin,
                &stderr_log_path,
                Some(&stdout_log_path),
                "auto parallel",
                &worker_env.extra_env,
                None,
                None,
            )
            .await?
        };
        if let Some(violation) = detect_forbidden_worker_remote_git_command(&stdout_log_path)? {
            bail!(
                "{harness} worker attempted forbidden remote git command `{}` in {}; lanes must leave remote sync to the host",
                violation.command,
                stdout_log_path.display()
            );
        }
        if !exit_status.success() {
            let exit_code = exit_status.code().unwrap_or(-1);
            let is_futility = exit_code == FUTILITY_EXIT_MARKER;
            consecutive_failures += 1;

            if let Some(commit) = auto_checkpoint_if_needed(
                repo_root,
                target_branch,
                &format!(
                    "auto parallel checkpoint (pre-retry {})",
                    consecutive_failures
                ),
            )? {
                println!("checkpoint:  committed partial changes at {commit}");
            }

            if consecutive_failures > args.max_retries {
                bail!(
                    "{harness} exited with status {} after {} consecutive failures; see {}",
                    if is_futility {
                        "futility".to_string()
                    } else {
                        exit_code.to_string()
                    },
                    consecutive_failures,
                    stderr_log_path.display()
                );
            }

            println!(
                "warning: {harness} exited non-zero ({}), retrying ({}/{})",
                if is_futility {
                    "futility spiral".to_string()
                } else {
                    format!("code {exit_code}")
                },
                consecutive_failures,
                args.max_retries
            );
            continue;
        }
        consecutive_failures = 0;

        println!();
        println!("{harness} iteration complete");

        let state_after = collect_tracked_repo_states(repo_root, reference_repos)?;
        match summarize_repo_progress(&state_before, &state_after) {
            RepoProgress::NewCommits => {
                let mut task_for_reconciliation = current_task.clone();
                let changed_files =
                    primary_repo_changed_files(repo_root, &state_before, &state_after)?;
                let completion_status = reconcile_parallel_landed_task_state(
                    repo_root,
                    &mut task_for_reconciliation,
                    &changed_files,
                )?;
                if repo_has_staged_queue_updates(repo_root)? {
                    let message = format!(
                        "{}: {} queue sync",
                        repo_name(repo_root),
                        task_for_reconciliation.id
                    );
                    commit_task_closeout(repo_root, &task_for_reconciliation.id, &message, false)?;
                }
                println!(
                    "host sync:   {} -> {}",
                    task_for_reconciliation.id,
                    loop_task_status_label(completion_status)
                );
            }
            RepoProgress::DirtyChanges(repos) => {
                bail!(
                    "tracked repo changes were left uncommitted in: {}; commit or revert them before continuing",
                    repos.join(", ")
                );
            }
            RepoProgress::None => {
                if let Some(commit) =
                    auto_checkpoint_if_needed(repo_root, target_branch, "auto parallel checkpoint")?
                {
                    iteration += 1;
                    println!("checkpoint:  committed iteration changes at {commit}");
                    println!();
                    println!("================ LOOP {} ================", iteration);
                    continue;
                }
                println!("no new commit detected; stopping.");
                break;
            }
        }

        if push_branch_with_remote_sync(repo_root, target_branch)? {
            println!("remote sync: rebased onto origin/{}", target_branch);
        }
        if let Some(commit) =
            auto_checkpoint_if_needed(repo_root, target_branch, "auto parallel checkpoint")?
        {
            println!("checkpoint:  committed trailing changes at {commit}");
        }
        iteration += 1;
        println!();
        println!("================ LOOP {} ================", iteration);
    }

    Ok(())
}

fn primary_repo_changed_files(
    repo_root: &Path,
    before: &[TrackedRepoState],
    after: &[TrackedRepoState],
) -> Result<Vec<String>> {
    let Some(before_state) = before.iter().find(|state| state.path == repo_root) else {
        return Ok(Vec::new());
    };
    let Some(after_state) = after.iter().find(|state| state.path == repo_root) else {
        return Ok(Vec::new());
    };
    if before_state.head == after_state.head {
        return Ok(Vec::new());
    }

    let range = format!("{}..{}", before_state.head, after_state.head);
    let output = git_stdout(repo_root, ["diff", "--name-only", range.as_str()])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn loop_task_status_label(status: LoopTaskStatus) -> &'static str {
    match status {
        LoopTaskStatus::Pending => "[ ]",
        LoopTaskStatus::Blocked => "[!]",
        LoopTaskStatus::Partial => "[~]",
        LoopTaskStatus::Done => "[x]",
    }
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
    let mut shelved_tasks = BTreeMap::<String, String>::new();
    let mut attempted_partial_followups = BTreeMap::<String, usize>::new();
    let mut deferred_partial_tasks = BTreeSet::<String>::new();
    let mut unblock_attempt_counts = BTreeMap::<String, usize>::new();
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
    let lane_config = LaneRunConfig::new(args, worker_env, preflight_report.prompt_clause());
    let review_config = LaneReviewConfig::from_run_config(&args.model, &args.codex_bin);
    try_checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger);
    let mut resumable_lanes = discover_resume_candidates(
        repo_root,
        run_root,
        target_branch,
        &lane_config,
        &plan,
        parallel_logger,
    )?;
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

            let ready = prioritize_ready_parallel_tasks(
                repo_root,
                ready_parallel_tasks(
                    &plan,
                    &active_tasks,
                    &shelved_tasks,
                    &deferred_partial_tasks,
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
            let mut operator_ready = Vec::new();
            let mut evidence_ready = Vec::new();
            let mut executable_ready = Vec::new();
            for task in ready {
                if is_operator_task(&task) {
                    operator_ready.push(task);
                } else if is_evidence_lane_task(&task) {
                    evidence_ready.push(task);
                } else {
                    executable_ready.push(task);
                }
            }
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
            if executable_ready.is_empty() {
                let message = format!(
                    "no executable dependency-ready code tasks remain; evidence queue: {} operator queue: {}",
                    evidence_ready
                        .iter()
                        .map(|task| task.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    operator_ready
                        .iter()
                        .map(|task| task.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                parallel_logger.info(&message);
                break;
            }

            let task = executable_ready[0].clone();
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

        if active_lanes.is_empty() {
            let queue = plan.queue_snapshot();
            if queue.pending_ids.is_empty() {
                if queue.blocked_ids.is_empty() {
                    parallel_logger.info("no unfinished `- [ ]` / `- [~]` tasks remain; stopping.");
                } else {
                    parallel_logger.info(format!(
                        "all remaining tasks are blocked `[!]`; stopping. blocked: {}",
                        queue.blocked_ids.join(", ")
                    ));
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

            parallel_logger.info(no_dependency_ready_stop_message(
                &plan,
                &active_tasks,
                &queue,
                &shelved_tasks,
                &deferred_partial_tasks,
                &unblock_attempt_counts,
                max_autonomous_unblock_attempts,
            ));
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
                    match land_parallel_lane_result(repo_root, target_branch, &mut assignment, &review_config).await {
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
                        Ok(LaneLandingOutcome::NeedsRecovery(recovery_note)) => {
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
                                        "warning: failed landing lane-{} `{}` after non-zero worker exit and no recovery attempts remain",
                                        assignment.lane_index, assignment.task.id
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
                                        "warning: failed restarting lane-{} `{}` after landing failure: {retry_err:#}",
                                        assignment.lane_index, assignment.task.id
                                    ));
                                }
                            }
                            shelved_tasks.insert(
                                assignment.task.id.clone(),
                                assignment.task.markdown.clone(),
                            );
                            continue;
                        }
                        Err(err) => {
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
                    &assignment,
                    parallel_logger,
                ) {
                    Ok(true) => {
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
                            "self-heal: worker exited cleanly without a commit, but canonical review/receipt/artifact evidence is complete; host marked the task done",
                        );
                        last_idle_summary = None;
                        continue;
                    }
                    Ok(false) => {}
                    Err(err) => {
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
                match land_parallel_lane_result(repo_root, target_branch, &mut assignment, &review_config).await {
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
                    Ok(LaneLandingOutcome::NeedsRecovery(recovery_note)) => {
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
                                parallel_logger.warn(format!(
                                    "warning: failed landing lane-{} `{}` and no recovery attempts remain",
                                    assignment.lane_index, assignment.task.id
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
                                    "warning: failed restarting lane-{} `{}` after landing failure: {retry_err:#}",
                                    assignment.lane_index, assignment.task.id
                                ));
                            }
                        }
                        shelved_tasks
                            .insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                        continue;
                    }
                    Err(err) => {
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

pub(crate) async fn refresh_parallel_plan(
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

pub(crate) fn spawn_parallel_lane_attempt(
    join_set: &mut JoinSet<LaneAttemptResult>,
    lane_config: &LaneRunConfig,
    prompt_template: &str,
    plan: &LoopPlanSnapshot,
    assignment: &mut ActiveLaneAssignment,
    target_branch: &str,
) -> Result<()> {
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
    let lane_config = lane_config.clone();

    join_set.spawn(async move {
        if let Err(err) = atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))
        {
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
                &lane_config.reasoning_effort,
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
                &lane_config.reasoning_effort,
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
        match exit_status {
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
        }
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
                let pid = match read_worker_pid(&assignment.worker_pid_path) {
                    Ok(pid) => pid,
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
                let Some(pid) = pid else {
                    assignment.clean_commit_since = None;
                    assignment.terminate_requested_at = None;
                    continue;
                };
                let alive = match worker_pid_is_alive(pid) {
                    Ok(alive) => alive,
                    Err(err) => {
                        eprintln!(
                            "warning: failed checking worker liveness for lane-{} `{}` pid {}: {err:#}",
                            assignment.lane_index, assignment.task.id, pid
                        );
                        assignment.clean_commit_since = None;
                        assignment.terminate_requested_at = None;
                        continue;
                    }
                };
                if !alive {
                    assignment.clean_commit_since = None;
                    assignment.terminate_requested_at = None;
                    continue;
                }

                let commit_since = assignment
                    .clean_commit_since
                    .get_or_insert_with(Instant::now);
                if let Some(requested_at) = assignment.terminate_requested_at {
                    if requested_at.elapsed() >= CLEAN_COMMIT_KILL_GRACE {
                        if let Err(err) = signal_worker(pid, "KILL") {
                            eprintln!(
                                "warning: failed sending SIGKILL to lingering worker pid {} for lane-{} `{}`: {err:#}",
                                pid, assignment.lane_index, assignment.task.id
                            );
                        } else {
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

                {
                    if let Err(err) = signal_worker(pid, "TERM") {
                        eprintln!(
                            "warning: failed sending SIGTERM to lingering worker pid {} for lane-{} `{}`: {err:#}",
                            pid, assignment.lane_index, assignment.task.id
                        );
                        assignment.terminate_requested_at = None;
                    } else {
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
    use std::time::UNIX_EPOCH;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
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
