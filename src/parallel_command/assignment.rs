use super::*;

#[derive(Clone, Debug)]
pub(crate) struct LaneRunConfig {
    pub(crate) claude: bool,
    pub(crate) max_turns: Option<usize>,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
    pub(crate) codex_bin: PathBuf,
    pub(crate) extra_env: Vec<(String, String)>,
    pub(crate) lane_local_cargo_target: bool,
    pub(crate) cargo_target_prompt_clause: String,
    pub(crate) preflight_prompt_clause: String,
}

impl LaneRunConfig {
    pub(crate) fn new(
        args: &ParallelArgs,
        worker_env: &LoopWorkerEnv,
        preflight_prompt_clause: String,
    ) -> Self {
        Self {
            claude: args.claude,
            max_turns: effective_parallel_claude_max_turns(args),
            model: args.model.clone(),
            reasoning_effort: args.reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            extra_env: worker_env.extra_env.clone(),
            lane_local_cargo_target: worker_env.lane_local_cargo_target,
            cargo_target_prompt_clause: worker_env.cargo_target_prompt_clause.clone(),
            preflight_prompt_clause,
        }
    }

    pub(crate) fn env_for_lane(&self, lane_root: &Path) -> Vec<(String, String)> {
        let mut extra_env = self.extra_env.clone();
        if self.lane_local_cargo_target {
            extra_env.push((
                "CARGO_TARGET_DIR".to_string(),
                lane_root
                    .join("cargo-target")
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        extra_env
    }

    pub(crate) fn assignment_worker_metadata(&self) -> LaneWorkerMetadata {
        if self.claude {
            let mut command = vec![
                "claude".to_string(),
                "-p".to_string(),
                "--verbose".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--model".to_string(),
                self.model.clone(),
                "--effort".to_string(),
                self.reasoning_effort.clone(),
                "--output-format".to_string(),
                "stream-json".to_string(),
            ];
            if let Some(max_turns) = self.max_turns {
                command.push("--max-turns".to_string());
                command.push(max_turns.to_string());
            }
            return LaneWorkerMetadata {
                harness: "claude".to_string(),
                command,
                model: self.model.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
                max_turns: self.max_turns,
            };
        }

        LaneWorkerMetadata {
            harness: "codex".to_string(),
            command: vec![
                self.codex_bin.display().to_string(),
                "exec".to_string(),
                "--json".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--skip-git-repo-check".to_string(),
                "-m".to_string(),
                self.model.clone(),
                "-c".to_string(),
                format!("model_reasoning_effort=\"{}\"", self.reasoning_effort),
            ],
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            max_turns: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveLaneAssignment {
    pub(crate) lane_index: usize,
    pub(crate) attempts: usize,
    pub(crate) task: LoopTask,
    pub(crate) resumed: bool,
    pub(crate) lane_root: PathBuf,
    pub(crate) lane_repo_root: PathBuf,
    pub(crate) base_commit: String,
    pub(crate) stdout_log_path: PathBuf,
    pub(crate) stderr_log_path: PathBuf,
    pub(crate) worker_pid_path: PathBuf,
    pub(crate) clean_commit_since: Option<Instant>,
    pub(crate) terminate_requested_at: Option<Instant>,
    pub(crate) host_recovery_note: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct LaneResumeCandidate {
    pub(crate) lane_index: usize,
    pub(crate) task: LoopTask,
    pub(crate) lane_root: PathBuf,
    pub(crate) lane_repo_root: PathBuf,
    pub(crate) base_commit: String,
    pub(crate) stdout_log_path: PathBuf,
    pub(crate) stderr_log_path: PathBuf,
    pub(crate) worker_pid_path: PathBuf,
    pub(crate) host_recovery_note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct LaneWorkerMetadata {
    pub(crate) harness: String,
    pub(crate) command: Vec<String>,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
    pub(crate) max_turns: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct LaneAssignmentMetadata {
    pub(crate) task_id: String,
    pub(crate) target_branch: String,
    pub(crate) base_commit: String,
    pub(crate) task_hash: u64,
    pub(crate) dependency_hash: u64,
    pub(crate) verification_hash: u64,
    pub(crate) worker: LaneWorkerMetadata,
    pub(crate) assignment_hash: u64,
}

#[derive(Debug)]
pub(crate) struct LaneAttemptResult {
    pub(crate) lane_index: usize,
    pub(crate) exit_status: Option<ExitStatus>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CherryPickFailurePolicy {
    Abort,
    LeaveInProgress,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LaneLandingOutcome {
    Landed {
        auto_repaired: bool,
        completion_status: LoopTaskStatus,
    },
    NeedsRecovery(String),
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LaneLandingRecoveryPrep {
    RebasedCleanly,
    NeedsWorkerResolution(String),
}

pub(crate) fn next_free_lane_index(
    max_concurrent_workers: usize,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
) -> Option<usize> {
    (1..=max_concurrent_workers).find(|lane_index| !active_lanes.contains_key(lane_index))
}

pub(crate) fn prepare_parallel_lane_assignment(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_config: &LaneRunConfig,
    lane_index: usize,
    task: LoopTask,
    resume_candidate: Option<LaneResumeCandidate>,
) -> Result<ActiveLaneAssignment> {
    let worker_metadata = lane_config.assignment_worker_metadata();
    if let Some(candidate) = resume_candidate {
        write_lane_task_id(&candidate.lane_root, &task.id)?;
        write_lane_assignment_metadata(
            &candidate.lane_root,
            target_branch,
            &candidate.base_commit,
            &task,
            &worker_metadata,
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
    write_lane_assignment_metadata(
        &lane_root,
        target_branch,
        base_commit.trim(),
        &task,
        &worker_metadata,
    )?;
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

pub(crate) fn reset_parallel_lane_root(lane_root: &Path) -> Result<()> {
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

pub(crate) fn reserve_stale_lane_root_path(lane_root: &Path) -> Result<PathBuf> {
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

pub(crate) fn prepare_parallel_lane_assignment_with_fallback(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_config: &LaneRunConfig,
    lane_index: usize,
    task: LoopTask,
    resume_candidate: Option<LaneResumeCandidate>,
) -> Result<ActiveLaneAssignment> {
    let resumable_snapshot = resume_candidate.clone();
    match prepare_parallel_lane_assignment(
        repo_root,
        run_root,
        target_branch,
        lane_config,
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
                lane_config,
                lane_index,
                task,
                None,
            )
        }
    }
}

pub(crate) fn discover_resume_candidates(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_config: &LaneRunConfig,
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
        if let Err(err) = validate_lane_assignment_metadata(
            &lane_root,
            target_branch,
            &base_commit,
            &lane_config.assignment_worker_metadata(),
            &task,
        ) {
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

pub(crate) async fn harvest_resumable_lane_results(
    repo_root: &Path,
    target_branch: &str,
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
    attempted_partial_followups: &mut BTreeMap<String, usize>,
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

pub(crate) fn take_resume_candidate_for_task(
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

pub(crate) fn refresh_assignment_task_from_plan(
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

pub(crate) fn parse_lane_index(name: &str) -> Option<usize> {
    name.strip_prefix("lane-")?.parse::<usize>().ok()
}

pub(crate) fn write_lane_task_id(lane_root: &Path, task_id: &str) -> Result<()> {
    atomic_write(&lane_root.join(LANE_TASK_ID_FILE), task_id.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            lane_root.join(LANE_TASK_ID_FILE).display()
        )
    })
}

pub(crate) fn write_lane_assignment_metadata(
    lane_root: &Path,
    target_branch: &str,
    base_commit: &str,
    task: &LoopTask,
    worker: &LaneWorkerMetadata,
) -> Result<()> {
    let task_hash = hash_stable(&task.markdown);
    let dependency_hash = hash_stable(&task.dependencies);
    let verification_hash = hash_stable(&task_field_body(
        &task.markdown,
        "Verification:",
        "Required tests:",
    ));
    let metadata = LaneAssignmentMetadata {
        task_id: task.id.clone(),
        target_branch: target_branch.to_string(),
        base_commit: base_commit.to_string(),
        task_hash,
        dependency_hash,
        verification_hash,
        worker: worker.clone(),
        assignment_hash: lane_assignment_hash(
            &task.id,
            target_branch,
            base_commit,
            task_hash,
            dependency_hash,
            verification_hash,
            worker,
        ),
    };
    let json = serde_json::to_vec_pretty(&metadata)?;
    atomic_write(&lane_root.join(LANE_ASSIGNMENT_FILE), &json).with_context(|| {
        format!(
            "failed to write {}",
            lane_root.join(LANE_ASSIGNMENT_FILE).display()
        )
    })
}

pub(crate) fn validate_lane_assignment_metadata(
    lane_root: &Path,
    target_branch: &str,
    base_commit: &str,
    worker: &LaneWorkerMetadata,
    task: &LoopTask,
) -> Result<LaneAssignmentMetadata> {
    let metadata_path = lane_root.join(LANE_ASSIGNMENT_FILE);
    let text = fs::read_to_string(&metadata_path)
        .with_context(|| format!("failed to read {}", metadata_path.display()))?;
    let metadata: LaneAssignmentMetadata = serde_json::from_str(&text)
        .with_context(|| format!("invalid {}", metadata_path.display()))?;
    if metadata.task_id != task.id {
        bail!(
            "task id changed from `{}` to `{}`",
            metadata.task_id,
            task.id
        );
    }
    if metadata.target_branch != target_branch {
        bail!(
            "target branch changed from `{}` to `{target_branch}`",
            metadata.target_branch
        );
    }
    if metadata.base_commit != base_commit {
        bail!(
            "base commit changed from `{}` to `{base_commit}`",
            metadata.base_commit
        );
    }
    if metadata.worker.model != worker.model {
        bail!(
            "worker model changed from `{}` to `{}`",
            metadata.worker.model,
            worker.model
        );
    }
    if metadata.worker.command != worker.command {
        bail!("worker command changed");
    }
    if metadata.worker.reasoning_effort != worker.reasoning_effort {
        bail!(
            "worker reasoning effort changed from `{}` to `{}`",
            metadata.worker.reasoning_effort,
            worker.reasoning_effort
        );
    }
    if metadata.worker.max_turns != worker.max_turns {
        bail!(
            "worker max turns changed from `{:?}` to `{:?}`",
            metadata.worker.max_turns,
            worker.max_turns
        );
    }
    if metadata.verification_hash
        != hash_stable(&task_field_body(
            &task.markdown,
            "Verification:",
            "Required tests:",
        ))
    {
        bail!("verification text hash changed");
    }
    if metadata.task_hash != hash_stable(&task.markdown) {
        bail!("task body hash changed");
    }
    if metadata.dependency_hash != hash_stable(&task.dependencies) {
        bail!("dependency hash changed");
    }
    let expected_assignment_hash = lane_assignment_hash(
        &task.id,
        target_branch,
        &metadata.base_commit,
        metadata.task_hash,
        metadata.dependency_hash,
        metadata.verification_hash,
        worker,
    );
    if metadata.assignment_hash != expected_assignment_hash {
        bail!("assignment hash changed");
    }
    Ok(metadata)
}

pub(crate) fn hash_stable<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn lane_assignment_hash(
    task_id: &str,
    target_branch: &str,
    base_commit: &str,
    task_hash: u64,
    dependency_hash: u64,
    verification_hash: u64,
    worker: &LaneWorkerMetadata,
) -> u64 {
    hash_stable(&(
        task_id,
        target_branch,
        base_commit,
        task_hash,
        dependency_hash,
        verification_hash,
        worker,
    ))
}

pub(crate) fn read_lane_task_id(lane_root: &Path) -> Result<Option<String>> {
    let task_id_path = lane_root.join(LANE_TASK_ID_FILE);
    if task_id_path.exists() {
        let task_id = fs::read_to_string(&task_id_path)
            .with_context(|| format!("failed to read {}", task_id_path.display()))?;
        let task_id = task_id.trim();
        if !task_id.is_empty() {
            return Ok(Some(task_id.to_string()));
        }
    }

    let mut latest_prompt: Option<(std::time::SystemTime, String)> = None;
    for entry in fs::read_dir(lane_root)
        .with_context(|| format!("failed to read {}", lane_root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", lane_root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some(task_id) = task_id_from_prompt_filename(&file_name) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        match &latest_prompt {
            Some((latest_modified, _)) if &modified <= latest_modified => {}
            _ => latest_prompt = Some((modified, task_id)),
        }
    }

    Ok(latest_prompt.map(|(_, task_id)| task_id))
}

pub(crate) fn lane_status_task_id(
    stored_task_id: &str,
    worker_running: bool,
    log_line: Option<&str>,
) -> String {
    if worker_running {
        return stored_task_id.to_string();
    }
    if log_line
        .map(str::trim)
        .is_some_and(|line| line.contains("] idle:"))
    {
        return "[idle]".to_string();
    }
    stored_task_id.to_string()
}

pub(crate) fn lane_worker_status(
    lane_root: &Path,
    lane_repo_root: &Path,
) -> Result<(bool, String)> {
    let pid_path = lane_root.join("worker.pid");
    let pid_state = match read_worker_pid(&pid_path) {
        Ok(Some(pid)) => match worker_pid_is_alive(pid) {
            Ok(true) => return Ok((true, format!("running pid {pid}"))),
            Ok(false) => Some(format!("stale pid {pid}")),
            Err(err) => Some(format!("pid liveness unknown: {err:#}")),
        },
        Ok(None) => None,
        Err(err) => Some(format!("worker pid unreadable: {err:#}")),
    };

    let descendant_pids = lane_repo_process_pids(lane_repo_root)?;
    if !descendant_pids.is_empty() {
        return Ok((
            true,
            format!(
                "running descendant pid(s) {}{}",
                descendant_pids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                pid_state
                    .map(|state| format!(" ({state})"))
                    .unwrap_or_default()
            ),
        ));
    }

    Ok((
        false,
        pid_state.unwrap_or_else(|| "no worker pid".to_string()),
    ))
}

pub(crate) fn task_id_from_prompt_filename(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix("-prompt.md")?;
    let (task_id, attempt) = stem.rsplit_once("-attempt-")?;
    if attempt.parse::<usize>().is_err() || task_id.is_empty() {
        return None;
    }
    Some(task_id.to_string())
}

#[cfg(test)]
mod tests {
    use crate::parallel_command::*;
    use std::time::UNIX_EPOCH;

    fn sample_worker_metadata() -> LaneWorkerMetadata {
        LaneWorkerMetadata {
            harness: "codex".to_string(),
            command: vec![
                "codex".to_string(),
                "exec".to_string(),
                "--json".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--skip-git-repo-check".to_string(),
                "-m".to_string(),
                "gpt-5.5".to_string(),
                "-c".to_string(),
                "model_reasoning_effort=\"high\"".to_string(),
            ],
            model: "gpt-5.5".to_string(),
            reasoning_effort: "high".to_string(),
            max_turns: None,
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    #[test]
    fn lane_status_task_id_reports_idle_when_latest_log_is_idle() {
        assert_eq!(
            lane_status_task_id(
                "OLD-TASK",
                false,
                Some("[auto parallel host lane-5 [idle]] idle: waiting on dependencies"),
            ),
            "[idle]"
        );
        assert_eq!(
            lane_status_task_id("OLD-TASK", true, Some("anything")),
            "OLD-TASK"
        );
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_task_body() {
        let lane_root = unique_temp_dir("lane-assignment-body");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed = task.clone();
        changed.markdown.push_str("Extra body\n");
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &worker, &changed)
                .expect_err("changed body rejected");
        assert!(format!("{err:#}").contains("task body hash changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_dependencies() {
        let lane_root = unique_temp_dir("lane-assignment-deps");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec!["TASK-000".to_string()],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: `TASK-000`\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed = task.clone();
        changed.dependencies = vec![];
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &worker, &changed)
                .expect_err("changed dependencies rejected");
        assert!(format!("{err:#}").contains("dependency hash changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_verification_text() {
        let lane_root = unique_temp_dir("lane-assignment-verification");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed = task.clone();
        changed.markdown = changed
            .markdown
            .replace("cargo test task_one", "cargo test task_two");
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &worker, &changed)
                .expect_err("changed verification rejected");
        assert!(format!("{err:#}").contains("verification text hash changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_base_commit() {
        let lane_root = unique_temp_dir("lane-assignment-base-commit");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let err = validate_lane_assignment_metadata(&lane_root, "main", "def456", &worker, &task)
            .expect_err("changed base commit rejected");
        assert!(format!("{err:#}").contains("base commit changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_worker_model() {
        let lane_root = unique_temp_dir("lane-assignment-worker-model");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed_worker = worker.clone();
        changed_worker.model = "gpt-6".to_string();
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &changed_worker, &task)
                .expect_err("changed worker model rejected");
        assert!(format!("{err:#}").contains("worker model changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_worker_command() {
        let lane_root = unique_temp_dir("lane-assignment-worker-command");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed_worker = worker.clone();
        changed_worker.command.push("--new-worker-flag".to_string());
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &changed_worker, &task)
                .expect_err("changed worker command rejected");
        assert!(format!("{err:#}").contains("worker command changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn prompt_filename_task_id_round_trips() {
        assert_eq!(
            task_id_from_prompt_filename("P-029C-attempt-03-prompt.md"),
            Some("P-029C".to_string())
        );
        assert_eq!(
            task_id_from_prompt_filename("WEB-CRAPS-D-attempt-1-prompt.md"),
            Some("WEB-CRAPS-D".to_string())
        );
        assert_eq!(task_id_from_prompt_filename("stderr.log"), None);
    }

    #[test]
    fn lane_task_id_prefers_metadata_and_falls_back_to_latest_prompt() {
        let lane_root = unique_temp_dir("parallel-lane-task-id");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        fs::write(lane_root.join("P-018B-attempt-01-prompt.md"), "")
            .expect("failed to write prompt");
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(lane_root.join("P-021-attempt-02-prompt.md"), "")
            .expect("failed to write prompt");

        assert_eq!(
            read_lane_task_id(&lane_root).expect("lane task id should read"),
            Some("P-021".to_string())
        );

        fs::write(lane_root.join(super::LANE_TASK_ID_FILE), "P-029C\n")
            .expect("failed to write metadata");
        assert_eq!(
            read_lane_task_id(&lane_root).expect("lane task id should read"),
            Some("P-029C".to_string())
        );

        fs::remove_dir_all(&lane_root).expect("failed to remove lane root");
    }

    #[test]
    fn reset_parallel_lane_root_rehomes_existing_contents() {
        let lane_root = unique_temp_dir("parallel-lane-reset");
        fs::create_dir_all(lane_root.join("repo")).expect("failed to create lane repo");
        fs::write(lane_root.join("repo").join("stale.txt"), "stale")
            .expect("failed to write stale file");

        reset_parallel_lane_root(&lane_root).expect("lane root should reset");

        assert!(lane_root.exists(), "lane root should exist after reset");
        assert!(
            fs::read_dir(&lane_root)
                .expect("lane root should be readable")
                .next()
                .is_none(),
            "lane root should be recreated empty"
        );

        let parent = lane_root.parent().expect("lane root should have parent");
        let prefix = format!(
            "{}.stale-",
            lane_root
                .file_name()
                .expect("lane root should have file name")
                .to_string_lossy()
        );
        let stale_dirs = fs::read_dir(parent)
            .expect("parent should be readable")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().starts_with(&prefix))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        assert!(
            stale_dirs.is_empty(),
            "stale lane roots should be pruned after reset"
        );

        fs::remove_dir_all(&lane_root).expect("failed to remove lane root");
    }

    #[test]
    fn resume_candidate_matches_requested_task() {
        let ready_tasks = [
            LoopTask {
                id: "P-019D".to_string(),
                title: "first".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: String::new(),
            },
            LoopTask {
                id: "P-021".to_string(),
                title: "second".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: String::new(),
            },
        ];
        let mut resumable = BTreeMap::new();
        resumable.insert(
            2,
            LaneResumeCandidate {
                lane_index: 2,
                task: ready_tasks[1].clone(),
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: None,
            },
        );
        resumable.insert(
            5,
            LaneResumeCandidate {
                lane_index: 5,
                task: ready_tasks[0].clone(),
                lane_root: PathBuf::from("/tmp/lane-5"),
                lane_repo_root: PathBuf::from("/tmp/lane-5/repo"),
                base_commit: "def456".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-5/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-5/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-5/worker.pid"),
                host_recovery_note: Some("recover this lane".to_string()),
            },
        );

        let matched = take_resume_candidate_for_task(
            &mut resumable,
            &ready_tasks[0].id,
            &BTreeMap::<usize, ActiveLaneAssignment>::new(),
        )
        .expect("expected a matching resumable lane");
        assert_eq!(matched.0, 5);
        assert_eq!(matched.1.task.id, "P-019D");
        assert_eq!(
            matched.1.host_recovery_note.as_deref(),
            Some("recover this lane")
        );
        assert!(resumable.contains_key(&2));
        assert!(!resumable.contains_key(&5));

        let mut rediscovered = BTreeMap::new();
        rediscovered.insert(
            2,
            LaneResumeCandidate {
                lane_index: 2,
                task: ready_tasks[1].clone(),
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: None,
            },
        );
        resumable
            .get_mut(&2)
            .expect("lane-2 should remain resumable")
            .host_recovery_note = Some("preserve this note".to_string());
        preserve_resume_recovery_notes(&mut rediscovered, &resumable);
        assert_eq!(
            rediscovered
                .get(&2)
                .and_then(|candidate| candidate.host_recovery_note.as_deref()),
            Some("preserve this note")
        );

        let mut active = BTreeMap::new();
        active.insert(
            2,
            ActiveLaneAssignment {
                lane_index: 2,
                attempts: 1,
                task: ready_tasks[1].clone(),
                resumed: true,
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                clean_commit_since: None,
                terminate_requested_at: None,
                host_recovery_note: None,
            },
        );
        assert!(
            take_resume_candidate_for_task(&mut resumable, &ready_tasks[1].id, &active).is_none()
        );
    }
}
