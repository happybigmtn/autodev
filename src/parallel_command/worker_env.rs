use super::*;

pub(crate) fn parallel_run_root(repo_root: &Path, args: &ParallelArgs) -> PathBuf {
    match args.run_root.as_deref() {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => repo_root.join(path),
        None => repo_root.join(".auto").join("parallel"),
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
    resolve_loop_worker_env(
        args.cargo_build_jobs,
        args.cargo_target,
        inherited.as_deref(),
        inherited_target.as_deref(),
        parallelism,
        args.max_concurrent_workers,
        repo_uses_cargo(repo_root),
        run_root,
    )
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
    fn parallel_run_root_resolves_relative_override_under_repo_root() {
        let args = ParallelArgs {
            action: None,
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
