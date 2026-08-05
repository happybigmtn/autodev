use super::*;

use serde::{Deserialize, Serialize};

const CANONICAL_GATE_MARKER_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CanonicalGateStatus {
    phase: String,
    scope: String,
    task_id: String,
    gate_label: Option<String>,
    reviewed_head: String,
}

#[derive(Deserialize)]
struct CanonicalGateMarker {
    version: u32,
    phase: String,
    scope: String,
    task_id: String,
    #[serde(default)]
    gate_label: Option<String>,
    reviewed_head: String,
}

#[derive(Deserialize)]
struct LaneHostPendingStatusMarker {
    version: u32,
    phase: String,
    run_id: String,
    task_id: String,
    lane: usize,
    attempt: usize,
}

#[derive(Serialize)]
struct LaneStatusRecord {
    lane: usize,
    task_id: String,
    idle: bool,
    running: bool,
    stale: bool,
    pid_state: String,
    last_log_age: String,
}

#[derive(Serialize)]
struct ParallelStatusReport {
    status_binary: BinaryProvenance,
    host_binary: Option<BinaryProvenance>,
    revision_match: Option<bool>,
    repo_root: String,
    branch: String,
    run_root: String,
    tmux_session: String,
    tmux_running: bool,
    host_pids: Vec<String>,
    lanes: Vec<LaneStatusRecord>,
    canonical_gate: Option<CanonicalGateStatus>,
    staged_task_ids: Vec<String>,
    host_pending_task_ids: Vec<String>,
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
    let status_binary = current_binary_provenance();
    if !json {
        println!("auto parallel status");
        println!("repo root:   {}", repo_root.display());
        println!("branch:      {}", current_branch);
        println!("run root:    {}", run_root.display());
        println!(
            "status binary: {} @ {} ({}, {})",
            status_binary.version, status_binary.commit, status_binary.dirty, status_binary.profile
        );
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
    let (host_binary, revision_match) =
        parallel_status_binary_provenance(&run_root, &host_processes);
    if !json {
        match (&host_binary, revision_match) {
            (Some(host), Some(true)) => println!(
                "host binary:   {} @ {} (matches status binary)",
                host.version, host.commit
            ),
            (Some(host), Some(false)) => println!(
                "host binary:   {} @ {} (REVISION MISMATCH)",
                host.version, host.commit
            ),
            _ => println!("host binary:   unknown (older or unreadable host metadata)"),
        }
    }

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
    let canonical_gate = current_canonical_gate_status(&repo_root);
    let mut active_recovery_lanes = Vec::new();
    let mut stale_recovery_lanes = Vec::new();
    let mut host_pending_task_ids = BTreeSet::new();
    let mut active_task_ids = BTreeSet::new();
    let staged_task_ids = canonical_gate
        .iter()
        .map(|gate| gate.task_id.clone())
        .collect::<Vec<_>>();
    active_task_ids.extend(staged_task_ids.iter().cloned());

    if !json {
        if let Some(gate) = &canonical_gate {
            println!(
                "canonical gate: {} | {} | {}",
                gate.task_id,
                gate.gate_label.as_deref().unwrap_or("unspecified"),
                gate.phase
            );
        }
    }

    if !json {
        println!("lanes:");
    }
    for (lane_index, lane_root) in lanes {
        let stored_task_id = read_lane_task_id(&lane_root)
            .ok()
            .flatten()
            .unwrap_or_else(|| "[unknown]".to_string());
        // Assigned lanes carry a run id. Unassigned lanes instead carry a
        // host heartbeat, while an untouched empty placeholder can exist in
        // the narrow startup window before that first heartbeat. Anything
        // ambiguous fails closed as stale rather than masquerading as idle.
        let idle = lane_is_current_idle_capacity(&run_root, &lane_root, !no_live_parallel_host);
        let stale = lane_is_from_previous_run(&run_root, &lane_root) && !idle;
        let lane_repo_root = lane_root.join("repo");
        let (worker_running, pid_state) = lane_worker_status(&lane_root, &lane_repo_root)
            .unwrap_or_else(|err| {
                (
                    false,
                    format!("worker liveness check failed for lane repo: {err:#}"),
                )
            });
        if !stale {
            if let Some(task_id) = current_lane_host_pending_task_id(
                &run_root,
                lane_index,
                &lane_root,
                &stored_task_id,
            ) {
                record_host_pending_task(
                    &mut host_pending_task_ids,
                    &mut active_task_ids,
                    task_id,
                    no_live_parallel_host,
                );
            }
        }
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
            idle,
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
    if !json && !host_pending_task_ids.is_empty() {
        println!(
            "host pending: {}",
            host_pending_task_ids
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
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
            status_binary,
            host_binary,
            revision_match,
            repo_root: repo_root.display().to_string(),
            branch: current_branch,
            run_root: run_root.display().to_string(),
            tmux_session: session_name,
            tmux_running,
            host_pids: host_processes,
            lanes: lane_records,
            canonical_gate,
            staged_task_ids,
            host_pending_task_ids: host_pending_task_ids.into_iter().collect(),
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

fn parallel_status_binary_provenance(
    run_root: &Path,
    host_processes: &[String],
) -> (Option<BinaryProvenance>, Option<bool>) {
    let detected_host_pids = host_processes
        .iter()
        .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
        .collect::<BTreeSet<_>>();
    let status_binary = current_binary_provenance();
    let host_binary = parallel_host_binary_provenance(run_root, &detected_host_pids);
    let revision_match = binary_revision_match(&status_binary, host_binary.as_ref());
    (host_binary, revision_match)
}

fn lane_is_current_idle_capacity(
    run_root: &Path,
    lane_root: &Path,
    live_parallel_host: bool,
) -> bool {
    if !lane_has_only_idle_artifacts(lane_root) {
        return false;
    }
    let current_run_id = current_parallel_run_id(run_root);
    match (current_run_id.as_deref(), lane_run_id(lane_root).as_deref()) {
        (Some(current), Some(lane)) => return current == lane,
        // An explicit old identity is never a startup placeholder.
        (None, Some(_)) => return false,
        (_, None) => {}
    }
    // tmux creates empty log placeholders before the host can stamp them.
    // Only that exact empty shape is trusted without a matching run id.
    let logs_empty = ["stdout.log", "stderr.log"].iter().all(|name| {
        let path = lane_root.join(name);
        !path.exists()
            || path
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 0)
    });
    if logs_empty {
        return true;
    }
    live_parallel_host && idle_heartbeat_is_from_current_run(run_root, lane_root)
}

fn lane_has_only_idle_artifacts(lane_root: &Path) -> bool {
    let Ok(root_metadata) = fs::symlink_metadata(lane_root) else {
        return false;
    };
    if !root_metadata.file_type().is_dir() || root_metadata.file_type().is_symlink() {
        return false;
    }
    let Ok(entries) = fs::read_dir(lane_root) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let name = entry.file_name();
        if name != "stdout.log" && name != "stderr.log" && name != LANE_RUN_ID_FILE {
            return false;
        }
        if !entry.file_type().is_ok_and(|file_type| file_type.is_file()) {
            return false;
        }
    }
    true
}

fn idle_heartbeat_is_from_current_run(run_root: &Path, lane_root: &Path) -> bool {
    let Ok(run_started) = run_root
        .join(CURRENT_RUN_ID_FILE)
        .metadata()
        .and_then(|metadata| metadata.modified())
    else {
        return false;
    };
    let stdout_path = lane_root.join("stdout.log");
    let Ok(heartbeat_modified) = stdout_path
        .metadata()
        .and_then(|metadata| metadata.modified())
    else {
        return false;
    };
    if heartbeat_modified < run_started {
        return false;
    }
    read_last_nonempty_line(&stdout_path)
        .ok()
        .flatten()
        .is_some_and(|line| {
            line.starts_with("[auto parallel host lane-") && line.contains("] idle:")
        })
}

fn current_lane_host_pending_task_id(
    run_root: &Path,
    lane_index: usize,
    lane_root: &Path,
    stored_task_id: &str,
) -> Option<String> {
    let current_run_id = current_parallel_run_id(run_root)?;
    if lane_run_id(lane_root).as_deref() != Some(current_run_id.as_str()) {
        return None;
    }
    let bytes = fs::read(lane_root.join(LANE_HOST_PENDING_FILE)).ok()?;
    let marker: LaneHostPendingStatusMarker = serde_json::from_slice(&bytes).ok()?;
    if marker.version != LANE_HOST_PENDING_VERSION
        || marker.phase != "awaiting_host"
        || marker.run_id != current_run_id
        || marker.lane != lane_index
        || marker.attempt == 0
        || marker.task_id != stored_task_id
        || !safe_status_identifier(&marker.task_id, 128, false)
    {
        return None;
    }
    Some(marker.task_id)
}

fn record_host_pending_task(
    host_pending_task_ids: &mut BTreeSet<String>,
    active_task_ids: &mut BTreeSet<String>,
    task_id: String,
    no_live_parallel_host: bool,
) {
    host_pending_task_ids.insert(task_id.clone());
    if !no_live_parallel_host {
        active_task_ids.insert(task_id);
    }
}

fn canonical_gate_marker_paths(repo_root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = git_path(repo_root, "auto-review-input-quarantine.json") {
        paths.push(path);
    }
    paths.push(repo_root.join(".auto/parallel/review-input-quarantine.json"));
    paths.push(repo_root.join(".auto-review-input-quarantine.json"));
    paths
}

fn safe_status_identifier(value: &str, max_len: usize, allow_space: bool) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | ':')
                || (allow_space && character == ' ')
        })
}

