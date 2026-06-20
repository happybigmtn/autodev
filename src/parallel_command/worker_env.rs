use super::*;

const WORKER_GIT_GUARD_DIR: &str = "worker-bin";
const WORKER_GIT_GUARD_BLOCKED_VERBS: [&str; 4] = ["push", "pull", "fetch", "rebase"];
const WORKER_GIT_GUARD_PROTOCOLS: [&str; 4] = ["ssh", "https", "http", "git"];

pub(crate) fn parallel_run_root(repo_root: &Path, args: &ParallelArgs) -> PathBuf {
    match args.run_root.as_deref() {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => repo_root.join(path),
        None => crate::util::auto_run_root_override(repo_root, "parallel")
            .unwrap_or_else(|| repo_root.join(".auto").join("parallel")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoopWorkerEnv {
    pub(crate) extra_env: Vec<(String, String)>,
    pub(crate) cargo_jobs_summary: String,
    pub(crate) cargo_target_summary: Option<String>,
    pub(crate) lane_local_cargo_target: bool,
    pub(crate) cargo_target_prompt_clause: String,
}

pub(crate) fn build_loop_worker_env(
    args: &ParallelArgs,
    repo_root: &Path,
    run_root: &Path,
) -> Result<LoopWorkerEnv> {
    let inherited = std::env::var("CARGO_BUILD_JOBS").ok();
    let inherited_target = std::env::var("CARGO_TARGET_DIR").ok();
    let parallelism = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4);
    let mut worker_env = resolve_loop_worker_env(
        args.cargo_build_jobs,
        args.cargo_target,
        inherited.as_deref(),
        inherited_target.as_deref(),
        parallelism,
        args.max_concurrent_workers,
        repo_uses_cargo(repo_root),
        run_root,
    )?;
    install_parallel_worker_git_guard(&mut worker_env.extra_env, run_root)?;
    Ok(worker_env)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_loop_worker_env(
    cargo_build_jobs: Option<usize>,
    cargo_target: ParallelCargoTarget,
    inherited_cargo_build_jobs: Option<&str>,
    inherited_cargo_target_dir: Option<&str>,
    available_parallelism: usize,
    max_concurrent_workers: usize,
    repo_uses_cargo: bool,
    run_root: &Path,
) -> Result<LoopWorkerEnv> {
    if let Some(jobs) = cargo_build_jobs {
        if jobs == 0 {
            bail!("--cargo-build-jobs must be greater than 0");
        }
        return Ok(cargo_build_jobs_env(
            jobs,
            format!("override CARGO_BUILD_JOBS={jobs}"),
            cargo_target,
            inherited_cargo_target_dir,
            max_concurrent_workers,
            repo_uses_cargo,
            run_root,
        ));
    }

    if let Some(value) = inherited_cargo_build_jobs {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(inherited_target_loop_worker_env(
                format!("inherited CARGO_BUILD_JOBS={value}"),
                cargo_target,
                inherited_cargo_target_dir,
                max_concurrent_workers,
                repo_uses_cargo,
                run_root,
            ));
        }
    }

    let jobs = default_cargo_build_jobs_for(available_parallelism, max_concurrent_workers);
    Ok(cargo_build_jobs_env(
        jobs,
        format!("auto CARGO_BUILD_JOBS={jobs}"),
        cargo_target,
        inherited_cargo_target_dir,
        max_concurrent_workers,
        repo_uses_cargo,
        run_root,
    ))
}

pub(crate) fn cargo_build_jobs_env(
    jobs: usize,
    cargo_jobs_summary: String,
    cargo_target: ParallelCargoTarget,
    inherited_cargo_target_dir: Option<&str>,
    max_concurrent_workers: usize,
    repo_uses_cargo: bool,
    run_root: &Path,
) -> LoopWorkerEnv {
    let mut env = inherited_target_loop_worker_env(
        cargo_jobs_summary,
        cargo_target,
        inherited_cargo_target_dir,
        max_concurrent_workers,
        repo_uses_cargo,
        run_root,
    );
    env.extra_env
        .push(("CARGO_BUILD_JOBS".to_string(), jobs.to_string()));
    env
}

pub(crate) fn inherited_target_loop_worker_env(
    cargo_jobs_summary: String,
    cargo_target: ParallelCargoTarget,
    inherited_cargo_target_dir: Option<&str>,
    max_concurrent_workers: usize,
    repo_uses_cargo: bool,
    run_root: &Path,
) -> LoopWorkerEnv {
    let mut extra_env = Vec::new();
    let cargo_target_layout = resolve_parallel_cargo_target_layout(
        cargo_target,
        inherited_cargo_target_dir,
        max_concurrent_workers,
        repo_uses_cargo,
        run_root,
    );
    let mut lane_local_cargo_target = false;
    let cargo_target_summary = match cargo_target_layout {
        ParallelCargoTargetLayout::None => None,
        ParallelCargoTargetLayout::Fixed(target_dir) => {
            extra_env.push(("CARGO_TARGET_DIR".to_string(), target_dir.clone()));
            Some(target_dir)
        }
        ParallelCargoTargetLayout::LaneLocal => {
            lane_local_cargo_target = true;
            Some(format!(
                "lane-local under {}/lanes/lane-*/cargo-target",
                run_root.display()
            ))
        }
    };
    let cargo_target_prompt_clause =
        cargo_target_prompt_clause(lane_local_cargo_target, cargo_target_summary.as_deref());
    LoopWorkerEnv {
        extra_env,
        cargo_jobs_summary,
        cargo_target_summary,
        lane_local_cargo_target,
        cargo_target_prompt_clause,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParallelCargoTargetLayout {
    None,
    Fixed(String),
    LaneLocal,
}

pub(crate) fn resolve_parallel_cargo_target_layout(
    cargo_target: ParallelCargoTarget,
    inherited_cargo_target_dir: Option<&str>,
    max_concurrent_workers: usize,
    repo_uses_cargo: bool,
    run_root: &Path,
) -> ParallelCargoTargetLayout {
    let inherited = inherited_cargo_target_dir
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    match cargo_target {
        ParallelCargoTarget::None => ParallelCargoTargetLayout::None,
        ParallelCargoTarget::Shared => ParallelCargoTargetLayout::Fixed(
            run_root
                .join("shared-cargo-target")
                .to_string_lossy()
                .into_owned(),
        ),
        ParallelCargoTarget::Lane => {
            if max_concurrent_workers > 1 {
                ParallelCargoTargetLayout::LaneLocal
            } else {
                ParallelCargoTargetLayout::Fixed(
                    run_root.join("cargo-target").to_string_lossy().into_owned(),
                )
            }
        }
        ParallelCargoTarget::Auto => {
            if let Some(target_dir) = inherited {
                ParallelCargoTargetLayout::Fixed(target_dir)
            } else if max_concurrent_workers > 1 && repo_uses_cargo {
                ParallelCargoTargetLayout::LaneLocal
            } else {
                ParallelCargoTargetLayout::None
            }
        }
    }
}

pub(crate) fn cargo_target_prompt_clause(lane_local: bool, summary: Option<&str>) -> String {
    if lane_local {
        return "Use the host-provided `CARGO_TARGET_DIR`; this run gives each lane its own target directory, so final proofs should go through `cargo test` or the repo's verification wrapper rather than direct binaries from another lane. Do not override it.".to_string();
    }
    if summary.is_some() {
        return "Use the host-provided `CARGO_TARGET_DIR`. If Cargo is busy, wait or narrow the proof instead of switching target directories. Do not use direct target-dir test binaries as proof unless you just built that exact artifact from this lane's source tree.".to_string();
    }
    "Use the repo's normal Cargo target behavior. Do not create ad hoc target directories unless the task explicitly requires isolation, and prefer `cargo test` or the repo's verification wrapper for final proof.".to_string()
}

pub(crate) fn repo_uses_cargo(repo_root: &Path) -> bool {
    repo_root.join("Cargo.toml").exists()
}

pub(crate) fn install_parallel_worker_git_guard(
    extra_env: &mut Vec<(String, String)>,
    run_root: &Path,
) -> Result<()> {
    let guard_dir = run_root.join(WORKER_GIT_GUARD_DIR);
    fs::create_dir_all(&guard_dir)
        .with_context(|| format!("failed to create {}", guard_dir.display()))?;
    let guard_path = guard_dir.join("git");
    atomic_write(&guard_path, worker_git_guard_script().as_bytes())
        .with_context(|| format!("failed to write {}", guard_path.display()))?;
    make_executable(&guard_path)?;

    let real_git = resolve_real_git_for_worker_guard(run_root)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
    upsert_env(extra_env, "AUTO_PARALLEL_GIT_GUARD", "remote-git-disabled");
    upsert_env(extra_env, "AUTO_REAL_GIT", &real_git.to_string_lossy());
    upsert_env(extra_env, "GIT_TERMINAL_PROMPT", "0");
    upsert_env(extra_env, "GIT_ASKPASS", "/bin/false");
    upsert_env(extra_env, "SSH_ASKPASS", "/bin/false");
    install_git_protocol_block_config(extra_env);

    let current_path = extra_env
        .iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .or_else(|| env::var("PATH").ok())
        .unwrap_or_default();
    let guarded_path = if current_path.trim().is_empty() {
        guard_dir.to_string_lossy().into_owned()
    } else {
        format!("{}:{current_path}", guard_dir.display())
    };
    upsert_env(extra_env, "PATH", &guarded_path);
    Ok(())
}

fn install_git_protocol_block_config(extra_env: &mut Vec<(String, String)>) {
    upsert_env(
        extra_env,
        "GIT_CONFIG_COUNT",
        &WORKER_GIT_GUARD_PROTOCOLS.len().to_string(),
    );
    for (index, protocol) in WORKER_GIT_GUARD_PROTOCOLS.iter().enumerate() {
        upsert_env(
            extra_env,
            &format!("GIT_CONFIG_KEY_{index}"),
            &format!("protocol.{protocol}.allow"),
        );
        upsert_env(extra_env, &format!("GIT_CONFIG_VALUE_{index}"), "never");
    }
}

fn worker_git_guard_script() -> String {
    let blocked_pattern = WORKER_GIT_GUARD_BLOCKED_VERBS.join("|");
    format!(
        r#"#!/bin/sh
verb=""
expect_value=0
for arg in "$@"; do
  if [ "$expect_value" = "1" ]; then
    expect_value=0
    continue
  fi
  case "$arg" in
    -C|-c|--git-dir|--work-tree|--namespace)
      expect_value=1
      continue
      ;;
    --git-dir=*|--work-tree=*|--namespace=*)
      continue
      ;;
    -*)
      continue
      ;;
    *)
      verb="$arg"
      break
      ;;
  esac
done

case "$verb" in
  {blocked_pattern})
    echo "AUTO_ENV_BLOCKER: auto parallel worker git guard blocked 'git $verb'; host owns remote sync and branch reconciliation" >&2
    exit 126
    ;;
esac

if [ -n "${{AUTO_REAL_GIT:-}}" ]; then
  exec "$AUTO_REAL_GIT" "$@"
fi
exec git "$@"
"#
    )
}

