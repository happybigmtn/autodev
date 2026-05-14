fn spawn_parallel_lane_attempt(
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

    // Session-survival: if a prior process wrote a checkpoint for this
    // lane (crash, SIGTERM, operator restart), surface it through the
    // host recovery note so the worker prompt knows what phase was last
    // completed. Only seed the note when the lane is actually resuming
    // so fresh attempts stay clean.
    if assignment.resumed && assignment.host_recovery_note.is_none() {
        if let Some(checkpoint) = load_lane_checkpoint(&assignment.lane_root) {
            let resume_note = format!(
                "Resuming lane after session interruption. Last recorded phase: `{}` at {}. Phase blob: {}",
                checkpoint.phase,
                checkpoint.written_at.to_rfc3339(),
                checkpoint.blob
            );
            assignment.host_recovery_note = Some(resume_note);
        }
    }

    record_lane_checkpoint(
        &assignment.lane_root,
        "analyze",
        serde_json::json!({
            "task_id": assignment.task.id,
            "attempt": assignment.attempts,
            "base_commit": assignment.base_commit,
            "target_branch": target_branch,
        }),
    );

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
    let checkpoint_lane_root = assignment.lane_root.clone();
    let checkpoint_attempt = assignment.attempts;

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
        record_lane_checkpoint(
            &checkpoint_lane_root,
            "implement",
            serde_json::json!({
                "task_id": task_id,
                "attempt": checkpoint_attempt,
                "prompt_path": prompt_path.display().to_string(),
            }),
        );
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
            Ok(exit_status) => {
                record_lane_checkpoint(
                    &checkpoint_lane_root,
                    "verify",
                    serde_json::json!({
                        "task_id": task_id,
                        "attempt": checkpoint_attempt,
                        "worker_exit_code": exit_status.code(),
                    }),
                );
                LaneAttemptResult {
                    lane_index,
                    exit_status: Some(exit_status),
                    error: None,
                }
            }
            Err(err) => {
                let message = format!("{err:#}");
                record_lane_checkpoint(
                    &checkpoint_lane_root,
                    "verify",
                    serde_json::json!({
                        "task_id": task_id,
                        "attempt": checkpoint_attempt,
                        "worker_error": message.clone(),
                    }),
                );
                LaneAttemptResult {
                    lane_index,
                    exit_status: None,
                    error: Some(message),
                }
            }
        }
    });
    Ok(())
}

fn refresh_assignment_task_from_plan(
    plan: &LoopPlanSnapshot,
    assignment: &mut ActiveLaneAssignment,
) {
    if let Some(task) = plan
        .tasks
        .iter()
        .find(|task| task.id == assignment.task.id)
        .cloned()
    {
        assignment.task = task;
    }
}

fn nudge_lingering_committed_lanes(active_lanes: &mut BTreeMap<usize, ActiveLaneAssignment>) {
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

                if commit_since.elapsed() >= CLEAN_COMMIT_GRACE {
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