fn current_canonical_gate_status(repo_root: &Path) -> Option<CanonicalGateStatus> {
    let current_head = git_stdout(repo_root, ["rev-parse", "--verify", "HEAD"])
        .ok()?
        .trim()
        .to_string();
    for path in canonical_gate_marker_paths(repo_root) {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => continue,
        };
        let marker: CanonicalGateMarker = match serde_json::from_slice(&bytes) {
            Ok(marker) => marker,
            Err(_) => continue,
        };
        let gate_label_is_safe = marker
            .gate_label
            .as_deref()
            .is_none_or(|label| safe_status_identifier(label, 128, true));
        let head_is_safe = matches!(marker.reviewed_head.len(), 40 | 64)
            && marker
                .reviewed_head
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit());
        if marker.version != CANONICAL_GATE_MARKER_VERSION
            || marker.phase != "in_progress"
            || !matches!(marker.scope.as_str(), "canonical_source" | "canonical_full")
            || !safe_status_identifier(&marker.task_id, 256, false)
            || !gate_label_is_safe
            || !head_is_safe
            || marker.reviewed_head != current_head
        {
            continue;
        }
        return Some(CanonicalGateStatus {
            phase: marker.phase,
            scope: marker.scope,
            task_id: marker.task_id,
            gate_label: marker.gate_label,
            reviewed_head: marker.reviewed_head,
        });
    }
    None
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
    parallel_host_processes_for_repo_strict(repo_root).unwrap_or_default()
}

