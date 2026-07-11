use super::*;

pub(crate) fn no_dependency_ready_stop_message(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    queue: &LoopQueueSnapshot,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
    unblock_attempt_counts: &BTreeMap<String, usize>,
    max_autonomous_unblock_attempts: usize,
) -> String {
    let blocked = if queue.blocked_ids.is_empty() {
        "none".to_string()
    } else {
        queue.blocked_ids.join(", ")
    };
    let frontier_suffix = format_parallel_blocker_frontier(
        plan,
        active_tasks,
        shelved_tasks,
        deferred_partial_tasks,
        6,
    )
    .map(|summary| format!(" frontier: {summary}"))
    .unwrap_or_default();
    let exhausted_suffix = exhausted_autonomous_unblock_suffix(
        shelved_tasks,
        deferred_partial_tasks,
        unblock_attempt_counts,
        max_autonomous_unblock_attempts,
    );
    let deferred = if deferred_partial_tasks.is_empty() {
        None
    } else {
        Some(
            deferred_partial_tasks
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        )
    };
    if shelved_tasks.is_empty() {
        if let Some(deferred) = deferred {
            return format!(
                "no dependency-ready tasks remain to dispatch; stopping with partial follow-up tasks after autonomous unblock attempts. pending: {} blocked: {} deferred: {}{}{}",
                queue.pending_ids.join(", "),
                blocked,
                deferred,
                exhausted_suffix,
                frontier_suffix
            );
        }
        return format!(
            "no dependency-ready tasks remain to dispatch; stopping. pending: {} blocked: {}{}{}",
            queue.pending_ids.join(", "),
            blocked,
            exhausted_suffix,
            frontier_suffix
        );
    }
    let shelved = shelved_tasks.keys().cloned().collect::<Vec<_>>().join(", ");
    let deferred_suffix = deferred
        .map(|deferred| format!(" deferred: {deferred}"))
        .unwrap_or_default();
    format!(
        "no dependency-ready tasks remain to dispatch; stopping with unresolved shelved tasks. pending: {} blocked: {} shelved: {}{}{}{}",
        queue.pending_ids.join(", "),
        blocked,
        shelved,
        deferred_suffix,
        exhausted_suffix,
        frontier_suffix
    )
}

pub(crate) fn autonomous_unblock_attempt_limit(max_retries: usize) -> usize {
    MIN_AUTONOMOUS_UNBLOCK_ATTEMPTS.max(max_retries.saturating_add(2))
}

pub(crate) fn exhausted_autonomous_unblock_suffix(
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
    unblock_attempt_counts: &BTreeMap<String, usize>,
    max_autonomous_unblock_attempts: usize,
) -> String {
    let exhausted = shelved_tasks
        .keys()
        .chain(deferred_partial_tasks.iter())
        .filter_map(|task_id| {
            let attempts = unblock_attempt_counts.get(task_id).copied().unwrap_or(0);
            (attempts >= max_autonomous_unblock_attempts)
                .then(|| format!("{task_id}={attempts}/{max_autonomous_unblock_attempts}"))
        })
        .collect::<Vec<_>>();
    if exhausted.is_empty() {
        String::new()
    } else {
        format!(" exhausted-unblock-attempts: {}", exhausted.join(", "))
    }
}

