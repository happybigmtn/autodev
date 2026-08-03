use super::*;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ParallelStartupPrep {
    Checkpointed(String),
    RemoteSynced,
    Noop,
}

pub(crate) fn prepare_parallel_startup(
    repo_root: &Path,
    target_branch: &str,
) -> Result<ParallelStartupPrep> {
    enforce_review_input_quarantine_before_dispatch(repo_root)?;
    let recovered = recover_unsealed_task_completion_transitions(repo_root)?;
    if !recovered.is_empty() {
        eprintln!(
            "recovery: demoted interrupted, unsealed task completion(s) to [~] before checkpoint: {}",
            recovered.join(", ")
        );
    }
    if let Some(commit) =
        auto_checkpoint_if_needed(repo_root, target_branch, "auto parallel checkpoint")?
    {
        return Ok(ParallelStartupPrep::Checkpointed(commit));
    }
    if sync_branch_with_remote(repo_root, target_branch)? {
        return Ok(ParallelStartupPrep::RemoteSynced);
    }
    Ok(ParallelStartupPrep::Noop)
}

pub(crate) fn log_parallel_startup_prep(prep: ParallelStartupPrep, target_branch: &str) {
    match prep {
        ParallelStartupPrep::Checkpointed(commit) => {
            println!("checkpoint:  committed pre-existing changes at {commit}");
        }
        ParallelStartupPrep::RemoteSynced => {
            println!("remote sync: rebased onto origin/{}", target_branch);
        }
        ParallelStartupPrep::Noop => {}
    }
}

