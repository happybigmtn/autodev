fn next_free_lane_index(
    max_concurrent_workers: usize,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
) -> Option<usize> {
    (1..=max_concurrent_workers).find(|lane_index| !active_lanes.contains_key(lane_index))
}

fn prepare_parallel_lane_assignment(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_index: usize,
    task: LoopTask,
    resume_candidate: Option<LaneResumeCandidate>,
) -> Result<ActiveLaneAssignment> {
    if let Some(candidate) = resume_candidate {
        write_lane_task_id(&candidate.lane_root, &task.id)?;
        write_lane_assignment_metadata(
            &candidate.lane_root,
            target_branch,
            &candidate.base_commit,
            &task,
        )?;
        return Ok(ActiveLaneAssignment {
            lane_index: candidate.lane_index,
            attempts: 0,
            task,
            resumed: true,
            lane_root: candidate.lane_root,
            lane_repo_root: candidate.lane_repo_root,
            base_commit: candidate.base_commit,
            stdout_log_path: candidate.stdout_log_path,
            stderr_log_path: candidate.stderr_log_path,
            worker_pid_path: candidate.worker_pid_path,
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: candidate.host_recovery_note,
        });
    }

    let lane_root = run_root.join("lanes").join(format!("lane-{lane_index}"));
    reset_parallel_lane_root(&lane_root)?;
    let lane_repo_root = lane_root.join("repo");
    clone_loop_lane_repo(repo_root, target_branch, &lane_repo_root)?;
    let base_commit = git_stdout(&lane_repo_root, ["rev-parse", "HEAD"])?;
    write_lane_task_id(&lane_root, &task.id)?;
    write_lane_assignment_metadata(&lane_root, target_branch, base_commit.trim(), &task)?;
    Ok(ActiveLaneAssignment {
        lane_index,
        attempts: 0,
        task,
        resumed: false,
        lane_root: lane_root.clone(),
        lane_repo_root,
        base_commit: base_commit.trim().to_string(),
        stdout_log_path: lane_root.join("stdout.log"),
        stderr_log_path: lane_root.join("stderr.log"),
        worker_pid_path: lane_root.join("worker.pid"),
        clean_commit_since: None,
        terminate_requested_at: None,
        host_recovery_note: None,
    })
}

fn reset_parallel_lane_root(lane_root: &Path) -> Result<()> {
    if lane_root.exists() {
        let stale_root = reserve_stale_lane_root_path(lane_root)?;
        fs::rename(lane_root, &stale_root).with_context(|| {
            format!(
                "failed to move stale lane root {} aside",
                lane_root.display()
            )
        })?;
        if let Err(err) = fs::remove_dir_all(&stale_root) {
            eprintln!(
                "warning: failed removing stale lane root {} after reset: {err}",
                stale_root.display()
            );
        }
    }
    fs::create_dir_all(lane_root)
        .with_context(|| format!("failed to create {}", lane_root.display()))?;
    Ok(())
}

fn reserve_stale_lane_root_path(lane_root: &Path) -> Result<PathBuf> {
    let parent = lane_root
        .parent()
        .with_context(|| format!("lane root {} had no parent", lane_root.display()))?;
    let stem = lane_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .with_context(|| format!("lane root {} had no file name", lane_root.display()))?;
    for attempt in 0..100usize {
        let candidate = if attempt == 0 {
            format!("{stem}.stale-{}", timestamp_slug())
        } else {
            format!("{stem}.stale-{}-{attempt}", timestamp_slug())
        };
        let path = parent.join(candidate);
        if !path.exists() {
            return Ok(path);
        }
    }
    bail!(
        "failed reserving stale lane root path near {}",
        lane_root.display()
    );
}

fn prepare_parallel_lane_assignment_with_fallback(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_index: usize,
    task: LoopTask,
    resume_candidate: Option<LaneResumeCandidate>,
) -> Result<ActiveLaneAssignment> {
    let resumable_snapshot = resume_candidate.clone();
    match prepare_parallel_lane_assignment(
        repo_root,
        run_root,
        target_branch,
        lane_index,
        task.clone(),
        resume_candidate,
    ) {
        Ok(assignment) => Ok(assignment),
        Err(err) => {
            let Some(candidate) = resumable_snapshot else {
                return Err(err);
            };
            eprintln!(
                "warning: failed resuming lane-{} `{}`; retrying with a fresh clone: {err:#}",
                candidate.lane_index, task.id
            );
            prepare_parallel_lane_assignment(
                repo_root,
                run_root,
                target_branch,
                lane_index,
                task,
                None,
            )
        }
    }
}