pub(crate) fn write_operator_actions_for_ready_tasks(
    run_root: &Path,
    tasks: &[LoopTask],
) -> Result<PathBuf> {
    let path = run_root.join("operator-actions.md");
    let mut body = String::from("# Operator Actions\n\n");
    body.push_str(
        "These dependency-ready tasks require an operator or live external authority. They are not dispatched to code workers.\n\n",
    );
    for task in tasks {
        body.push_str(&format!("## `{}` {}\n", task.id, task.title));
        body.push_str("- Lane kind: operator\n");
        body.push_str("- Expected operator action: run the task's live commands or capture the named external evidence, then rerun `auto parallel`.\n\n");
        body.push_str("```markdown\n");
        body.push_str(&task.markdown);
        body.push_str("\n```\n\n");
    }
    atomic_write(&path, body.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

pub(crate) fn clear_stale_operator_actions(run_root: &Path, parallel_logger: &ParallelEventLogger) {
    let path = run_root.join("operator-actions.md");
    if !path.exists() {
        return;
    }
    match fs::remove_file(&path) {
        Ok(()) => parallel_logger.info(format!(
            "operator-queue: cleared stale operator action queue at {}",
            path.display()
        )),
        Err(err) => parallel_logger.warn(format!(
            "warning: failed clearing stale operator action queue {}: {err:#}",
            path.display()
        )),
    }
}

pub(crate) fn rebuild_active_tasks(
    active_tasks: &mut BTreeSet<String>,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
) {
    active_tasks.clear();
    active_tasks.extend(
        active_lanes
            .values()
            .map(|assignment| assignment.task.id.clone()),
    );
}

pub(crate) fn classify_task_execution_kind(task: &LoopTask) -> &'static str {
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

pub(crate) fn is_operator_task(task: &LoopTask) -> bool {
    task.lane_kind == LaneKind::Operator
}

pub(crate) fn is_evidence_lane_task(task: &LoopTask) -> bool {
    task.lane_kind == LaneKind::Evidence || is_verification_only_task(task)
}

pub(crate) fn describe_parallel_idle_state(
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

pub(crate) fn ready_parallel_tasks(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
) -> Vec<LoopTask> {
    ready_parallel_tasks_with_gate_holds(
        plan,
        active_tasks,
        shelved_tasks,
        deferred_partial_tasks,
        &BTreeSet::new(),
    )
}

/// Dispatch-ready tasks, holding back dependents of any gate-held Partial in
/// `gate_held` (see [`LoopPlanSnapshot::ready_tasks_with_gate_holds`]). The
/// gate-held Partial itself still surfaces for its own closeout.
pub(crate) fn ready_parallel_tasks_with_gate_holds(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
    gate_held: &BTreeSet<String>,
) -> Vec<LoopTask> {
    let ready = plan
        .ready_tasks_with_gate_holds(active_tasks, gate_held)
        .into_iter()
        .filter(|task| !shelved_tasks.contains_key(&task.id))
        .filter(|task| !deferred_partial_tasks.contains(&task.id))
        .collect::<Vec<_>>();
    let (mut pending, partials): (Vec<_>, Vec<_>) = ready
        .into_iter()
        .partition(|task| task.status == LoopTaskStatus::Pending);
    // Frontier-first: dispatch pending tasks that unblock the most downstream
    // work before independent leaves, so a dependency chain's critical path is
    // never left waiting behind a task nothing depends on. Stable sort keeps
    // plan order as the tie-break. Partials (landed-partial backlog) always
    // follow all pending, so a re-dispatch never preempts a ready pending task.
    let dependents = transitive_dependent_counts(plan);
    pending.sort_by_key(|task| std::cmp::Reverse(dependents.get(&task.id).copied().unwrap_or(0)));
    pending.extend(partials);
    pending
}

/// For each task id, how many OTHER tasks transitively depend on it (reachable
/// through `dependencies` edges). Used to order the ready queue frontier-first.
pub(crate) fn transitive_dependent_counts(plan: &LoopPlanSnapshot) -> BTreeMap<String, usize> {
    // Reverse adjacency: dep -> tasks that directly list it.
    let mut dependents_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for task in &plan.tasks {
        for dep in &task.dependencies {
            dependents_of
                .entry(dep.as_str())
                .or_default()
                .push(task.id.as_str());
        }
    }
    let mut counts = BTreeMap::new();
    for task in &plan.tasks {
        let mut seen = BTreeSet::new();
        let mut stack = vec![task.id.as_str()];
        while let Some(current) = stack.pop() {
            if let Some(children) = dependents_of.get(current) {
                for &child in children {
                    if seen.insert(child) {
                        stack.push(child);
                    }
                }
            }
        }
        counts.insert(task.id.clone(), seen.len());
    }
    counts
}

pub(crate) fn prioritize_ready_parallel_tasks(
    repo_root: &Path,
    ready: Vec<LoopTask>,
) -> Vec<LoopTask> {
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

pub(crate) fn stable_partition_tasks_by_dirty_overlap(
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

pub(crate) fn repo_dirty_paths_for_parallel_dispatch(repo_root: &Path) -> BTreeSet<String> {
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

pub(crate) fn parse_parallel_status_path(line: &str) -> Option<String> {
    let line = line.trim_end();
    if line.trim().is_empty() || line.starts_with("##") {
        return None;
    }
    let path = line.get(3..)?.trim();
    let path = path.rsplit_once(" -> ").map(|(_, rhs)| rhs).unwrap_or(path);
    let path = path.trim_matches('"').trim();
    (!path.is_empty()).then(|| path.to_string())
}

pub(crate) fn parallel_dispatch_path_is_ignored(path: &str) -> bool {
    if path.starts_with(".auto/symphony/verification-receipts/")
        || path.starts_with("auto/symphony/verification-receipts/")
    {
        return true;
    }
    let first_segment = path.split('/').next().unwrap_or(path);
    first_segment == ".auto"
        || first_segment == "auto"
        || first_segment == "bug"
        || first_segment == "genesis"
        || first_segment == "nemesis"
        || first_segment == "steward"
        || first_segment.starts_with("gen-")
}

pub(crate) fn task_overlaps_dirty_canonical_paths(
    task: &LoopTask,
    dirty_paths: &BTreeSet<String>,
) -> bool {
    dirty_paths.iter().any(|path| task.markdown.contains(path))
}

pub(crate) fn unresolved_frontier_dependency_ids(
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

pub(crate) fn parallel_blocker_frontier(
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

pub(crate) fn format_parallel_blocker_frontier(
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
pub(crate) enum PartialFollowUpDisposition {
    RetryLaterThisRun,
    ParkForRestOfRun,
}

pub(crate) fn partial_followup_attempt_limit() -> usize {
    std::env::var("AUTO_PARTIAL_FOLLOWUP_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2)
}

pub(crate) fn record_partial_follow_up(
    task_id: &str,
    attempted_partial_followups: &mut BTreeMap<String, usize>,
    deferred_partial_tasks: &mut BTreeSet<String>,
) -> PartialFollowUpDisposition {
    let limit = partial_followup_attempt_limit();
    let count = attempted_partial_followups
        .entry(task_id.to_string())
        .or_insert(0);
    *count += 1;
    if *count <= limit {
        deferred_partial_tasks.remove(task_id);
        PartialFollowUpDisposition::RetryLaterThisRun
    } else {
        deferred_partial_tasks.insert(task_id.to_string());
        PartialFollowUpDisposition::ParkForRestOfRun
    }
}

pub(crate) fn clear_partial_follow_up_tracking(
    task_id: &str,
    attempted_partial_followups: &mut BTreeMap<String, usize>,
    deferred_partial_tasks: &mut BTreeSet<String>,
) {
    attempted_partial_followups.remove(task_id);
    deferred_partial_tasks.remove(task_id);
}

pub(crate) fn attach_partial_follow_up_note(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    attempted_partial_followups: &BTreeMap<String, usize>,
) {
    if assignment.task.status != LoopTaskStatus::Partial || assignment.host_recovery_note.is_some()
    {
        return;
    }

    let evidence =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    let assessment = assess_task_completion_gap(&assignment.task.markdown, &evidence);
    let pass_label = if attempted_partial_followups.contains_key(&assignment.task.id) {
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

pub(crate) fn completion_status_suffix(
    task_id: &str,
    completion_status: LoopTaskStatus,
    attempted_partial_followups: &mut BTreeMap<String, usize>,
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
pub(crate) enum ParallelUnblockCandidateKind {
    ShelvedResume,
    DeferredPartialCloseout,
}

impl ParallelUnblockCandidateKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ShelvedResume => "shelved-resume",
            Self::DeferredPartialCloseout => "tail-closeout",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParallelUnblockCandidate {
    pub(crate) task: LoopTask,
    pub(crate) kind: ParallelUnblockCandidateKind,
    pub(crate) downstream: Vec<String>,
}

pub(crate) fn next_parallel_unblock_candidate(
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

pub(crate) fn unblock_candidate_priority(kind: ParallelUnblockCandidateKind) -> usize {
    match kind {
        ParallelUnblockCandidateKind::ShelvedResume => 0,
        ParallelUnblockCandidateKind::DeferredPartialCloseout => 1,
    }
}

pub(crate) fn render_parallel_unblock_note(candidate: &ParallelUnblockCandidate) -> String {
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

pub(crate) fn is_verification_only_task(task: &LoopTask) -> bool {
    // Only treat as verification-only when the scope boundary LEADS with the
    // directive — not when an ordinary code task merely mentions the phrase
    // mid-sentence (e.g. "...fail-closed verification only; do not add commands").
    // This is the same body-prose false-positive class that `infer_lane_kind`
    // was already fixed for; an incidental mention previously forced a real code
    // task onto the non-dispatchable evidence lane and stalled the frontier.
    task_field_body(&task.markdown, "Scope boundary:", "Acceptance criteria:")
        .map(|body| body.trim().to_ascii_lowercase().starts_with("verification only"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use crate::parallel_command::*;
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

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    #[test]
    fn parallel_dispatch_ignores_run_artifact_roots() {
        for path in [
            ".auto/orchestrator/run/receipt.md",
            "auto/symphony/verification-receipts/TASK-1.json",
            "bug/report.md",
            "genesis/GBRAIN-CONTEXT.md",
            "gen-20260601-125649/IMPLEMENTATION_PLAN.md",
            "nemesis/report.md",
            "steward/final-review.md",
        ] {
            assert!(
                parallel_dispatch_path_is_ignored(path),
                "{path} should be treated as run evidence, not dispatch source"
            );
        }
        assert!(!parallel_dispatch_path_is_ignored("IMPLEMENTATION_PLAN.md"));
        assert!(!parallel_dispatch_path_is_ignored("src/lib.rs"));
    }

    #[test]
    fn no_dependency_ready_stop_message_calls_out_shelved_tasks() {
        let plan = parse_loop_plan(
            r#"
- [ ] `TASK-1` First
  Dependencies: `TASK-3`
- [ ] `TASK-2` Second
  Dependencies: `TASK-4`
- [ ] `TASK-3` Blocker one
  Dependencies: none
- [ ] `TASK-4` Blocker two
  Dependencies: none
"#,
        );
        let queue = LoopQueueSnapshot {
            pending_ids: vec!["TASK-1".to_string(), "TASK-2".to_string()],
            blocked_ids: vec!["TASK-9".to_string()],
        };
        let mut shelved = BTreeMap::new();
        shelved.insert("TASK-3".to_string(), "- [ ] `TASK-3`".to_string());
        shelved.insert("TASK-4".to_string(), "- [ ] `TASK-4`".to_string());
        let deferred = BTreeSet::from(["TASK-5".to_string()]);

        let attempts = BTreeMap::from([("TASK-5".to_string(), 4usize)]);
        let message = no_dependency_ready_stop_message(
            &plan,
            &BTreeSet::new(),
            &queue,
            &shelved,
            &deferred,
            &attempts,
            4,
        );
        assert!(message.contains("stopping with unresolved shelved tasks"));
        assert!(message.contains("pending: TASK-1, TASK-2"));
        assert!(message.contains("blocked: TASK-9"));
        assert!(message.contains("shelved: TASK-3, TASK-4"));
        assert!(message.contains("deferred: TASK-5"));
        assert!(message.contains("exhausted-unblock-attempts: TASK-5=4/4"));
        assert!(message.contains("frontier: TASK-3 [shelved]"));
    }

    #[test]
    fn parallel_blocker_frontier_lists_unlanded_shelved_not_landed_partial() {
        // TASK-S is an unlanded `[ ]` blocker (shelved this run) -> it still
        // blocks its dependent and belongs on the frontier. TASK-P is a `[~]`
        // partial whose code already landed (only its closeout is deferred), so
        // it must NOT block its dependent and must not appear as a blocker.
        let plan = parse_loop_plan(
            r#"
- [ ] `TASK-A` waits on shelved
  Dependencies: `TASK-S`
- [ ] `TASK-B` waits on partial
  Dependencies: `TASK-P`
- [ ] `TASK-S` shelved blocker
  Dependencies: none
- [~] `TASK-P` partial blocker
  Dependencies: none
"#,
        );
        let shelved = BTreeMap::from([(
            "TASK-S".to_string(),
            "- [ ] `TASK-S` shelved blocker".to_string(),
        )]);
        let deferred = BTreeSet::from(["TASK-P".to_string()]);

        let frontier = parallel_blocker_frontier(&plan, &BTreeSet::new(), &shelved, &deferred);
        assert_eq!(frontier.len(), 1);
        assert_eq!(frontier[0].task_id, "TASK-S");
        assert_eq!(frontier[0].kind, ParallelBlockerKind::Shelved);
        assert!(
            frontier.iter().all(|blocker| blocker.task_id != "TASK-P"),
            "a landed `[~]` partial must not appear as a dependency blocker"
        );
    }

    #[test]
    fn next_parallel_unblock_candidate_prefers_resumable_shelved_blocker() {
        let plan = parse_loop_plan(
            r#"
- [ ] `TASK-A` blocked by shelved
  Dependencies: `TASK-S`
- [ ] `TASK-B` blocked by shelved
  Dependencies: `TASK-S`
- [ ] `TASK-C` blocked by deferred
  Dependencies: `TASK-P`
- [ ] `TASK-S` ready shelved blocker
  Dependencies: none
- [~] `TASK-P` ready deferred blocker
  Dependencies: none
"#,
        );
        let task_s = plan.task("TASK-S").expect("TASK-S should exist").clone();
        let shelved = BTreeMap::from([("TASK-S".to_string(), task_s.markdown.clone())]);
        let deferred = BTreeSet::from(["TASK-P".to_string()]);
        let resumable = BTreeMap::from([(
            2usize,
            LaneResumeCandidate {
                lane_index: 2,
                task: task_s,
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: Some("recover".to_string()),
            },
        )]);

        let candidate = next_parallel_unblock_candidate(
            &plan,
            &BTreeSet::new(),
            &shelved,
            &deferred,
            &resumable,
            &BTreeMap::new(),
            4,
        )
        .expect("expected an unblock candidate");
        assert_eq!(candidate.task.id, "TASK-S");
        assert_eq!(candidate.kind, ParallelUnblockCandidateKind::ShelvedResume);
    }

    #[test]
    fn next_parallel_unblock_candidate_retries_until_attempt_limit() {
        let plan = parse_loop_plan(
            r#"
- [ ] `TASK-A` blocked by partial
  Dependencies: `TASK-P`
- [~] `TASK-P` partial blocker
  Dependencies: none
"#,
        );
        let deferred = BTreeSet::from(["TASK-P".to_string()]);
        let mut attempts = BTreeMap::from([("TASK-P".to_string(), 3usize)]);

        let candidate = next_parallel_unblock_candidate(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &deferred,
            &BTreeMap::new(),
            &attempts,
            4,
        )
        .expect("attempt 4 should still be eligible");
        assert_eq!(candidate.task.id, "TASK-P");
        assert_eq!(
            candidate.kind,
            ParallelUnblockCandidateKind::DeferredPartialCloseout
        );

        attempts.insert("TASK-P".to_string(), 4);
        assert!(next_parallel_unblock_candidate(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &deferred,
            &BTreeMap::new(),
            &attempts,
            4,
        )
        .is_none());
    }

    #[test]
    fn verification_only_tasks_are_detected() {
        let verification_only = LoopTask {
            id: "WEB-CRAPS-C".to_string(),
            title: "checkpoint".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec!["WEB-CRAPS-B".to_string()],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `WEB-CRAPS-C` Checkpoint\n  Scope boundary: verification only.\n  Acceptance criteria:\n    - pass".to_string(),
        };
        let normal = LoopTask {
            id: "WEB-CRAPS-D".to_string(),
            title: "real work".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec!["WEB-CRAPS-C".to_string()],
            estimated_scope: Some("M".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `WEB-CRAPS-D` Real work\n  Scope boundary: state source only.\n  Acceptance criteria:\n    - ship".to_string(),
        };

        assert!(is_verification_only_task(&verification_only));
        assert!(!is_verification_only_task(&normal));
    }

    #[test]
    fn operator_actions_file_records_full_task_contract() {
        let run_root = unique_temp_dir("operator-actions");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let task = LoopTask {
            id: "POOL-300426-07".to_string(),
            title: "Generate live keypairs".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Operator,
            markdown: "- [ ] `POOL-300426-07` Generate live keypairs\n  Lane kind: operator\n  Verification: `ssh root@loom make keys`\n  Dependencies: none\n".to_string(),
        };
        let path = write_operator_actions_for_ready_tasks(&run_root, &[task])
            .expect("operator queue should write");
        let text = fs::read_to_string(&path).expect("operator queue should be readable");
        assert!(text.contains("POOL-300426-07"));
        assert!(text.contains("ssh root@loom make keys"));
        fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn ready_parallel_tasks_skips_partials_deferred_for_this_run() {
        let plan = r#"
- [~] `TASK-001` Evidence gap still needs follow-up
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-002` Independent ready task
  Dependencies: none
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let ready = ready_parallel_tasks(
            &snapshot,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::from(["TASK-001".to_string()]),
        );
        assert_eq!(
            ready.into_iter().map(|task| task.id).collect::<Vec<_>>(),
            vec!["TASK-002"]
        );
    }

    #[test]
    fn ready_parallel_tasks_prioritizes_pending_before_partial_followups() {
        let plan = r#"
- [~] `TASK-001` Evidence gap still needs follow-up
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-002` Fresh ready task
  Dependencies: none
  Estimated scope: S
- [~] `TASK-003` Another partial
  Dependencies: none
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let ready = ready_parallel_tasks(
            &snapshot,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        );
        assert_eq!(
            ready.into_iter().map(|task| task.id).collect::<Vec<_>>(),
            vec!["TASK-002", "TASK-001", "TASK-003"]
        );
    }

    #[test]
    fn ready_parallel_tasks_orders_frontier_first() {
        // Chain A <- B <- C (C depends on B, B on A) plus independent D. Only A
        // and D are dependency-ready. A unblocks two downstream tasks; D none.
        // D is listed first in plan order to prove the reorder actually fires.
        let plan = r#"
- [ ] `TASK-D` independent leaf
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-A` critical-path root
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-B` depends on A
  Dependencies: `TASK-A`
  Estimated scope: S
- [ ] `TASK-C` depends on B
  Dependencies: `TASK-B`
  Estimated scope: S
"#;
        let snapshot = parse_loop_plan(plan);
        let counts = transitive_dependent_counts(&snapshot);
        assert_eq!(counts.get("TASK-A").copied(), Some(2));
        assert_eq!(counts.get("TASK-B").copied(), Some(1));
        assert_eq!(counts.get("TASK-C").copied(), Some(0));
        assert_eq!(counts.get("TASK-D").copied(), Some(0));

        let ready = ready_parallel_tasks(
            &snapshot,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        );
        assert_eq!(
            ready.into_iter().map(|task| task.id).collect::<Vec<_>>(),
            vec!["TASK-A", "TASK-D"],
            "frontier task A must dispatch before independent leaf D"
        );
    }

    #[test]
    fn ready_parallel_tasks_keeps_flat_graph_in_plan_order() {
        // No dependency edges -> all counts 0 -> stable sort preserves order.
        let plan = r#"
- [ ] `TASK-1` first
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-2` second
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-3` third
  Dependencies: none
  Estimated scope: S
"#;
        let snapshot = parse_loop_plan(plan);
        let ready = ready_parallel_tasks(
            &snapshot,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        );
        assert_eq!(
            ready.into_iter().map(|task| task.id).collect::<Vec<_>>(),
            vec!["TASK-1", "TASK-2", "TASK-3"]
        );
    }

    #[test]
    fn ready_parallel_tasks_backlog_partial_never_preempts_pending() {
        // A high-fan-out partial P must still follow the plain pending Q: the
        // frontier reorder applies only within the pending group.
        let plan = r#"
- [~] `TASK-P` partial with many dependents
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-Q` plain pending
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-R` depends on P
  Dependencies: `TASK-P`
  Estimated scope: S
"#;
        let snapshot = parse_loop_plan(plan);
        let ready = ready_parallel_tasks(
            &snapshot,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        );
        // Q (pending) before P (partial), even though P has a dependent.
        let ids = ready.into_iter().map(|task| task.id).collect::<Vec<_>>();
        assert_eq!(ids.first().map(String::as_str), Some("TASK-Q"));
        assert!(
            ids.iter().position(|id| id == "TASK-Q")
                < ids.iter().position(|id| id == "TASK-P"),
            "pending must precede backlog partial: {ids:?}"
        );
    }

    #[test]
    fn prioritize_ready_parallel_tasks_avoids_canonical_dirty_paths() {
        let repo = unique_temp_dir("parallel-ready-priority");
        init_git_repo(&repo);
        fs::write(repo.join("src.txt"), "base\n").expect("failed to write src file");
        run_git_in(&repo, ["add", "src.txt"]);
        run_git_in(&repo, ["commit", "-m", "initial"]);
        fs::write(repo.join("src.txt"), "dirty\n").expect("failed to dirty src file");

        let ready = vec![
            LoopTask {
                id: "TASK-001".to_string(),
                title: "touches dirty file".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [ ] `TASK-001`\n  Owns: `src.txt`\n".to_string(),
            },
            LoopTask {
                id: "TASK-002".to_string(),
                title: "clean task".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [ ] `TASK-002`\n  Owns: `docs/proof.md`\n".to_string(),
            },
        ];

        let ordered = prioritize_ready_parallel_tasks(&repo, ready);
        assert_eq!(
            ordered.into_iter().map(|task| task.id).collect::<Vec<_>>(),
            vec!["TASK-002", "TASK-001"]
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn record_partial_follow_up_gives_one_retry_then_parks() {
        // Default AUTO_PARTIAL_FOLLOWUP_MAX is 2, so 2 retries then park on 3rd.
        // Set env var explicitly so this test is independent of caller env.
        std::env::set_var("AUTO_PARTIAL_FOLLOWUP_MAX", "1");
        let mut attempted = BTreeMap::new();
        let mut deferred = BTreeSet::new();

        assert_eq!(
            record_partial_follow_up("TASK-001", &mut attempted, &mut deferred),
            PartialFollowUpDisposition::RetryLaterThisRun
        );
        assert!(attempted.contains_key("TASK-001"));
        assert!(!deferred.contains("TASK-001"));

        assert_eq!(
            record_partial_follow_up("TASK-001", &mut attempted, &mut deferred),
            PartialFollowUpDisposition::ParkForRestOfRun
        );
        assert!(attempted.contains_key("TASK-001"));
        assert!(deferred.contains("TASK-001"));

        clear_partial_follow_up_tracking("TASK-001", &mut attempted, &mut deferred);
        assert!(!attempted.contains_key("TASK-001"));
        assert!(!deferred.contains("TASK-001"));
    }

    #[test]
    fn clean_commit_harvest_waits_for_post_commit_output_quiet_period() {
        assert!(!clean_commit_harvest_ready(
            CLEAN_COMMIT_GRACE + Duration::from_secs(1),
            Some(Duration::from_secs(0)),
        ));
        assert!(!clean_commit_harvest_ready(
            CLEAN_COMMIT_GRACE - Duration::from_secs(1),
            Some(CLEAN_COMMIT_QUIET_GRACE + Duration::from_secs(1)),
        ));
        assert!(clean_commit_harvest_ready(
            CLEAN_COMMIT_GRACE + Duration::from_secs(1),
            Some(CLEAN_COMMIT_QUIET_GRACE + Duration::from_secs(1)),
        ));
        assert!(clean_commit_harvest_ready(
            CLEAN_COMMIT_GRACE + Duration::from_secs(1),
            None,
        ));
    }
}