fn upsert_env(extra_env: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some((_, existing)) = extra_env
        .iter_mut()
        .rev()
        .find(|(existing, _)| existing == key)
    {
        *existing = value.to_string();
    } else {
        extra_env.push((key.to_string(), value.to_string()));
    }
}

fn resolve_real_git_for_worker_guard(run_root: &Path) -> Option<PathBuf> {
    if let Some(path) = env::var_os("AUTO_REAL_GIT")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .filter(|path| path.exists())
    {
        return Some(path);
    }

    let path_env = env::var_os("PATH")?;
    for path_dir in env::split_paths(&path_env) {
        let candidate = path_dir.join("git");
        if candidate.starts_with(run_root) {
            continue;
        }
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn effective_parallel_claude_max_turns(args: &ParallelArgs) -> Option<usize> {
    args.max_turns
}

pub(crate) fn default_cargo_build_jobs_for(
    available_parallelism: usize,
    max_concurrent_workers: usize,
) -> usize {
    let available_parallelism = available_parallelism.max(1);
    let workers = max_concurrent_workers.max(1);
    (available_parallelism / (workers + 1)).clamp(1, 4)
}

#[cfg(test)]
mod tests {
    use super::{make_executable, worker_git_guard_script, WORKER_GIT_GUARD_DIR};
    use crate::parallel_command::*;
    use crate::util::output_retrying_etxtbsy;
    use std::time::UNIX_EPOCH;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    #[test]
    fn parallel_run_root_resolves_relative_override_under_repo_root() {
        let args = ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            max_iterations: None,
            max_concurrent_workers: 5,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "gpt-5.5".to_string(),
            reasoning_effort: "high".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: Some(PathBuf::from(".auto/super/run-1")),
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        };

        assert_eq!(
            parallel_run_root(&PathBuf::from("/repo"), &args),
            PathBuf::from("/repo/.auto/super/run-1")
        );
    }

    #[test]
    fn default_cargo_build_jobs_caps_nested_parallelism() {
        assert_eq!(default_cargo_build_jobs_for(22, 1), 4);
        assert_eq!(default_cargo_build_jobs_for(22, 5), 3);
        assert_eq!(default_cargo_build_jobs_for(12, 4), 2);
        assert_eq!(default_cargo_build_jobs_for(3, 2), 1);
        assert_eq!(default_cargo_build_jobs_for(1, 1), 1);
    }

    #[test]
    fn loop_worker_env_respects_override_and_inherited_cargo_jobs() {
        let run_root = unique_temp_dir("loop-worker-env");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let shared_target = run_root
            .join("shared-cargo-target")
            .to_string_lossy()
            .into_owned();

        let inherited = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            Some("8"),
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert!(inherited.extra_env.is_empty());
        assert_eq!(inherited.cargo_jobs_summary, "inherited CARGO_BUILD_JOBS=8");
        assert!(inherited.lane_local_cargo_target);
        assert!(inherited
            .cargo_target_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("lane-local")));

        let overridden = resolve_loop_worker_env(
            Some(3),
            ParallelCargoTarget::Auto,
            Some("8"),
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            overridden.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert_eq!(overridden.cargo_jobs_summary, "override CARGO_BUILD_JOBS=3");
        assert!(overridden.lane_local_cargo_target);

        let automatic = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            None,
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            automatic.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert_eq!(automatic.cargo_jobs_summary, "auto CARGO_BUILD_JOBS=3");
        assert!(automatic.lane_local_cargo_target);

        let shared = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Shared,
            None,
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            shared.extra_env,
            vec![
                ("CARGO_TARGET_DIR".to_string(), shared_target),
                ("CARGO_BUILD_JOBS".to_string(), "3".to_string())
            ]
        );
        assert!(!shared.lane_local_cargo_target);

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn loop_worker_env_rejects_zero_cargo_jobs_override() {
        let run_root = unique_temp_dir("loop-worker-env-error");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let err = resolve_loop_worker_env(
            Some(0),
            ParallelCargoTarget::Auto,
            None,
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--cargo-build-jobs"));
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn build_loop_worker_env_installs_git_guard() {
        let repo_root = unique_temp_dir("loop-worker-env-git-guard-repo");
        let run_root = unique_temp_dir("loop-worker-env-git-guard-run");
        fs::create_dir_all(&repo_root).expect("failed to create repo root");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let args = ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            max_iterations: None,
            max_concurrent_workers: 2,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::None,
            prompt_file: None,
            model: "gpt-5.5".to_string(),
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

        let env = build_loop_worker_env(&args, &repo_root, &run_root)
            .expect("worker env should include git guard");
        let guard_dir = run_root.join(WORKER_GIT_GUARD_DIR);
        let guard_path = guard_dir.join("git");
        assert!(guard_path.exists(), "missing {}", guard_path.display());
        assert_eq!(
            env.extra_env
                .iter()
                .find(|(key, _)| key == "AUTO_PARALLEL_GIT_GUARD")
                .map(|(_, value)| value.as_str()),
            Some("remote-git-disabled")
        );
        assert!(env
            .extra_env
            .iter()
            .find(|(key, _)| key == "PATH")
            .is_some_and(|(_, value)| value.starts_with(&format!("{}:", guard_dir.display()))));
        assert_eq!(
            env.extra_env
                .iter()
                .find(|(key, _)| key == "GIT_CONFIG_COUNT")
                .map(|(_, value)| value.as_str()),
            Some("4")
        );
        assert!(env
            .extra_env
            .iter()
            .any(|(key, value)| { key == "GIT_CONFIG_KEY_0" && value == "protocol.ssh.allow" }));

        fs::remove_dir_all(&repo_root).expect("failed to remove repo root");
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn git_guard_script_blocks_remote_sync_verbs_before_real_git() {
        let run_root = unique_temp_dir("loop-worker-env-git-guard-script");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let guard_path = run_root.join("git");
        atomic_write(&guard_path, worker_git_guard_script().as_bytes())
            .expect("failed to write guard");
        make_executable(&guard_path).expect("failed to chmod guard");

        let mut blocked_command = Command::new(&guard_path);
        blocked_command
            .arg("-C")
            .arg("/tmp/repo")
            .arg("push")
            .arg("origin")
            .arg("main")
            .env("AUTO_REAL_GIT", "/bin/echo");
        let blocked = output_retrying_etxtbsy(&mut blocked_command).expect("guard should run");
        assert_eq!(blocked.status.code(), Some(126));
        assert!(String::from_utf8_lossy(&blocked.stderr).contains("AUTO_ENV_BLOCKER"));

        let mut allowed_command = Command::new(&guard_path);
        allowed_command
            .arg("status")
            .env("AUTO_REAL_GIT", "/bin/echo");
        let allowed = output_retrying_etxtbsy(&mut allowed_command).expect("guard should delegate");
        assert!(allowed.status.success());
        assert_eq!(String::from_utf8_lossy(&allowed.stdout).trim(), "status");

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn git_guard_env_blocks_absolute_git_network_transport() {
        let run_root = unique_temp_dir("loop-worker-env-git-guard-protocol");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let mut extra_env = Vec::new();
        install_parallel_worker_git_guard(&mut extra_env, &run_root)
            .expect("git guard should install");
        let real_git = extra_env
            .iter()
            .find(|(key, _)| key == "AUTO_REAL_GIT")
            .map(|(_, value)| value.clone())
            .expect("guard should record real git");

        let blocked = Command::new(real_git)
            .arg("ls-remote")
            .arg("https://example.com/repo.git")
            .envs(extra_env.iter().map(|(key, value)| (key, value)))
            .output()
            .expect("absolute git should run");
        assert!(!blocked.status.success());
        let stderr = String::from_utf8_lossy(&blocked.stderr);
        assert!(
            stderr.contains("transport 'https' not allowed")
                || stderr.contains("transport 'https' is not allowed"),
            "{stderr}"
        );

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn loop_worker_env_respects_inherited_cargo_target_dir() {
        let run_root = unique_temp_dir("loop-worker-env-inherited-target");
        fs::create_dir_all(&run_root).expect("failed to create run root");

        let env = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            None,
            Some("/tmp/shared-target"),
            22,
            5,
            true,
            &run_root,
        )
        .expect("worker env should resolve");
        assert_eq!(
            env.extra_env,
            vec![
                (
                    "CARGO_TARGET_DIR".to_string(),
                    "/tmp/shared-target".to_string()
                ),
                ("CARGO_BUILD_JOBS".to_string(), "3".to_string())
            ]
        );
        assert_eq!(
            env.cargo_target_summary,
            Some("/tmp/shared-target".to_string())
        );
        assert!(!env.lane_local_cargo_target);

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn parallel_claude_has_no_implicit_turn_budget() {
        let args = ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            max_iterations: None,
            max_concurrent_workers: 5,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "opus".to_string(),
            reasoning_effort: "xhigh".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: None,
            codex_bin: PathBuf::from("codex"),
            claude: true,
            max_turns: None,
            max_retries: 2,
        };

        assert_eq!(effective_parallel_claude_max_turns(&args), None);
    }
}