fn discover_resume_candidates(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    plan: &LoopPlanSnapshot,
    parallel_logger: &ParallelEventLogger,
) -> Result<BTreeMap<usize, LaneResumeCandidate>> {
    let lanes_root = run_root.join("lanes");
    if !lanes_root.exists() {
        return Ok(BTreeMap::new());
    }

    let pending_tasks = plan
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                LoopTaskStatus::Pending | LoopTaskStatus::Partial
            )
        })
        .map(|task| (task.id.clone(), task.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = BTreeMap::new();

    for entry in fs::read_dir(&lanes_root)
        .with_context(|| format!("failed to read {}", lanes_root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", lanes_root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if !file_type.is_dir() {
            continue;
        }

        let lane_root = entry.path();
        let lane_name = entry.file_name();
        let Some(lane_index) = parse_lane_index(&lane_name.to_string_lossy()) else {
            continue;
        };
        let lane_repo_root = lane_root.join("repo");
        if !lane_repo_root.join(".git").exists() {
            continue;
        }

        let Some(task_id) = read_lane_task_id(&lane_root)? else {
            continue;
        };
        let Some(task) = pending_tasks.get(&task_id).cloned() else {
            continue;
        };
        if let Err(err) = validate_lane_assignment_metadata(&lane_root, target_branch, &task) {
            eprintln!(
                "warning: skipping resumable lane-{} `{}` because assignment metadata is stale or missing: {err:#}",
                lane_index, task_id
            );
            continue;
        }

        let stdout_log_path = lane_root.join("stdout.log");
        let stderr_log_path = lane_root.join("stderr.log");
        let worker_pid_path = lane_root.join("worker.pid");
        if let Err(err) = clear_stale_worker_pid(&worker_pid_path) {
            eprintln!(
                "warning: skipping resumable lane-{} because its worker pid file could not be cleaned up: {err:#}",
                lane_index
            );
            continue;
        }
        match read_worker_pid(&worker_pid_path) {
            Ok(Some(pid)) => match worker_pid_is_alive(pid) {
                Ok(true) => {
                    eprintln!(
                        "warning: skipping resumable lane-{} because worker pid {} is still alive in {}",
                        lane_index,
                        pid,
                        lane_root.display()
                    );
                    continue;
                }
                Ok(false) => {
                    if let Err(err) = fs::remove_file(&worker_pid_path) {
                        eprintln!(
                            "warning: skipping resumable lane-{} because stale worker pid cleanup failed: {err:#}",
                            lane_index
                        );
                        continue;
                    }
                }
                Err(err) => {
                    eprintln!(
                        "warning: skipping resumable lane-{} because worker pid liveness check failed: {err:#}",
                        lane_index
                    );
                    continue;
                }
            },
            Ok(None) => {}
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because its worker pid file is unreadable: {err:#}",
                    lane_index
                );
                continue;
            }
        }

        match retire_superseded_lane_cherry_pick_recovery(repo_root, &lane_repo_root, &task_id) {
            Ok(Some(superseded)) => {
                parallel_logger.info(format!(
                    "recovery-retire: lane-{} `{}` had stale duplicate landing recovery; {}",
                    lane_index,
                    task_id,
                    superseded.summary()
                ));
                continue;
            }
            Ok(None) => {}
            Err(err) => {
                parallel_logger.warn(format!(
                    "warning: lane-{} `{}` stale-recovery retirement check failed; keeping lane resumable: {err:#}",
                    lane_index, task_id
                ));
            }
        }

        let base_commit = match infer_lane_base_commit(&lane_repo_root, target_branch) {
            Ok(base_commit) => base_commit,
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because its base commit could not be inferred: {err:#}",
                    lane_index
                );
                continue;
            }
        };
        let mut host_recovery_note = match inspect_lane_repo_progress(&lane_repo_root, &base_commit)
        {
            Ok(LaneRepoProgress::None) => continue,
            Ok(LaneRepoProgress::Dirty(status) | LaneRepoProgress::NewCommitsWithDirty(status)) => {
                Some(lane_repo_recovery_note(
                    &lane_repo_root,
                    target_branch,
                    &status,
                ))
            }
            Ok(LaneRepoProgress::NewCommits) => None,
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because repo progress inspection failed: {err:#}",
                    lane_index
                );
                continue;
            }
        };
        if host_recovery_note.is_none() {
            host_recovery_note =
                salvage_recovery_note(&lane_root, lane_index, &task_id, target_branch);
        }

        candidates.insert(
            lane_index,
            LaneResumeCandidate {
                lane_index,
                task,
                lane_root,
                lane_repo_root,
                base_commit,
                stdout_log_path,
                stderr_log_path,
                worker_pid_path,
                host_recovery_note,
            },
        );
    }

    Ok(candidates)
}

