fn no_dependency_ready_stop_message(
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

fn autonomous_unblock_attempt_limit(max_retries: usize) -> usize {
    MIN_AUTONOMOUS_UNBLOCK_ATTEMPTS.max(max_retries.saturating_add(2))
}

fn exhausted_autonomous_unblock_suffix(
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

fn write_operator_actions_for_ready_tasks(run_root: &Path, tasks: &[LoopTask]) -> Result<PathBuf> {
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

fn clear_stale_operator_actions(run_root: &Path, parallel_logger: &ParallelEventLogger) {
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

fn write_parallel_salvage_record(
    assignment: &ActiveLaneAssignment,
    landing_error: &str,
) -> Result<()> {
    let lane_head = git_stdout(&assignment.lane_repo_root, ["rev-parse", "HEAD"])
        .unwrap_or_else(|_| "unknown".to_string())
        .trim()
        .to_string();
    let lane_status = git_stdout(
        &assignment.lane_repo_root,
        ["status", "--short", "--branch"],
    )
    .unwrap_or_else(|_| "unknown".to_string());
    let run_root = assignment
        .lane_root
        .parent()
        .and_then(Path::parent)
        .context("failed to infer parallel run root from lane path")?;
    let salvage_root = run_root.join(SALVAGE_DIR);
    fs::create_dir_all(&salvage_root)
        .with_context(|| format!("failed to create {}", salvage_root.display()))?;
    let filename = format!(
        "lane-{}-{}.md",
        assignment.lane_index,
        sanitize_salvage_filename(&assignment.task.id)
    );
    let path = salvage_root.join(filename);
    let content = format!(
        "# auto parallel salvage\n\n\
Task: `{}` {}\n\
Lane: lane-{}\n\
Attempts: {}\n\
Lane repo: `{}`\n\
Lane head: `{}`\n\n\
## Lane Status\n\n```text\n{}\n```\n\n\
## Landing Error\n\n```text\n{}\n```\n\n\
## Recovery\n\n\
The lane has clean committed work that the host could not land automatically. Reconcile it semantically onto the current target branch, verify it, then remove this salvage note when the task lands.\n",
        assignment.task.id,
        assignment.task.title,
        assignment.lane_index,
        assignment.attempts,
        assignment.lane_repo_root.display(),
        lane_head,
        lane_status.trim(),
        landing_error.trim()
    );
    atomic_write(&path, content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &format!("salvage: wrote {}", path.display()),
    );
    Ok(())
}

fn parallel_salvage_record_path(lane_root: &Path, task_id: &str, lane_index: usize) -> PathBuf {
    let run_root = lane_root
        .parent()
        .and_then(Path::parent)
        .unwrap_or(lane_root);
    run_root.join(SALVAGE_DIR).join(format!(
        "lane-{}-{}.md",
        lane_index,
        sanitize_salvage_filename(task_id)
    ))
}

fn salvage_recovery_note(
    lane_root: &Path,
    lane_index: usize,
    task_id: &str,
    target_branch: &str,
) -> Option<String> {
    let path = parallel_salvage_record_path(lane_root, task_id, lane_index);
    let content = fs::read_to_string(&path).ok()?;
    let landing_error = task_field_body(&content, "## Landing Error", "## Recovery")
        .map(|body| {
            body.lines()
                .filter(|line| !line.trim_start().starts_with("```"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "previous host landing failure recorded in salvage note".to_string());
    Some(landing_recovery_note(target_branch, landing_error.trim()))
}

fn sanitize_salvage_filename(raw: &str) -> String {
    let rendered = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let rendered = rendered.trim_matches('-');
    if rendered.is_empty() {
        "task".to_string()
    } else {
        rendered.to_string()
    }
}

fn detect_lane_environment_blocker(assignment: &ActiveLaneAssignment) -> Option<String> {
    let combined = [
        read_recent_log_text(&assignment.stdout_log_path, 200).ok(),
        read_recent_log_text(&assignment.stderr_log_path, 200).ok(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    environment_blocker_reason(&combined)
}

fn environment_blocker_reason(log_text: &str) -> Option<String> {
    for line in log_text.lines().rev() {
        if let Some(reason) = line
            .split_once("AUTO_ENV_BLOCKER:")
            .map(|(_, reason)| reason)
        {
            let reason = reason.trim();
            if !reason.is_empty() {
                return Some(reason.to_string());
            }
        }
    }

    let lower = log_text.to_ascii_lowercase();
    let patterns = [
        (
            "agent-browser daemon failed to start",
            "daemon failed to start",
        ),
        (
            "agent-browser daemon socket missing",
            "agent-browser/default.sock",
        ),
        (
            "Docker daemon unavailable",
            "cannot connect to the docker daemon",
        ),
        ("Docker compose stack is not running", "docker compose ps"),
        ("local service refused a connection", "connection refused"),
        ("local service refused a connection", "econnrefused"),
        ("regtest stack is unavailable", "regtest stack"),
        ("regtest RPC is unavailable", "127.0.0.1:18443"),
        (
            "Playwright browser dependencies are missing",
            "playwright install",
        ),
        ("browser executable is missing", "executable doesn't exist"),
    ];
    patterns
        .iter()
        .find_map(|(reason, pattern)| lower.contains(pattern).then(|| (*reason).to_string()))
}

fn read_recent_log_text(path: &Path, max_lines: usize) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut lines = content.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    Ok(lines.join("\n"))
}

