fn setup_parallel_tmux_windows(run_root: &Path, lanes: usize, host_pid: u32) -> Result<()> {
    let Some(tmux_pane) = env::var_os("TMUX_PANE") else {
        return Ok(());
    };
    if tmux_pane.is_empty() {
        return Ok(());
    }

    let pane_target = tmux_pane
        .into_string()
        .map_err(|_| anyhow::anyhow!("TMUX_PANE contained invalid UTF-8"))?;
    let session_name = tmux_stdout([
        "display-message",
        "-p",
        "-t",
        &pane_target,
        "#{session_name}",
    ])?;

    for window_name in tmux_window_names(&session_name)? {
        if window_name.starts_with("loop-lane-") || window_name.starts_with("parallel-lane-") {
            run_tmux([
                "kill-window",
                "-t",
                &format!("{session_name}:{window_name}"),
            ])?;
        }
    }

    for lane in 1..=lanes {
        let window_name = format!("parallel-lane-{lane}");
        let lane_root = run_root.join("lanes").join(format!("lane-{lane}"));
        let stdout_log = shell_quote(&lane_root.join("stdout.log").display().to_string());
        let stderr_log = shell_quote(&lane_root.join("stderr.log").display().to_string());
        let script = format!(
            "mkdir -p {lane_root}; touch {stdout_log} {stderr_log}; tail -q --pid={host_pid} -n +1 -F {stdout_log} {stderr_log} || true; printf '\\n[auto parallel lane-{lane}] host process {host_pid} exited; log tail stopped.\\n'; exec bash",
            lane_root = shell_quote(&lane_root.display().to_string()),
            stdout_log = stdout_log,
            stderr_log = stderr_log,
            host_pid = host_pid,
            lane = lane,
        );
        let command = format!("bash -lc {}", shell_quote(&script));
        run_tmux([
            "new-window",
            "-t",
            &session_name,
            "-n",
            &window_name,
            &command,
        ])?;
    }

    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum TmuxLaunchStatus {
    Launched,
    AlreadyRunning,
}

fn should_launch_parallel_tmux(args: &ParallelArgs) -> bool {
    // Skip self-bootstrap when we're already inside a tmux session.
    // Detached sessions (`tmux new-session -d`) don't always propagate
    // TMUX_PANE, but they always set TMUX (the canonical tmux env var).
    // Checking both prevents the orphan-session class of bug observed
    // 2026-05-07: supervisor scripts launching parallel inside their own
    // tmux session would still trigger this bootstrap, creating an
    // untracked sibling session that broke session-end detection.
    let inside_tmux = env::var_os("TMUX").is_some_and(|v| !v.is_empty())
        || env::var_os("TMUX_PANE").is_some_and(|v| !v.is_empty());
    args.max_concurrent_workers > 1
        && env::var_os("AUTO_PARALLEL_TMUX_BOOTSTRAPPED").is_none()
        && !inside_tmux
}

fn parallel_host_stdout_log_path(run_root: &Path) -> PathBuf {
    run_root.join("host.stdout.log")
}

fn parallel_host_stderr_log_path(run_root: &Path) -> PathBuf {
    run_root.join("host.stderr.log")
}

fn launch_parallel_tmux_session(
    session_name: &str,
    run_root: &Path,
    args: &ParallelArgs,
) -> Result<TmuxLaunchStatus> {
    if tmux_session_exists(session_name)? {
        return Ok(TmuxLaunchStatus::AlreadyRunning);
    }

    let command = parallel_tmux_command(run_root, args)?;
    let working_dir = env::current_dir()
        .context("failed to resolve current directory")?
        .display()
        .to_string();
    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            session_name,
            "-c",
            &working_dir,
            &command,
        ])
        .output()
        .context("failed to launch tmux")?;
    if !output.status.success() {
        bail!(
            "tmux command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(TmuxLaunchStatus::Launched)
}

fn tmux_session_exists(session_name: &str) -> Result<bool> {
    let output = Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .output()
        .context("failed to launch tmux")?;
    Ok(output.status.success())
}

fn parallel_tmux_session_name(repo_root: &Path) -> String {
    let slug: String = repo_name(repo_root)
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "repo" } else { slug };
    format!("{slug}-parallel")
}

