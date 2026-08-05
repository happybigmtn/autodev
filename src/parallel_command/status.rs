use super::*;

use serde::Serialize;

#[derive(Serialize)]
struct LaneStatusRecord {
    lane: usize,
    task_id: String,
    running: bool,
    stale: bool,
    pid_state: String,
    last_log_age: String,
}

#[derive(Serialize)]
struct ParallelStatusReport {
    repo_root: String,
    branch: String,
    run_root: String,
    tmux_session: String,
    tmux_running: bool,
    host_pids: Vec<String>,
    lanes: Vec<LaneStatusRecord>,
    active_task_ids: Vec<String>,
    frontier: Option<String>,
    safety_verdict: Option<String>,
    health: String,
}

pub(crate) fn run_parallel_status(args: &ParallelArgs) -> Result<()> {
    let json = args.json;
    let repo_root = git_repo_root()?;
    let run_root = parallel_run_root(&repo_root, args);
    let session_name = parallel_tmux_session_name(&repo_root);
    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut lane_records: Vec<LaneStatusRecord> = Vec::new();
    if !json {
        println!("auto parallel status");
        println!("repo root:   {}", repo_root.display());
        println!("branch:      {}", current_branch);
        println!("run root:    {}", run_root.display());
    }
    let mut tmux_worker_running = false;
    let mut tmux_running = false;
    match tmux_session_exists(&session_name) {
        Ok(true) => {
            tmux_running = true;
            if !json {
                println!("tmux:        {session_name} running");
            }
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
                        if !json {
                            println!("  {line}");
                        }
                    }
                }
                Err(err) => {
                    if !json {
                        println!("  warning: failed to inspect tmux windows: {err:#}")
                    }
                }
            }
        }
        Ok(false) => {
            if !json {
                println!("tmux:        {session_name} not running");
            }
        }
        Err(err) => {
            if !json {
                println!("tmux:        unknown ({err:#})");
            }
        }
    }

    let host_processes = parallel_host_processes_for_repo(&repo_root);
    if !json {
        if host_processes.is_empty() {
            println!("host pids:   none detected");
        } else {
            println!("host pids:");
            for line in &host_processes {
                println!("  {line}");
            }
        }
    }
    let no_live_parallel_host = host_processes.is_empty() && !tmux_worker_running;

    let lanes_root = run_root.join("lanes");
    let mut lanes = if !lanes_root.exists() {
        if !json {
            println!("lanes:       none ({})", lanes_root.display());
        }
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

    if !json {
        println!("lanes:");
    }
    for (lane_index, lane_root) in lanes {
        let stored_task_id = read_lane_task_id(&lane_root)
            .ok()
            .flatten()
            .unwrap_or_else(|| "[unknown]".to_string());
        let stale = lane_is_from_previous_run(&run_root, &lane_root);
        let lane_repo_root = lane_root.join("repo");
        let (worker_running, pid_state) = lane_worker_status(&lane_root, &lane_repo_root)
            .unwrap_or_else(|err| {
                (
                    false,
                    format!("worker liveness check failed for lane repo: {err:#}"),
                )
            });
        // A lane from a previous run is not live work for THIS run: keep it out
        // of the active-task set and the health assessment so a dead run's
        // lanes never read as in-flight.
        if worker_running && !stale {
            active_task_ids.insert(stored_task_id.clone());
        }
        let (log_age, _) = latest_lane_log_line(&lane_root);
        lane_records.push(LaneStatusRecord {
            lane: lane_index,
            task_id: stored_task_id.clone(),
            running: worker_running && !stale,
            stale,
            pid_state: pid_state.clone(),
            last_log_age: log_age.clone(),
        });
        if stale {
            if !json {
                let (_, log_line) = latest_lane_log_line(&lane_root);
                println!(
                    "  lane-{lane_index}: {stored_task_id} | stale (previous run) | last log {log_age}"
                );
                if let Some(line) = log_line {
                    println!("    {line}");
                }
            }
            continue;
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
        if !json {
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
    }
    let mut report_frontier: Option<String> = None;
    let mut report_verdict: Option<String> = None;
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
            if !json {
                println!("frontier:    {frontier}");
            }
            report_frontier = Some(frontier);
        }
        let verdict = parallel_status_safety_verdict(
            &plan,
            &active_task_ids,
            &shelved,
            &deferred,
            no_live_parallel_host,
            &active_recovery_lanes,
            &stale_recovery_lanes,
        );
        if !json {
            println!("safety verdict: {verdict}");
        }
        report_verdict = Some(verdict);
    }
    let health = render_parallel_health_summary(
        &preflight_warnings,
        &recent_host_warnings,
        receipt_drift.as_deref(),
        &active_recovery_lanes,
        &stale_recovery_lanes,
    );
    if !json {
        println!("health:      {health}");
    } else {
        let report = ParallelStatusReport {
            repo_root: repo_root.display().to_string(),
            branch: current_branch,
            run_root: run_root.display().to_string(),
            tmux_session: session_name,
            tmux_running,
            host_pids: host_processes,
            lanes: lane_records,
            active_task_ids: active_task_ids.into_iter().collect(),
            frontier: report_frontier,
            safety_verdict: report_verdict,
            health,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|err| format!(
                "{{\"error\":\"failed to serialize status: {err}\"}}"
            ))
        );
    }
    Ok(())
}

