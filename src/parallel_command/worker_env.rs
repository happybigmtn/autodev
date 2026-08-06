use super::*;

const WORKER_GIT_GUARD_DIR: &str = "worker-bin";
const WORKER_GIT_GUARD_BLOCKED_VERBS: [&str; 4] = ["push", "pull", "fetch", "rebase"];
const WORKER_GIT_GUARD_PROTOCOLS: [&str; 4] = ["ssh", "https", "http", "git"];
pub(crate) const WORKER_CARGO_LEASE_ENV: &str = "AUTO_PARALLEL_VALIDATION_LEASE";
pub(crate) const WORKER_LANE_CARGO_LEASE_ENV: &str = "AUTO_PARALLEL_LANE_CARGO_LEASE";

pub(crate) fn lane_cargo_lease_path(lane_root: &Path) -> PathBuf {
    lane_root.join("cargo-resource.lock")
}

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
    if repo_uses_cargo(repo_root) {
        install_parallel_worker_cargo_lease(&mut worker_env.extra_env, run_root)?;
    }
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
            if lane_persistent_cargo_target_enabled() {
                Some(format!(
                    "lane-local persistent under {}/lane-caches/lane-*/cargo-target (survives per-task worktree churn)",
                    run_root.display()
                ))
            } else {
                Some(format!(
                    "lane-local under {}/lanes/lane-*/cargo-target",
                    run_root.display()
                ))
            }
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
            if max_concurrent_workers > 1 && repo_uses_cargo {
                ParallelCargoTargetLayout::LaneLocal
            } else if let Some(target_dir) = inherited {
                ParallelCargoTargetLayout::Fixed(target_dir)
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

/// Route every lane-owned Cargo process through a shared validation lease.
///
/// Cargo commands from different lanes may overlap one another. Each lane is
/// serialized before taking the shared global lease, so duplicate commands in
/// one lane do not contend on that lane's target artifacts or hold the global
/// lease while waiting. The host's canonical workspace probe takes the global
/// exclusive lease. Agent reasoning, editing, and non-Cargo checks remain
/// parallel while either lease is held.
fn install_parallel_worker_cargo_lease(
    extra_env: &mut Vec<(String, String)>,
    run_root: &Path,
) -> Result<()> {
    let guard_dir = run_root.join(WORKER_GIT_GUARD_DIR);
    fs::create_dir_all(&guard_dir)
        .with_context(|| format!("failed to create {}", guard_dir.display()))?;
    let guard_path = guard_dir.join("cargo");
    atomic_write(&guard_path, worker_cargo_lease_script().as_bytes())
        .with_context(|| format!("failed to write {}", guard_path.display()))?;
    make_executable(&guard_path)?;

    let real_cargo = resolve_real_executable_outside_run_root("cargo", run_root)
        .context("could not resolve real cargo for the parallel validation lease")?;
    let flock = resolve_real_executable_outside_run_root("flock", run_root)
        .context("could not resolve flock for the parallel validation lease")?;
    upsert_env(extra_env, "AUTO_REAL_CARGO", &real_cargo.to_string_lossy());
    upsert_env(extra_env, "AUTO_PARALLEL_FLOCK", &flock.to_string_lossy());
    upsert_env(
        extra_env,
        WORKER_CARGO_LEASE_ENV,
        &validation_lease_path(run_root).to_string_lossy(),
    );
    // Cargo intentionally sets CARGO to its own executable for build scripts
    // and built programs. Preserve that behavior once Cargo is running. At the
    // worker root, however, CARGO may be an unrelated absolute Cargo inherited
    // by auto itself. Point that value at the cargo-compatible lease shim so a
    // prebuilt binary or helper that invokes $CARGO cannot bypass the lane and
    // canonical-validation locks before any governed Cargo parent exists.
    upsert_env(extra_env, "CARGO", &guard_path.to_string_lossy());
    Ok(())
}

fn worker_cargo_lease_script() -> &'static str {
    r#"#!/bin/sh
if [ -z "${AUTO_REAL_CARGO:-}" ] || [ -z "${AUTO_PARALLEL_FLOCK:-}" ] || [ -z "${AUTO_PARALLEL_VALIDATION_LEASE:-}" ] || [ -z "${AUTO_PARALLEL_LANE_CARGO_LEASE:-}" ]; then
  echo "AUTO_ENV_BLOCKER: auto parallel cargo lease is missing required host environment" >&2
  exit 126
fi
exec "$AUTO_PARALLEL_FLOCK" --exclusive "$AUTO_PARALLEL_LANE_CARGO_LEASE" "$AUTO_PARALLEL_FLOCK" --shared "$AUTO_PARALLEL_VALIDATION_LEASE" "$AUTO_REAL_CARGO" "$@"
"#
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

fn resolve_real_executable_outside_run_root(name: &str, run_root: &Path) -> Option<PathBuf> {
    let path_env = env::var_os("PATH")?;
    for path_dir in env::split_paths(&path_env) {
        let candidate = path_dir.join(name);
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
    use super::{
        install_parallel_worker_cargo_lease, make_executable, worker_cargo_lease_script,
        worker_git_guard_script, WORKER_GIT_GUARD_DIR,
    };
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
            json: false,
            apply: false,
            include_caches: false,
            max_iterations: None,
            max_concurrent_workers: 5,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "gpt-5.6-sol".to_string(),
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
        fs::write(repo_root.join("Cargo.toml"), "[workspace]\nmembers = []\n")
            .expect("failed to write Cargo manifest");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let args = ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            json: false,
            apply: false,
            include_caches: false,
            max_iterations: None,
            max_concurrent_workers: 2,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::None,
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

        let env = build_loop_worker_env(&args, &repo_root, &run_root)
            .expect("worker env should include git guard");
        let guard_dir = run_root.join(WORKER_GIT_GUARD_DIR);
        let guard_path = guard_dir.join("git");
        assert!(guard_path.exists(), "missing {}", guard_path.display());
        assert!(guard_dir.join("cargo").exists(), "missing cargo lease shim");
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
        assert!(env.extra_env.iter().any(|(key, value)| {
            key == "AUTO_PARALLEL_VALIDATION_LEASE"
                && value == &validation_lease_path(&run_root).to_string_lossy()
        }));
        assert!(env
            .extra_env
            .iter()
            .any(|(key, value)| key == "AUTO_REAL_CARGO" && Path::new(value).is_absolute()));
        assert_eq!(
            env.extra_env
                .iter()
                .find(|(key, _)| key == "CARGO")
                .map(|(_, value)| PathBuf::from(value)),
            Some(guard_dir.join("cargo")),
            "worker-root CARGO must re-enter the lease shim"
        );

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

    #[tokio::test]
    async fn cargo_guard_waits_while_canonical_validation_owns_exclusive_lease() {
        let run_root = unique_temp_dir("loop-worker-env-cargo-lease");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let guard_path = run_root.join("cargo");
        atomic_write(&guard_path, worker_cargo_lease_script().as_bytes())
            .expect("failed to write cargo guard");
        make_executable(&guard_path).expect("failed to chmod cargo guard");

        let lease = acquire_exclusive_validation_lease(&run_root)
            .await
            .expect("canonical lease should lock");
        let mut worker = Command::new(&guard_path)
            .arg("test")
            .stdout(std::process::Stdio::piped())
            .env("AUTO_REAL_CARGO", "/bin/echo")
            .env("AUTO_PARALLEL_FLOCK", "/usr/bin/flock")
            .env(
                "AUTO_PARALLEL_VALIDATION_LEASE",
                validation_lease_path(&run_root),
            )
            .env(
                WORKER_LANE_CARGO_LEASE_ENV,
                run_root.join("lane-1-cargo.lock"),
            )
            .spawn()
            .expect("cargo guard should start");
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            worker.try_wait().expect("inspect cargo guard").is_none(),
            "lane cargo must wait while canonical validation owns the lease"
        );

        drop(lease);
        let output = worker
            .wait_with_output()
            .expect("cargo guard should resume");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "test");
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[tokio::test]
    async fn inherited_cargo_env_reenters_lease_for_prebuilt_helper_chains() {
        let run_root = unique_temp_dir("loop-worker-env-inherited-cargo");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let guard_path = run_root.join("cargo");
        atomic_write(&guard_path, worker_cargo_lease_script().as_bytes())
            .expect("failed to write cargo guard");
        make_executable(&guard_path).expect("failed to chmod cargo guard");

        let lease = acquire_exclusive_validation_lease(&run_root)
            .await
            .expect("canonical lease should lock");
        let mut helper = Command::new("/bin/sh")
            .arg("-c")
            .arg("exec \"$CARGO\" test --locked")
            .stdout(std::process::Stdio::piped())
            .env("CARGO", &guard_path)
            .env("AUTO_REAL_CARGO", "/bin/echo")
            .env("AUTO_PARALLEL_FLOCK", "/usr/bin/flock")
            .env(WORKER_CARGO_LEASE_ENV, validation_lease_path(&run_root))
            .env(
                WORKER_LANE_CARGO_LEASE_ENV,
                run_root.join("lane-1-cargo.lock"),
            )
            .spawn()
            .expect("prebuilt helper chain should start");
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            helper.try_wait().expect("inspect helper chain").is_none(),
            "an inherited $CARGO invocation must wait for canonical validation"
        );

        drop(lease);
        let output = helper
            .wait_with_output()
            .expect("helper chain should resume");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "test --locked"
        );
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn cargo_root_shim_preserves_cargo_owned_build_script_semantics() {
        let run_root = unique_temp_dir("loop-worker-env-cargo-semantics");
        let project_root = run_root.join("project");
        let source_root = project_root.join("src");
        fs::create_dir_all(&source_root).expect("failed to create test project");
        fs::write(
            project_root.join("Cargo.toml"),
            "[package]\nname = \"cargo-env-semantics\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("failed to write test manifest");
        fs::write(
            project_root.join("build.rs"),
            "fn main() { println!(\"cargo:rustc-env=BUILD_SCRIPT_CARGO={}\", std::env::var(\"CARGO\").unwrap()); }\n",
        )
        .expect("failed to write build script");
        fs::write(
            source_root.join("main.rs"),
            "fn main() { println!(\"{}\", env!(\"BUILD_SCRIPT_CARGO\")); }\n",
        )
        .expect("failed to write test program");

        let mut worker_env = vec![("CARGO".to_string(), "/inherited/real/cargo".to_string())];
        install_parallel_worker_cargo_lease(&mut worker_env, &run_root)
            .expect("cargo lease should install");
        let env_value = |key: &str| {
            worker_env
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("missing {key}"))
        };
        let guard_path = PathBuf::from(env_value("CARGO"));
        assert_eq!(
            guard_path,
            run_root.join(WORKER_GIT_GUARD_DIR).join("cargo")
        );

        let output = Command::new(&guard_path)
            .arg("run")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(project_root.join("Cargo.toml"))
            .envs(worker_env.iter().map(|(key, value)| (key, value)))
            .env(
                WORKER_LANE_CARGO_LEASE_ENV,
                run_root.join("cargo-semantics-lane.lock"),
            )
            .env("CARGO_TARGET_DIR", run_root.join("cargo-target"))
            .output()
            .expect("guarded Cargo should run the test project");
        assert!(
            output.status.success(),
            "guarded Cargo failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let build_script_cargo = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        assert!(build_script_cargo.is_absolute());
        assert_ne!(
            build_script_cargo, guard_path,
            "Cargo must retain ownership of CARGO inside build scripts"
        );
        let version = Command::new(&build_script_cargo)
            .arg("--version")
            .output()
            .expect("build-script CARGO should be executable");
        assert!(version.status.success());

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[tokio::test]
    async fn canonical_validation_waits_for_running_lane_cargo_to_drain() {
        let run_root = unique_temp_dir("loop-worker-env-cargo-drain");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let guard_path = run_root.join("cargo");
        atomic_write(&guard_path, worker_cargo_lease_script().as_bytes())
            .expect("failed to write cargo guard");
        make_executable(&guard_path).expect("failed to chmod cargo guard");
        let fake_cargo = run_root.join("fake-real-cargo");
        atomic_write(
            &fake_cargo,
            b"#!/bin/sh\nprintf started > \"$AUTO_TEST_CARGO_MARKER\"\nsleep 0.3\n",
        )
        .expect("failed to write fake cargo");
        make_executable(&fake_cargo).expect("failed to chmod fake cargo");
        let marker = run_root.join("cargo-started");

        let mut worker = Command::new(&guard_path)
            .env("AUTO_REAL_CARGO", &fake_cargo)
            .env("AUTO_PARALLEL_FLOCK", "/usr/bin/flock")
            .env(
                "AUTO_PARALLEL_VALIDATION_LEASE",
                validation_lease_path(&run_root),
            )
            .env(
                WORKER_LANE_CARGO_LEASE_ENV,
                run_root.join("lane-1-cargo.lock"),
            )
            .env("AUTO_TEST_CARGO_MARKER", &marker)
            .spawn()
            .expect("cargo guard should start");
        for _ in 0..50 {
            if marker.is_file() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.is_file(), "fake lane cargo never started");

        let started = Instant::now();
        let lease = acquire_exclusive_validation_lease(&run_root)
            .await
            .expect("canonical lease should wait then lock");
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "canonical validation must wait for an existing lane cargo command"
        );
        drop(lease);
        assert!(worker.wait().expect("fake cargo should exit").success());
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn cargo_guard_serializes_concurrent_commands_from_the_same_lane() {
        let run_root = unique_temp_dir("loop-worker-env-cargo-same-lane");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let guard_path = run_root.join("cargo");
        atomic_write(&guard_path, worker_cargo_lease_script().as_bytes())
            .expect("failed to write cargo guard");
        make_executable(&guard_path).expect("failed to chmod cargo guard");
        let fake_cargo = run_root.join("fake-real-cargo");
        atomic_write(
            &fake_cargo,
            b"#!/bin/sh\nmkdir -p \"$AUTO_TEST_STARTED\"\ntouch \"$AUTO_TEST_STARTED/$AUTO_TEST_ID\"\nwhile [ ! -e \"$AUTO_TEST_RELEASE\" ]; do sleep 0.01; done\n",
        )
        .expect("failed to write fake cargo");
        make_executable(&fake_cargo).expect("failed to chmod fake cargo");
        let started = run_root.join("started");
        let release = run_root.join("release");
        let lane_lease = lane_cargo_lease_path(&run_root.join("lanes/lane-1"));
        fs::create_dir_all(lane_lease.parent().expect("lane lease parent"))
            .expect("create lane root");

        let mut first_command = cargo_guard_test_command(
            &guard_path,
            &fake_cargo,
            &run_root,
            &lane_lease,
            &started,
            &release,
            "first",
        );
        let mut first = spawn_retrying_etxtbsy(&mut first_command, "first cargo guard");
        wait_for_test_path(&started.join("first"));

        let mut second_command = cargo_guard_test_command(
            &guard_path,
            &fake_cargo,
            &run_root,
            &lane_lease,
            &started,
            &release,
            "second",
        );
        let mut second = spawn_retrying_etxtbsy(&mut second_command, "second cargo guard");
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !started.join("second").exists(),
            "same-lane Cargo must wait outside the real Cargo process"
        );

        fs::write(&release, "go\n").expect("release fake cargo");
        assert!(first.wait().expect("first cargo should exit").success());
        assert!(second.wait().expect("second cargo should exit").success());
        assert!(
            started.join("second").exists(),
            "queued same-lane Cargo must run after its predecessor"
        );
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn cargo_guard_keeps_different_lanes_concurrent() {
        let run_root = unique_temp_dir("loop-worker-env-cargo-cross-lane");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let guard_path = run_root.join("cargo");
        atomic_write(&guard_path, worker_cargo_lease_script().as_bytes())
            .expect("failed to write cargo guard");
        make_executable(&guard_path).expect("failed to chmod cargo guard");
        let fake_cargo = run_root.join("fake-real-cargo");
        atomic_write(
            &fake_cargo,
            b"#!/bin/sh\nmkdir -p \"$AUTO_TEST_STARTED\"\ntouch \"$AUTO_TEST_STARTED/$AUTO_TEST_ID\"\nwhile [ ! -e \"$AUTO_TEST_RELEASE\" ]; do sleep 0.01; done\n",
        )
        .expect("failed to write fake cargo");
        make_executable(&fake_cargo).expect("failed to chmod fake cargo");
        let started = run_root.join("started");
        let release = run_root.join("release");
        fs::create_dir_all(run_root.join("lanes/lane-1")).expect("create lane one root");
        fs::create_dir_all(run_root.join("lanes/lane-2")).expect("create lane two root");

        let mut first_command = cargo_guard_test_command(
            &guard_path,
            &fake_cargo,
            &run_root,
            &lane_cargo_lease_path(&run_root.join("lanes/lane-1")),
            &started,
            &release,
            "first",
        );
        let mut first = spawn_retrying_etxtbsy(&mut first_command, "first lane cargo");
        let mut second_command = cargo_guard_test_command(
            &guard_path,
            &fake_cargo,
            &run_root,
            &lane_cargo_lease_path(&run_root.join("lanes/lane-2")),
            &started,
            &release,
            "second",
        );
        let mut second = spawn_retrying_etxtbsy(&mut second_command, "second lane cargo");
        wait_for_test_path(&started.join("first"));
        wait_for_test_path(&started.join("second"));

        fs::write(&release, "go\n").expect("release fake cargo");
        assert!(first.wait().expect("first cargo should exit").success());
        assert!(second.wait().expect("second cargo should exit").success());
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    fn cargo_guard_test_command(
        guard_path: &Path,
        fake_cargo: &Path,
        run_root: &Path,
        lane_lease: &Path,
        started: &Path,
        release: &Path,
        id: &str,
    ) -> Command {
        let mut command = Command::new(guard_path);
        command
            .env("AUTO_REAL_CARGO", fake_cargo)
            .env("AUTO_PARALLEL_FLOCK", "/usr/bin/flock")
            .env(
                "AUTO_PARALLEL_VALIDATION_LEASE",
                validation_lease_path(run_root),
            )
            .env(WORKER_LANE_CARGO_LEASE_ENV, lane_lease)
            .env("AUTO_TEST_STARTED", started)
            .env("AUTO_TEST_RELEASE", release)
            .env("AUTO_TEST_ID", id);
        command
    }

    fn spawn_retrying_etxtbsy(command: &mut Command, description: &str) -> std::process::Child {
        for attempt in 0..20 {
            match command.spawn() {
                Ok(child) => return child,
                Err(err) if err.raw_os_error() == Some(26) && attempt < 19 => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("{description} should start: {err}"),
            }
        }
        unreachable!("bounded spawn retry must return or panic")
    }

    fn wait_for_test_path(path: &Path) {
        for _ in 0..100 {
            if path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {}", path.display());
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
    fn loop_worker_env_uses_lane_local_target_for_multi_lane_rust_runs() {
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
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert!(
            env.cargo_target_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("lane-local")),
            "multi-lane Rust runs should not inherit shared CARGO_TARGET_DIR"
        );
        assert!(env.lane_local_cargo_target);

        let single_lane = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            None,
            Some("/tmp/shared-target"),
            22,
            1,
            true,
            &run_root,
        )
        .expect("worker env should resolve");
        assert_eq!(
            single_lane.extra_env,
            vec![
                (
                    "CARGO_TARGET_DIR".to_string(),
                    "/tmp/shared-target".to_string()
                ),
                ("CARGO_BUILD_JOBS".to_string(), "4".to_string())
            ]
        );
        assert_eq!(
            single_lane.cargo_target_summary,
            Some("/tmp/shared-target".to_string())
        );
        assert!(!single_lane.lane_local_cargo_target);

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn parallel_claude_has_no_implicit_turn_budget() {
        let args = ParallelArgs {
            action: None,
            apply_receipt_backfill_handoffs: false,
            json: false,
            apply: false,
            include_caches: false,
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