fn parallel_tmux_command(run_root: &Path, args: &ParallelArgs) -> Result<String> {
    let executable = env::current_exe()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .or_else(|| env::args().next())
        .context("failed to resolve current executable")?;
    let mut parts = vec![
        "AUTO_PARALLEL_TMUX_BOOTSTRAPPED=1".to_string(),
        shell_quote(&executable),
        "parallel".to_string(),
    ];
    push_optional_usize_arg(&mut parts, "--max-iterations", args.max_iterations);
    parts.push("--threads".to_string());
    parts.push(args.max_concurrent_workers.to_string());
    push_optional_usize_arg(&mut parts, "--cargo-build-jobs", args.cargo_build_jobs);
    parts.push("--cargo-target".to_string());
    parts.push(
        match args.cargo_target {
            ParallelCargoTarget::Auto => "auto",
            ParallelCargoTarget::Shared => "shared",
            ParallelCargoTarget::Lane => "lane",
            ParallelCargoTarget::None => "none",
        }
        .to_string(),
    );
    push_optional_path_arg(&mut parts, "--prompt-file", args.prompt_file.as_deref());
    parts.push("--model".to_string());
    parts.push(shell_quote(&args.model));
    parts.push("--reasoning-effort".to_string());
    parts.push(shell_quote(&args.reasoning_effort));
    push_optional_str_arg(&mut parts, "--branch", args.branch.as_deref());
    for reference_repo in &args.reference_repos {
        parts.push("--reference-repo".to_string());
        parts.push(shell_quote(&reference_repo.display().to_string()));
    }
    if args.include_siblings {
        parts.push("--include-siblings".to_string());
    }
    push_optional_path_arg(&mut parts, "--run-root", args.run_root.as_deref());
    parts.push("--codex-bin".to_string());
    parts.push(shell_quote(&args.codex_bin.display().to_string()));
    if args.claude {
        parts.push("--claude".to_string());
    }
    push_optional_usize_arg(&mut parts, "--max-turns", args.max_turns);
    parts.push("--max-retries".to_string());
    parts.push(args.max_retries.to_string());
    if let Some(action) = args.action {
        parts.push(
            match action {
                ParallelAction::Status => "status",
            }
            .to_string(),
        );
    }
    let host_command = parts.join(" ");
    let stdout_log_path = parallel_host_stdout_log_path(run_root);
    let stderr_log_path = parallel_host_stderr_log_path(run_root);
    let run_root = shell_quote(&run_root.display().to_string());
    let stdout_log = shell_quote(&stdout_log_path.display().to_string());
    let stderr_log = shell_quote(&stderr_log_path.display().to_string());
    let script = format!(
        "mkdir -p {run_root}; touch {stdout_log} {stderr_log}; ({host_command}) > >(tee -a {stdout_log}) 2> >(tee -a {stderr_log} >&2); status=$?; printf '\\n[auto parallel host] exited with status %s. stdout: %s stderr: %s\\n' \"$status\" {stdout_label} {stderr_label} | tee -a {stdout_log}; exec bash",
        run_root = run_root,
        stdout_log = stdout_log,
        stderr_log = stderr_log,
        host_command = host_command,
        stdout_label = shell_quote(&stdout_log_path.display().to_string()),
        stderr_label = shell_quote(&stderr_log_path.display().to_string()),
    );
    Ok(format!("bash -lc {}", shell_quote(&script)))
}

fn push_optional_usize_arg(parts: &mut Vec<String>, flag: &str, value: Option<usize>) {
    if let Some(value) = value {
        parts.push(flag.to_string());
        parts.push(value.to_string());
    }
}

fn push_optional_str_arg(parts: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        parts.push(flag.to_string());
        parts.push(shell_quote(value));
    }
}

fn push_optional_path_arg(parts: &mut Vec<String>, flag: &str, value: Option<&Path>) {
    if let Some(value) = value {
        parts.push(flag.to_string());
        parts.push(shell_quote(&value.display().to_string()));
    }
}

fn rebuild_active_tasks(
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

fn tmux_window_names(session_name: &str) -> Result<Vec<String>> {
    Ok(
        tmux_stdout(["list-windows", "-t", session_name, "-F", "#{window_name}"])?
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn tmux_stdout<const N: usize>(args: [&str; N]) -> Result<String> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .context("failed to launch tmux")?;
    if !output.status.success() {
        bail!(
            "tmux command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_tmux<const N: usize>(args: [&str; N]) -> Result<()> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .context("failed to launch tmux")?;
    if !output.status.success() {
        bail!(
            "tmux command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn shell_quote(raw: &str) -> String {
    if raw.is_empty() {
        return "''".to_string();
    }
    let escaped = raw.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn normalize_parallel_live_log_message(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn redact_parallel_live_log_message(message: &str) -> String {
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

