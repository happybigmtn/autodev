fn classify_task_execution_kind(task: &LoopTask) -> &'static str {
    if task.lane_kind != LaneKind::Code {
        return task.lane_kind.label();
    }
    let text = format!("{} {}", task.id, task.title).to_ascii_uppercase();
    if text.contains("DEPLOY") || text.contains("MONITOR") || text.contains("OPS") {
        "ops"
    } else if text.contains("AUDIT")
        || text.contains("CHECKPOINT")
        || text.contains("SMOKE")
        || text.contains("COVERAGE")
    {
        "verification"
    } else {
        "code"
    }
}

fn is_operator_task(task: &LoopTask) -> bool {
    // Autonomous-execution mode: tasks tagged `Lane kind: operator` are
    // dispatched as code lanes alongside everything else. The historical
    // operator-queue concept (which shelved these tasks waiting on a human
    // operator to run live commands) is retired -- the worker handles
    // them with full tool access. Keep the parsed LaneKind value for
    // observability but don't use it to gate dispatch.
    let _ = task;
    false
}

fn is_evidence_lane_task(task: &LoopTask) -> bool {
    task.lane_kind == LaneKind::Evidence || is_verification_only_task(task)
}

fn describe_parallel_idle_state(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
) -> String {
    let ready = ready_parallel_tasks(plan, active_tasks, shelved_tasks, deferred_partial_tasks);
    let operator_ready = ready.iter().filter(|task| is_operator_task(task)).count();
    let (verification_only, executable_ready): (Vec<_>, Vec<_>) = ready
        .into_iter()
        .filter(|task| !is_operator_task(task))
        .partition(is_evidence_lane_task);
    if executable_ready.is_empty() && !verification_only.is_empty() {
        return format!(
            "manual/evidence checkpoints are ready: {}{}",
            verification_only
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if operator_ready == 0 {
                String::new()
            } else {
                format!("; operator queue: {operator_ready}")
            }
        );
    }
    if executable_ready.is_empty() && operator_ready > 0 {
        return format!("operator queue has {operator_ready} item(s); no code lanes are ready");
    }

    let waiting_on = unresolved_frontier_dependency_ids(
        plan,
        active_tasks,
        shelved_tasks,
        deferred_partial_tasks,
    );
    if waiting_on.is_empty() {
        if deferred_partial_tasks.is_empty() {
            "no dependency-ready task is currently available".to_string()
        } else {
            format!(
                "partial follow-up tasks parked for this run: {}",
                deferred_partial_tasks
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    } else {
        let waiting = format!(
            "waiting on dependencies: {}",
            waiting_on.into_iter().collect::<Vec<_>>().join(", ")
        );
        let frontier_suffix = format_parallel_blocker_frontier(
            plan,
            active_tasks,
            shelved_tasks,
            deferred_partial_tasks,
            4,
        )
        .map(|summary| format!("; frontier: {summary}"))
        .unwrap_or_default();
        if deferred_partial_tasks.is_empty() {
            format!("{waiting}{frontier_suffix}")
        } else {
            format!(
                "{}{}; partial follow-up tasks parked for this run: {}",
                waiting,
                frontier_suffix,
                deferred_partial_tasks
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

fn ready_parallel_tasks(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
) -> Vec<LoopTask> {
    let ready = plan
        .ready_tasks(active_tasks)
        .into_iter()
        .filter(|task| !shelved_tasks.contains_key(&task.id))
        .filter(|task| !deferred_partial_tasks.contains(&task.id))
        .collect::<Vec<_>>();
    let (mut pending, partials): (Vec<_>, Vec<_>) = ready
        .into_iter()
        .partition(|task| task.status == LoopTaskStatus::Pending);
    pending.extend(partials);
    pending
}

fn prioritize_ready_parallel_tasks(repo_root: &Path, ready: Vec<LoopTask>) -> Vec<LoopTask> {
    let dirty_paths = repo_dirty_paths_for_parallel_dispatch(repo_root);
    if dirty_paths.is_empty() {
        return ready;
    }

    let (pending, partials): (Vec<_>, Vec<_>) = ready
        .into_iter()
        .partition(|task| task.status == LoopTaskStatus::Pending);
    let mut ordered = stable_partition_tasks_by_dirty_overlap(pending, &dirty_paths);
    ordered.extend(stable_partition_tasks_by_dirty_overlap(
        partials,
        &dirty_paths,
    ));
    ordered
}

fn stable_partition_tasks_by_dirty_overlap(
    tasks: Vec<LoopTask>,
    dirty_paths: &BTreeSet<String>,
) -> Vec<LoopTask> {
    let mut clean = Vec::new();
    let mut overlapping = Vec::new();
    for task in tasks {
        if task_overlaps_dirty_canonical_paths(&task, dirty_paths) {
            overlapping.push(task);
        } else {
            clean.push(task);
        }
    }
    clean.extend(overlapping);
    clean
}

fn repo_dirty_paths_for_parallel_dispatch(repo_root: &Path) -> BTreeSet<String> {
    git_stdout(
        repo_root,
        ["status", "--short", "--untracked-files=all", "--", "."],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(parse_parallel_status_path)
    .filter(|path| !parallel_dispatch_path_is_ignored(path))
    .collect()
}

fn parse_parallel_status_path(line: &str) -> Option<String> {
    let line = line.trim_end();
    if line.trim().is_empty() || line.starts_with("##") {
        return None;
    }
    let path = line.get(3..)?.trim();
    let path = path.rsplit_once(" -> ").map(|(_, rhs)| rhs).unwrap_or(path);
    let path = path.trim_matches('"').trim();
    (!path.is_empty()).then(|| path.to_string())
}

fn parallel_dispatch_path_is_ignored(path: &str) -> bool {
    if path.starts_with(".auto/symphony/verification-receipts/")
        || path.starts_with("auto/symphony/verification-receipts/")
    {
        return true;
    }
    let first_segment = path.split('/').next().unwrap_or(path);
    first_segment == ".auto"
        || first_segment == "auto"
        || first_segment == "bug"
        || first_segment == "nemesis"
        || first_segment.starts_with("gen-")
}

fn task_overlaps_dirty_canonical_paths(task: &LoopTask, dirty_paths: &BTreeSet<String>) -> bool {
    dirty_paths.iter().any(|path| task.markdown.contains(path))
}

fn unresolved_frontier_dependency_ids(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
) -> BTreeSet<String> {
    let unresolved = plan.unresolved_dependency_ids(active_tasks);
    plan.tasks
        .iter()
        .filter(|task| plan.is_actionable_unfinished(task))
        .filter(|task| !active_tasks.contains(&task.id))
        .filter(|task| !shelved_tasks.contains_key(&task.id))
        .filter(|task| !deferred_partial_tasks.contains(&task.id))
        .flat_map(|task| {
            task.dependencies
                .iter()
                .filter(|dep| unresolved.contains(dep.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect()
}

fn parallel_blocker_frontier(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
) -> Vec<ParallelBlockerDetail> {
    let mut details = unresolved_frontier_dependency_ids(
        plan,
        active_tasks,
        shelved_tasks,
        deferred_partial_tasks,
    )
    .into_iter()
    .map(|task_id| {
        let kind = if active_tasks.contains(&task_id) {
            ParallelBlockerKind::InFlight
        } else if shelved_tasks.contains_key(&task_id) {
            ParallelBlockerKind::Shelved
        } else if deferred_partial_tasks.contains(&task_id) {
            ParallelBlockerKind::DeferredPartial
        } else {
            match plan.task(&task_id).map(|task| task.status) {
                Some(LoopTaskStatus::Blocked) => ParallelBlockerKind::Blocked,
                _ => ParallelBlockerKind::Pending,
            }
        };
        let downstream = plan.direct_unfinished_dependents(&task_id);
        ParallelBlockerDetail {
            task_id,
            kind,
            downstream,
        }
    })
    .collect::<Vec<_>>();
    details.sort_by(|left, right| {
        right
            .downstream
            .len()
            .cmp(&left.downstream.len())
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    details
}

fn format_parallel_blocker_frontier(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
    max_items: usize,
) -> Option<String> {
    let rendered =
        parallel_blocker_frontier(plan, active_tasks, shelved_tasks, deferred_partial_tasks)
            .into_iter()
            .take(max_items)
            .map(|detail| {
                let downstream = if detail.downstream.is_empty() {
                    "no direct unfinished dependents".to_string()
                } else {
                    detail.downstream.join(", ")
                };
                format!(
                    "{} [{}] -> {}",
                    detail.task_id,
                    detail.kind.label(),
                    downstream
                )
            })
            .collect::<Vec<_>>();
    (!rendered.is_empty()).then(|| rendered.join(" | "))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PartialFollowUpDisposition {
    RetryLaterThisRun,
    ParkForRestOfRun,
}

fn record_partial_follow_up(
    task_id: &str,
    attempted_partial_followups: &mut BTreeSet<String>,
    deferred_partial_tasks: &mut BTreeSet<String>,
) -> PartialFollowUpDisposition {
    if attempted_partial_followups.insert(task_id.to_string()) {
        deferred_partial_tasks.remove(task_id);
        PartialFollowUpDisposition::RetryLaterThisRun
    } else {
        deferred_partial_tasks.insert(task_id.to_string());
        PartialFollowUpDisposition::ParkForRestOfRun
    }
}

fn clear_partial_follow_up_tracking(
    task_id: &str,
    attempted_partial_followups: &mut BTreeSet<String>,
    deferred_partial_tasks: &mut BTreeSet<String>,
) {
    attempted_partial_followups.remove(task_id);
    deferred_partial_tasks.remove(task_id);
}

fn attach_partial_follow_up_note(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    attempted_partial_followups: &BTreeSet<String>,
) {
    if assignment.task.status != LoopTaskStatus::Partial || assignment.host_recovery_note.is_some()
    {
        return;
    }

    let evidence =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    let assessment = assess_task_completion_gap(&assignment.task.markdown, &evidence);
    let pass_label = if attempted_partial_followups.contains(&assignment.task.id) {
        "This is the automatic evidence-repair pass for a task that already landed code earlier in this run."
    } else {
        "This task is already marked `- [~]`; treat this lane as follow-up work to close the remaining evidence gap rather than redoing landed implementation."
    };
    let gap_kind = match assessment.kind {
        CompletionGapKind::None => {
            "The host currently sees no missing local evidence, so verify whether the partial marker is stale and finish the task cleanly if it is."
        }
        CompletionGapKind::LocalRepairable => {
            "The remaining gap looks repo-local: focus on missing verification evidence and declared artifacts from this lane."
        }
        CompletionGapKind::ExternalOrLiveFollowUp => {
            "The remaining gap appears to depend on live or external proof. First check whether the repo now contains enough scaffolding to satisfy it locally; if not, capture the blocker precisely instead of broadening scope."
        }
    };
    let missing = if assessment.missing_reasons.is_empty() {
        "none recorded by the host".to_string()
    } else {
        assessment.missing_reasons.join("; ")
    };
    let verification_commands = if assessment.verification_commands.is_empty() {
        "- none parsed as literal shell commands from the task's `Verification:` block".to_string()
    } else {
        assessment
            .verification_commands
            .iter()
            .map(|step| format!("- {step}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let verification_guidance = if assessment.verification_guidance.is_empty() {
        "- none".to_string()
    } else {
        assessment
            .verification_guidance
            .iter()
            .map(|step| format!("- {step}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assignment.host_recovery_note = Some(format!(
        "{pass_label}\n\nHost evidence summary:\n- Remaining gaps: {missing}\n- Guidance: {gap_kind}\n- Re-run the executable verification commands below through the repo wrapper when required.\n- Do not treat narrative verification prose as literal shell input; if no executable commands were parsed, derive the narrowest truthful proof yourself instead of patching the wrapper.\n- If the only remaining blocker is genuinely external/live proof, print `AUTO_ENV_BLOCKER: <short reason>` before exiting non-zero.\n\nExecutable verification commands parsed by the host:\n{verification_commands}\n\nNarrative verification guidance preserved from the task:\n{verification_guidance}"
    ));
}

fn completion_status_suffix(
    task_id: &str,
    completion_status: LoopTaskStatus,
    attempted_partial_followups: &mut BTreeSet<String>,
    deferred_partial_tasks: &mut BTreeSet<String>,
) -> &'static str {
    match completion_status {
        LoopTaskStatus::Done => {
            clear_partial_follow_up_tracking(
                task_id,
                attempted_partial_followups,
                deferred_partial_tasks,
            );
            ""
        }
        LoopTaskStatus::Partial => match record_partial_follow_up(
            task_id,
            attempted_partial_followups,
            deferred_partial_tasks,
        ) {
            PartialFollowUpDisposition::RetryLaterThisRun => {
                " [~ evidence gap remains; follow-up pass queued]"
            }
            PartialFollowUpDisposition::ParkForRestOfRun => {
                " [~ evidence gap remains; queued for autonomous unblock]"
            }
        },
        LoopTaskStatus::Pending | LoopTaskStatus::Blocked => "",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParallelUnblockCandidateKind {
    ShelvedResume,
    DeferredPartialCloseout,
}

impl ParallelUnblockCandidateKind {
    fn label(self) -> &'static str {
        match self {
            Self::ShelvedResume => "shelved-resume",
            Self::DeferredPartialCloseout => "tail-closeout",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParallelUnblockCandidate {
    task: LoopTask,
    kind: ParallelUnblockCandidateKind,
    downstream: Vec<String>,
}

fn next_parallel_unblock_candidate(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
    resumable_lanes: &BTreeMap<usize, LaneResumeCandidate>,
    unblock_attempt_counts: &BTreeMap<String, usize>,
    max_autonomous_unblock_attempts: usize,
) -> Option<ParallelUnblockCandidate> {
    let mut candidates = plan
        .ready_tasks(active_tasks)
        .into_iter()
        .filter(|task| {
            unblock_attempt_counts.get(&task.id).copied().unwrap_or(0)
                < max_autonomous_unblock_attempts
        })
        .filter_map(|task| {
            let downstream = plan.direct_unfinished_dependents(&task.id);
            if shelved_tasks.contains_key(&task.id) {
                resumable_lanes
                    .values()
                    .any(|candidate| candidate.task.id == task.id)
                    .then_some(ParallelUnblockCandidate {
                        task,
                        kind: ParallelUnblockCandidateKind::ShelvedResume,
                        downstream,
                    })
            } else if deferred_partial_tasks.contains(&task.id) {
                Some(ParallelUnblockCandidate {
                    task,
                    kind: ParallelUnblockCandidateKind::DeferredPartialCloseout,
                    downstream,
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .downstream
            .len()
            .cmp(&left.downstream.len())
            .then_with(|| {
                unblock_candidate_priority(left.kind).cmp(&unblock_candidate_priority(right.kind))
            })
            .then_with(|| left.task.id.cmp(&right.task.id))
    });
    candidates.into_iter().next()
}

fn unblock_candidate_priority(kind: ParallelUnblockCandidateKind) -> usize {
    match kind {
        ParallelUnblockCandidateKind::ShelvedResume => 0,
        ParallelUnblockCandidateKind::DeferredPartialCloseout => 1,
    }
}

fn prepend_host_recovery_note(assignment: &mut ActiveLaneAssignment, note: &str) {
    assignment.host_recovery_note = Some(match assignment.host_recovery_note.take() {
        Some(existing) if !existing.trim().is_empty() => {
            format!("{}\n\n{}", note.trim(), existing.trim())
        }
        _ => note.trim().to_string(),
    });
}

fn render_parallel_unblock_note(candidate: &ParallelUnblockCandidate) -> String {
    let downstream = if candidate.downstream.is_empty() {
        "no direct unfinished dependents recorded in the plan".to_string()
    } else {
        candidate.downstream.join(", ")
    };
    match candidate.kind {
        ParallelUnblockCandidateKind::ShelvedResume => format!(
            "This lane is a host-directed dependency-unblock attempt. The normal ready queue is empty because this previously shelved task is still load-bearing.\n\nDownstream tasks blocked by `{}`: {}\n\nFocus on salvaging and landing the already-started work instead of broadening scope.",
            candidate.task.id, downstream
        ),
        ParallelUnblockCandidateKind::DeferredPartialCloseout => format!(
            "This lane is the final same-run closeout pass for a parked `[~]` task. The normal ready queue is empty and the remaining frontier depends on closing this task honestly.\n\nDownstream tasks blocked by `{}`: {}\n\nDo not redo landed implementation. Focus only on the remaining review/verification/artifact gap.",
            candidate.task.id, downstream
        ),
    }
}