pub(crate) fn parallel_status_safety_verdict(
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

pub(crate) fn parallel_host_processes_for_repo(repo_root: &Path) -> Vec<String> {
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

pub(crate) fn tmux_status_line_has_live_worker(line: &str) -> bool {
    if !line.contains(":dead=0:") {
        return false;
    }
    let command = line
        .rsplit_once(":cmd=")
        .map(|(_, command)| command.trim())
        .unwrap_or_default();
    !matches!(command, "" | "bash" | "sh" | "zsh" | "fish")
}

pub(crate) fn process_line_cwd_matches_repo(line: &str, repo_root: &Path) -> bool {
    let Some(pid) = line
        .split_whitespace()
        .next()
        .and_then(|raw| raw.parse::<u32>().ok())
    else {
        return true;
    };
    fs::read_link(format!("/proc/{pid}/cwd")).map_or(true, |cwd| cwd == repo_root)
}

pub(crate) fn lane_repo_status_summary(repo_root: &Path) -> String {
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

pub(crate) fn preflight_warning_names(run_root: &Path) -> Vec<String> {
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

pub(crate) fn recent_parallel_host_warnings(run_root: &Path, max_lines: usize) -> Vec<String> {
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

pub(crate) fn render_parallel_health_summary(
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

pub(crate) fn receipt_drift_status_summary(repo_root: &Path) -> Option<String> {
    let path = repo_root.join("RECEIPTS-DRIFT.md");
    let content = fs::read_to_string(&path).ok()?;
    // The report describes a particular canonical source state. A normal
    // closeout may commit the generated report immediately after that source
    // state, so accept HEAD or its direct parent. Anything older is historical
    // evidence, not current pipeline degradation; the next exhaustive sweep
    // will regenerate it if the drift still exists.
    if !receipt_drift_report_is_fresh(repo_root, &content) {
        return None;
    }
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

const RECEIPT_DRIFT_SOURCE_HEAD_MARKER: &str = "<!-- auto-receipts-drift-source-head: ";

fn receipt_drift_report_source_head(content: &str) -> Option<&str> {
    let tail = content.split_once(RECEIPT_DRIFT_SOURCE_HEAD_MARKER)?.1;
    let source = tail.split_once(" -->")?.0.trim();
    (!source.is_empty() && source.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(source)
}

fn receipt_drift_report_is_fresh(repo_root: &Path, content: &str) -> bool {
    let Some(source_head) = receipt_drift_report_source_head(content) else {
        return false;
    };
    let current_head = git_stdout(repo_root, ["rev-parse", "HEAD"])
        .unwrap_or_default()
        .trim()
        .to_string();
    if current_head == source_head {
        return true;
    }
    git_stdout(repo_root, ["rev-parse", "HEAD^"])
        .unwrap_or_default()
        .trim()
        == source_head
}

pub(crate) fn latest_lane_log_line(lane_root: &Path) -> (String, Option<String>) {
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
pub(crate) struct LastParallelStopState {
    pub(crate) shelved: BTreeSet<String>,
    pub(crate) deferred: BTreeSet<String>,
}

pub(crate) fn last_parallel_stop_state(run_root: &Path) -> Option<LastParallelStopState> {
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

pub(crate) fn parse_parallel_stop_ids(line: &str, label: &str) -> BTreeSet<String> {
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

pub(crate) fn read_last_nonempty_line(path: &Path) -> Result<Option<String>> {
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

pub(crate) fn format_system_time_age(time: SystemTime) -> String {
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

#[cfg(test)]
mod tests {
    use crate::parallel_command::*;
    use std::time::UNIX_EPOCH;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    #[test]
    fn tmux_status_worker_detection_ignores_parked_shells() {
        assert!(tmux_status_line_has_live_worker("0:host:dead=0:cmd=auto"));
        assert!(tmux_status_line_has_live_worker(
            "1:lane-1:dead=0:cmd=codex"
        ));
        assert!(!tmux_status_line_has_live_worker("0:host:dead=0:cmd=bash"));
        assert!(!tmux_status_line_has_live_worker(
            "1:lane-1:dead=1:cmd=auto"
        ));
    }

    #[test]
    fn parse_parallel_stop_ids_extracts_fields() {
        let line = "no dependency-ready tasks remain to dispatch; stopping with unresolved shelved tasks. pending: A, B blocked: none shelved: C, D deferred: E frontier: C [shelved] -> A, B";
        assert_eq!(
            parse_parallel_stop_ids(line, "shelved:"),
            BTreeSet::from(["C".to_string(), "D".to_string()])
        );
        assert_eq!(
            parse_parallel_stop_ids(line, "deferred:"),
            BTreeSet::from(["E".to_string()])
        );
    }

    #[test]
    fn last_parallel_stop_state_reads_latest_stop_line() {
        let run_root = unique_temp_dir("parallel-stop-state");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        fs::write(
            run_root.join("live.log"),
            "idle: something\nno dependency-ready tasks remain to dispatch; stopping with unresolved shelved tasks. pending: A blocked: none shelved: C, D deferred: E frontier: C [shelved] -> A\n",
        )
        .expect("failed to write live log");
        let state = last_parallel_stop_state(&run_root).expect("expected stop state");
        assert_eq!(
            state.shelved,
            BTreeSet::from(["C".to_string(), "D".to_string()])
        );
        assert_eq!(state.deferred, BTreeSet::from(["E".to_string()]));
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn parallel_health_summary_reports_preflight_host_and_recovery_issues() {
        let run_root = unique_temp_dir("parallel-health-summary");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        fs::write(
            run_root.join("preflight.txt"),
            "- ok cargo: Rust workspace detected\n- warn agent-browser: missing\n- warn docker compose: missing\n",
        )
        .expect("failed to write preflight");
        fs::write(
            run_root.join("live.log"),
            "warning: failed syncing host-owned queue state\nwarning: failed syncing host-owned queue state\nwarning: lane-1 something else\n",
        )
        .expect("failed to write live log");

        let preflight = preflight_warning_names(&run_root);
        let host_warnings = recent_parallel_host_warnings(&run_root, 50);
        let summary = render_parallel_health_summary(
            &preflight,
            &host_warnings,
            Some("2 completed task(s); see RECEIPTS-DRIFT.md"),
            &["lane-1 TASK-1".to_string(), "lane-3 TASK-3".to_string()],
            &["lane-2 TASK-2".to_string()],
        );
        assert_eq!(
            preflight,
            vec!["agent-browser".to_string(), "docker compose".to_string()]
        );
        assert_eq!(
            host_warnings.len(),
            2,
            "host warnings should be de-duplicated with source freshness"
        );
        assert!(host_warnings[0].contains("live.log"));
        assert!(host_warnings[0].contains("ago"));
        assert!(host_warnings[0].contains("warning: failed syncing host-owned queue state"));
        assert!(host_warnings[1].contains("warning: lane-1 something else"));
        assert!(summary.contains("degraded"));
        assert!(summary.contains("preflight warnings: agent-browser, docker compose"));
        assert!(summary.contains("recent host warnings: live.log"));
        assert!(summary.contains("receipt drift: 2 completed task(s); see RECEIPTS-DRIFT.md"));
        assert!(summary.contains("active recovery lanes: lane-1 TASK-1, lane-3 TASK-3"));
        assert!(summary.contains("stale recovery lanes: lane-2 TASK-2"));

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn receipt_drift_status_ignores_reports_older_than_the_closeout_commit() {
        let repo = unique_temp_dir("parallel-status-stale-drift");
        fs::create_dir_all(&repo).expect("create repo");
        run_git(&repo, ["init", "-q", "-b", "main"]).expect("init git repo");
        run_git(&repo, ["config", "user.name", "autodev tests"]).expect("set git name");
        run_git(&repo, ["config", "user.email", "autodev@example.com"]).expect("set git email");
        fs::write(repo.join("source.txt"), "one\n").expect("write source");
        run_git(&repo, ["add", "source.txt"]).expect("stage source");
        run_git(&repo, ["commit", "-q", "-m", "source"]).expect("commit source");
        let source_head = git_stdout(&repo, ["rev-parse", "HEAD"])
            .expect("read source head")
            .trim()
            .to_string();
        let report = format!(
            "# Receipt Drift Triage\n\n<!-- auto-receipts-drift-source-head: {source_head} -->\n\n## Completed Tasks With Drift\n\n- [x] `TASK-1` stale\n"
        );
        fs::write(repo.join("RECEIPTS-DRIFT.md"), &report).expect("write report");

        assert!(super::receipt_drift_status_summary(&repo).is_some());
        run_git(&repo, ["add", "RECEIPTS-DRIFT.md"]).expect("stage report");
        run_git(&repo, ["commit", "-q", "-m", "receipt drift closeout"]).expect("commit report");
        assert!(
            super::receipt_drift_status_summary(&repo).is_some(),
            "the direct closeout child of the recorded source remains fresh"
        );

        fs::write(repo.join("source.txt"), "two\n").expect("change source");
        run_git(&repo, ["add", "source.txt"]).expect("stage changed source");
        run_git(&repo, ["commit", "-q", "-m", "new source"]).expect("commit changed source");
        assert_eq!(
            super::receipt_drift_status_summary(&repo),
            None,
            "a historical report must not degrade current pipeline health"
        );

        fs::remove_dir_all(&repo).expect("remove repo");
    }

    #[test]
    fn parallel_status_prints_launch_resume_land_safety_verdict() {
        let plan = parse_loop_plan(
            "# IMPLEMENTATION_PLAN\n\n- [ ] `TASK-1` Ready\nDependencies: none\n\n- [ ] `TASK-2` Blocked\nDependencies: `TASK-1`\n",
        );

        let go = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            &[],
            &[],
        );
        assert!(go.starts_with("GO:"), "{go}");
        assert!(go.contains("TASK-1"), "{go}");

        let recover = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            &[],
            &["lane-2 TASK-2".to_string()],
        );
        assert!(recover.starts_with("RECOVER:"), "{recover}");

        let stop = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            &["lane-1 TASK-1".to_string()],
            &[],
        );
        assert!(stop.starts_with("STOP:"), "{stop}");
    }
}