pub(crate) fn setup_parallel_tmux_windows(
    run_root: &Path,
    lanes: usize,
    host_pid: u32,
) -> Result<()> {
    let Some(tmux_pane) = env::var_os("TMUX_PANE") else {
        return Ok(());
    };
    if tmux_pane.is_empty() {
        return Ok(());
    }

    let pane_target = tmux_pane
        .into_string()
        .map_err(|_| anyhow::anyhow!("TMUX_PANE contained invalid UTF-8"))?;
    // Target the session by its stable id (e.g. `$0`) rather than its name.
    // A session whose name is purely numeric (e.g. tmux's default "0") makes a
    // bare `-t <name>` ambiguous: tmux parses `new-window -t 0` as window index
    // 0 and fails with "create window failed: index 0 in use". The `$<id>` form
    // is always unambiguous as a session target, in every position below.
    let session_target =
        tmux_stdout(["display-message", "-p", "-t", &pane_target, "#{session_id}"])?;

    for window_name in tmux_window_names(&session_target)? {
        if window_name.starts_with("loop-lane-") || window_name.starts_with("parallel-lane-") {
            run_tmux([
                "kill-window",
                "-t",
                &format!("{session_target}:{window_name}"),
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
            &session_target,
            "-n",
            &window_name,
            &command,
        ])?;
    }

    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TmuxLaunchStatus {
    Launched,
    AlreadyRunning,
}

pub(crate) fn should_launch_parallel_tmux(args: &ParallelArgs) -> bool {
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

pub(crate) fn parallel_host_stdout_log_path(run_root: &Path) -> PathBuf {
    run_root.join("host.stdout.log")
}

pub(crate) fn parallel_host_stderr_log_path(run_root: &Path) -> PathBuf {
    run_root.join("host.stderr.log")
}

pub(crate) fn launch_parallel_tmux_session(
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

pub(crate) fn tmux_session_exists(session_name: &str) -> Result<bool> {
    let output = Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .output()
        .context("failed to launch tmux")?;
    Ok(output.status.success())
}

pub(crate) fn parallel_tmux_session_name(repo_root: &Path) -> String {
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

pub(crate) fn parallel_tmux_command(run_root: &Path, args: &ParallelArgs) -> Result<String> {
    let executable = env::current_exe()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .or_else(|| env::args().next())
        .context("failed to resolve current executable")?;
    let skip_remote_sync = match env::var_os("AUTO_SKIP_REMOTE_SYNC") {
        Some(value) if !value.is_empty() => value
            .into_string()
            .map_err(|_| anyhow::anyhow!("AUTO_SKIP_REMOTE_SYNC contained invalid UTF-8"))?,
        _ => String::new(),
    };
    let mut parts = vec![
        "AUTO_PARALLEL_TMUX_BOOTSTRAPPED=1".to_string(),
        format!("AUTO_SKIP_REMOTE_SYNC={}", shell_quote(&skip_remote_sync)),
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
                ParallelAction::PlanCheck => "plan-check",
                ParallelAction::ReceiptBackfill => "receipt-backfill",
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

pub(crate) fn push_optional_usize_arg(parts: &mut Vec<String>, flag: &str, value: Option<usize>) {
    if let Some(value) = value {
        parts.push(flag.to_string());
        parts.push(value.to_string());
    }
}

pub(crate) fn push_optional_str_arg(parts: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        parts.push(flag.to_string());
        parts.push(shell_quote(value));
    }
}

pub(crate) fn push_optional_path_arg(parts: &mut Vec<String>, flag: &str, value: Option<&Path>) {
    if let Some(value) = value {
        parts.push(flag.to_string());
        parts.push(shell_quote(&value.display().to_string()));
    }
}

pub(crate) fn tmux_window_names(session_name: &str) -> Result<Vec<String>> {
    Ok(
        tmux_stdout(["list-windows", "-t", session_name, "-F", "#{window_name}"])?
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

pub(crate) fn tmux_stdout<const N: usize>(args: [&str; N]) -> Result<String> {
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

pub(crate) fn run_tmux<const N: usize>(args: [&str; N]) -> Result<()> {
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

pub(crate) fn shell_quote(raw: &str) -> String {
    if raw.is_empty() {
        return "''".to_string();
    }
    let escaped = raw.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use crate::parallel_command::*;
    use std::time::UNIX_EPOCH;

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

    fn init_remote_and_clones(name: &str, branch: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = unique_temp_dir(name);
        let remote = root.join("remote.git");
        let upstream = root.join("upstream");
        let worker = root.join("worker");

        fs::create_dir_all(&root).expect("failed to create temp root");
        run_git_in(
            &root,
            [
                "init",
                "--bare",
                remote.to_str().expect("remote path should be utf-8"),
            ],
        );
        run_git_in(
            &root,
            [
                "clone",
                remote.to_str().expect("remote path should be utf-8"),
                upstream.to_str().expect("upstream path should be utf-8"),
            ],
        );
        run_git_in(&upstream, ["config", "user.name", "autodev tests"]);
        run_git_in(&upstream, ["config", "user.email", "autodev@example.com"]);
        fs::write(upstream.join("README.md"), "# init\n").expect("failed to write README");
        run_git_in(&upstream, ["add", "README.md"]);
        run_git_in(&upstream, ["commit", "-m", "init"]);
        run_git_in(&upstream, ["branch", "-M", branch]);
        run_git_in(&upstream, ["push", "-u", "origin", branch]);

        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                branch,
                remote.to_str().expect("remote path should be utf-8"),
                worker.to_str().expect("worker path should be utf-8"),
            ],
        );
        run_git_in(&worker, ["config", "user.name", "autodev tests"]);
        run_git_in(&worker, ["config", "user.email", "autodev@example.com"]);

        (root, remote, upstream, worker)
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    #[test]
    fn parallel_tmux_session_name_uses_repo_slug() {
        assert_eq!(
            parallel_tmux_session_name(&PathBuf::from("/home/r/Coding/bitino")),
            "bitino-parallel"
        );
        assert_eq!(
            parallel_tmux_session_name(&PathBuf::from("/tmp/weird:repo name")),
            "weird-repo-name-parallel"
        );
    }

    #[test]
    fn parallel_tmux_command_persists_host_logs_and_keeps_shell_open() {
        let args = ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            json: false,
            max_iterations: Some(3),
            max_concurrent_workers: 8,
            cargo_build_jobs: Some(2),
            cargo_target: ParallelCargoTarget::Lane,
            prompt_file: Some(PathBuf::from("/tmp/prompt.md")),
            model: "gpt-5.6-sol".to_string(),
            reasoning_effort: "high".to_string(),
            branch: Some("main".to_string()),
            reference_repos: vec![PathBuf::from("/tmp/reference repo")],
            include_siblings: true,
            run_root: Some(PathBuf::from("/tmp/auto-parallel")),
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        };
        let command = parallel_tmux_command(&PathBuf::from("/tmp/auto-parallel"), &args)
            .expect("tmux command should render");

        assert!(command.contains("host.stdout.log"));
        assert!(command.contains("host.stderr.log"));
        assert!(command.contains("tee -a"));
        assert!(command.contains("exec bash"));
        assert!(command.contains(" parallel "));
        assert!(command.contains("--threads 8"));
        assert!(command.contains("--max-iterations 3"));
        assert!(command.contains("--cargo-target lane"));
        assert!(command.contains("--reference-repo"));
        assert!(command.contains("--include-siblings"));
        assert!(!command.contains(" super "));
    }

    #[test]
    fn parallel_tmux_command_renders_status_action_when_requested() {
        let args = ParallelArgs {
            action: Some(ParallelAction::Status),
            apply_receipt_backfill_handoffs: false,
            json: false,
            max_iterations: None,
            max_concurrent_workers: 2,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "gpt-5.6-sol".to_string(),
            reasoning_effort: "high".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: None,
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        };
        let command = parallel_tmux_command(&PathBuf::from("/tmp/auto-parallel"), &args)
            .expect("tmux command should render");

        assert!(command.contains(" parallel "));
        assert!(command.contains(" status"));
    }

    #[test]
    fn parallel_tmux_command_pins_local_only_mode_to_the_calling_environment() {
        let _env_lock = crate::util::test_process_env_lock()
            .lock()
            .expect("failed to lock process env");
        let previous = std::env::var_os("AUTO_SKIP_REMOTE_SYNC");
        struct RestoreSkipRemoteSync(Option<std::ffi::OsString>);
        impl Drop for RestoreSkipRemoteSync {
            fn drop(&mut self) {
                match &self.0 {
                    Some(value) => std::env::set_var("AUTO_SKIP_REMOTE_SYNC", value),
                    None => std::env::remove_var("AUTO_SKIP_REMOTE_SYNC"),
                }
            }
        }
        let _restore = RestoreSkipRemoteSync(previous);
        let args = ParallelArgs {
            action: Some(ParallelAction::Status),
            apply_receipt_backfill_handoffs: false,
            json: false,
            max_iterations: None,
            max_concurrent_workers: 2,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "gpt-5.6-sol".to_string(),
            reasoning_effort: "high".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: None,
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        };
        let render_script = || {
            let command = parallel_tmux_command(&PathBuf::from("/tmp/auto-parallel"), &args)
                .expect("tmux command should render");
            let parser =
                format!("set -- {command}; [ \"$#\" -eq 3 ] || exit 91; printf '%s' \"$3\"");
            let output = Command::new("bash")
                .args(["-c", &parser])
                .output()
                .expect("failed to parse rendered tmux command");
            assert!(
                output.status.success(),
                "rendered command should remain valid shell: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).expect("rendered script should be UTF-8")
        };

        std::env::set_var(
            "AUTO_SKIP_REMOTE_SYNC",
            "local only ' $HOME; touch /tmp/must-not-run",
        );
        let local_only = render_script();
        assert!(
            local_only.contains(
                "AUTO_SKIP_REMOTE_SYNC='local only '\"'\"' $HOME; touch /tmp/must-not-run'"
            ),
            "{local_only}"
        );

        std::env::set_var("AUTO_SKIP_REMOTE_SYNC", "");
        let explicitly_empty = render_script();
        assert!(
            explicitly_empty.contains("AUTO_SKIP_REMOTE_SYNC=''"),
            "{explicitly_empty}"
        );

        std::env::remove_var("AUTO_SKIP_REMOTE_SYNC");
        let unset = render_script();
        assert!(
            unset.contains("AUTO_SKIP_REMOTE_SYNC=''"),
            "an existing tmux server must not leak a stale local-only value: {unset}"
        );
    }

    #[test]
    fn parallel_startup_prep_checkpoints_dirty_worktree_before_bootstrap() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-startup-prep", "trunk");

        fs::create_dir_all(worker.join("notes")).expect("failed to create notes dir");
        fs::write(worker.join("notes").join("draft.md"), "draft\n").expect("failed to write draft");

        let prep =
            prepare_parallel_startup(&worker, "trunk").expect("parallel startup prep should work");
        let commit = match prep {
            ParallelStartupPrep::Checkpointed(commit) => commit,
            other => panic!("expected checkpointed startup prep, got {other:?}"),
        };

        assert!(!commit.is_empty());
        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        assert!(worker.join("notes").join("draft.md").exists());
        let log = run_git_in(&worker, ["log", "--format=%s", "-2"]);
        assert_eq!(log, "worker: auto parallel checkpoint\ninit\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }
}
