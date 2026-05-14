fn run_parallel_status(args: &ParallelArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    let run_root = args
        .run_root
        .clone()
        .unwrap_or_else(|| repo_root.join(".auto").join("parallel"));
    let session_name = parallel_tmux_session_name(&repo_root);
    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])
        .unwrap_or_default()
        .trim()
        .to_string();
    println!("auto parallel status");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", current_branch);
    println!("run root:    {}", run_root.display());
    let mut tmux_worker_running = false;
    match tmux_session_exists(&session_name) {
        Ok(true) => {
            println!("tmux:        {session_name} running");
            match tmux_stdout([
                "list-windows",
                "-t",
                &session_name,
                "-F",
                "#{window_index}:#{window_name}:dead=#{pane_dead}:cmd=#{pane_current_command}",
            ]) {
                Ok(windows) => {
                    for line in windows.lines().filter(|line| !line.trim().is_empty()) {
                        tmux_worker_running |= tmux_status_line_has_live_worker(line);
                        println!("  {line}");
                    }
                }
                Err(err) => println!("  warning: failed to inspect tmux windows: {err:#}"),
            }
        }
        Ok(false) => {
            println!("tmux:        {session_name} not running");
        }
        Err(err) => {
            println!("tmux:        unknown ({err:#})");
        }
    }

    let host_processes = parallel_host_processes_for_repo(&repo_root);
    if host_processes.is_empty() {
        println!("host pids:   none detected");
    } else {
        println!("host pids:");
        for line in &host_processes {
            println!("  {line}");
        }
    }
    let no_live_parallel_host = host_processes.is_empty() && !tmux_worker_running;

    let lanes_root = run_root.join("lanes");
    let mut lanes = if !lanes_root.exists() {
        println!("lanes:       none ({})", lanes_root.display());
        Vec::new()
    } else {
        fs::read_dir(&lanes_root)
            .with_context(|| format!("failed to read {}", lanes_root.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                parse_lane_index(&name).map(|index| (index, entry.path()))
            })
            .collect::<Vec<_>>()
    };
    lanes.sort_by_key(|(index, _)| *index);

    let preflight_warnings = preflight_warning_names(&run_root);
    let recent_host_warnings = recent_parallel_host_warnings(&run_root, 200);
    let receipt_drift = receipt_drift_status_summary(&repo_root);
    let stop_state = last_parallel_stop_state(&run_root);
    let mut active_recovery_lanes = Vec::new();
    let mut stale_recovery_lanes = Vec::new();
    let mut active_task_ids = BTreeSet::new();

    println!("lanes:");
    for (lane_index, lane_root) in lanes {
        let stored_task_id = read_lane_task_id(&lane_root)
            .ok()
            .flatten()
            .unwrap_or_else(|| "[unknown]".to_string());
        let lane_repo_root = lane_root.join("repo");
        let (worker_running, pid_state) = lane_worker_status(&lane_root, &lane_repo_root)
            .unwrap_or_else(|err| {
                (
                    false,
                    format!("worker liveness check failed for lane repo: {err:#}"),
                )
            });
        if worker_running {
            active_task_ids.insert(stored_task_id.clone());
        }
        let superseded_recovery =
            superseded_lane_cherry_pick_recovery(&repo_root, &lane_repo_root, &stored_task_id)
                .ok()
                .flatten();
        let recovery_active = superseded_recovery.is_none()
            && (lane_repo_has_active_cherry_pick(&lane_repo_root)
                || lane_repo_has_rebase_recovery(&lane_repo_root));
        if recovery_active {
            if no_live_parallel_host && !worker_running {
                stale_recovery_lanes.push(format!("lane-{lane_index} {stored_task_id}"));
            } else {
                active_recovery_lanes.push(format!("lane-{lane_index} {stored_task_id}"));
            }
        }
        let repo_status = lane_repo_status_summary(&lane_repo_root);
        let (log_age, log_line) = latest_lane_log_line(&lane_root);
        let task_id = lane_status_task_id(&stored_task_id, worker_running, log_line.as_deref());
        println!(
            "  lane-{lane_index}: {task_id} | {pid_state} | {repo_status} | last log {log_age}"
        );
        if let Some(reason) = superseded_recovery {
            println!(
                "    recovery: superseded duplicate ({}); next host resume can retire it",
                reason.summary()
            );
        }
        if recovery_active && no_live_parallel_host && !worker_running {
            println!(
                "    recovery: stale recovery (no host pid or tmux session); not active progress"
            );
            println!("    recovery artifact: {}", lane_repo_root.display());
            println!(
                "    reset command: rm -rf {} # after preserving task-owned work",
                shell_quote(&lane_root.display().to_string())
            );
        }
        if let Some(line) = log_line {
            println!("    {line}");
        }
    }
    if let Ok(plan) = inspect_loop_plan(&repo_root) {
        let shelved = stop_state
            .as_ref()
            .map(|state| {
                state
                    .shelved
                    .iter()
                    .map(|task_id| {
                        let markdown = plan
                            .task(task_id)
                            .map(|task| task.markdown.clone())
                            .unwrap_or_default();
                        (task_id.clone(), markdown)
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let deferred = stop_state
            .as_ref()
            .map(|state| state.deferred.clone())
            .unwrap_or_default();
        if let Some(frontier) =
            format_parallel_blocker_frontier(&plan, &active_task_ids, &shelved, &deferred, 8)
        {
            println!("frontier:    {frontier}");
        }
        println!(
            "safety verdict: {}",
            parallel_status_safety_verdict(
                &plan,
                &active_task_ids,
                &shelved,
                &deferred,
                no_live_parallel_host,
                &active_recovery_lanes,
                &stale_recovery_lanes,
            )
        );
    }
    println!(
        "health:      {}",
        render_parallel_health_summary(
            &preflight_warnings,
            &recent_host_warnings,
            receipt_drift.as_deref(),
            &active_recovery_lanes,
            &stale_recovery_lanes,
        )
    );
    Ok(())
}

fn parallel_status_safety_verdict(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
    no_live_parallel_host: bool,
    active_recovery_lanes: &[String],
    stale_recovery_lanes: &[String],
) -> String {
    if !active_recovery_lanes.is_empty() {
        return "STOP: active lane recovery is in progress; do not launch another host".to_string();
    }
    if no_live_parallel_host && !stale_recovery_lanes.is_empty() {
        return "RECOVER: stale lane recovery exists; preserve task work, clear stale lane metadata, then resume".to_string();
    }
    if !active_tasks.is_empty() {
        return format!(
            "MONITOR: live lane work in progress for {}",
            active_tasks.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    let ready = ready_parallel_tasks(plan, active_tasks, shelved_tasks, deferred_partial_tasks);
    let operator_ready = ready
        .iter()
        .filter(|task| is_operator_task(task))
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let code_ready = ready
        .iter()
        .filter(|task| !is_operator_task(task) && !is_evidence_lane_task(task))
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let evidence_ready = ready
        .iter()
        .filter(|task| !is_operator_task(task) && is_evidence_lane_task(task))
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    if ready.is_empty() {
        "NO-GO: no dependency-ready tasks remain for this run".to_string()
    } else if code_ready.is_empty() && !operator_ready.is_empty() {
        format!(
            "OPERATOR: no code lanes ready; operator queue: {}{}",
            operator_ready.join(", "),
            if evidence_ready.is_empty() {
                String::new()
            } else {
                format!("; evidence queue: {}", evidence_ready.join(", "))
            }
        )
    } else if code_ready.is_empty() && !evidence_ready.is_empty() {
        format!(
            "EVIDENCE: no code lanes ready; evidence queue: {}",
            evidence_ready.join(", ")
        )
    } else {
        format!(
            "GO: safe to launch or resume; code lanes ready: {}{}{}",
            code_ready.join(", "),
            if evidence_ready.is_empty() {
                String::new()
            } else {
                format!("; evidence queue: {}", evidence_ready.join(", "))
            },
            if operator_ready.is_empty() {
                String::new()
            } else {
                format!("; operator queue: {}", operator_ready.join(", "))
            }
        )
    }
}

pub(crate) async fn run_parallel_inline(args: ParallelArgs) -> Result<()> {
    let previous = env::var_os("AUTO_PARALLEL_TMUX_BOOTSTRAPPED");
    env::set_var("AUTO_PARALLEL_TMUX_BOOTSTRAPPED", "1");
    let result = run_parallel(args).await;
    match previous {
        Some(value) => env::set_var("AUTO_PARALLEL_TMUX_BOOTSTRAPPED", value),
        None => env::remove_var("AUTO_PARALLEL_TMUX_BOOTSTRAPPED"),
    }
    result
}

fn parallel_host_processes_for_repo(repo_root: &Path) -> Vec<String> {
    let current_pid = std::process::id();
    command_stdout(Path::new("."), ["pgrep", "-af", "auto parallel"])
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            line.split_whitespace()
                .next()
                .and_then(|raw| raw.parse::<u32>().ok())
                != Some(current_pid)
        })
        .filter(|line| {
            line.split_once(' ')
                .map(|(_, command)| {
                    let command = command.trim_start();
                    command.starts_with("auto parallel ")
                        || command.split_once(' ').is_some_and(|(program, rest)| {
                            program.ends_with("/auto") && rest.trim_start().starts_with("parallel ")
                        })
                })
                .unwrap_or(false)
        })
        .filter(|line| !line.contains(" parallel status"))
        .filter(|line| process_line_cwd_matches_repo(line, repo_root))
        .map(str::to_string)
        .collect()
}

fn tmux_status_line_has_live_worker(line: &str) -> bool {
    if !line.contains(":dead=0:") {
        return false;
    }
    let command = line
        .rsplit_once(":cmd=")
        .map(|(_, command)| command.trim())
        .unwrap_or_default();
    !matches!(command, "" | "bash" | "sh" | "zsh" | "fish")
}

fn process_line_cwd_matches_repo(line: &str, repo_root: &Path) -> bool {
    let Some(pid) = line
        .split_whitespace()
        .next()
        .and_then(|raw| raw.parse::<u32>().ok())
    else {
        return true;
    };
    fs::read_link(format!("/proc/{pid}/cwd")).map_or(true, |cwd| cwd == repo_root)
}

fn lane_repo_status_summary(repo_root: &Path) -> String {
    if !repo_root.join(".git").exists() {
        return "no repo".to_string();
    }
    let branch = git_stdout(repo_root, ["status", "--short", "--branch"]).unwrap_or_default();
    let mut lines = branch.lines();
    let head = lines.next().unwrap_or("## unknown").trim();
    let dirty_count = lines.count();
    let recovery_clause = if let Some(issue) = lane_repo_rebase_recovery_issue(repo_root) {
        format!("; {issue}")
    } else if lane_repo_has_active_cherry_pick(repo_root) {
        "; cherry-pick recovery".to_string()
    } else {
        String::new()
    };
    if dirty_count == 0 {
        format!("{head}{recovery_clause}; clean")
    } else {
        format!("{head}{recovery_clause}; {dirty_count} dirty path(s)")
    }
}

fn preflight_warning_names(run_root: &Path) -> Vec<String> {
    let path = run_root.join("preflight.txt");
    let Ok(content) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for line in content.lines() {
        let Some(rest) = line.trim().strip_prefix("- warn ") else {
            continue;
        };
        let name = rest.split(':').next().unwrap_or(rest).trim();
        if !name.is_empty() && !warnings.iter().any(|existing| existing == name) {
            warnings.push(name.to_string());
        }
    }
    warnings
}

fn recent_parallel_host_warnings(run_root: &Path, max_lines: usize) -> Vec<String> {
    let log_path = run_root.join("live.log");
    let Ok(log_text) = read_recent_log_text(&log_path, max_lines) else {
        return Vec::new();
    };
    let source_age = log_path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .map(format_system_time_age)
        .unwrap_or_else(|_| "unknown age".to_string());
    let mut warnings = Vec::new();
    for line in log_text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("warning:") {
            continue;
        }
        let warning = format!("live.log {source_age}: {trimmed}");
        if !warnings.iter().any(|existing| existing == &warning) {
            warnings.push(warning);
        }
    }
    warnings
}

fn render_parallel_health_summary(
    preflight_warnings: &[String],
    recent_host_warnings: &[String],
    receipt_drift: Option<&str>,
    active_recovery_lanes: &[String],
    stale_recovery_lanes: &[String],
) -> String {
    let mut issues = Vec::new();
    if !preflight_warnings.is_empty() {
        issues.push(format!(
            "preflight warnings: {}",
            preflight_warnings.join(", ")
        ));
    }
    if !recent_host_warnings.is_empty() {
        issues.push(format!(
            "recent host warnings: {}",
            recent_host_warnings.join(" | ")
        ));
    }
    if let Some(receipt_drift) = receipt_drift.filter(|summary| !summary.trim().is_empty()) {
        issues.push(format!("receipt drift: {receipt_drift}"));
    }
    if !active_recovery_lanes.is_empty() {
        issues.push(format!(
            "active recovery lanes: {}",
            active_recovery_lanes.join(", ")
        ));
    }
    if !stale_recovery_lanes.is_empty() {
        issues.push(format!(
            "stale recovery lanes: {}",
            stale_recovery_lanes.join(", ")
        ));
    }
    if issues.is_empty() {
        "healthy".to_string()
    } else {
        format!("degraded ({})", issues.join("; "))
    }
}

fn receipt_drift_status_summary(repo_root: &Path) -> Option<String> {
    let path = repo_root.join("RECEIPTS-DRIFT.md");
    let content = fs::read_to_string(&path).ok()?;
    if content.contains("No repo-local receipt drift detected.") {
        return None;
    }
    let mut completed = 0usize;
    let mut candidates = 0usize;
    let mut section = None::<&str>;
    for line in content.lines() {
        match line.trim() {
            "## Completed Tasks With Drift" => {
                section = Some("completed");
                continue;
            }
            "## Manual Closeout Candidates" => {
                section = Some("candidates");
                continue;
            }
            _ => {}
        }
        let trimmed = line.trim_start();
        if !trimmed.starts_with("- [") || trimmed == "- None" {
            continue;
        }
        match section {
            Some("completed") => completed += 1,
            Some("candidates") => candidates += 1,
            _ => {}
        }
    }
    if completed == 0 && candidates == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if completed > 0 {
        parts.push(format!("{completed} completed task(s)"));
    }
    if candidates > 0 {
        parts.push(format!("{candidates} manual closeout candidate(s)"));
    }
    Some(format!("{}; see RECEIPTS-DRIFT.md", parts.join(", ")))
}

fn latest_lane_log_line(lane_root: &Path) -> (String, Option<String>) {
    let candidates = [lane_root.join("stdout.log"), lane_root.join("stderr.log")];
    let latest = candidates
        .iter()
        .filter_map(|path| {
            let modified = path
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()?;
            let line = read_last_nonempty_line(path).ok().flatten()?;
            Some((modified, line))
        })
        .max_by_key(|(modified, _)| *modified);
    let Some((modified, line)) = latest else {
        return ("never".to_string(), None);
    };
    (format_system_time_age(modified), Some(line))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LastParallelStopState {
    shelved: BTreeSet<String>,
    deferred: BTreeSet<String>,
}

fn last_parallel_stop_state(run_root: &Path) -> Option<LastParallelStopState> {
    let log_text = read_recent_log_text(&run_root.join("live.log"), 400).ok()?;
    let stop_line = log_text
        .lines()
        .rev()
        .find(|line| line.contains("no dependency-ready tasks remain to dispatch"))?;
    Some(LastParallelStopState {
        shelved: parse_parallel_stop_ids(stop_line, "shelved:"),
        deferred: parse_parallel_stop_ids(stop_line, "deferred:"),
    })
}

fn parse_parallel_stop_ids(line: &str, label: &str) -> BTreeSet<String> {
    const LABELS: [&str; 5] = ["pending:", "blocked:", "shelved:", "deferred:", "frontier:"];
    let Some(start) = line.find(label).map(|index| index + label.len()) else {
        return BTreeSet::new();
    };
    let tail = &line[start..];
    let end = LABELS
        .into_iter()
        .filter(|candidate| *candidate != label)
        .filter_map(|candidate| tail.find(candidate))
        .min()
        .unwrap_or(tail.len());
    tail[..end]
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty() && *item != "none")
        .map(str::to_string)
        .collect()
}

fn read_last_nonempty_line(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(content
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string))
}

fn format_system_time_age(time: SystemTime) -> String {
    let elapsed = SystemTime::now()
        .duration_since(time)
        .unwrap_or_else(|_| Duration::from_secs(0));
    if elapsed.as_secs() < 60 {
        format!("{}s ago", elapsed.as_secs())
    } else if elapsed.as_secs() < 3600 {
        format!("{}m ago", elapsed.as_secs() / 60)
    } else {
        format!("{}h ago", elapsed.as_secs() / 3600)
    }
}