/// Strict process discovery for destructive callers. `pgrep` exit 1 means no
/// matches; launch failures and all other exit statuses are unknown, not safe.
pub(crate) fn parallel_host_processes_for_repo_strict(repo_root: &Path) -> Result<Vec<String>> {
    let current_pid = std::process::id();
    let output = Command::new("pgrep")
        .args(["-af", "auto parallel"])
        .output()
        .context("failed to launch pgrep for parallel host discovery")?;
    if !classify_pgrep_parallel_host_exit(
        output.status.code(),
        &String::from_utf8_lossy(&output.stderr),
    )? {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
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
        .collect())
}

fn classify_pgrep_parallel_host_exit(code: Option<i32>, stderr: &str) -> Result<bool> {
    match code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        code => bail!(
            "pgrep parallel host discovery failed with status {}: {}",
            code.map(|value| value.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr.trim()
        ),
    }
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
    // A process can exit between `pgrep` and this lookup. Treat an unreadable
    // cwd as not-live instead of attributing that vanished process to whatever
    // repo happened to invoke status.
    fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd == repo_root)
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
    use super::classify_pgrep_parallel_host_exit;
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
    fn strict_pgrep_probe_fails_closed_on_unknown_exit() {
        assert!(classify_pgrep_parallel_host_exit(Some(0), "").expect("matches"));
        assert!(!classify_pgrep_parallel_host_exit(Some(1), "").expect("no matches"));
        assert!(classify_pgrep_parallel_host_exit(Some(2), "permission denied").is_err());
        assert!(classify_pgrep_parallel_host_exit(None, "terminated").is_err());
    }

    #[test]
    fn newer_status_client_reports_older_live_host_provenance_as_unknown() {
        let run_root = unique_temp_dir("parallel-status-older-live-host");
        fs::create_dir_all(&run_root).expect("create run root");
        fs::write(run_root.join(CURRENT_RUN_ID_FILE), "older-host-run")
            .expect("write older host run id");
        let host_processes = vec!["4242 auto parallel --threads 8".to_string()];

        let (host_binary, revision_match) =
            super::parallel_status_binary_provenance(&run_root, &host_processes);

        assert_eq!(host_binary, None);
        assert_eq!(revision_match, None);
        fs::remove_dir_all(run_root).expect("cleanup run root");
    }

    #[test]
    fn live_current_run_heartbeat_identifies_unstamped_idle_capacity() {
        let run_root = unique_temp_dir("parallel-status-live-idle-heartbeat");
        let lane_root = run_root.join("lanes/lane-2");
        fs::create_dir_all(&lane_root).expect("create lane root");
        fs::write(run_root.join(CURRENT_RUN_ID_FILE), "current-run\n")
            .expect("write current run id");
        fs::write(
            lane_root.join("stdout.log"),
            "[auto parallel host lane-2 [idle]] idle: waiting on dependencies\n",
        )
        .expect("write current idle heartbeat");
        fs::write(lane_root.join("stderr.log"), "").expect("write stderr log");

        assert!(super::lane_is_current_idle_capacity(
            &run_root, &lane_root, true
        ));
        assert!(!super::lane_is_current_idle_capacity(
            &run_root, &lane_root, false
        ));
        fs::remove_dir_all(run_root).expect("cleanup run root");
    }

    #[test]
    fn explicit_previous_run_identity_is_never_idle_even_with_empty_logs() {
        let run_root = unique_temp_dir("parallel-status-old-empty-idle");
        let lane_root = run_root.join("lanes/lane-3");
        fs::create_dir_all(&lane_root).expect("create lane root");
        fs::write(run_root.join(CURRENT_RUN_ID_FILE), "current-run\n")
            .expect("write current run id");
        fs::write(lane_root.join(LANE_RUN_ID_FILE), "previous-run\n")
            .expect("write previous lane run id");
        fs::write(lane_root.join("stdout.log"), "").expect("write stdout log");
        fs::write(lane_root.join("stderr.log"), "").expect("write stderr log");

        assert!(!super::lane_is_current_idle_capacity(
            &run_root, &lane_root, true
        ));
        fs::remove_dir_all(run_root).expect("cleanup run root");
    }

    #[cfg(unix)]
    #[test]
    fn idle_capacity_rejects_symlinked_log_artifacts() {
        use std::os::unix::fs::symlink;

        let run_root = unique_temp_dir("parallel-status-idle-log-symlink");
        let lane_root = run_root.join("lanes/lane-4");
        fs::create_dir_all(&lane_root).expect("create lane root");
        fs::write(run_root.join(CURRENT_RUN_ID_FILE), "current-run\n")
            .expect("write current run id");
        let outside = run_root.join("outside.log");
        fs::write(
            &outside,
            "[auto parallel host lane-4 [idle]] idle: forged\n",
        )
        .expect("write outside log");
        symlink(&outside, lane_root.join("stdout.log")).expect("symlink stdout log");

        assert!(!super::lane_is_current_idle_capacity(
            &run_root, &lane_root, true
        ));
        fs::remove_dir_all(run_root).expect("cleanup run root");
    }

    fn init_status_repo(label: &str) -> (PathBuf, String) {
        let repo = unique_temp_dir(label);
        fs::create_dir_all(&repo).expect("create status repo");
        run_git(&repo, ["init", "-q", "-b", "main"]).expect("init status repo");
        run_git(&repo, ["config", "user.name", "autodev tests"]).expect("set git name");
        run_git(&repo, ["config", "user.email", "autodev@example.com"]).expect("set git email");
        fs::write(repo.join("source.txt"), "seed\n").expect("write source");
        run_git(&repo, ["add", "source.txt"]).expect("stage source");
        run_git(&repo, ["commit", "-q", "-m", "seed"]).expect("commit source");
        let head = git_stdout(&repo, ["rev-parse", "HEAD"])
            .expect("read head")
            .trim()
            .to_string();
        (repo, head)
    }

    fn write_canonical_gate_marker(repo: &Path, json: &str) {
        let marker = git_path(repo, "auto-review-input-quarantine.json")
            .expect("resolve canonical gate marker");
        fs::write(marker, json).expect("write canonical gate marker");
    }

    #[test]
    fn canonical_gate_status_reads_current_marker_without_leaking_internal_paths() {
        let (repo, head) = init_status_repo("parallel-status-canonical-gate");
        write_canonical_gate_marker(
            &repo,
            &format!(
                r#"{{
  "version": 1,
  "phase": "in_progress",
  "scope": "canonical_source",
  "task_id": "TASK-1",
  "gate_label": "definition-of-done",
  "reviewed_head": "{head}",
  "reviewed_path_states": {{"/unsafe/private/path": ["secret-state"]}},
  "reason": "private reason"
}}"#
            ),
        );

        let gate = super::current_canonical_gate_status(&repo).expect("current gate");
        assert_eq!(gate.task_id, "TASK-1");
        assert_eq!(gate.gate_label.as_deref(), Some("definition-of-done"));
        assert_eq!(gate.reviewed_head, head);
        let serialized = serde_json::to_string(&gate).expect("serialize sanitized gate");
        assert!(!serialized.contains("unsafe"));
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("reason"));
        assert!(!serialized.contains("path_states"));

        fs::remove_dir_all(&repo).expect("remove status repo");
    }

    #[test]
    fn canonical_gate_status_ignores_absent_malformed_and_stale_markers() {
        let (repo, head) = init_status_repo("parallel-status-invalid-canonical-gate");
        assert_eq!(super::current_canonical_gate_status(&repo), None);

        write_canonical_gate_marker(&repo, "{not json");
        assert_eq!(super::current_canonical_gate_status(&repo), None);

        write_canonical_gate_marker(
            &repo,
            &format!(
                r#"{{"version":1,"phase":"in_progress","scope":"canonical_source","task_id":"TASK-1","gate_label":"definition-of-done","reviewed_head":"{}"}}"#,
                "0".repeat(head.len())
            ),
        );
        assert_eq!(super::current_canonical_gate_status(&repo), None);

        write_canonical_gate_marker(
            &repo,
            &format!(
                r#"{{"version":1,"phase":"mutation","scope":"canonical_source","task_id":"TASK-1","gate_label":"definition-of-done","reviewed_head":"{head}"}}"#
            ),
        );
        assert_eq!(super::current_canonical_gate_status(&repo), None);

        fs::remove_dir_all(&repo).expect("remove status repo");
    }

    #[test]
    fn canonical_gate_status_rejects_unsafe_identifiers() {
        let (repo, head) = init_status_repo("parallel-status-unsafe-canonical-gate");
        write_canonical_gate_marker(
            &repo,
            &format!(
                r#"{{"version":1,"phase":"in_progress","scope":"canonical_source","task_id":"../../private","gate_label":"definition-of-done","reviewed_head":"{head}"}}"#
            ),
        );
        assert_eq!(super::current_canonical_gate_status(&repo), None);

        write_canonical_gate_marker(
            &repo,
            &format!(
                r#"{{"version":1,"phase":"in_progress","scope":"canonical_source","task_id":"TASK-1","gate_label":"/private/path","reviewed_head":"{head}"}}"#
            ),
        );
        assert_eq!(super::current_canonical_gate_status(&repo), None);

        fs::remove_dir_all(&repo).expect("remove status repo");
    }

    #[test]
    fn live_host_pending_candidates_are_active_but_orphaned_candidates_are_not() {
        let mut pending = BTreeSet::new();
        let mut active = BTreeSet::new();
        super::record_host_pending_task(&mut pending, &mut active, "TASK-LIVE".to_string(), false);
        super::record_host_pending_task(&mut pending, &mut active, "TASK-ORPHAN".to_string(), true);
        assert_eq!(
            pending,
            BTreeSet::from(["TASK-LIVE".to_string(), "TASK-ORPHAN".to_string()])
        );
        assert_eq!(active, BTreeSet::from(["TASK-LIVE".to_string()]));
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
    fn vanished_process_never_counts_as_a_repo_host() {
        assert!(!process_line_cwd_matches_repo(
            "4294967295 auto parallel --threads 8",
            Path::new("/tmp/repo")
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

        let monitor = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::from(["TASK-1".to_string()]),
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            &[],
            &[],
        );
        assert_eq!(
            monitor, "MONITOR: live lane work in progress for TASK-1",
            "a host-owned canonical gate is active work even after its lane worker exits"
        );
    }
}