async fn harvest_resumable_lane_results(
    repo_root: &Path,
    target_branch: &str,
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
    attempted_partial_followups: &mut BTreeSet<String>,
    deferred_partial_tasks: &mut BTreeSet<String>,
    linear_tracker: &mut Option<LinearTracker>,
    parallel_logger: &ParallelEventLogger,
) -> Result<usize> {
    let mut landed = 0usize;
    let lane_indexes = resumable_lanes.keys().copied().collect::<Vec<_>>();
    for lane_index in lane_indexes {
        let should_land = match resumable_lanes.get(&lane_index) {
            Some(candidate) => {
                match inspect_lane_repo_progress(&candidate.lane_repo_root, &candidate.base_commit)
                {
                    Ok(LaneRepoProgress::NewCommits) => true,
                    Ok(
                        LaneRepoProgress::Dirty(_)
                        | LaneRepoProgress::NewCommitsWithDirty(_)
                        | LaneRepoProgress::None,
                    ) => false,
                    Err(err) => {
                        eprintln!(
                            "warning: skipping resumable lane-{} because repo progress inspection failed: {err:#}",
                            lane_index
                        );
                        false
                    }
                }
            }
            None => false,
        };
        if !should_land {
            continue;
        }
        let Some(candidate) = resumable_lanes.remove(&lane_index) else {
            continue;
        };
        let mut assignment = ActiveLaneAssignment {
            lane_index: candidate.lane_index,
            attempts: 0,
            task: candidate.task,
            resumed: true,
            lane_root: candidate.lane_root,
            lane_repo_root: candidate.lane_repo_root,
            base_commit: candidate.base_commit,
            stdout_log_path: candidate.stdout_log_path,
            stderr_log_path: candidate.stderr_log_path,
            worker_pid_path: candidate.worker_pid_path,
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: candidate.host_recovery_note,
        };
        match land_parallel_lane_result(repo_root, target_branch, &mut assignment) {
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
                    attempted_partial_followups,
                    deferred_partial_tasks,
                );
                parallel_logger.info(format!(
                    "resumed:     landed {} from lane-{} before dispatch{}{} (total landed: {})",
                    assignment.task.id,
                    assignment.lane_index,
                    if auto_repaired {
                        " after host auto-repair"
                    } else {
                        ""
                    },
                    status_suffix,
                    landed
                ));
            }
            Ok(LaneLandingOutcome::NeedsRecovery(recovery_note)) => {
                parallel_logger.warn(format!(
                    "warning: resume harvest for lane-{} `{}` prepared a landing-recovery attempt instead of landing; keeping lane resumable",
                    assignment.lane_index, assignment.task.id
                ));
                resumable_lanes.insert(
                    lane_index,
                    LaneResumeCandidate {
                        lane_index: assignment.lane_index,
                        task: assignment.task,
                        lane_root: assignment.lane_root,
                        lane_repo_root: assignment.lane_repo_root,
                        base_commit: assignment.base_commit,
                        stdout_log_path: assignment.stdout_log_path,
                        stderr_log_path: assignment.stderr_log_path,
                        worker_pid_path: assignment.worker_pid_path,
                        host_recovery_note: Some(recovery_note),
                    },
                );
            }
            Err(error) => {
                parallel_logger.warn(format!(
                    "warning: resume harvest for lane-{} `{}` failed; keeping lane resumable instead: {error:#}",
                    assignment.lane_index, assignment.task.id
                ));
                resumable_lanes.insert(
                    lane_index,
                    LaneResumeCandidate {
                        lane_index: assignment.lane_index,
                        task: assignment.task,
                        lane_root: assignment.lane_root,
                        lane_repo_root: assignment.lane_repo_root,
                        base_commit: assignment.base_commit,
                        stdout_log_path: assignment.stdout_log_path,
                        stderr_log_path: assignment.stderr_log_path,
                        worker_pid_path: assignment.worker_pid_path,
                        host_recovery_note: Some(landing_recovery_note(
                            target_branch,
                            &format!("{error:#}"),
                        )),
                    },
                );
            }
        }
    }
    Ok(landed)
}

fn take_resume_candidate_for_task(
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
    task_id: &str,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
) -> Option<(usize, LaneResumeCandidate)> {
    let lane_index = resumable_lanes
        .iter()
        .find(|(lane_index, candidate)| {
            !active_lanes.contains_key(lane_index) && candidate.task.id == task_id
        })
        .map(|(lane_index, _)| *lane_index)?;
    let candidate = resumable_lanes.remove(&lane_index)?;
    Some((lane_index, candidate))
}

fn preserve_resume_recovery_notes(
    rediscovered: &mut BTreeMap<usize, LaneResumeCandidate>,
    previous: &BTreeMap<usize, LaneResumeCandidate>,
) {
    for (lane_index, candidate) in rediscovered {
        if candidate.host_recovery_note.is_some() {
            continue;
        }
        let Some(previous_candidate) = previous.get(lane_index) else {
            continue;
        };
        if previous_candidate.task.id == candidate.task.id {
            candidate.host_recovery_note = previous_candidate.host_recovery_note.clone();
        }
    }
}

fn clone_loop_lane_repo(
    repo_root: &Path,
    target_branch: &str,
    lane_repo_root: &Path,
) -> Result<()> {
    let output = Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg("--local")
        .arg("--branch")
        .arg(target_branch)
        .arg("--single-branch")
        .arg(repo_root)
        .arg(lane_repo_root)
        .output()
        .with_context(|| {
            format!(
                "failed to clone loop lane repo from {} to {}",
                repo_root.display(),
                lane_repo_root.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "git clone failed for loop lane {}: {}",
            lane_repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let remotes = git_stdout(lane_repo_root, ["remote"]).unwrap_or_default();
    if remotes.lines().any(|remote| remote.trim() == "origin") {
        run_git(lane_repo_root, ["remote", "rename", "origin", "canonical"])?;
    }
    Ok(())
}
