use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use regex::Regex;
use tokio::task::JoinSet;

use crate::claude_exec::{describe_claude_harness, run_claude_exec_with_env, FUTILITY_EXIT_MARKER};
use crate::codex_exec::run_codex_exec_with_env;
use crate::completion_artifacts::{
    assess_task_completion_gap, ensure_host_review_handoff, inspect_task_completion_evidence,
    verification_plan, CompletionGapKind,
};
use crate::linear_tracker::LinearTracker;
use crate::symphony_command::run_sync;
use crate::task_parser::{
    parse_task_header as parse_shared_task_header, parse_tasks as parse_shared_tasks,
    PlanTask as SharedPlanTask, TaskStatus as SharedTaskStatus,
};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root,
    git_status_short_filtered, git_stdout, push_branch_with_remote_sync, repo_name, run_git,
    sync_branch_with_remote, timestamp_slug,
};
use crate::{ParallelAction, ParallelArgs, ParallelCargoTarget, SymphonySyncArgs};

const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];
const SHARED_QUEUE_FILES: [&str; 5] = [
    "IMPLEMENTATION_PLAN.md",
    "COMPLETED.md",
    "WORKLIST.md",
    "REVIEW.md",
    "AGENTS.md",
];
const HOST_QUEUE_STATE_FILES: [&str; 5] = [
    "IMPLEMENTATION_PLAN.md",
    "COMPLETED.md",
    "WORKLIST.md",
    "REVIEW.md",
    "ARCHIVED.md",
];
const LANE_POLL_INTERVAL: Duration = Duration::from_secs(5);
const CLEAN_COMMIT_GRACE: Duration = Duration::from_secs(15);
const CLEAN_COMMIT_KILL_GRACE: Duration = Duration::from_secs(5);
const SALVAGE_DIR: &str = "salvage";
const DIRECT_REVIEW_QUEUE_PARALLEL_CLAUSE: &str = r#"

Repo-specific direct `REVIEW.md` handoff:
- This repo normally records completion notes in `REVIEW.md`, but `auto parallel` treats queue and review files as host-owned state.
- Do not edit `REVIEW.md`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `WORKLIST.md`, or `ARCHIVED.md` from a lane.
- Preserve blocker or completion evidence in your committed code/tests and command output; the host will reconcile queue and review docs after landing."#;
const LANE_TASK_ID_FILE: &str = "task-id";

pub(crate) async fn run_parallel(args: ParallelArgs) -> Result<()> {
    if args.action == Some(ParallelAction::Status) {
        return run_parallel_status(&args);
    }

    if args.max_concurrent_workers == 0 {
        bail!("--max-concurrent-workers must be greater than 0");
    }
    if args.claude && args.max_turns == Some(0) {
        bail!("--max-turns must be greater than 0");
    }

    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let run_root = parallel_run_root(&repo_root, &args);
    let reference_repos =
        resolve_reference_repos(&repo_root, &args.reference_repos, args.include_siblings)?;

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    let target_branch = resolve_loop_branch(&repo_root, args.branch.as_deref(), &current_branch)?;
    if current_branch != target_branch {
        bail!(
            "auto parallel must run on branch `{}` (current: `{}`)",
            target_branch,
            current_branch
        );
    }
    if args.max_concurrent_workers > 1 && should_launch_parallel_tmux(&args) {
        fs::create_dir_all(&run_root)
            .with_context(|| format!("failed to create {}", run_root.display()))?;
        log_parallel_startup_prep(
            prepare_parallel_startup(&repo_root, target_branch.as_str())?,
            target_branch.as_str(),
        );
        let session_name = parallel_tmux_session_name(&repo_root);
        match launch_parallel_tmux_session(&session_name, &run_root)? {
            TmuxLaunchStatus::Launched => {
                println!("auto parallel launched tmux session `{session_name}`");
            }
            TmuxLaunchStatus::AlreadyRunning => {
                println!("auto parallel tmux session `{session_name}` is already running");
            }
        }
        println!("attach: tmux attach -t {session_name}");
        return Ok(());
    }

    let mut prompt_template = match &args.prompt_file {
        Some(path) => {
            let prompt = fs::read_to_string(path)
                .with_context(|| format!("failed to read prompt file {}", path.display()))?;
            append_reference_repo_clause(prompt, &reference_repos)
        }
        None => render_default_parallel_prompt(&target_branch, &reference_repos),
    };
    if repo_forbids_legacy_review_trackers(&repo_root) {
        prompt_template.push_str(DIRECT_REVIEW_QUEUE_PARALLEL_CLAUSE);
    }
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let parallel_logger = ParallelEventLogger::new(&run_root)?;
    if args.max_concurrent_workers > 1 {
        setup_parallel_tmux_windows(&run_root, args.max_concurrent_workers, std::process::id())?;
    }
    let worker_env = build_loop_worker_env(&args, &repo_root, &run_root)?;
    let mut linear_tracker = match LinearTracker::maybe_from_repo(&repo_root).await {
        Ok(Some(tracker)) => Some(tracker),
        Ok(None) => None,
        Err(err) => {
            eprintln!("warning: Linear adapter disabled: {err:#}");
            None
        }
    };

    println!("auto parallel");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", target_branch);
    if args.claude {
        println!(
            "harness:     {}",
            describe_claude_harness(&args.model, &args.reasoning_effort)
        );
        println!(
            "max turns:   {}",
            effective_parallel_claude_max_turns(&args)
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unlimited".to_string())
        );
        println!("max retries: {}", args.max_retries);
    } else {
        println!("model:       {}", args.model);
        println!("reasoning:   {}", args.reasoning_effort);
    }
    println!("run root:    {}", run_root.display());
    if args.max_concurrent_workers > 1 {
        println!(
            "mode:        auto parallel ({} workers)",
            args.max_concurrent_workers
        );
    } else {
        println!("mode:        auto parallel (single lane)");
    }
    println!("cargo jobs:  {}", worker_env.cargo_jobs_summary);
    if let Some(target_summary) = &worker_env.cargo_target_summary {
        println!("cargo target: {}", target_summary);
    }
    println!(
        "linear:      {}",
        linear_tracker
            .as_ref()
            .map(LinearTracker::summary)
            .unwrap_or_else(|| "disabled".to_string())
    );
    if !reference_repos.is_empty() {
        println!("references:  {}", reference_repos.len());
        for path in &reference_repos {
            println!("  - {}", path.display());
        }
    }
    println!(
        "prompt:      {}",
        args.prompt_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "built-in Ralph worker".to_string())
    );

    log_parallel_startup_prep(
        prepare_parallel_startup(&repo_root, target_branch.as_str())?,
        target_branch.as_str(),
    );

    if args.max_concurrent_workers > 1 {
        run_parallel_loop(
            &repo_root,
            &args,
            &target_branch,
            &prompt_template,
            &run_root,
            &worker_env,
            &mut linear_tracker,
            &parallel_logger,
        )
        .await
    } else {
        run_serial_loop(
            &repo_root,
            &reference_repos,
            &args,
            &target_branch,
            &prompt_template,
            &run_root,
            &worker_env,
        )
        .await
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ParallelStartupPrep {
    Checkpointed(String),
    RemoteSynced,
    Noop,
}

fn prepare_parallel_startup(repo_root: &Path, target_branch: &str) -> Result<ParallelStartupPrep> {
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

fn log_parallel_startup_prep(prep: ParallelStartupPrep, target_branch: &str) {
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

fn parallel_run_root(repo_root: &Path, args: &ParallelArgs) -> PathBuf {
    match args.run_root.as_deref() {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => repo_root.join(path),
        None => repo_root.join(".auto").join("parallel"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoopWorkerEnv {
    extra_env: Vec<(String, String)>,
    cargo_jobs_summary: String,
    cargo_target_summary: Option<String>,
    lane_local_cargo_target: bool,
    cargo_target_prompt_clause: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LoopQueueSnapshot {
    pending_ids: Vec<String>,
    blocked_ids: Vec<String>,
}

fn build_iteration_prompt(prompt_template: &str, queue: &LoopQueueSnapshot) -> String {
    let blocked_clause = if queue.blocked_ids.is_empty() {
        "Blocked tasks marked `- [!]`: none".to_string()
    } else {
        format!(
            "Blocked tasks marked `- [!]` to skip this iteration: {}",
            queue.blocked_ids.join(", ")
        )
    };
    format!(
        "{prompt_template}\n\nCurrent queue state for this iteration:\n- First actionable unfinished task: `{}`\n- Unfinished task count: {}\n- {}\n\nExecute the instructions above.",
        queue.pending_ids[0],
        queue.pending_ids.len(),
        blocked_clause
    )
}

fn build_loop_worker_env(
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
fn resolve_loop_worker_env(
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

fn cargo_build_jobs_env(
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

fn inherited_target_loop_worker_env(
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
enum ParallelCargoTargetLayout {
    None,
    Fixed(String),
    LaneLocal,
}

fn resolve_parallel_cargo_target_layout(
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

fn cargo_target_prompt_clause(lane_local: bool, summary: Option<&str>) -> String {
    if lane_local {
        return "Use the host-provided `CARGO_TARGET_DIR`; this run gives each lane its own target directory, so final proofs should go through `cargo test` or the repo's verification wrapper rather than direct binaries from another lane. Do not override it.".to_string();
    }
    if summary.is_some() {
        return "Use the host-provided `CARGO_TARGET_DIR`. If Cargo is busy, wait or narrow the proof instead of switching target directories. Do not use direct target-dir test binaries as proof unless you just built that exact artifact from this lane's source tree.".to_string();
    }
    "Use the repo's normal Cargo target behavior. Do not create ad hoc target directories unless the task explicitly requires isolation, and prefer `cargo test` or the repo's verification wrapper for final proof.".to_string()
}

fn repo_uses_cargo(repo_root: &Path) -> bool {
    repo_root.join("Cargo.toml").exists()
}

fn effective_parallel_claude_max_turns(args: &ParallelArgs) -> Option<usize> {
    args.max_turns
}

fn default_cargo_build_jobs_for(
    available_parallelism: usize,
    max_concurrent_workers: usize,
) -> usize {
    let available_parallelism = available_parallelism.max(1);
    let workers = max_concurrent_workers.max(1);
    (available_parallelism / (workers + 1)).clamp(1, 4)
}

fn read_loop_plan(repo_root: &Path) -> Result<String> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoopTaskStatus {
    Pending,
    Blocked,
    Partial,
    Done,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoopTask {
    id: String,
    title: String,
    status: LoopTaskStatus,
    dependencies: Vec<String>,
    estimated_scope: Option<String>,
    completion_path_target: Option<String>,
    markdown: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LoopPlanSnapshot {
    tasks: Vec<LoopTask>,
}

impl LoopPlanSnapshot {
    fn task(&self, task_id: &str) -> Option<&LoopTask> {
        self.tasks.iter().find(|task| task.id == task_id)
    }

    fn queue_snapshot(&self) -> LoopQueueSnapshot {
        let mut queue = LoopQueueSnapshot::default();
        for task in &self.tasks {
            match task.status {
                LoopTaskStatus::Pending => queue.pending_ids.push(task.id.clone()),
                LoopTaskStatus::Partial => {
                    if !self.is_completion_path_placeholder(task) {
                        queue.pending_ids.push(task.id.clone());
                    }
                }
                LoopTaskStatus::Blocked => queue.blocked_ids.push(task.id.clone()),
                LoopTaskStatus::Done => {}
            }
        }
        queue
    }

    fn ready_tasks(&self, inflight: &BTreeSet<String>) -> Vec<LoopTask> {
        let unresolved = self.unresolved_dependency_ids(inflight);

        self.tasks
            .iter()
            .filter(|task| self.is_actionable_unfinished(task))
            .filter(|task| !inflight.contains(&task.id))
            .filter(|task| {
                task.dependencies
                    .iter()
                    .all(|dep| !unresolved.contains(dep))
            })
            .cloned()
            .collect()
    }

    fn is_actionable_unfinished(&self, task: &LoopTask) -> bool {
        matches!(
            task.status,
            LoopTaskStatus::Pending | LoopTaskStatus::Partial
        ) && !self.is_completion_path_placeholder(task)
    }

    fn unresolved_dependency_ids(&self, inflight: &BTreeSet<String>) -> BTreeSet<String> {
        let mut unresolved = self
            .tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.status,
                    LoopTaskStatus::Pending | LoopTaskStatus::Blocked | LoopTaskStatus::Partial
                )
            })
            .filter(|task| !self.is_completion_path_placeholder(task))
            .map(|task| task.id.clone())
            .chain(inflight.iter().cloned())
            .collect::<BTreeSet<_>>();

        for task in &self.tasks {
            let Some(target_id) = self.completion_path_target(task) else {
                continue;
            };
            if unresolved.contains(target_id) {
                unresolved.insert(task.id.clone());
            }
        }

        unresolved
    }

    fn completion_path_target<'a>(&'a self, task: &'a LoopTask) -> Option<&'a str> {
        if task.status != LoopTaskStatus::Partial {
            return None;
        }
        let target = task.completion_path_target.as_deref()?;
        if target == task.id {
            return None;
        }
        self.tasks
            .iter()
            .any(|candidate| candidate.id == target)
            .then_some(target)
    }

    fn is_completion_path_placeholder(&self, task: &LoopTask) -> bool {
        self.completion_path_target(task).is_some()
    }

    fn direct_unfinished_dependents(&self, task_id: &str) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|task| self.is_actionable_unfinished(task))
            .filter(|task| task.id != task_id)
            .filter(|task| task.dependencies.iter().any(|dep| dep == task_id))
            .map(|task| task.id.clone())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParallelBlockerKind {
    Pending,
    Blocked,
    Shelved,
    DeferredPartial,
    InFlight,
}

impl ParallelBlockerKind {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Shelved => "shelved",
            Self::DeferredPartial => "deferred-partial",
            Self::InFlight => "in-flight",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParallelBlockerDetail {
    task_id: String,
    kind: ParallelBlockerKind,
    downstream: Vec<String>,
}

fn parse_loop_plan(plan: &str) -> LoopPlanSnapshot {
    LoopPlanSnapshot {
        tasks: parse_shared_tasks(plan)
            .into_iter()
            .map(finalize_task)
            .collect(),
    }
}

fn finalize_task(task: SharedPlanTask) -> LoopTask {
    let SharedPlanTask {
        id,
        title,
        status,
        dependencies,
        completion_path_target,
        markdown,
        ..
    } = task;
    let mut status = loop_task_status(status);
    if matches!(
        status,
        LoopTaskStatus::Pending | LoopTaskStatus::Blocked | LoopTaskStatus::Partial
    ) {
        if task_is_deferred_not_shipped_placeholder(&title, &markdown) {
            status = LoopTaskStatus::Blocked;
        } else if matches!(status, LoopTaskStatus::Pending | LoopTaskStatus::Blocked)
            && task_is_non_actionable_placeholder(&title, &markdown)
        {
            status = LoopTaskStatus::Done;
        }
    }
    LoopTask {
        id,
        title,
        status,
        dependencies,
        estimated_scope: task_field_line_value(&markdown, "Estimated scope:"),
        completion_path_target,
        markdown,
    }
}

fn loop_task_status(status: SharedTaskStatus) -> LoopTaskStatus {
    match status {
        SharedTaskStatus::Pending => LoopTaskStatus::Pending,
        SharedTaskStatus::Blocked => LoopTaskStatus::Blocked,
        SharedTaskStatus::Partial => LoopTaskStatus::Partial,
        SharedTaskStatus::Done => LoopTaskStatus::Done,
    }
}

fn task_is_non_actionable_placeholder(title: &str, markdown: &str) -> bool {
    if title
        .trim()
        .to_ascii_lowercase()
        .starts_with("merged into ")
    {
        return true;
    }

    markdown.lines().any(|line| {
        let Some(rest) = line.trim_start().strip_prefix("Status:") else {
            return false;
        };
        let rest = rest.to_ascii_lowercase();
        rest.contains("placeholder") || rest.contains("merged into")
    })
}

fn task_is_deferred_not_shipped_placeholder(title: &str, markdown: &str) -> bool {
    std::iter::once(title).chain(markdown.lines()).any(|line| {
        let normalized = line
            .chars()
            .map(|ch| if ch.is_ascii_punctuation() { ' ' } else { ch })
            .collect::<String>()
            .to_ascii_lowercase();
        normalized.contains("deferred") && normalized.contains("not shipped")
    })
}

fn parse_task_header(line: &str) -> Option<(LoopTaskStatus, String, String)> {
    let (status, id, title) = parse_shared_task_header(line)?;
    Some((loop_task_status(status), id, title))
}

fn strip_list_bullet(line: &str) -> &str {
    let trimmed = line.trim_start();
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return rest;
        }
    }
    trimmed
}

fn task_field_line_value(markdown: &str, field: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        strip_list_bullet(line)
            .strip_prefix(field)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
    })
}

fn task_field_body(markdown: &str, field: &str, next_field: &str) -> Option<String> {
    let mut collecting = false;
    let mut body = Vec::new();
    for line in markdown.lines() {
        let unbulleted = strip_list_bullet(line);
        if let Some(rest) = unbulleted.strip_prefix(field) {
            collecting = true;
            if !rest.trim().is_empty() {
                body.push(rest.trim().to_string());
            }
            continue;
        }
        if collecting && unbulleted.starts_with(next_field) {
            break;
        }
        if collecting {
            body.push(line.to_string());
        }
    }
    collecting.then(|| body.join("\n"))
}

#[derive(Clone, Debug)]
struct LaneRunConfig {
    claude: bool,
    max_turns: Option<usize>,
    model: String,
    reasoning_effort: String,
    codex_bin: PathBuf,
    extra_env: Vec<(String, String)>,
    lane_local_cargo_target: bool,
    cargo_target_prompt_clause: String,
    preflight_prompt_clause: String,
}

impl LaneRunConfig {
    fn new(
        args: &ParallelArgs,
        worker_env: &LoopWorkerEnv,
        preflight_prompt_clause: String,
    ) -> Self {
        Self {
            claude: args.claude,
            max_turns: effective_parallel_claude_max_turns(args),
            model: args.model.clone(),
            reasoning_effort: args.reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            extra_env: worker_env.extra_env.clone(),
            lane_local_cargo_target: worker_env.lane_local_cargo_target,
            cargo_target_prompt_clause: worker_env.cargo_target_prompt_clause.clone(),
            preflight_prompt_clause,
        }
    }

    fn env_for_lane(&self, lane_root: &Path) -> Vec<(String, String)> {
        let mut extra_env = self.extra_env.clone();
        if self.lane_local_cargo_target {
            extra_env.push((
                "CARGO_TARGET_DIR".to_string(),
                lane_root
                    .join("cargo-target")
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        extra_env
    }
}

#[derive(Clone, Debug)]
struct ActiveLaneAssignment {
    lane_index: usize,
    attempts: usize,
    task: LoopTask,
    resumed: bool,
    lane_root: PathBuf,
    lane_repo_root: PathBuf,
    base_commit: String,
    stdout_log_path: PathBuf,
    stderr_log_path: PathBuf,
    worker_pid_path: PathBuf,
    clean_commit_since: Option<Instant>,
    terminate_requested_at: Option<Instant>,
    host_recovery_note: Option<String>,
}

#[derive(Clone, Debug)]
struct LaneResumeCandidate {
    lane_index: usize,
    task: LoopTask,
    lane_root: PathBuf,
    lane_repo_root: PathBuf,
    base_commit: String,
    stdout_log_path: PathBuf,
    stderr_log_path: PathBuf,
    worker_pid_path: PathBuf,
    host_recovery_note: Option<String>,
}

#[derive(Debug)]
struct LaneAttemptResult {
    lane_index: usize,
    exit_status: Option<ExitStatus>,
    error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LaneRepoProgress {
    None,
    Dirty(String),
    NewCommits,
    NewCommitsWithDirty(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CherryPickFailurePolicy {
    Abort,
    LeaveInProgress,
}

#[derive(Debug, Eq, PartialEq)]
enum LaneLandingOutcome {
    Landed {
        auto_repaired: bool,
        completion_status: LoopTaskStatus,
    },
    NeedsRecovery(String),
}

#[derive(Debug, Eq, PartialEq)]
enum LaneLandingRecoveryPrep {
    RebasedCleanly,
    NeedsWorkerResolution(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LinearAutoSyncState {
    disabled_reason: Option<String>,
}

impl LinearAutoSyncState {
    fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    fn disable_for_run(&mut self, reason: impl Into<String>) -> bool {
        if self.disabled_reason.is_some() {
            return false;
        }
        self.disabled_reason = Some(reason.into());
        true
    }
}

#[derive(Clone, Debug)]
struct ParallelEventLogger {
    live_log_path: PathBuf,
}

impl ParallelEventLogger {
    fn new(run_root: &Path) -> Result<Self> {
        let live_log_path = run_root.join("live.log");
        fs::write(&live_log_path, b"")
            .with_context(|| format!("failed to initialize {}", live_log_path.display()))?;
        Ok(Self { live_log_path })
    }

    fn info(&self, message: impl AsRef<str>) {
        let message = message.as_ref();
        println!("{message}");
        if let Err(err) = self.append(message) {
            eprintln!("warning: failed writing parallel live log: {err:#}");
        }
    }

    fn warn(&self, message: impl AsRef<str>) {
        let message = message.as_ref();
        eprintln!("{message}");
        if let Err(err) = self.append(message) {
            eprintln!("warning: failed writing parallel live log: {err:#}");
        }
    }

    fn append(&self, message: &str) -> Result<()> {
        let normalized = normalize_parallel_live_log_message(message);
        if normalized.is_empty() {
            return Ok(());
        }
        let redacted = redact_parallel_live_log_message(&normalized);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.live_log_path)
            .with_context(|| format!("failed to open {}", self.live_log_path.display()))?;
        writeln!(file, "{redacted}")
            .with_context(|| format!("failed to append {}", self.live_log_path.display()))
    }
}

fn append_lane_host_event(log_path: &Path, lane_index: usize, task_id: &str, message: &str) {
    let rendered = format!(
        "[auto parallel host lane-{lane_index} {task_id}] {message}",
        lane_index = lane_index,
        task_id = task_id,
        message = message.trim()
    );
    if let Err(err) = append_lane_log_line(log_path, &rendered) {
        eprintln!(
            "warning: failed appending lane host event to {}: {err:#}",
            log_path.display()
        );
    }
}

fn append_lane_log_line(log_path: &Path, line: &str) -> Result<()> {
    if line.trim().is_empty() {
        return Ok(());
    }
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("failed to append {}", log_path.display()))
}

fn append_idle_status_to_free_lanes(
    run_root: &Path,
    max_concurrent_workers: usize,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
    summary: &str,
) {
    for lane_index in 1..=max_concurrent_workers {
        if active_lanes.contains_key(&lane_index) {
            continue;
        }
        let lane_root = run_root.join("lanes").join(format!("lane-{lane_index}"));
        append_lane_host_event(
            &lane_root.join("stdout.log"),
            lane_index,
            "[idle]",
            &format!("idle: {summary}"),
        );
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ParallelPreflightReport {
    checks: Vec<ParallelPreflightCheck>,
}

impl ParallelPreflightReport {
    fn add(&mut self, status: PreflightStatus, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(ParallelPreflightCheck {
            status,
            name: name.into(),
            detail: detail.into(),
        });
    }

    fn prompt_clause(&self) -> String {
        self.checks
            .iter()
            .map(|check| {
                format!(
                    "- {} {}: {}",
                    check.status.label(),
                    check.name,
                    check.detail
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn summary(&self) -> String {
        let warnings = self
            .checks
            .iter()
            .filter(|check| check.status == PreflightStatus::Warn)
            .count();
        if warnings == 0 {
            format!("{} checks ok", self.checks.len())
        } else {
            format!("{} checks, {} warning(s)", self.checks.len(), warnings)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParallelPreflightCheck {
    status: PreflightStatus,
    name: String,
    detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreflightStatus {
    Ok,
    Warn,
}

impl PreflightStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
        }
    }
}

fn run_parallel_preflight(
    repo_root: &Path,
    plan: &LoopPlanSnapshot,
    run_root: &Path,
    parallel_logger: &ParallelEventLogger,
) -> Result<ParallelPreflightReport> {
    let mut report = ParallelPreflightReport::default();
    let task_text = plan
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                LoopTaskStatus::Pending | LoopTaskStatus::Partial
            )
        })
        .map(|task| format!("{} {}\n{}", task.id, task.title, task.markdown))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();

    let preflight_needs = classify_parallel_preflight_needs(&task_text, repo_root);

    if repo_uses_cargo(repo_root) {
        report.add(
            PreflightStatus::Ok,
            "cargo",
            "Rust workspace detected; worker Cargo target policy is included in every lane prompt",
        );
    }

    if preflight_needs.browser {
        if command_exists("agent-browser") {
            let socket = default_agent_browser_socket();
            if socket.exists() || try_warm_agent_browser_daemon(repo_root, &socket) {
                report.add(
                    PreflightStatus::Ok,
                    "agent-browser",
                    format!(
                        "CLI present and daemon socket is ready at {}",
                        socket.display()
                    ),
                );
            } else {
                report.add(
                    PreflightStatus::Warn,
                    "agent-browser",
                    format!(
                        "CLI present but daemon socket is still missing at {} after warm-up; browser lanes should report AUTO_ENV_BLOCKER if they cannot repair it",
                        socket.display()
                    ),
                );
            }
        } else {
            report.add(
                PreflightStatus::Warn,
                "agent-browser",
                "`agent-browser` is not on PATH; browser/e2e lanes may block",
            );
        }
    }

    if preflight_needs.docker {
        if !command_exists("docker") {
            report.add(
                PreflightStatus::Warn,
                "docker",
                "`docker` is not on PATH; Docker-backed smoke tests may block",
            );
        } else if repo_root.join("docker-compose.yml").exists()
            || repo_root.join("compose.yml").exists()
            || repo_root.join("compose.yaml").exists()
        {
            match command_stdout(repo_root, ["docker", "compose", "config", "--quiet"]) {
                Ok(_) => match command_stdout(
                    repo_root,
                    [
                        "docker",
                        "compose",
                        "ps",
                        "--services",
                        "--status",
                        "running",
                    ],
                ) {
                    Ok(services) if !services.trim().is_empty() => report.add(
                        PreflightStatus::Ok,
                        "docker compose",
                        format!(
                            "running services: {}",
                            services.lines().collect::<Vec<_>>().join(", ")
                        ),
                    ),
                    Ok(_) => report.add(
                        PreflightStatus::Warn,
                        "docker compose",
                        "compose config is valid but no services are currently running",
                    ),
                    Err(err) => report.add(
                        PreflightStatus::Warn,
                        "docker compose",
                        format!("could not inspect running services: {err}"),
                    ),
                },
                Err(err) => report.add(
                    PreflightStatus::Warn,
                    "docker compose",
                    format!("compose config check failed: {err}"),
                ),
            }
        } else {
            report.add(
                PreflightStatus::Warn,
                "docker compose",
                "tasks mention Docker or explicit regtest infrastructure but no compose file was found",
            );
        }
    }

    if preflight_needs.regtest {
        if command_exists("curl") {
            match command_stdout(
                repo_root,
                [
                    "curl",
                    "-sf",
                    "--max-time",
                    "2",
                    "http://127.0.0.1:18443/",
                    "-u",
                    "bitino:bitino",
                    "-H",
                    "content-type: application/json",
                    "--data",
                    "{\"jsonrpc\":\"1.0\",\"id\":\"auto-preflight\",\"method\":\"getblockchaininfo\",\"params\":[]}",
                ],
            ) {
                Ok(_) => report.add(
                    PreflightStatus::Ok,
                    "regtest rpc",
                    "127.0.0.1:18443 answered getblockchaininfo",
                ),
                Err(err) => report.add(
                    PreflightStatus::Warn,
                    "regtest rpc",
                    format!("127.0.0.1:18443 did not answer getblockchaininfo: {err}"),
                ),
            }
        } else {
            report.add(
                PreflightStatus::Warn,
                "regtest rpc",
                "`curl` is not on PATH; cannot probe local regtest RPC",
            );
        }
    }

    if report.checks.is_empty() {
        report.add(
            PreflightStatus::Ok,
            "general",
            "no browser, Docker, explicit regtest, or Cargo preflight checks were triggered by pending tasks",
        );
    }

    let rendered = report.prompt_clause();
    atomic_write(&run_root.join("preflight.txt"), rendered.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            run_root.join("preflight.txt").display()
        )
    })?;
    parallel_logger.info(format!("preflight:   {}", report.summary()));
    for check in &report.checks {
        if check.status == PreflightStatus::Warn {
            parallel_logger.warn(format!(
                "preflight:   warn {}: {}",
                check.name, check.detail
            ));
        }
    }
    Ok(report)
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn contains_term(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, _)| {
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_term_char(ch));
        let end = start + needle.len();
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|ch| !is_term_char(ch));
        before_ok && after_ok
    })
}

fn contains_any_term(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| contains_term(haystack, needle))
}

fn is_term_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParallelPreflightNeeds {
    browser: bool,
    docker: bool,
    regtest: bool,
}

fn classify_parallel_preflight_needs(task_text: &str, repo_root: &Path) -> ParallelPreflightNeeds {
    let browser = contains_any(
        task_text,
        &["agent-browser", "playwright", "browser", "e2e", "web"],
    );
    let regtest = contains_any(task_text, &["regtest", "rbtc-regtest", "bitcoin-regtest"]);
    let docker = contains_any_term(task_text, &["docker", "podman"])
        || contains_any(
            task_text,
            &[
                "docker compose",
                "docker-compose",
                "compose.yml",
                "compose.yaml",
            ],
        )
        || regtest
        || repo_root.join("docker-compose.yml").exists()
        || repo_root.join("compose.yml").exists()
        || repo_root.join("compose.yaml").exists();

    ParallelPreflightNeeds {
        browser,
        docker,
        regtest,
    }
}

fn command_exists(command: &str) -> bool {
    Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {}", shell_quote(command)))
        .output()
        .is_ok_and(|output| output.status.success())
}

fn command_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String> {
    let Some((program, rest)) = args.split_first() else {
        bail!("empty command");
    };
    let output = Command::new(program)
        .args(rest)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run `{}` in {}", args.join(" "), cwd.display()))?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    String::from_utf8(output.stdout).context("command stdout was not valid UTF-8")
}

fn default_agent_browser_socket() -> PathBuf {
    if let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir)
            .join("agent-browser")
            .join("default.sock");
    }
    let uid = command_stdout(Path::new("."), ["id", "-u"]).unwrap_or_else(|_| "1000".to_string());
    PathBuf::from("/run/user")
        .join(uid.trim())
        .join("agent-browser")
        .join("default.sock")
}

fn try_warm_agent_browser_daemon(repo_root: &Path, socket: &Path) -> bool {
    let open = Command::new("agent-browser")
        .args(["open", "about:blank"])
        .current_dir(repo_root)
        .output();
    if !open.as_ref().is_ok_and(|output| output.status.success()) {
        return false;
    }

    let _ = Command::new("agent-browser")
        .arg("close")
        .current_dir(repo_root)
        .output();
    socket.exists()
}

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
        let (worker_running, pid_state) = match read_worker_pid(&lane_root.join("worker.pid")) {
            Ok(Some(pid)) => match worker_pid_is_alive(pid) {
                Ok(true) => {
                    active_task_ids.insert(stored_task_id.clone());
                    (true, format!("running pid {pid}"))
                }
                Ok(false) => (false, format!("stale pid {pid}")),
                Err(err) => (false, format!("pid {pid} liveness unknown: {err:#}")),
            },
            Ok(None) => (false, "no worker pid".to_string()),
            Err(err) => (false, format!("worker pid unreadable: {err:#}")),
        };
        let repo_root = lane_root.join("repo");
        let recovery_active = lane_repo_has_active_cherry_pick(&repo_root)
            || lane_repo_has_rebase_recovery(&repo_root);
        if recovery_active {
            if no_live_parallel_host && !worker_running {
                stale_recovery_lanes.push(format!("lane-{lane_index} {stored_task_id}"));
            } else {
                active_recovery_lanes.push(format!("lane-{lane_index} {stored_task_id}"));
            }
        }
        let repo_status = lane_repo_status_summary(&repo_root);
        let (log_age, log_line) = latest_lane_log_line(&lane_root);
        let task_id = lane_status_task_id(&stored_task_id, worker_running, log_line.as_deref());
        println!(
            "  lane-{lane_index}: {task_id} | {pid_state} | {repo_status} | last log {log_age}"
        );
        if recovery_active && no_live_parallel_host && !worker_running {
            println!(
                "    recovery: stale recovery (no host pid or tmux session); not active progress"
            );
            println!("    recovery artifact: {}", repo_root.display());
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
    }
    println!(
        "health:      {}",
        render_parallel_health_summary(
            &preflight_warnings,
            &recent_host_warnings,
            &active_recovery_lanes,
            &stale_recovery_lanes,
        )
    );
    Ok(())
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

async fn run_serial_loop(
    repo_root: &Path,
    reference_repos: &[PathBuf],
    args: &ParallelArgs,
    target_branch: &str,
    prompt_template: &str,
    run_root: &Path,
    worker_env: &LoopWorkerEnv,
) -> Result<()> {
    let stderr_log_path = run_root.join("stderr.log");
    let harness = if args.claude { "Claude" } else { "Codex" };
    let mut iteration = 0usize;
    let mut consecutive_failures = 0usize;

    loop {
        if args.max_iterations.is_some_and(|limit| iteration >= limit) {
            println!(
                "reached max iterations: {}",
                args.max_iterations.unwrap_or_default()
            );
            break;
        }

        let plan = inspect_loop_plan(repo_root)?;
        let queue = plan.queue_snapshot();
        if queue.pending_ids.is_empty() {
            if queue.blocked_ids.is_empty() {
                println!("no unfinished `- [ ]` / `- [~]` tasks remain; stopping.");
            } else {
                println!(
                    "all remaining tasks are blocked `[!]`; stopping. blocked: {}",
                    queue.blocked_ids.join(", ")
                );
            }
            break;
        }

        let ready = plan.ready_tasks(&BTreeSet::new());
        if ready.is_empty() {
            println!(
                "no dependency-ready `- [ ]` tasks remain; stopping. blocked: {}",
                if queue.blocked_ids.is_empty() {
                    "none".to_string()
                } else {
                    queue.blocked_ids.join(", ")
                }
            );
            break;
        }

        let current_task = ready[0].id.clone();
        println!("next task:   {}", current_task);
        if !queue.blocked_ids.is_empty() {
            println!("blocked:     {}", queue.blocked_ids.join(", "));
        }

        let full_prompt = build_iteration_prompt(
            prompt_template,
            &LoopQueueSnapshot {
                pending_ids: ready.iter().map(|task| task.id.clone()).collect(),
                blocked_ids: queue.blocked_ids.clone(),
            },
        );

        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("loop-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        println!("prompt log:  {}", prompt_path.display());

        let state_before = collect_tracked_repo_states(repo_root, reference_repos)?;
        println!();
        println!("running {harness} iteration {}", iteration + 1);

        let exit_status = if args.claude {
            run_claude_exec_with_env(
                repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                args.max_turns,
                &stderr_log_path,
                None,
                "auto parallel",
                &worker_env.extra_env,
                None,
                None,
            )
            .await?
        } else {
            run_codex_exec_with_env(
                repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                &args.codex_bin,
                &stderr_log_path,
                None,
                "auto parallel",
                &worker_env.extra_env,
                None,
                None,
            )
            .await?
        };
        if !exit_status.success() {
            let exit_code = exit_status.code().unwrap_or(-1);
            let is_futility = exit_code == FUTILITY_EXIT_MARKER;
            consecutive_failures += 1;

            if let Some(commit) = auto_checkpoint_if_needed(
                repo_root,
                target_branch,
                &format!(
                    "auto parallel checkpoint (pre-retry {})",
                    consecutive_failures
                ),
            )? {
                println!("checkpoint:  committed partial changes at {commit}");
            }

            if consecutive_failures > args.max_retries {
                bail!(
                    "{harness} exited with status {} after {} consecutive failures; see {}",
                    if is_futility {
                        "futility".to_string()
                    } else {
                        exit_code.to_string()
                    },
                    consecutive_failures,
                    stderr_log_path.display()
                );
            }

            println!(
                "warning: {harness} exited non-zero ({}), retrying ({}/{})",
                if is_futility {
                    "futility spiral".to_string()
                } else {
                    format!("code {exit_code}")
                },
                consecutive_failures,
                args.max_retries
            );
            continue;
        }
        consecutive_failures = 0;

        println!();
        println!("{harness} iteration complete");

        let state_after = collect_tracked_repo_states(repo_root, reference_repos)?;
        match summarize_repo_progress(&state_before, &state_after) {
            RepoProgress::NewCommits => {}
            RepoProgress::DirtyChanges(repos) => {
                bail!(
                    "tracked repo changes were left uncommitted in: {}; commit or revert them before continuing",
                    repos.join(", ")
                );
            }
            RepoProgress::None => {
                if let Some(commit) =
                    auto_checkpoint_if_needed(repo_root, target_branch, "auto parallel checkpoint")?
                {
                    iteration += 1;
                    println!("checkpoint:  committed iteration changes at {commit}");
                    println!();
                    println!("================ LOOP {} ================", iteration);
                    continue;
                }
                println!("no new commit detected; stopping.");
                break;
            }
        }

        if push_branch_with_remote_sync(repo_root, target_branch)? {
            println!("remote sync: rebased onto origin/{}", target_branch);
        }
        if let Some(commit) =
            auto_checkpoint_if_needed(repo_root, target_branch, "auto parallel checkpoint")?
        {
            println!("checkpoint:  committed trailing changes at {commit}");
        }
        iteration += 1;
        println!();
        println!("================ LOOP {} ================", iteration);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_parallel_loop(
    repo_root: &Path,
    args: &ParallelArgs,
    target_branch: &str,
    prompt_template: &str,
    run_root: &Path,
    worker_env: &LoopWorkerEnv,
    linear_tracker: &mut Option<LinearTracker>,
    parallel_logger: &ParallelEventLogger,
) -> Result<()> {
    let harness = if args.claude { "Claude" } else { "Codex" };
    let mut join_set = JoinSet::<LaneAttemptResult>::new();
    let mut active_lanes = BTreeMap::<usize, ActiveLaneAssignment>::new();
    let mut active_tasks = BTreeSet::<String>::new();
    let mut shelved_tasks = BTreeMap::<String, String>::new();
    let mut attempted_partial_followups = BTreeSet::<String>::new();
    let mut deferred_partial_tasks = BTreeSet::<String>::new();
    let mut unblock_attempted_tasks = BTreeSet::<String>::new();
    let mut linear_auto_sync_state = LinearAutoSyncState::default();
    let mut landed = 0usize;
    let mut plan = refresh_parallel_plan(
        repo_root,
        linear_tracker,
        &mut linear_auto_sync_state,
        parallel_logger,
    )
    .await?;
    let preflight_report = run_parallel_preflight(repo_root, &plan, run_root, parallel_logger)?;
    let lane_config = LaneRunConfig::new(args, worker_env, preflight_report.prompt_clause());
    try_checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger);
    let mut resumable_lanes = discover_resume_candidates(run_root, target_branch, &plan)?;
    landed += harvest_resumable_lane_results(
        repo_root,
        target_branch,
        &mut resumable_lanes,
        &mut attempted_partial_followups,
        &mut deferred_partial_tasks,
        linear_tracker,
        parallel_logger,
    )
    .await?;
    plan = refresh_parallel_plan_or_last_good(
        repo_root,
        linear_tracker,
        &mut linear_auto_sync_state,
        &plan,
        parallel_logger,
    )
    .await;
    try_checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger);
    let mut rediscovered_lanes = discover_resume_candidates(run_root, target_branch, &plan)?;
    preserve_resume_recovery_notes(&mut rediscovered_lanes, &resumable_lanes);
    resumable_lanes = rediscovered_lanes;
    let mut last_idle_summary = None::<String>;

    loop {
        nudge_lingering_committed_lanes(&mut active_lanes);
        plan = refresh_parallel_plan_or_last_good(
            repo_root,
            linear_tracker,
            &mut linear_auto_sync_state,
            &plan,
            parallel_logger,
        )
        .await;
        try_checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger);
        shelved_tasks.retain(|task_id, markdown| {
            plan.tasks
                .iter()
                .find(|task| task.id == *task_id)
                .is_some_and(|task| task.markdown == *markdown)
        });
        attempted_partial_followups.retain(|task_id| {
            plan.tasks
                .iter()
                .find(|task| task.id == *task_id)
                .is_some_and(|task| task.status == LoopTaskStatus::Partial)
        });
        deferred_partial_tasks.retain(|task_id| {
            plan.tasks
                .iter()
                .find(|task| task.id == *task_id)
                .is_some_and(|task| task.status == LoopTaskStatus::Partial)
        });

        if args
            .max_iterations
            .is_some_and(|limit| landed >= limit && active_lanes.is_empty())
        {
            println!(
                "reached max iterations: {}",
                args.max_iterations.unwrap_or_default()
            );
            break;
        }

        loop {
            let available_slots = args
                .max_concurrent_workers
                .saturating_sub(active_lanes.len());
            if available_slots == 0 {
                break;
            }
            let remaining_budget = args
                .max_iterations
                .map(|limit| limit.saturating_sub(landed + active_lanes.len()))
                .unwrap_or(usize::MAX);
            if remaining_budget == 0 {
                break;
            }

            let queue = plan.queue_snapshot();
            if queue.pending_ids.is_empty() {
                break;
            }

            let ready = prioritize_ready_parallel_tasks(
                repo_root,
                ready_parallel_tasks(
                    &plan,
                    &active_tasks,
                    &shelved_tasks,
                    &deferred_partial_tasks,
                ),
            );
            if ready.is_empty() {
                if let Some(candidate) = next_parallel_unblock_candidate(
                    &plan,
                    &active_tasks,
                    &shelved_tasks,
                    &deferred_partial_tasks,
                    &resumable_lanes,
                    &unblock_attempted_tasks,
                ) {
                    let (lane_index, resume_candidate) = if let Some((
                        lane_index,
                        candidate_resume,
                    )) = take_resume_candidate_for_task(
                        &mut resumable_lanes,
                        &candidate.task.id,
                        &active_lanes,
                    ) {
                        (lane_index, Some(candidate_resume))
                    } else {
                        (
                            next_free_lane_index(args.max_concurrent_workers, &active_lanes)
                                .context("failed to find a free loop lane for unblock recovery")?,
                            None,
                        )
                    };
                    parallel_logger.info(format!(
                        "unblock:     lane-{} -> {} [{}] because the normal ready queue is empty; downstream: {}",
                        lane_index,
                        candidate.task.id,
                        candidate.kind.label(),
                        if candidate.downstream.is_empty() {
                            "none".to_string()
                        } else {
                            candidate.downstream.join(", ")
                        }
                    ));
                    unblock_attempted_tasks.insert(candidate.task.id.clone());
                    match candidate.kind {
                        ParallelUnblockCandidateKind::ShelvedResume => {
                            shelved_tasks.remove(&candidate.task.id);
                        }
                        ParallelUnblockCandidateKind::DeferredPartialCloseout => {
                            deferred_partial_tasks.remove(&candidate.task.id);
                        }
                    }
                    let mut assignment = match prepare_parallel_lane_assignment_with_fallback(
                        repo_root,
                        run_root,
                        target_branch,
                        lane_index,
                        candidate.task.clone(),
                        resume_candidate,
                    ) {
                        Ok(assignment) => assignment,
                        Err(err) => {
                            parallel_logger.warn(format!(
                                "warning: failed preparing lane-{} for unblock task `{}`; keeping it parked for this run: {err:#}",
                                lane_index,
                                candidate.task.id
                            ));
                            match candidate.kind {
                                ParallelUnblockCandidateKind::ShelvedResume => {
                                    shelved_tasks.insert(
                                        candidate.task.id.clone(),
                                        candidate.task.markdown.clone(),
                                    );
                                }
                                ParallelUnblockCandidateKind::DeferredPartialCloseout => {
                                    deferred_partial_tasks.insert(candidate.task.id.clone());
                                }
                            }
                            continue;
                        }
                    };
                    attach_partial_follow_up_note(
                        repo_root,
                        &mut assignment,
                        &attempted_partial_followups,
                    );
                    prepend_host_recovery_note(
                        &mut assignment,
                        &render_parallel_unblock_note(&candidate),
                    );
                    if let Err(err) = spawn_parallel_lane_attempt(
                        &mut join_set,
                        &lane_config,
                        prompt_template,
                        &plan,
                        &mut assignment,
                        target_branch,
                    ) {
                        parallel_logger.warn(format!(
                            "warning: failed starting unblock lane-{} `{}`; keeping it parked for this run: {err:#}",
                            assignment.lane_index, assignment.task.id
                        ));
                        match candidate.kind {
                            ParallelUnblockCandidateKind::ShelvedResume => {
                                shelved_tasks.insert(
                                    candidate.task.id.clone(),
                                    candidate.task.markdown.clone(),
                                );
                            }
                            ParallelUnblockCandidateKind::DeferredPartialCloseout => {
                                deferred_partial_tasks.insert(candidate.task.id.clone());
                            }
                        }
                        continue;
                    }
                    active_tasks.insert(assignment.task.id.clone());
                    active_lanes.insert(assignment.lane_index, assignment);
                    last_idle_summary = None;
                    continue;
                }
                if active_lanes.len() < args.max_concurrent_workers {
                    let idle_summary = describe_parallel_idle_state(
                        &plan,
                        &active_tasks,
                        &shelved_tasks,
                        &deferred_partial_tasks,
                    );
                    if last_idle_summary.as_deref() != Some(idle_summary.as_str()) {
                        parallel_logger.info(format!(
                            "idle:        {} of {} lanes active; {}",
                            active_lanes.len(),
                            args.max_concurrent_workers,
                            idle_summary
                        ));
                        append_idle_status_to_free_lanes(
                            run_root,
                            args.max_concurrent_workers,
                            &active_lanes,
                            &idle_summary,
                        );
                        last_idle_summary = Some(idle_summary);
                    }
                }
                break;
            }
            let (verification_only, executable_ready): (Vec<_>, Vec<_>) =
                ready.into_iter().partition(is_verification_only_task);
            if executable_ready.is_empty() {
                let message = format!(
                    "no executable dependency-ready tasks remain; manual verification-only checkpoints must be cleared before continuing: {}",
                    verification_only
                        .iter()
                        .map(|task| task.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                parallel_logger.info(&message);
                break;
            }

            let task = executable_ready[0].clone();
            let (lane_index, resume_candidate) = if let Some((lane_index, candidate)) =
                take_resume_candidate_for_task(&mut resumable_lanes, &task.id, &active_lanes)
            {
                (lane_index, Some(candidate))
            } else {
                (
                    next_free_lane_index(args.max_concurrent_workers, &active_lanes)
                        .context("failed to find a free loop lane")?,
                    None,
                )
            };
            let mut assignment = match prepare_parallel_lane_assignment_with_fallback(
                repo_root,
                run_root,
                target_branch,
                lane_index,
                task.clone(),
                resume_candidate,
            ) {
                Ok(assignment) => assignment,
                Err(err) => {
                    parallel_logger.warn(format!(
                        "warning: failed preparing lane-{} for `{}`; shelving for the rest of this run: {err:#}",
                        lane_index,
                        task.id
                    ));
                    shelved_tasks.insert(task.id.clone(), task.markdown.clone());
                    continue;
                }
            };
            attach_partial_follow_up_note(repo_root, &mut assignment, &attempted_partial_followups);
            if let Err(err) = spawn_parallel_lane_attempt(
                &mut join_set,
                &lane_config,
                prompt_template,
                &plan,
                &mut assignment,
                target_branch,
            ) {
                parallel_logger.warn(format!(
                    "warning: failed starting lane-{} for `{}`; shelving for the rest of this run: {err:#}",
                    assignment.lane_index, assignment.task.id
                ));
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            if let Some(tracker) = linear_tracker.as_mut() {
                if let Err(err) = tracker.note_dispatch(&assignment.task.id).await {
                    eprintln!(
                        "warning: failed to move `{}` to in-progress in Linear: {err:#}",
                        assignment.task.id
                    );
                }
            }
            parallel_logger.info(format!(
                "dispatch:    [{}] lane-{} -> {} {}{}",
                classify_task_execution_kind(&assignment.task),
                lane_index,
                assignment.task.id,
                assignment.task.title,
                if assignment.resumed { " [resume]" } else { "" }
            ));
            let dispatch_message = if assignment.resumed {
                format!("dispatch: resumed `{}`", assignment.task.title)
            } else {
                format!("dispatch: started `{}`", assignment.task.title)
            };
            append_lane_host_event(
                &assignment.stdout_log_path,
                lane_index,
                &assignment.task.id,
                &dispatch_message,
            );
            active_tasks.insert(assignment.task.id.clone());
            active_lanes.insert(lane_index, assignment);
            last_idle_summary = None;
        }

        if active_lanes.is_empty() {
            let queue = plan.queue_snapshot();
            if queue.pending_ids.is_empty() {
                if queue.blocked_ids.is_empty() {
                    parallel_logger.info("no unfinished `- [ ]` / `- [~]` tasks remain; stopping.");
                } else {
                    parallel_logger.info(format!(
                        "all remaining tasks are blocked `[!]`; stopping. blocked: {}",
                        queue.blocked_ids.join(", ")
                    ));
                }
                break;
            }

            parallel_logger.info(no_dependency_ready_stop_message(
                &plan,
                &active_tasks,
                &queue,
                &shelved_tasks,
                &deferred_partial_tasks,
            ));
            break;
        }

        let joined = match tokio::time::timeout(LANE_POLL_INTERVAL, join_set.join_next()).await {
            Ok(Some(result)) => result,
            Ok(None) => {
                parallel_logger.warn(
                    "warning: parallel lane join set became empty while active lanes remained; stopping this host run so unfinished lane repos can be resumed safely on the next launch",
                );
                break;
            }
            Err(_) => continue,
        };
        let lane_result = match joined {
            Ok(lane_result) => lane_result,
            Err(err) => {
                parallel_logger.warn(format!(
                    "warning: parallel lane task panicked; stopping this host run so unfinished lane repos can be resumed safely on the next launch: {err}"
                ));
                break;
            }
        };
        let Some(mut assignment) = active_lanes.remove(&lane_result.lane_index) else {
            parallel_logger.warn(format!(
                "warning: missing active state for lane-{} after a worker completed; rebuilding active task bookkeeping and dropping the result",
                lane_result.lane_index
            ));
            rebuild_active_tasks(&mut active_tasks, &active_lanes);
            continue;
        };
        active_tasks.remove(&assignment.task.id);

        if let Some(error) = lane_result.error {
            eprintln!(
                "warning: lane-{} `{}` failed before producing an exit status; shelving for the rest of this run: {}",
                assignment.lane_index, assignment.task.id, error
            );
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!("shelved: host failure before exit status: {error}"),
            );
            shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
            continue;
        }

        let Some(exit_status) = lane_result.exit_status else {
            shelve_lane_after_host_failure(
                &assignment,
                parallel_logger,
                &mut shelved_tasks,
                "lane attempt completed without an exit status or error",
            );
            continue;
        };

        if !exit_status.success() {
            let Some(progress) = inspect_lane_repo_progress_or_shelve(
                &assignment,
                parallel_logger,
                &mut shelved_tasks,
                "failed inspecting lane repo after a non-zero worker exit",
            ) else {
                continue;
            };
            match progress {
                LaneRepoProgress::NewCommits => {
                    match land_parallel_lane_result(repo_root, target_branch, &mut assignment) {
                        Ok(LaneLandingOutcome::Landed {
                            auto_repaired,
                            completion_status,
                        }) => {
                            if completion_status == LoopTaskStatus::Done {
                                if let Some(tracker) = linear_tracker.as_mut() {
                                    if let Err(err) = tracker.note_done(&assignment.task.id).await {
                                        eprintln!(
                                            "warning: failed to archive `{}` in Linear: {err:#}",
                                            assignment.task.id
                                        );
                                    }
                                }
                            };
                            landed += 1;
                            let status_suffix = completion_status_suffix(
                                &assignment.task.id,
                                completion_status,
                                &mut attempted_partial_followups,
                                &mut deferred_partial_tasks,
                            );
                            let result_label = if auto_repaired {
                                "landed-with-host-repair-after-nonzero"
                            } else if completion_status == LoopTaskStatus::Partial {
                                "landed-partial-after-nonzero"
                            } else {
                                "landed-after-nonzero"
                            };
                            parallel_logger.info(format!(
                                "{result_label}: [{}] {} via lane-{}{} (total landed: {})",
                                classify_task_execution_kind(&assignment.task),
                                assignment.task.id,
                                assignment.lane_index,
                                status_suffix,
                                landed
                            ));
                            append_lane_host_event(
                                &assignment.stdout_log_path,
                                assignment.lane_index,
                                &assignment.task.id,
                                if auto_repaired {
                                    if completion_status == LoopTaskStatus::Partial {
                                        "landed-with-host-repair-after-nonzero: task remains [~] until local evidence is complete"
                                    } else {
                                        "landed-with-host-repair-after-nonzero: host harvested committed work"
                                    }
                                } else if completion_status == LoopTaskStatus::Partial {
                                    "landed-partial-after-nonzero: task remains [~] until local evidence is complete"
                                } else {
                                    "landed-after-nonzero: host harvested committed work"
                                },
                            );
                            last_idle_summary = None;
                            continue;
                        }
                        Ok(LaneLandingOutcome::NeedsRecovery(recovery_note)) => {
                            match try_spawn_lane_recovery_attempt(
                                &mut join_set,
                                &lane_config,
                                prompt_template,
                                &plan,
                                &mut assignment,
                                target_branch,
                                args.max_retries,
                                parallel_logger,
                                "failed to land committed work after a non-zero worker exit",
                                recovery_note,
                            ) {
                                Ok(true) => {
                                    active_tasks.insert(assignment.task.id.clone());
                                    active_lanes.insert(assignment.lane_index, assignment);
                                    continue;
                                }
                                Ok(false) => {
                                    parallel_logger.warn(format!(
                                        "warning: failed landing lane-{} `{}` after non-zero worker exit and no recovery attempts remain",
                                        assignment.lane_index, assignment.task.id
                                    ));
                                    if let Err(salvage_err) = write_parallel_salvage_record(
                                        &assignment,
                                        "host exhausted landing-recovery attempts after a non-zero worker exit",
                                    ) {
                                        parallel_logger.warn(format!(
                                            "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                            assignment.lane_index, assignment.task.id
                                        ));
                                    }
                                }
                                Err(retry_err) => {
                                    parallel_logger.warn(format!(
                                        "warning: failed restarting lane-{} `{}` after landing failure: {retry_err:#}",
                                        assignment.lane_index, assignment.task.id
                                    ));
                                }
                            }
                            shelved_tasks.insert(
                                assignment.task.id.clone(),
                                assignment.task.markdown.clone(),
                            );
                            continue;
                        }
                        Err(err) => {
                            parallel_logger.warn(format!(
                                "warning: failed landing lane-{} `{}` after non-zero worker exit and no recovery attempts remain: {err:#}",
                                assignment.lane_index, assignment.task.id
                            ));
                            if let Err(salvage_err) =
                                write_parallel_salvage_record(&assignment, &format!("{err:#}"))
                            {
                                parallel_logger.warn(format!(
                                    "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                    assignment.lane_index, assignment.task.id
                                ));
                            }
                            shelved_tasks.insert(
                                assignment.task.id.clone(),
                                assignment.task.markdown.clone(),
                            );
                            continue;
                        }
                    }
                }
                LaneRepoProgress::Dirty(_)
                | LaneRepoProgress::NewCommitsWithDirty(_)
                | LaneRepoProgress::None => {}
            }
            if let Some(reason) = detect_lane_environment_blocker(&assignment) {
                let recovery_note = environment_blocker_recovery_note(
                    &reason,
                    &lane_config.preflight_prompt_clause,
                );
                match try_spawn_lane_recovery_attempt(
                    &mut join_set,
                    &lane_config,
                    prompt_template,
                    &plan,
                    &mut assignment,
                    target_branch,
                    args.max_retries,
                    parallel_logger,
                    "hit an external environment blocker",
                    recovery_note,
                ) {
                    Ok(true) => {
                        active_tasks.insert(assignment.task.id.clone());
                        active_lanes.insert(assignment.lane_index, assignment);
                        continue;
                    }
                    Ok(false) => {
                        parallel_logger.warn(format!(
                            "env-blocked: lane-{} `{}` exhausted retries after external blocker; shelving for the rest of this run: {}",
                            assignment.lane_index, assignment.task.id, reason
                        ));
                    }
                    Err(err) => {
                        parallel_logger.warn(format!(
                            "warning: failed restarting lane-{} `{}` after environment blocker: {err:#}; shelving for the rest of this run: {}",
                            assignment.lane_index, assignment.task.id, reason
                        ));
                    }
                }
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    &format!("env-blocked: {reason}"),
                );
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            let exit_code = exit_status.code().unwrap_or(-1);
            let is_futility = exit_code == FUTILITY_EXIT_MARKER;
            if assignment.attempts > args.max_retries {
                parallel_logger.warn(format!(
                    "warning: {} lane-{} (`{}`) exited with status {} after {} attempts; shelving for the rest of this run. see {}",
                    harness,
                    assignment.lane_index,
                    assignment.task.id,
                    if is_futility {
                        "futility".to_string()
                    } else {
                        exit_code.to_string()
                    },
                    assignment.attempts,
                    assignment.stderr_log_path.display()
                ));
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    &format!(
                        "shelved: worker exited {} after {} attempts",
                        if is_futility {
                            "with futility spiral".to_string()
                        } else {
                            format!("with code {exit_code}")
                        },
                        assignment.attempts
                    ),
                );
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }

            parallel_logger.info(format!(
                "warning: lane-{} `{}` exited non-zero ({}), retrying attempt {}/{}",
                assignment.lane_index,
                assignment.task.id,
                if is_futility {
                    "futility spiral".to_string()
                } else {
                    format!("code {exit_code}")
                },
                assignment.attempts,
                args.max_retries + 1
            ));
            append_lane_host_event(
                &assignment.stdout_log_path,
                assignment.lane_index,
                &assignment.task.id,
                &format!(
                    "retrying: worker exited {} on attempt {}/{}",
                    if is_futility {
                        "with futility spiral".to_string()
                    } else {
                        format!("with code {exit_code}")
                    },
                    assignment.attempts,
                    args.max_retries + 1
                ),
            );
            let plan_for_prompt = refresh_parallel_plan_or_last_good(
                repo_root,
                linear_tracker,
                &mut linear_auto_sync_state,
                &plan,
                parallel_logger,
            )
            .await;
            if let Err(err) = spawn_parallel_lane_attempt(
                &mut join_set,
                &lane_config,
                prompt_template,
                &plan_for_prompt,
                &mut assignment,
                target_branch,
            ) {
                parallel_logger.warn(format!(
                    "warning: failed restarting lane-{} `{}`; shelving for the rest of this run: {err:#}",
                    assignment.lane_index, assignment.task.id
                ));
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            active_tasks.insert(assignment.task.id.clone());
            active_lanes.insert(assignment.lane_index, assignment);
            continue;
        }

        let Some(progress) = inspect_lane_repo_progress_or_shelve(
            &assignment,
            parallel_logger,
            &mut shelved_tasks,
            "failed inspecting lane repo after a successful worker exit",
        ) else {
            continue;
        };
        match progress {
            LaneRepoProgress::Dirty(status) | LaneRepoProgress::NewCommitsWithDirty(status) => {
                let recovery_note =
                    lane_repo_recovery_note(&assignment.lane_repo_root, target_branch, &status);
                match try_spawn_lane_recovery_attempt(
                    &mut join_set,
                    &lane_config,
                    prompt_template,
                    &plan,
                    &mut assignment,
                    target_branch,
                    args.max_retries,
                    parallel_logger,
                    "exited cleanly but left a dirty worktree",
                    recovery_note,
                ) {
                    Ok(true) => {
                        active_tasks.insert(assignment.task.id.clone());
                        active_lanes.insert(assignment.lane_index, assignment);
                        continue;
                    }
                    Ok(false) => {
                        parallel_logger.warn(format!(
                            "warning: parallel lane-{} (`{}`) exited cleanly but left uncommitted changes and no recovery attempts remain; shelving for the rest of this run:\n{}",
                            assignment.lane_index,
                            assignment.task.id,
                            status
                        ));
                    }
                    Err(err) => {
                        parallel_logger.warn(format!(
                            "warning: failed restarting lane-{} `{}` for dirty-worktree recovery: {err:#}; shelving for the rest of this run:\n{}",
                            assignment.lane_index, assignment.task.id, status
                        ));
                    }
                }
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    "shelved: worker exited cleanly but left uncommitted changes",
                );
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            LaneRepoProgress::None => {
                match reconcile_parallel_clean_no_commit(
                    repo_root,
                    target_branch,
                    &assignment,
                    parallel_logger,
                ) {
                    Ok(true) => {
                        if let Some(tracker) = linear_tracker.as_mut() {
                            if let Err(err) = tracker.note_done(&assignment.task.id).await {
                                eprintln!(
                                    "warning: failed to archive `{}` in Linear: {err:#}",
                                    assignment.task.id
                                );
                            }
                        }
                        landed += 1;
                        attempted_partial_followups.remove(&assignment.task.id);
                        deferred_partial_tasks.remove(&assignment.task.id);
                        parallel_logger.info(format!(
                            "self-heal:   [{}] {} closed from canonical evidence after lane-{} exited cleanly without a commit (total landed: {})",
                            classify_task_execution_kind(&assignment.task),
                            assignment.task.id,
                            assignment.lane_index,
                            landed
                        ));
                        append_lane_host_event(
                            &assignment.stdout_log_path,
                            assignment.lane_index,
                            &assignment.task.id,
                            "self-heal: worker exited cleanly without a commit, but canonical review/receipt/artifact evidence is complete; host marked the task done",
                        );
                        last_idle_summary = None;
                        continue;
                    }
                    Ok(false) => {}
                    Err(err) => {
                        parallel_logger.warn(format!(
                            "warning: failed checking canonical evidence for clean no-commit lane-{} `{}`: {err:#}",
                            assignment.lane_index, assignment.task.id
                        ));
                    }
                }
                parallel_logger.warn(format!(
                    "warning: parallel lane-{} (`{}`) exited cleanly without producing a local commit; shelving for the rest of this run. see {}",
                    assignment.lane_index,
                    assignment.task.id,
                    assignment.stderr_log_path.display()
                ));
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    "shelved: worker exited cleanly without producing a local commit",
                );
                shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                continue;
            }
            LaneRepoProgress::NewCommits => {
                match land_parallel_lane_result(repo_root, target_branch, &mut assignment) {
                    Ok(LaneLandingOutcome::Landed {
                        auto_repaired,
                        completion_status,
                    }) => {
                        if completion_status == LoopTaskStatus::Done {
                            if let Some(tracker) = linear_tracker.as_mut() {
                                if let Err(err) = tracker.note_done(&assignment.task.id).await {
                                    eprintln!(
                                        "warning: failed to archive `{}` in Linear: {err:#}",
                                        assignment.task.id
                                    );
                                }
                            }
                        }
                        landed += 1;
                        let status_suffix = completion_status_suffix(
                            &assignment.task.id,
                            completion_status,
                            &mut attempted_partial_followups,
                            &mut deferred_partial_tasks,
                        );
                        let result_label = if auto_repaired {
                            "landed-with-host-repair"
                        } else if completion_status == LoopTaskStatus::Partial {
                            "landed-partial"
                        } else {
                            "landed-clean"
                        };
                        parallel_logger.info(format!(
                            "{result_label}: [{}] {} via lane-{}{} (total landed: {})",
                            classify_task_execution_kind(&assignment.task),
                            assignment.task.id,
                            assignment.lane_index,
                            status_suffix,
                            landed
                        ));
                        append_lane_host_event(
                            &assignment.stdout_log_path,
                            assignment.lane_index,
                            &assignment.task.id,
                            if auto_repaired {
                                if completion_status == LoopTaskStatus::Partial {
                                    "landed-with-host-repair: task remains [~] until local evidence is complete"
                                } else {
                                    "landed-with-host-repair: host harvested committed work"
                                }
                            } else if completion_status == LoopTaskStatus::Partial {
                                "landed-partial: task remains [~] until local evidence is complete"
                            } else {
                                "landed-clean: host harvested committed work"
                            },
                        );
                        last_idle_summary = None;
                    }
                    Ok(LaneLandingOutcome::NeedsRecovery(recovery_note)) => {
                        match try_spawn_lane_recovery_attempt(
                            &mut join_set,
                            &lane_config,
                            prompt_template,
                            &plan,
                            &mut assignment,
                            target_branch,
                            args.max_retries,
                            parallel_logger,
                            "failed to land committed work",
                            recovery_note,
                        ) {
                            Ok(true) => {
                                active_tasks.insert(assignment.task.id.clone());
                                active_lanes.insert(assignment.lane_index, assignment);
                                continue;
                            }
                            Ok(false) => {
                                parallel_logger.warn(format!(
                                    "warning: failed landing lane-{} `{}` and no recovery attempts remain",
                                    assignment.lane_index, assignment.task.id
                                ));
                                if let Err(salvage_err) = write_parallel_salvage_record(
                                    &assignment,
                                    "host exhausted landing-recovery attempts",
                                ) {
                                    parallel_logger.warn(format!(
                                        "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                        assignment.lane_index, assignment.task.id
                                    ));
                                }
                            }
                            Err(retry_err) => {
                                parallel_logger.warn(format!(
                                    "warning: failed restarting lane-{} `{}` after landing failure: {retry_err:#}",
                                    assignment.lane_index, assignment.task.id
                                ));
                            }
                        }
                        shelved_tasks
                            .insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                        continue;
                    }
                    Err(err) => {
                        parallel_logger.warn(format!(
                            "warning: failed landing lane-{} `{}` and no recovery attempts remain; shelving for the rest of this run: {err:#}",
                            assignment.lane_index, assignment.task.id
                        ));
                        if let Err(salvage_err) =
                            write_parallel_salvage_record(&assignment, &format!("{err:#}"))
                        {
                            parallel_logger.warn(format!(
                                "warning: failed writing salvage record for lane-{} `{}`: {salvage_err:#}",
                                assignment.lane_index, assignment.task.id
                            ));
                        }
                        shelved_tasks
                            .insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                        continue;
                    }
                }
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn try_spawn_lane_recovery_attempt(
    join_set: &mut JoinSet<LaneAttemptResult>,
    lane_config: &LaneRunConfig,
    prompt_template: &str,
    plan: &LoopPlanSnapshot,
    assignment: &mut ActiveLaneAssignment,
    target_branch: &str,
    max_retries: usize,
    parallel_logger: &ParallelEventLogger,
    reason: &str,
    recovery_note: String,
) -> Result<bool> {
    if assignment.attempts > max_retries {
        return Ok(false);
    }

    let next_attempt = assignment.attempts + 1;
    let total_attempts = max_retries + 1;
    parallel_logger.info(format!(
        "retry-needed: lane-{} `{}` {}; retrying attempt {}/{}",
        assignment.lane_index, assignment.task.id, reason, next_attempt, total_attempts
    ));
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &format!("retry-needed: {reason}; retrying attempt {next_attempt}/{total_attempts}"),
    );
    assignment.host_recovery_note = Some(recovery_note);
    spawn_parallel_lane_attempt(
        join_set,
        lane_config,
        prompt_template,
        plan,
        assignment,
        target_branch,
    )?;
    Ok(true)
}

fn landing_recovery_note(branch: &str, error: &str) -> String {
    format!(
        r#"The host tried to land this lane's committed work onto `{branch}`, but Git reported a landing conflict.

Required recovery:
1. Keep the task's intent and previous committed work.
2. Fetch the current target branch from the lane remote, then reconcile your lane onto the latest `{branch}` with judgment. Prefer `git fetch canonical {branch}` when the lane has a `canonical` remote; otherwise use `origin`.
3. Resolve conflicts semantically against the latest code. Do not blindly choose one side.
4. If a rebase continue step needs a commit message, use `GIT_EDITOR=true git rebase --continue` or `git -c core.editor=true rebase --continue` so the lane cannot block on an editor.
5. End with local task commit(s) based on the latest `{branch}` and a clean `git status --short`.
6. Do not push or edit shared queue files; the host still owns landing and queue reconciliation.

Original host landing error:
{error}"#
    )
}

fn prepared_landing_recovery_note(branch: &str, error: &str, repair_error: &str) -> String {
    format!(
        r#"The host could not land this lane's committed work onto `{branch}` automatically, so it already put this lane repo into landing-recovery mode.

Current repo state:
1. The lane repo has already been reset onto the latest `{branch}` from its lane remote.
2. The lane's committed task range has already been re-applied with `git cherry-pick`.
3. Git stopped on a conflict and left that in-progress cherry-pick in place for you to finish.

Required recovery:
1. Run `git status --short` and `git status` to inspect the conflicted paths and the in-progress cherry-pick summary.
2. Resolve conflicts semantically against the latest `{branch}`. Do not blindly choose one side.
3. Finish with `GIT_EDITOR=true git cherry-pick --continue` or `git -c core.editor=true cherry-pick --continue` so the lane cannot block on an editor.
4. If you truly must restart the repair, inspect the existing task commit(s) first, then use `git cherry-pick --abort` and re-apply them intentionally onto the latest `{branch}`. Do not discard task-owned work.
5. End with local task commit(s) based on the latest `{branch}` and a clean `git status --short`.
6. Do not push or edit shared queue files; the host still owns landing and queue reconciliation.

Original host landing error:
{error}

Host-side recovery status:
{repair_error}"#
    )
}

fn resumed_landing_recovery_note(branch: &str, status: &str) -> String {
    format!(
        r#"This lane repo still has an in-progress landing-recovery cherry-pick against the latest `{branch}`.

Required recovery:
1. Run `git status --short` and `git status` to inspect the conflicted paths and cherry-pick summary.
2. Resolve conflicts semantically against the latest `{branch}`. Do not blindly choose one side.
3. Finish with `GIT_EDITOR=true git cherry-pick --continue` or `git -c core.editor=true cherry-pick --continue`.
4. End with local task commit(s) based on the latest `{branch}` and a clean `git status --short`.
5. Do not push or edit shared queue files; the host still owns landing and queue reconciliation.

Dirty status seen by the host:
{status}"#
    )
}

fn dirty_worktree_recovery_note(status: &str) -> String {
    format!(
        r#"The previous attempt exited successfully, but the lane worktree was still dirty.

Required recovery:
1. Run `git status --short` and inspect every listed path.
2. If a dirty file is task-owned work, include it in a local task commit.
3. If a dirty file is unrelated formatter spillover, accidental exploration, or stale scratch work, revert just that file.
4. End only after `git status --short` is empty and the task has at least one local commit.
5. Do not push or edit shared queue files; the host still owns landing and queue reconciliation.

Dirty status seen by the host:
{status}"#
    )
}

fn lane_repo_recovery_note(lane_repo_root: &Path, branch: &str, status: &str) -> String {
    if let Some(issue) = lane_repo_rebase_recovery_issue(lane_repo_root) {
        stale_rebase_recovery_note(branch, status, &issue)
    } else if lane_repo_has_active_cherry_pick(lane_repo_root) {
        resumed_landing_recovery_note(branch, status)
    } else {
        dirty_worktree_recovery_note(status)
    }
}

fn lane_repo_has_rebase_recovery(lane_repo_root: &Path) -> bool {
    lane_repo_rebase_recovery_issue(lane_repo_root).is_some()
}

fn lane_repo_rebase_recovery_issue(lane_repo_root: &Path) -> Option<String> {
    let rebase_merge = git_path(lane_repo_root, "rebase-merge")?;
    if !rebase_merge.exists() {
        return None;
    }
    let expected = ["head-name", "onto", "orig-head"];
    let missing = expected
        .into_iter()
        .filter(|name| !rebase_merge.join(name).exists())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Some("rebase recovery".to_string())
    } else {
        Some(format!("stale rebase-merge missing {}", missing.join(", ")))
    }
}

fn stale_rebase_recovery_note(branch: &str, status: &str, issue: &str) -> String {
    format!(
        r#"This lane repo has an in-progress or stale Git rebase state against `{branch}`.

Detected state:
{issue}

Required recovery:
1. Run `git status` and inspect `.git/rebase-merge` with `git rev-parse --git-path rebase-merge`.
2. Try `git rebase --abort` first.
3. If Git reports incomplete rebase metadata or leaves only stale files behind, remove the remaining files under the reported `rebase-merge` directory, then `rmdir` that directory.
4. If Git saved an autostash, inspect it before dropping or applying it; do not discard task-owned work blindly.
5. Rebase or cherry-pick the task commits onto the latest `{branch}`, rerun verification, and finish with clean `git status --short`.
6. Do not push or edit shared queue files; the host still owns landing and queue reconciliation.

Dirty status seen by the host:
{status}"#
    )
}

fn lane_repo_has_active_cherry_pick(lane_repo_root: &Path) -> bool {
    if !lane_repo_root.join(".git").exists() {
        return false;
    }
    Command::new("git")
        .arg("-C")
        .arg(lane_repo_root)
        .args(["rev-parse", "--verify", "--quiet", "CHERRY_PICK_HEAD"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn git_path(repo_root: &Path, path: &str) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-path", path])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rendered = String::from_utf8(output.stdout).ok()?;
    let rendered = rendered.trim();
    if rendered.is_empty() {
        return None;
    }
    let path = PathBuf::from(rendered);
    Some(if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    })
}

fn environment_blocker_recovery_note(reason: &str, preflight_report: &str) -> String {
    let preflight = if preflight_report.trim().is_empty() {
        "No host preflight details were recorded.".to_string()
    } else {
        preflight_report.trim().to_string()
    };
    format!(
        r#"The previous attempt appears blocked by external infrastructure, not by the task's code diff.

Detected blocker:
{reason}

Host preflight:
{preflight}

Required recovery:
1. Re-check the missing service/tool/browser/Docker dependency before changing code.
2. If the infrastructure can be repaired from this lane without touching shared queue files, do that and rerun the exact verification.
3. If the infrastructure is still unavailable, print `AUTO_ENV_BLOCKER: <short reason>` and exit non-zero without pretending code proof failed.
        4. If you did make task-owned code changes before finding the blocker, keep them only when they are independently correct, committed, and leave `git status --short` clean."#
    )
}

fn no_dependency_ready_stop_message(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    queue: &LoopQueueSnapshot,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
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
                "no dependency-ready tasks remain to dispatch; stopping with partial follow-up tasks deferred for the rest of this run. pending: {} blocked: {} deferred: {}{}",
                queue.pending_ids.join(", "),
                blocked,
                deferred,
                frontier_suffix
            );
        }
        return format!(
            "no dependency-ready tasks remain to dispatch; stopping. pending: {} blocked: {}{}",
            queue.pending_ids.join(", "),
            blocked,
            frontier_suffix
        );
    }
    let shelved = shelved_tasks.keys().cloned().collect::<Vec<_>>().join(", ");
    let deferred_suffix = deferred
        .map(|deferred| format!(" deferred: {deferred}"))
        .unwrap_or_default();
    format!(
        "no dependency-ready tasks remain to dispatch; stopping with unresolved shelved tasks. pending: {} blocked: {} shelved: {}{}{}",
        queue.pending_ids.join(", "),
        blocked,
        shelved,
        deferred_suffix,
        frontier_suffix
    )
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

fn checkpoint_parallel_host_queue_changes(
    repo_root: &Path,
    target_branch: &str,
    parallel_logger: &ParallelEventLogger,
) -> Result<Option<String>> {
    let queue_files = host_queue_state_files_for_repo(repo_root);
    if queue_files.is_empty() {
        return Ok(None);
    }

    let mut status_args = vec!["status", "--short", "--"];
    status_args.extend(queue_files.iter().copied());
    let status = git_stdout(repo_root, status_args)?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    let mut add_args = vec!["add", "--all", "--"];
    add_args.extend(queue_files.iter().copied());
    run_git(repo_root, add_args)?;
    let message = format!("{}: parallel host queue sync", repo_name(repo_root));
    run_git(repo_root, ["commit", "-m", &message])?;
    let commit = git_stdout(repo_root, ["rev-parse", "--short", "HEAD"])?;
    let commit = commit.trim().to_string();
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        parallel_logger.info(format!(
            "remote sync: rebased onto origin/{} after host queue sync",
            target_branch
        ));
    }
    parallel_logger.info(format!(
        "host sync:  committed queue-state changes at {commit}"
    ));
    Ok(Some(commit))
}

fn try_checkpoint_parallel_host_queue_changes(
    repo_root: &Path,
    target_branch: &str,
    parallel_logger: &ParallelEventLogger,
) {
    if let Err(err) =
        checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger)
    {
        parallel_logger.warn(format!(
            "warning: failed syncing host-owned queue state; continuing without a host queue commit: {err:#}"
        ));
    }
}

fn host_queue_state_files_for_repo(repo_root: &Path) -> Vec<&'static str> {
    HOST_QUEUE_STATE_FILES
        .into_iter()
        .filter(|relative| repo_path_exists_or_is_tracked(repo_root, relative))
        .collect()
}

fn repo_path_exists_or_is_tracked(repo_root: &Path, relative: &str) -> bool {
    if repo_root.join(relative).exists() {
        return true;
    }
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "--error-unmatch", "--", relative])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn inspect_lane_repo_progress_or_shelve(
    assignment: &ActiveLaneAssignment,
    parallel_logger: &ParallelEventLogger,
    shelved_tasks: &mut BTreeMap<String, String>,
    action: &str,
) -> Option<LaneRepoProgress> {
    match inspect_lane_repo_progress(&assignment.lane_repo_root, &assignment.base_commit) {
        Ok(progress) => Some(progress),
        Err(err) => {
            shelve_lane_after_host_failure(
                assignment,
                parallel_logger,
                shelved_tasks,
                &format!("{action}: {err:#}"),
            );
            None
        }
    }
}

fn shelve_lane_after_host_failure(
    assignment: &ActiveLaneAssignment,
    parallel_logger: &ParallelEventLogger,
    shelved_tasks: &mut BTreeMap<String, String>,
    reason: &str,
) {
    parallel_logger.warn(format!(
        "warning: host-side bookkeeping failed for lane-{} `{}`; shelving for the rest of this run: {}",
        assignment.lane_index, assignment.task.id, reason
    ));
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &format!("shelved: host-side bookkeeping failure: {reason}"),
    );
    shelved_tasks.insert(assignment.task.id.clone(), assignment.task.markdown.clone());
}

fn inspect_loop_plan(repo_root: &Path) -> Result<LoopPlanSnapshot> {
    let plan = read_loop_plan(repo_root)?;
    Ok(parse_loop_plan(&plan))
}

fn audit_parallel_completion_drift(
    repo_root: &Path,
    plan_text: &str,
    parallel_logger: &ParallelEventLogger,
) -> Result<String> {
    let snapshot = parse_loop_plan(plan_text);
    let mut updated = plan_text.to_string();
    let mut demoted = Vec::new();
    let mut promoted = Vec::new();

    for task in snapshot
        .tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Done)
    {
        let evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
        if evidence.is_fully_evidenced() {
            continue;
        }
        updated = update_task_completion_in_plan_text(&updated, &task.id, LoopTaskStatus::Partial);
        demoted.push(task.id.clone());
    }

    for task in snapshot
        .tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Partial)
    {
        let evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
        if !evidence.is_fully_evidenced() {
            continue;
        }
        updated = update_task_completion_in_plan_text(&updated, &task.id, LoopTaskStatus::Done);
        promoted.push(task.id.clone());
    }

    if updated != plan_text {
        let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
        atomic_write(&plan_path, updated.as_bytes())
            .with_context(|| format!("failed to write {}", plan_path.display()))?;
        if !demoted.is_empty() {
            parallel_logger.warn(format!(
                "warning: demoted {} completed task(s) to `[~]` because repo-local completion evidence drifted ({})",
                demoted.len(),
                demoted.join(", ")
            ));
        }
        if !promoted.is_empty() {
            parallel_logger.info(format!(
                "self-heal: promoted {} partial task(s) to `[x]` because repo-local completion evidence is complete ({})",
                promoted.len(),
                promoted.join(", ")
            ));
        }
    }

    Ok(updated)
}

async fn refresh_parallel_plan(
    repo_root: &Path,
    linear_tracker: &mut Option<LinearTracker>,
    linear_auto_sync_state: &mut LinearAutoSyncState,
    parallel_logger: &ParallelEventLogger,
) -> Result<LoopPlanSnapshot> {
    let mut plan_text = read_loop_plan(repo_root)?;
    plan_text = audit_parallel_completion_drift(repo_root, &plan_text, parallel_logger)?;
    if let Some(tracker) = linear_tracker.as_mut() {
        if let Err(err) = tracker.refresh_if_plan_changed(&plan_text).await {
            if !maybe_disable_linear_auto_sync_for_run(
                &err,
                linear_auto_sync_state,
                parallel_logger,
                "refreshing the Linear task cache from the updated plan",
            ) {
                parallel_logger.warn(format!(
                    "warning: failed to refresh Linear task cache from updated plan: {err:#}"
                ));
            }
        } else if !linear_auto_sync_state.is_disabled()
            && tracker.should_attempt_auto_sync(&plan_text)
        {
            let drift = tracker.coverage_drift(&plan_text);
            if !drift.is_empty() {
                let mut reasons = Vec::new();
                if !drift.missing_task_ids.is_empty() {
                    reasons.push(format!("missing {}", drift.missing_task_ids.join(", ")));
                }
                if !drift.stale_task_ids.is_empty() {
                    reasons.push(format!("stale {}", drift.stale_task_ids.join(", ")));
                }
                if !drift.terminal_task_ids.is_empty() {
                    reasons.push(format!("terminal {}", drift.terminal_task_ids.join(", ")));
                }
                if !drift.completed_active_task_ids.is_empty() {
                    reasons.push(format!(
                        "completed-active {}",
                        drift.completed_active_task_ids.join(", ")
                    ));
                }
                parallel_logger.info(format!(
                    "linear drift: {}. running `auto symphony sync --no-ai-planner` before dispatch",
                    reasons.join(" | ")
                ));
                tracker.mark_auto_sync_attempt(&plan_text);
                if let Err(err) = run_sync(SymphonySyncArgs {
                    repo_root: Some(repo_root.to_path_buf()),
                    project_slug: None,
                    todo_state: "Todo".to_string(),
                    planner_model: "gpt-5.5".to_string(),
                    planner_reasoning_effort: "high".to_string(),
                    codex_bin: PathBuf::from("codex"),
                    no_ai_planner: true,
                })
                .await
                {
                    if !maybe_disable_linear_auto_sync_for_run(
                        &err,
                        linear_auto_sync_state,
                        parallel_logger,
                        "automatic `auto symphony sync --no-ai-planner`",
                    ) {
                        parallel_logger.warn(format!(
                            "warning: automatic `auto symphony sync --no-ai-planner` failed; continuing without refreshed Linear coverage: {err:#}"
                        ));
                    }
                } else {
                    plan_text = read_loop_plan(repo_root)?;
                    if let Err(err) = tracker.refresh_after_sync(&plan_text).await {
                        if !maybe_disable_linear_auto_sync_for_run(
                            &err,
                            linear_auto_sync_state,
                            parallel_logger,
                            "refreshing the Linear cache after automatic sync",
                        ) {
                            parallel_logger.warn(format!(
                                "warning: failed refreshing Linear cache after automatic sync: {err:#}"
                            ));
                        }
                    } else {
                        parallel_logger.info(
                            "linear:      automatic `auto symphony sync --no-ai-planner` completed",
                        );
                    }
                }
            }
        }
    }
    Ok(parse_loop_plan(&plan_text))
}

async fn refresh_parallel_plan_or_last_good(
    repo_root: &Path,
    linear_tracker: &mut Option<LinearTracker>,
    linear_auto_sync_state: &mut LinearAutoSyncState,
    last_good_plan: &LoopPlanSnapshot,
    parallel_logger: &ParallelEventLogger,
) -> LoopPlanSnapshot {
    match refresh_parallel_plan(
        repo_root,
        linear_tracker,
        linear_auto_sync_state,
        parallel_logger,
    )
    .await
    {
        Ok(plan) => plan,
        Err(err) => {
            parallel_logger.warn(format!(
                "warning: failed to refresh IMPLEMENTATION_PLAN.md; continuing with the last good queue snapshot: {err:#}"
            ));
            last_good_plan.clone()
        }
    }
}

fn is_linear_usage_limit_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("usage_limit_exceeded")
        || message.contains("usage limit exceeded")
        || message.contains("exceeded the free issue limit")
        || message.contains("\"activeissuecount\"")
}

fn maybe_disable_linear_auto_sync_for_run(
    err: &anyhow::Error,
    linear_auto_sync_state: &mut LinearAutoSyncState,
    parallel_logger: &ParallelEventLogger,
    context: &str,
) -> bool {
    if !is_linear_usage_limit_error(err) {
        return false;
    }

    let warning = format!(
        "warning: {context} hit Linear workspace usage limits; disabling further automatic Linear sync for this run and continuing from IMPLEMENTATION_PLAN.md only: {err:#}"
    );
    if linear_auto_sync_state.disable_for_run(warning.clone()) {
        parallel_logger.warn(warning);
    }
    true
}

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
    args.max_concurrent_workers > 1
        && env::var_os("AUTO_PARALLEL_TMUX_BOOTSTRAPPED").is_none()
        && env::var_os("TMUX_PANE").is_none_or(|pane| pane.is_empty())
}

fn parallel_host_stdout_log_path(run_root: &Path) -> PathBuf {
    run_root.join("host.stdout.log")
}

fn parallel_host_stderr_log_path(run_root: &Path) -> PathBuf {
    run_root.join("host.stderr.log")
}

fn launch_parallel_tmux_session(session_name: &str, run_root: &Path) -> Result<TmuxLaunchStatus> {
    if tmux_session_exists(session_name)? {
        return Ok(TmuxLaunchStatus::AlreadyRunning);
    }

    let command = parallel_tmux_command(run_root)?;
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

fn parallel_tmux_command(run_root: &Path) -> Result<String> {
    let executable = env::current_exe()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .or_else(|| env::args().next())
        .context("failed to resolve current executable")?;
    let mut parts = vec![
        "AUTO_PARALLEL_TMUX_BOOTSTRAPPED=1".to_string(),
        shell_quote(&executable),
    ];
    parts.extend(env::args().skip(1).map(|arg| shell_quote(&arg)));
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

fn classify_task_execution_kind(task: &LoopTask) -> &'static str {
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

fn describe_parallel_idle_state(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
) -> String {
    let ready = ready_parallel_tasks(plan, active_tasks, shelved_tasks, deferred_partial_tasks);
    let (verification_only, executable_ready): (Vec<_>, Vec<_>) =
        ready.into_iter().partition(is_verification_only_task);
    if executable_ready.is_empty() && !verification_only.is_empty() {
        return format!(
            "manual verification-only checkpoints are ready: {}",
            verification_only
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
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

fn ready_parallel_tasks(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
) -> Vec<LoopTask> {
    let ready = plan
        .ready_tasks(active_tasks)
        .into_iter()
        .filter(|task| !shelved_tasks.contains_key(&task.id))
        .filter(|task| !deferred_partial_tasks.contains(&task.id))
        .collect::<Vec<_>>();
    let (mut pending, partials): (Vec<_>, Vec<_>) = ready
        .into_iter()
        .partition(|task| task.status == LoopTaskStatus::Pending);
    pending.extend(partials);
    pending
}

fn prioritize_ready_parallel_tasks(repo_root: &Path, ready: Vec<LoopTask>) -> Vec<LoopTask> {
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

fn stable_partition_tasks_by_dirty_overlap(
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

fn repo_dirty_paths_for_parallel_dispatch(repo_root: &Path) -> BTreeSet<String> {
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

fn parse_parallel_status_path(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with("##") {
        return None;
    }
    let path = trimmed.get(3..)?.trim();
    let path = path.rsplit_once(" -> ").map(|(_, rhs)| rhs).unwrap_or(path);
    let path = path.trim_matches('"').trim();
    (!path.is_empty()).then(|| path.to_string())
}

fn parallel_dispatch_path_is_ignored(path: &str) -> bool {
    let first_segment = path.split('/').next().unwrap_or(path);
    first_segment == ".auto"
        || first_segment == "bug"
        || first_segment == "nemesis"
        || first_segment.starts_with("gen-")
}

fn task_overlaps_dirty_canonical_paths(task: &LoopTask, dirty_paths: &BTreeSet<String>) -> bool {
    dirty_paths.iter().any(|path| task.markdown.contains(path))
}

fn unresolved_frontier_dependency_ids(
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

fn parallel_blocker_frontier(
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

fn format_parallel_blocker_frontier(
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
enum PartialFollowUpDisposition {
    RetryLaterThisRun,
    ParkForRestOfRun,
}

fn record_partial_follow_up(
    task_id: &str,
    attempted_partial_followups: &mut BTreeSet<String>,
    deferred_partial_tasks: &mut BTreeSet<String>,
) -> PartialFollowUpDisposition {
    if attempted_partial_followups.insert(task_id.to_string()) {
        deferred_partial_tasks.remove(task_id);
        PartialFollowUpDisposition::RetryLaterThisRun
    } else {
        deferred_partial_tasks.insert(task_id.to_string());
        PartialFollowUpDisposition::ParkForRestOfRun
    }
}

fn clear_partial_follow_up_tracking(
    task_id: &str,
    attempted_partial_followups: &mut BTreeSet<String>,
    deferred_partial_tasks: &mut BTreeSet<String>,
) {
    attempted_partial_followups.remove(task_id);
    deferred_partial_tasks.remove(task_id);
}

fn attach_partial_follow_up_note(
    repo_root: &Path,
    assignment: &mut ActiveLaneAssignment,
    attempted_partial_followups: &BTreeSet<String>,
) {
    if assignment.task.status != LoopTaskStatus::Partial || assignment.host_recovery_note.is_some()
    {
        return;
    }

    let evidence =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    let assessment = assess_task_completion_gap(&assignment.task.markdown, &evidence);
    let pass_label = if attempted_partial_followups.contains(&assignment.task.id) {
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

fn completion_status_suffix(
    task_id: &str,
    completion_status: LoopTaskStatus,
    attempted_partial_followups: &mut BTreeSet<String>,
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
                " [~ evidence gap remains; parked after follow-up]"
            }
        },
        LoopTaskStatus::Pending | LoopTaskStatus::Blocked => "",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParallelUnblockCandidateKind {
    ShelvedResume,
    DeferredPartialCloseout,
}

impl ParallelUnblockCandidateKind {
    fn label(self) -> &'static str {
        match self {
            Self::ShelvedResume => "shelved-resume",
            Self::DeferredPartialCloseout => "tail-closeout",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParallelUnblockCandidate {
    task: LoopTask,
    kind: ParallelUnblockCandidateKind,
    downstream: Vec<String>,
}

fn next_parallel_unblock_candidate(
    plan: &LoopPlanSnapshot,
    active_tasks: &BTreeSet<String>,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
    resumable_lanes: &BTreeMap<usize, LaneResumeCandidate>,
    unblock_attempted_tasks: &BTreeSet<String>,
) -> Option<ParallelUnblockCandidate> {
    let mut candidates = plan
        .ready_tasks(active_tasks)
        .into_iter()
        .filter(|task| !unblock_attempted_tasks.contains(&task.id))
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

fn unblock_candidate_priority(kind: ParallelUnblockCandidateKind) -> usize {
    match kind {
        ParallelUnblockCandidateKind::ShelvedResume => 0,
        ParallelUnblockCandidateKind::DeferredPartialCloseout => 1,
    }
}

fn prepend_host_recovery_note(assignment: &mut ActiveLaneAssignment, note: &str) {
    assignment.host_recovery_note = Some(match assignment.host_recovery_note.take() {
        Some(existing) if !existing.trim().is_empty() => {
            format!("{}\n\n{}", note.trim(), existing.trim())
        }
        _ => note.trim().to_string(),
    });
}

fn render_parallel_unblock_note(candidate: &ParallelUnblockCandidate) -> String {
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

fn next_free_lane_index(
    max_concurrent_workers: usize,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
) -> Option<usize> {
    (1..=max_concurrent_workers).find(|lane_index| !active_lanes.contains_key(lane_index))
}

fn prepare_parallel_lane_assignment(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_index: usize,
    task: LoopTask,
    resume_candidate: Option<LaneResumeCandidate>,
) -> Result<ActiveLaneAssignment> {
    if let Some(candidate) = resume_candidate {
        write_lane_task_id(&candidate.lane_root, &task.id)?;
        return Ok(ActiveLaneAssignment {
            lane_index: candidate.lane_index,
            attempts: 0,
            task,
            resumed: true,
            lane_root: candidate.lane_root,
            lane_repo_root: candidate.lane_repo_root,
            base_commit: candidate.base_commit,
            stdout_log_path: candidate.stdout_log_path,
            stderr_log_path: candidate.stderr_log_path,
            worker_pid_path: candidate.worker_pid_path,
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: candidate.host_recovery_note,
        });
    }

    let lane_root = run_root.join("lanes").join(format!("lane-{lane_index}"));
    reset_parallel_lane_root(&lane_root)?;
    let lane_repo_root = lane_root.join("repo");
    clone_loop_lane_repo(repo_root, target_branch, &lane_repo_root)?;
    let base_commit = git_stdout(&lane_repo_root, ["rev-parse", "HEAD"])?;
    write_lane_task_id(&lane_root, &task.id)?;
    Ok(ActiveLaneAssignment {
        lane_index,
        attempts: 0,
        task,
        resumed: false,
        lane_root: lane_root.clone(),
        lane_repo_root,
        base_commit: base_commit.trim().to_string(),
        stdout_log_path: lane_root.join("stdout.log"),
        stderr_log_path: lane_root.join("stderr.log"),
        worker_pid_path: lane_root.join("worker.pid"),
        clean_commit_since: None,
        terminate_requested_at: None,
        host_recovery_note: None,
    })
}

fn reset_parallel_lane_root(lane_root: &Path) -> Result<()> {
    if lane_root.exists() {
        let stale_root = reserve_stale_lane_root_path(lane_root)?;
        fs::rename(lane_root, &stale_root).with_context(|| {
            format!(
                "failed to move stale lane root {} aside",
                lane_root.display()
            )
        })?;
        if let Err(err) = fs::remove_dir_all(&stale_root) {
            eprintln!(
                "warning: failed removing stale lane root {} after reset: {err}",
                stale_root.display()
            );
        }
    }
    fs::create_dir_all(lane_root)
        .with_context(|| format!("failed to create {}", lane_root.display()))?;
    Ok(())
}

fn reserve_stale_lane_root_path(lane_root: &Path) -> Result<PathBuf> {
    let parent = lane_root
        .parent()
        .with_context(|| format!("lane root {} had no parent", lane_root.display()))?;
    let stem = lane_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .with_context(|| format!("lane root {} had no file name", lane_root.display()))?;
    for attempt in 0..100usize {
        let candidate = if attempt == 0 {
            format!("{stem}.stale-{}", timestamp_slug())
        } else {
            format!("{stem}.stale-{}-{attempt}", timestamp_slug())
        };
        let path = parent.join(candidate);
        if !path.exists() {
            return Ok(path);
        }
    }
    bail!(
        "failed reserving stale lane root path near {}",
        lane_root.display()
    );
}

fn prepare_parallel_lane_assignment_with_fallback(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_index: usize,
    task: LoopTask,
    resume_candidate: Option<LaneResumeCandidate>,
) -> Result<ActiveLaneAssignment> {
    let resumable_snapshot = resume_candidate.clone();
    match prepare_parallel_lane_assignment(
        repo_root,
        run_root,
        target_branch,
        lane_index,
        task.clone(),
        resume_candidate,
    ) {
        Ok(assignment) => Ok(assignment),
        Err(err) => {
            let Some(candidate) = resumable_snapshot else {
                return Err(err);
            };
            eprintln!(
                "warning: failed resuming lane-{} `{}`; retrying with a fresh clone: {err:#}",
                candidate.lane_index, task.id
            );
            prepare_parallel_lane_assignment(
                repo_root,
                run_root,
                target_branch,
                lane_index,
                task,
                None,
            )
        }
    }
}

fn discover_resume_candidates(
    run_root: &Path,
    target_branch: &str,
    plan: &LoopPlanSnapshot,
) -> Result<BTreeMap<usize, LaneResumeCandidate>> {
    let lanes_root = run_root.join("lanes");
    if !lanes_root.exists() {
        return Ok(BTreeMap::new());
    }

    let pending_tasks = plan
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                LoopTaskStatus::Pending | LoopTaskStatus::Partial
            )
        })
        .map(|task| (task.id.clone(), task.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = BTreeMap::new();

    for entry in fs::read_dir(&lanes_root)
        .with_context(|| format!("failed to read {}", lanes_root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", lanes_root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if !file_type.is_dir() {
            continue;
        }

        let lane_root = entry.path();
        let lane_name = entry.file_name();
        let Some(lane_index) = parse_lane_index(&lane_name.to_string_lossy()) else {
            continue;
        };
        let lane_repo_root = lane_root.join("repo");
        if !lane_repo_root.join(".git").exists() {
            continue;
        }

        let Some(task_id) = read_lane_task_id(&lane_root)? else {
            continue;
        };
        let Some(task) = pending_tasks.get(&task_id).cloned() else {
            continue;
        };

        let stdout_log_path = lane_root.join("stdout.log");
        let stderr_log_path = lane_root.join("stderr.log");
        let worker_pid_path = lane_root.join("worker.pid");
        if let Err(err) = clear_stale_worker_pid(&worker_pid_path) {
            eprintln!(
                "warning: skipping resumable lane-{} because its worker pid file could not be cleaned up: {err:#}",
                lane_index
            );
            continue;
        }
        match read_worker_pid(&worker_pid_path) {
            Ok(Some(pid)) => match worker_pid_is_alive(pid) {
                Ok(true) => {
                    eprintln!(
                        "warning: skipping resumable lane-{} because worker pid {} is still alive in {}",
                        lane_index,
                        pid,
                        lane_root.display()
                    );
                    continue;
                }
                Ok(false) => {
                    if let Err(err) = fs::remove_file(&worker_pid_path) {
                        eprintln!(
                            "warning: skipping resumable lane-{} because stale worker pid cleanup failed: {err:#}",
                            lane_index
                        );
                        continue;
                    }
                }
                Err(err) => {
                    eprintln!(
                        "warning: skipping resumable lane-{} because worker pid liveness check failed: {err:#}",
                        lane_index
                    );
                    continue;
                }
            },
            Ok(None) => {}
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because its worker pid file is unreadable: {err:#}",
                    lane_index
                );
                continue;
            }
        }

        let base_commit = match infer_lane_base_commit(&lane_repo_root, target_branch) {
            Ok(base_commit) => base_commit,
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because its base commit could not be inferred: {err:#}",
                    lane_index
                );
                continue;
            }
        };
        let mut host_recovery_note = match inspect_lane_repo_progress(&lane_repo_root, &base_commit)
        {
            Ok(LaneRepoProgress::None) => continue,
            Ok(LaneRepoProgress::Dirty(status) | LaneRepoProgress::NewCommitsWithDirty(status)) => {
                Some(lane_repo_recovery_note(
                    &lane_repo_root,
                    target_branch,
                    &status,
                ))
            }
            Ok(LaneRepoProgress::NewCommits) => None,
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because repo progress inspection failed: {err:#}",
                    lane_index
                );
                continue;
            }
        };
        if host_recovery_note.is_none() {
            host_recovery_note =
                salvage_recovery_note(&lane_root, lane_index, &task_id, target_branch);
        }

        candidates.insert(
            lane_index,
            LaneResumeCandidate {
                lane_index,
                task,
                lane_root,
                lane_repo_root,
                base_commit,
                stdout_log_path,
                stderr_log_path,
                worker_pid_path,
                host_recovery_note,
            },
        );
    }

    Ok(candidates)
}

async fn harvest_resumable_lane_results(
    repo_root: &Path,
    target_branch: &str,
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
    attempted_partial_followups: &mut BTreeSet<String>,
    deferred_partial_tasks: &mut BTreeSet<String>,
    linear_tracker: &mut Option<LinearTracker>,
    parallel_logger: &ParallelEventLogger,
) -> Result<usize> {
    let mut landed = 0usize;
    let lane_indexes = resumable_lanes.keys().copied().collect::<Vec<_>>();
    for lane_index in lane_indexes {
        let should_land = match resumable_lanes.get(&lane_index) {
            Some(candidate) => {
                match inspect_lane_repo_progress(&candidate.lane_repo_root, &candidate.base_commit)
                {
                    Ok(LaneRepoProgress::NewCommits) => true,
                    Ok(
                        LaneRepoProgress::Dirty(_)
                        | LaneRepoProgress::NewCommitsWithDirty(_)
                        | LaneRepoProgress::None,
                    ) => false,
                    Err(err) => {
                        eprintln!(
                            "warning: skipping resumable lane-{} because repo progress inspection failed: {err:#}",
                            lane_index
                        );
                        false
                    }
                }
            }
            None => false,
        };
        if !should_land {
            continue;
        }
        let Some(candidate) = resumable_lanes.remove(&lane_index) else {
            continue;
        };
        let mut assignment = ActiveLaneAssignment {
            lane_index: candidate.lane_index,
            attempts: 0,
            task: candidate.task,
            resumed: true,
            lane_root: candidate.lane_root,
            lane_repo_root: candidate.lane_repo_root,
            base_commit: candidate.base_commit,
            stdout_log_path: candidate.stdout_log_path,
            stderr_log_path: candidate.stderr_log_path,
            worker_pid_path: candidate.worker_pid_path,
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: candidate.host_recovery_note,
        };
        match land_parallel_lane_result(repo_root, target_branch, &mut assignment) {
            Ok(LaneLandingOutcome::Landed {
                auto_repaired,
                completion_status,
            }) => {
                if completion_status == LoopTaskStatus::Done {
                    if let Some(tracker) = linear_tracker.as_mut() {
                        if let Err(err) = tracker.note_done(&assignment.task.id).await {
                            eprintln!(
                                "warning: failed to archive `{}` in Linear: {err:#}",
                                assignment.task.id
                            );
                        }
                    }
                }
                landed += 1;
                let status_suffix = completion_status_suffix(
                    &assignment.task.id,
                    completion_status,
                    attempted_partial_followups,
                    deferred_partial_tasks,
                );
                parallel_logger.info(format!(
                    "resumed:     landed {} from lane-{} before dispatch{}{} (total landed: {})",
                    assignment.task.id,
                    assignment.lane_index,
                    if auto_repaired {
                        " after host auto-repair"
                    } else {
                        ""
                    },
                    status_suffix,
                    landed
                ));
            }
            Ok(LaneLandingOutcome::NeedsRecovery(recovery_note)) => {
                parallel_logger.warn(format!(
                    "warning: resume harvest for lane-{} `{}` prepared a landing-recovery attempt instead of landing; keeping lane resumable",
                    assignment.lane_index, assignment.task.id
                ));
                resumable_lanes.insert(
                    lane_index,
                    LaneResumeCandidate {
                        lane_index: assignment.lane_index,
                        task: assignment.task,
                        lane_root: assignment.lane_root,
                        lane_repo_root: assignment.lane_repo_root,
                        base_commit: assignment.base_commit,
                        stdout_log_path: assignment.stdout_log_path,
                        stderr_log_path: assignment.stderr_log_path,
                        worker_pid_path: assignment.worker_pid_path,
                        host_recovery_note: Some(recovery_note),
                    },
                );
            }
            Err(error) => {
                parallel_logger.warn(format!(
                    "warning: resume harvest for lane-{} `{}` failed; keeping lane resumable instead: {error:#}",
                    assignment.lane_index, assignment.task.id
                ));
                resumable_lanes.insert(
                    lane_index,
                    LaneResumeCandidate {
                        lane_index: assignment.lane_index,
                        task: assignment.task,
                        lane_root: assignment.lane_root,
                        lane_repo_root: assignment.lane_repo_root,
                        base_commit: assignment.base_commit,
                        stdout_log_path: assignment.stdout_log_path,
                        stderr_log_path: assignment.stderr_log_path,
                        worker_pid_path: assignment.worker_pid_path,
                        host_recovery_note: Some(landing_recovery_note(
                            target_branch,
                            &format!("{error:#}"),
                        )),
                    },
                );
            }
        }
    }
    Ok(landed)
}

fn take_resume_candidate_for_task(
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
    task_id: &str,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
) -> Option<(usize, LaneResumeCandidate)> {
    let lane_index = resumable_lanes
        .iter()
        .find(|(lane_index, candidate)| {
            !active_lanes.contains_key(lane_index) && candidate.task.id == task_id
        })
        .map(|(lane_index, _)| *lane_index)?;
    let candidate = resumable_lanes.remove(&lane_index)?;
    Some((lane_index, candidate))
}

fn preserve_resume_recovery_notes(
    rediscovered: &mut BTreeMap<usize, LaneResumeCandidate>,
    previous: &BTreeMap<usize, LaneResumeCandidate>,
) {
    for (lane_index, candidate) in rediscovered {
        if candidate.host_recovery_note.is_some() {
            continue;
        }
        let Some(previous_candidate) = previous.get(lane_index) else {
            continue;
        };
        if previous_candidate.task.id == candidate.task.id {
            candidate.host_recovery_note = previous_candidate.host_recovery_note.clone();
        }
    }
}

fn clone_loop_lane_repo(
    repo_root: &Path,
    target_branch: &str,
    lane_repo_root: &Path,
) -> Result<()> {
    let output = Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg("--local")
        .arg("--branch")
        .arg(target_branch)
        .arg("--single-branch")
        .arg(repo_root)
        .arg(lane_repo_root)
        .output()
        .with_context(|| {
            format!(
                "failed to clone loop lane repo from {} to {}",
                repo_root.display(),
                lane_repo_root.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "git clone failed for loop lane {}: {}",
            lane_repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let remotes = git_stdout(lane_repo_root, ["remote"]).unwrap_or_default();
    if remotes.lines().any(|remote| remote.trim() == "origin") {
        run_git(lane_repo_root, ["remote", "rename", "origin", "canonical"])?;
    }
    Ok(())
}

fn spawn_parallel_lane_attempt(
    join_set: &mut JoinSet<LaneAttemptResult>,
    lane_config: &LaneRunConfig,
    prompt_template: &str,
    plan: &LoopPlanSnapshot,
    assignment: &mut ActiveLaneAssignment,
    target_branch: &str,
) -> Result<()> {
    assignment.attempts += 1;
    assignment.clean_commit_since = None;
    assignment.terminate_requested_at = None;
    refresh_assignment_task_from_plan(plan, assignment);
    let full_prompt = build_parallel_lane_prompt(
        prompt_template,
        plan,
        &assignment.task,
        target_branch,
        &lane_config.cargo_target_prompt_clause,
        &lane_config.preflight_prompt_clause,
        assignment.host_recovery_note.as_deref(),
    );
    let prompt_path = assignment.lane_root.join(format!(
        "{}-attempt-{:02}-prompt.md",
        assignment.task.id, assignment.attempts
    ));
    let repo_root = assignment.lane_repo_root.clone();
    let stderr_log_path = assignment.stderr_log_path.clone();
    let stdout_log_path = assignment.stdout_log_path.clone();
    let worker_pid_path = assignment.worker_pid_path.clone();
    let extra_env = lane_config.env_for_lane(&assignment.lane_root);
    let lane_index = assignment.lane_index;
    let task_id = assignment.task.id.clone();
    let lane_config = lane_config.clone();

    join_set.spawn(async move {
        if let Err(err) = atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))
        {
            return LaneAttemptResult {
                lane_index,
                exit_status: None,
                error: Some(format!("{err:#}")),
            };
        }
        let context_label = format!("auto parallel lane-{lane_index} {task_id}");
        let exit_status = if lane_config.claude {
            run_claude_exec_with_env(
                &repo_root,
                &full_prompt,
                &lane_config.model,
                &lane_config.reasoning_effort,
                lane_config.max_turns,
                &stderr_log_path,
                Some(&stdout_log_path),
                &context_label,
                &extra_env,
                Some(&worker_pid_path),
                None,
            )
            .await
        } else {
            run_codex_exec_with_env(
                &repo_root,
                &full_prompt,
                &lane_config.model,
                &lane_config.reasoning_effort,
                &lane_config.codex_bin,
                &stderr_log_path,
                Some(&stdout_log_path),
                &context_label,
                &extra_env,
                Some(&worker_pid_path),
                None,
            )
            .await
        };
        match exit_status {
            Ok(exit_status) => LaneAttemptResult {
                lane_index,
                exit_status: Some(exit_status),
                error: None,
            },
            Err(err) => LaneAttemptResult {
                lane_index,
                exit_status: None,
                error: Some(format!("{err:#}")),
            },
        }
    });
    Ok(())
}

fn refresh_assignment_task_from_plan(
    plan: &LoopPlanSnapshot,
    assignment: &mut ActiveLaneAssignment,
) {
    if let Some(task) = plan
        .tasks
        .iter()
        .find(|task| task.id == assignment.task.id)
        .cloned()
    {
        assignment.task = task;
    }
}

fn nudge_lingering_committed_lanes(active_lanes: &mut BTreeMap<usize, ActiveLaneAssignment>) {
    for assignment in active_lanes.values_mut() {
        let progress = match inspect_lane_repo_progress(
            &assignment.lane_repo_root,
            &assignment.base_commit,
        ) {
            Ok(progress) => progress,
            Err(err) => {
                eprintln!(
                    "warning: failed inspecting lane-{} `{}` while checking for harvestable commits: {err:#}",
                    assignment.lane_index, assignment.task.id
                );
                assignment.clean_commit_since = None;
                assignment.terminate_requested_at = None;
                continue;
            }
        };
        match progress {
            LaneRepoProgress::NewCommits => {
                let pid = match read_worker_pid(&assignment.worker_pid_path) {
                    Ok(pid) => pid,
                    Err(err) => {
                        eprintln!(
                            "warning: failed reading worker pid for lane-{} `{}`: {err:#}",
                            assignment.lane_index, assignment.task.id
                        );
                        assignment.clean_commit_since = None;
                        assignment.terminate_requested_at = None;
                        continue;
                    }
                };
                let Some(pid) = pid else {
                    assignment.clean_commit_since = None;
                    assignment.terminate_requested_at = None;
                    continue;
                };
                let alive = match worker_pid_is_alive(pid) {
                    Ok(alive) => alive,
                    Err(err) => {
                        eprintln!(
                            "warning: failed checking worker liveness for lane-{} `{}` pid {}: {err:#}",
                            assignment.lane_index, assignment.task.id, pid
                        );
                        assignment.clean_commit_since = None;
                        assignment.terminate_requested_at = None;
                        continue;
                    }
                };
                if !alive {
                    assignment.clean_commit_since = None;
                    assignment.terminate_requested_at = None;
                    continue;
                }

                let commit_since = assignment
                    .clean_commit_since
                    .get_or_insert_with(Instant::now);
                if let Some(requested_at) = assignment.terminate_requested_at {
                    if requested_at.elapsed() >= CLEAN_COMMIT_KILL_GRACE {
                        if let Err(err) = signal_worker(pid, "KILL") {
                            eprintln!(
                                "warning: failed sending SIGKILL to lingering worker pid {} for lane-{} `{}`: {err:#}",
                                pid, assignment.lane_index, assignment.task.id
                            );
                        } else {
                            println!(
                                "harvest:     lane-{} `{}` still lingered after clean commit; sent SIGKILL to pid {}",
                                assignment.lane_index, assignment.task.id, pid
                            );
                            append_lane_host_event(
                                &assignment.stdout_log_path,
                                assignment.lane_index,
                                &assignment.task.id,
                                &format!(
                                    "harvest: sent SIGKILL to lingering worker pid {pid} after clean commit"
                                ),
                            );
                        }
                        assignment.terminate_requested_at = None;
                    }
                    continue;
                }

                if commit_since.elapsed() >= CLEAN_COMMIT_GRACE {
                    if let Err(err) = signal_worker(pid, "TERM") {
                        eprintln!(
                            "warning: failed sending SIGTERM to lingering worker pid {} for lane-{} `{}`: {err:#}",
                            pid, assignment.lane_index, assignment.task.id
                        );
                        assignment.terminate_requested_at = None;
                    } else {
                        println!(
                            "harvest:     lane-{} `{}` has a clean local commit; sent SIGTERM to lingering pid {}",
                            assignment.lane_index, assignment.task.id, pid
                        );
                        append_lane_host_event(
                            &assignment.stdout_log_path,
                            assignment.lane_index,
                            &assignment.task.id,
                            &format!(
                                "harvest: sent SIGTERM to lingering worker pid {pid} after clean commit"
                            ),
                        );
                        assignment.terminate_requested_at = Some(Instant::now());
                    }
                }
            }
            LaneRepoProgress::Dirty(_)
            | LaneRepoProgress::NewCommitsWithDirty(_)
            | LaneRepoProgress::None => {
                assignment.clean_commit_since = None;
                assignment.terminate_requested_at = None;
            }
        }
    }
}

fn read_worker_pid(path: &Path) -> Result<Option<u32>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let pid = trimmed
        .parse::<u32>()
        .with_context(|| format!("invalid pid in {}", path.display()))?;
    Ok(Some(pid))
}

fn clear_stale_worker_pid(path: &Path) -> Result<()> {
    let Some(pid) = read_worker_pid(path)? else {
        return Ok(());
    };
    if worker_pid_is_alive(pid)? {
        return Ok(());
    }
    fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
}

fn parse_lane_index(name: &str) -> Option<usize> {
    name.strip_prefix("lane-")?.parse::<usize>().ok()
}

fn write_lane_task_id(lane_root: &Path, task_id: &str) -> Result<()> {
    atomic_write(&lane_root.join(LANE_TASK_ID_FILE), task_id.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            lane_root.join(LANE_TASK_ID_FILE).display()
        )
    })
}

fn read_lane_task_id(lane_root: &Path) -> Result<Option<String>> {
    let task_id_path = lane_root.join(LANE_TASK_ID_FILE);
    if task_id_path.exists() {
        let task_id = fs::read_to_string(&task_id_path)
            .with_context(|| format!("failed to read {}", task_id_path.display()))?;
        let task_id = task_id.trim();
        if !task_id.is_empty() {
            return Ok(Some(task_id.to_string()));
        }
    }

    let mut latest_prompt: Option<(std::time::SystemTime, String)> = None;
    for entry in fs::read_dir(lane_root)
        .with_context(|| format!("failed to read {}", lane_root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", lane_root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some(task_id) = task_id_from_prompt_filename(&file_name) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        match &latest_prompt {
            Some((latest_modified, _)) if &modified <= latest_modified => {}
            _ => latest_prompt = Some((modified, task_id)),
        }
    }

    Ok(latest_prompt.map(|(_, task_id)| task_id))
}

fn lane_status_task_id(
    stored_task_id: &str,
    worker_running: bool,
    log_line: Option<&str>,
) -> String {
    if worker_running {
        return stored_task_id.to_string();
    }
    if log_line
        .map(str::trim)
        .is_some_and(|line| line.contains("] idle:"))
    {
        return "[idle]".to_string();
    }
    stored_task_id.to_string()
}

fn task_id_from_prompt_filename(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix("-prompt.md")?;
    let (task_id, attempt) = stem.rsplit_once("-attempt-")?;
    if attempt.parse::<usize>().is_err() || task_id.is_empty() {
        return None;
    }
    Some(task_id.to_string())
}

fn infer_lane_base_commit(lane_repo_root: &Path, target_branch: &str) -> Result<String> {
    let remote_name = lane_remote_name(lane_repo_root)?;
    run_git(
        lane_repo_root,
        ["fetch", "--quiet", &remote_name, target_branch],
    )?;
    let base_commit = git_stdout(lane_repo_root, ["merge-base", "HEAD", "FETCH_HEAD"])?;
    let base_commit = base_commit.trim();
    if base_commit.is_empty() {
        bail!(
            "failed to infer base commit for resumable lane repo {}",
            lane_repo_root.display()
        );
    }
    Ok(base_commit.to_string())
}

fn lane_remote_name(lane_repo_root: &Path) -> Result<String> {
    let remotes = git_stdout(lane_repo_root, ["remote"])?;
    for remote in remotes.lines().map(str::trim) {
        if remote == "canonical" {
            return Ok("canonical".to_string());
        }
    }
    for remote in remotes.lines().map(str::trim) {
        if remote == "origin" {
            return Ok("origin".to_string());
        }
    }
    bail!(
        "lane repo {} has no `canonical` or `origin` remote",
        lane_repo_root.display()
    );
}

fn worker_pid_is_alive(pid: u32) -> Result<bool> {
    let status = Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .context("failed to run kill -0")?;
    Ok(status.success())
}

fn signal_worker(pid: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .with_context(|| format!("failed to send SIG{signal} to pid {pid}"))?;
    if !status.success() {
        if worker_pid_is_alive(pid)? {
            bail!("kill -{signal} {pid} failed");
        }
        return Ok(());
    }
    Ok(())
}

fn build_parallel_lane_prompt(
    prompt_template: &str,
    plan: &LoopPlanSnapshot,
    task: &LoopTask,
    branch: &str,
    cargo_target_clause: &str,
    preflight_clause: &str,
    host_recovery_note: Option<&str>,
) -> String {
    let queue = plan.queue_snapshot();
    let blocked_clause = if queue.blocked_ids.is_empty() {
        "none".to_string()
    } else {
        queue.blocked_ids.join(", ")
    };
    let dependency_clause = if task.dependencies.is_empty() {
        "none".to_string()
    } else {
        task.dependencies.join(", ")
    };
    let protected_files = SHARED_QUEUE_FILES
        .into_iter()
        .map(|file| format!("`{file}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let protected_clause = format!(
        "Do not edit these shared queue files in this lane. The host owns queue reconciliation in parallel mode: {}.",
        protected_files
    );
    let recovery_clause = host_recovery_note
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(|note| format!("\nHost recovery context:\n{note}\n"))
        .unwrap_or_default();
    let preflight_clause = preflight_clause
        .trim()
        .is_empty()
        .then(String::new)
        .unwrap_or_else(|| format!("\nHost preflight report:\n{}\n", preflight_clause.trim()));
    let verification = verification_plan(&task.markdown);
    let verification_commands_clause = if verification.executable_commands.is_empty() {
        "none parsed".to_string()
    } else {
        verification
            .executable_commands
            .iter()
            .map(|command| format!("`{command}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let verification_guidance_clause = if verification.narrative_guidance.is_empty() {
        "none".to_string()
    } else {
        verification.narrative_guidance.join(" | ")
    };
    format!(
        "{prompt_template}\n\nParallel assignment for this worker:\n- Assigned task for this lane: `{task_id}` {title}\n- This task is already dependency-ready for this run: {dependency_clause}\n- The host owns queue reconciliation and branch landing in parallel mode.\n- Do not push to `origin/{branch}` or any other remote. Create local commit(s) only; the host will land them onto `{branch}`.\n- Before finishing, run `git status --short`. Finish only with at least one local commit for this task and a clean worktree. If files are still dirty, either commit task-owned leftovers or revert unrelated/formatter spillover before exiting.\n- {protected_clause}\n- {cargo_target_clause}\n- If the repo contains `scripts/run-task-verification.sh`, run the host-parsed executable verification commands through that wrapper instead of invoking them bare. Do not treat narrative `Verification:` prose as literal shell input.\n- Host-parsed executable verification commands: {verification_commands_clause}\n- Narrative verification guidance preserved from the task: {verification_guidance_clause}\n- Source-of-truth discipline: runtime/engine/API owners define facts; UI/presentation code renders those facts. Do not duplicate runtime-owned catalogs, constants, settlement math, risk classifications, eligibility rules, balances, or status derivations in UI code.\n- Runtime-first order: when the task touches both runtime and UI, implement or confirm the runtime/API contract first, regenerate/check generated bindings or schemas second, then update UI consumers.\n- Fixture boundary: production code must not import fixture/demo/sample data as fallback truth. Fixture data belongs in tests, stories, demos, or explicit dev-only harnesses.\n- Contract generation: if the task names generated artifacts or changes runtime/API shapes, run the named generator/check or record `AUTO_ENV_BLOCKER`/`AUTO_VERIFICATION_BLOCKER` with the exact reason it could not run.\n- Cross-surface proof: if UI consumers are named, include at least one runtime-output-to-UI/readback proof or a clear blocker. Component-only tests are insufficient when the original risk is runtime/UI drift.\n- Retire-first cleanup: if the task names retired or superseded surfaces, delete/archive/tombstone them and clean callers/indexes in the same lane when in scope. Do not leave stale active doctrine as a TODO unless the task explicitly gates it.\n- Independent closeout: before your final answer, re-check the original task fields (`Source of truth`, `Runtime owner`, `UI consumers`, `Generated artifacts`, `Fixture boundary`, `Retired surfaces`, and `Review/closeout`) and state how each was satisfied or blocked.\n- If no executable verification commands were parsed, derive the narrowest truthful proof yourself and record blockers honestly instead of patching the wrapper to accept prose.\n- If a proof command exits successfully but reports `0 tests`, treat that proof as not run. Find the exact test/package target or report the verification blocker; do not count zero-test output as passing evidence.\n- Do not use direct target-dir test binaries as final proof unless you built that exact artifact from this lane's current source tree in the immediately preceding command. Prefer `cargo test` or the repo's verification wrapper.\n- If missing external infrastructure blocks verification or runtime smoke tests, print `AUTO_ENV_BLOCKER: <short reason>` before exiting non-zero. Do not present an environment blocker as a code proof failure.\n- Never hand-edit verification receipt files. They are execution evidence, not notes.\n- The host marks this task `- [x]` only when local review handoff, verification evidence, and declared completion artifacts are present. Otherwise it leaves the task `- [~]` for follow-up instead of bluffing completion.\n{preflight_clause}{recovery_clause}\nCanonical queue snapshot when this lane started:\n- Unfinished task count: {pending_count}\n- Currently blocked tasks: {blocked_clause}\n\nAssigned task markdown:\n{markdown}\n",
        task_id = task.id,
        title = task.title,
        dependency_clause = dependency_clause,
        branch = branch,
        protected_clause = protected_clause,
        cargo_target_clause = cargo_target_clause,
        verification_commands_clause = verification_commands_clause,
        verification_guidance_clause = verification_guidance_clause,
        preflight_clause = preflight_clause,
        recovery_clause = recovery_clause,
        pending_count = queue.pending_ids.len(),
        blocked_clause = blocked_clause,
        markdown = task.markdown
    )
}

fn inspect_lane_repo_progress(repo_root: &Path, base_commit: &str) -> Result<LaneRepoProgress> {
    let status = git_stdout(repo_root, ["status", "--short"])?;
    let head = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    let has_new_commits = head.trim() != base_commit;
    let status = status.trim();
    match (has_new_commits, status.is_empty()) {
        (false, true) => Ok(LaneRepoProgress::None),
        (false, false) => Ok(LaneRepoProgress::Dirty(status.to_string())),
        (true, true) => Ok(LaneRepoProgress::NewCommits),
        (true, false) => Ok(LaneRepoProgress::NewCommitsWithDirty(status.to_string())),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
struct LaneScopeBudget {
    max_changed_files: usize,
    max_package_roots: usize,
    max_area_roots: usize,
}

#[allow(dead_code)]
fn render_lane_scope_budget(task: &LoopTask) -> String {
    let budget = lane_scope_budget(task);
    let scope_label = task
        .estimated_scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("M");
    format!(
        "{scope_label} => <= {} changed files, <= {} Rust packages, <= {} top-level areas",
        budget.max_changed_files, budget.max_package_roots, budget.max_area_roots
    )
}

fn is_verification_only_task(task: &LoopTask) -> bool {
    task_field_body(&task.markdown, "Scope boundary:", "Acceptance criteria:")
        .map(|body| body.to_ascii_lowercase().contains("verification only"))
        .unwrap_or(false)
}

#[allow(dead_code)]
fn lane_scope_budget(task: &LoopTask) -> LaneScopeBudget {
    let scope = task
        .estimated_scope
        .as_deref()
        .map(str::trim)
        .unwrap_or("M")
        .to_ascii_uppercase();
    match scope.as_str() {
        "XS" => LaneScopeBudget {
            max_changed_files: 8,
            max_package_roots: 1,
            max_area_roots: 2,
        },
        "S" => LaneScopeBudget {
            max_changed_files: 16,
            max_package_roots: 2,
            max_area_roots: 3,
        },
        _ => LaneScopeBudget {
            max_changed_files: 28,
            max_package_roots: 3,
            max_area_roots: 4,
        },
    }
}

fn land_parallel_lane_result(
    repo_root: &Path,
    target_branch: &str,
    assignment: &mut ActiveLaneAssignment,
) -> Result<LaneLandingOutcome> {
    let mut auto_repaired = false;
    let mut canonical_checkpointed = false;
    let (final_lane_head, final_range_base) = loop {
        let lane_head = git_stdout(&assignment.lane_repo_root, ["rev-parse", "HEAD"])?;
        let lane_head = lane_head.trim().to_string();
        fetch_lane_commit(repo_root, &assignment.lane_repo_root, &lane_head)?;
        let landing_base = git_stdout(repo_root, ["merge-base", "HEAD", "FETCH_HEAD"])?;
        let landing_base = landing_base.trim().to_string();
        let range_base = if landing_base.is_empty() {
            assignment.base_commit.clone()
        } else {
            landing_base
        };
        if !git_ref_is_ancestor(repo_root, "FETCH_HEAD", "HEAD")? {
            if let Err(err) = cherry_pick_lane_range(
                repo_root,
                &range_base,
                "FETCH_HEAD",
                CherryPickFailurePolicy::Abort,
            ) {
                if !canonical_checkpointed
                    && landing_error_suggests_dirty_canonical_worktree(&err)
                    && try_auto_checkpoint_canonical_for_landing(
                        repo_root,
                        target_branch,
                        assignment,
                        "before retrying lane landing against local canonical changes",
                    )?
                {
                    canonical_checkpointed = true;
                    continue;
                }
                if auto_repaired {
                    return Err(err).with_context(|| {
                        format!(
                            "failed landing lane-{} task `{}` from {} after host auto-repair",
                            assignment.lane_index,
                            assignment.task.id,
                            assignment.lane_repo_root.display()
                        )
                    });
                }
                match prepare_lane_landing_recovery(
                    assignment,
                    target_branch,
                    &range_base,
                    &format!("{err:#}"),
                )
                .with_context(|| {
                    format!(
                        "failed preparing lane-{} task `{}` for landing recovery",
                        assignment.lane_index, assignment.task.id
                    )
                })? {
                    LaneLandingRecoveryPrep::RebasedCleanly => {
                        auto_repaired = true;
                        continue;
                    }
                    LaneLandingRecoveryPrep::NeedsWorkerResolution(recovery_note) => {
                        return Ok(LaneLandingOutcome::NeedsRecovery(recovery_note));
                    }
                }
            }
        }
        break (lane_head, range_base);
    };
    let changed_files = lane_changed_files(
        &assignment.lane_repo_root,
        &final_range_base,
        &final_lane_head,
    )?;
    let completion_status = reconcile_parallel_landed_task(repo_root, assignment, &changed_files)?;
    if completion_status == LoopTaskStatus::Done {
        assignment.task.status = LoopTaskStatus::Done;
    } else if completion_status == LoopTaskStatus::Partial {
        assignment.task.status = LoopTaskStatus::Partial;
    }
    if repo_has_staged_queue_updates(repo_root)? {
        let message = format!(
            "{}: {} queue sync",
            repo_name(repo_root),
            assignment.task.id
        );
        run_git(repo_root, ["commit", "-m", &message])?;
    }
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        println!("remote sync: rebased onto origin/{}", target_branch);
    }
    Ok(LaneLandingOutcome::Landed {
        auto_repaired,
        completion_status,
    })
}

fn lane_changed_files(repo_root: &Path, base_commit: &str, head_ref: &str) -> Result<Vec<String>> {
    if base_commit == head_ref {
        return Ok(Vec::new());
    }
    let range = format!("{base_commit}..{head_ref}");
    let output = git_stdout(repo_root, ["diff", "--name-only", &range])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn reconcile_parallel_clean_no_commit(
    repo_root: &Path,
    target_branch: &str,
    assignment: &ActiveLaneAssignment,
    parallel_logger: &ParallelEventLogger,
) -> Result<bool> {
    let evidence_before =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    let review_can_complete_evidence = !evidence_before.has_review_handoff
        && evidence_before.verification_receipt_present
        && evidence_before.missing_completion_artifacts.is_empty()
        && evidence_before.unresolved_audit_findings.is_empty();
    let review_added = if evidence_before.is_fully_evidenced() || review_can_complete_evidence {
        ensure_host_review_handoff(repo_root, &assignment.task.id, &[], &evidence_before)?
    } else {
        false
    };
    let evidence_after =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    if !evidence_after.is_fully_evidenced() {
        return Ok(false);
    }

    let plan_updated =
        update_task_completion_in_plan(repo_root, &assignment.task.id, LoopTaskStatus::Done)?;
    if review_added || plan_updated {
        let mut queue_files = Vec::new();
        if review_added {
            queue_files.push("REVIEW.md");
        }
        if plan_updated {
            queue_files.push("IMPLEMENTATION_PLAN.md");
        }
        let mut args = vec!["add"];
        args.extend(queue_files);
        run_git(repo_root, args)?;
        if repo_has_staged_queue_updates(repo_root)? {
            let message = format!(
                "{}: {} evidence self-heal",
                repo_name(repo_root),
                assignment.task.id
            );
            run_git(repo_root, ["commit", "-m", &message])?;
            if push_branch_with_remote_sync(repo_root, target_branch)? {
                parallel_logger.info(format!(
                    "remote sync: rebased onto origin/{} after evidence self-heal",
                    target_branch
                ));
            }
        }
    }

    Ok(true)
}

fn reconcile_parallel_landed_task(
    repo_root: &Path,
    assignment: &ActiveLaneAssignment,
    changed_files: &[String],
) -> Result<LoopTaskStatus> {
    let evidence_before =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    let review_added = ensure_host_review_handoff(
        repo_root,
        &assignment.task.id,
        changed_files,
        &evidence_before,
    )?;
    let evidence_after =
        inspect_task_completion_evidence(repo_root, &assignment.task.id, &assignment.task.markdown);
    let completion_status = if evidence_after.is_fully_evidenced() {
        LoopTaskStatus::Done
    } else {
        LoopTaskStatus::Partial
    };

    let plan_updated =
        update_task_completion_in_plan(repo_root, &assignment.task.id, completion_status)?;
    if review_added || plan_updated {
        let mut queue_files = Vec::new();
        if review_added {
            queue_files.push("REVIEW.md");
        }
        if plan_updated {
            queue_files.push("IMPLEMENTATION_PLAN.md");
        }
        if !queue_files.is_empty() {
            let mut args = vec!["add"];
            args.extend(queue_files);
            run_git(repo_root, args)?;
        }
    }
    Ok(completion_status)
}

fn repo_has_staged_queue_updates(repo_root: &Path) -> Result<bool> {
    let output = git_stdout(repo_root, ["diff", "--cached", "--name-only"])?;
    Ok(output.lines().any(|line| !line.trim().is_empty()))
}

fn git_ref_is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .with_context(|| {
            format!(
                "failed checking whether {ancestor} is an ancestor of {descendant} in {}",
                repo_root.display()
            )
        })?;
    Ok(output.status.success())
}

fn fetch_lane_commit(repo_root: &Path, lane_repo_root: &Path, lane_head: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("fetch")
        .arg(lane_repo_root)
        .arg(lane_head)
        .output()
        .with_context(|| {
            format!(
                "failed to fetch lane commit {} from {}",
                lane_head,
                lane_repo_root.display()
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git fetch failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn prepare_lane_landing_recovery(
    assignment: &mut ActiveLaneAssignment,
    target_branch: &str,
    range_base: &str,
    landing_error: &str,
) -> Result<LaneLandingRecoveryPrep> {
    let status = git_stdout(&assignment.lane_repo_root, ["status", "--short"])?;
    let status = status.trim();
    if !status.is_empty() {
        bail!(
            "lane-{} `{}` cannot enter landing recovery because its repo is already dirty:\n{}",
            assignment.lane_index,
            assignment.task.id,
            status
        );
    }

    let original_lane_head = git_stdout(&assignment.lane_repo_root, ["rev-parse", "HEAD"])?;
    let original_lane_head = original_lane_head.trim().to_string();
    let remote_name = lane_remote_name(&assignment.lane_repo_root)?;
    run_git(
        &assignment.lane_repo_root,
        ["fetch", "--quiet", &remote_name, target_branch],
    )?;
    let recovery_base = git_stdout(&assignment.lane_repo_root, ["rev-parse", "FETCH_HEAD"])?;
    let recovery_base = recovery_base.trim().to_string();
    if recovery_base.is_empty() {
        bail!(
            "lane-{} `{}` landing recovery could not resolve FETCH_HEAD",
            assignment.lane_index,
            assignment.task.id
        );
    }

    run_git(
        &assignment.lane_repo_root,
        ["reset", "--hard", recovery_base.as_str()],
    )?;
    assignment.base_commit = recovery_base;
    match cherry_pick_lane_range(
        &assignment.lane_repo_root,
        range_base,
        &original_lane_head,
        CherryPickFailurePolicy::LeaveInProgress,
    ) {
        Ok(()) => Ok(LaneLandingRecoveryPrep::RebasedCleanly),
        Err(err) => Ok(LaneLandingRecoveryPrep::NeedsWorkerResolution(
            prepared_landing_recovery_note(target_branch, landing_error, &format!("{err:#}")),
        )),
    }
}

fn cherry_pick_lane_range(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
    failure_policy: CherryPickFailurePolicy,
) -> Result<()> {
    if lane_changed_files(repo_root, base_commit, head_ref)?.is_empty() {
        return Ok(());
    }

    let range = format!("{base_commit}..{head_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cherry-pick")
        .arg("--empty=drop")
        .arg(&range)
        .output()
        .with_context(|| format!("failed to cherry-pick {range} in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }

    if failure_policy == CherryPickFailurePolicy::Abort {
        let _ = run_git(repo_root, ["cherry-pick", "--abort"]);
    }
    bail!(
        "git cherry-pick failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn landing_error_suggests_dirty_canonical_worktree(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("would be overwritten by merge")
        || message.contains("please commit your changes or stash them")
        || message.contains("untracked working tree files would be overwritten")
}

fn try_auto_checkpoint_canonical_for_landing(
    repo_root: &Path,
    target_branch: &str,
    assignment: &ActiveLaneAssignment,
    reason: &str,
) -> Result<bool> {
    let Some(commit) =
        auto_checkpoint_if_needed(repo_root, target_branch, "auto parallel checkpoint")?
    else {
        return Ok(false);
    };
    println!(
        "checkpoint:  committed canonical changes at {commit} {reason} for lane-{} `{}`",
        assignment.lane_index, assignment.task.id
    );
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &format!("checkpoint: committed canonical changes at {commit} {reason}"),
    );
    Ok(true)
}

fn update_task_completion_in_plan(
    repo_root: &Path,
    task_id: &str,
    status: LoopTaskStatus,
) -> Result<bool> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(false);
    }

    let plan = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let updated = update_task_completion_in_plan_text(&plan, task_id, status);
    if updated == plan {
        return Ok(false);
    }

    atomic_write(&plan_path, updated.as_bytes())
        .with_context(|| format!("failed to write {}", plan_path.display()))?;
    Ok(true)
}

fn update_task_completion_in_plan_text(
    plan: &str,
    task_id: &str,
    status: LoopTaskStatus,
) -> String {
    let mut updated = String::new();

    for chunk in plan.split_inclusive('\n') {
        let line = chunk.trim_end_matches('\n').trim_end_matches('\r');
        if let Some((_, current_task_id, _)) = parse_task_header(line) {
            if current_task_id == task_id {
                updated.push_str(&mark_task_header_status(chunk, status));
                continue;
            }
        }
        updated.push_str(chunk);
    }

    updated
}

fn mark_task_header_status(line: &str, status: LoopTaskStatus) -> String {
    let newline = if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    let indent_len = line.len() - line.trim_start().len();
    let indent = &line[..indent_len];
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed.strip_prefix("- [!] "))
        .or_else(|| trimmed.strip_prefix("- [~] "))
        .or_else(|| trimmed.strip_prefix("- [x] "))
        .or_else(|| trimmed.strip_prefix("- [X] "))
        .unwrap_or(trimmed);
    let marker = match status {
        LoopTaskStatus::Pending => "- [ ]",
        LoopTaskStatus::Blocked => "- [!]",
        LoopTaskStatus::Partial => "- [~]",
        LoopTaskStatus::Done => "- [x]",
    };
    format!("{indent}{marker} {rest}{newline}")
}

fn render_default_parallel_prompt(branch: &str, reference_repos: &[PathBuf]) -> String {
    append_reference_repo_clause(
        crate::loop_command::DEFAULT_LOOP_PROMPT_TEMPLATE.replace("{branch}", branch),
        reference_repos,
    )
}

fn repo_forbids_legacy_review_trackers(repo_root: &Path) -> bool {
    ["AGENTS.md", "WORKFLOW.md"].iter().any(|relative| {
        fs::read_to_string(repo_root.join(relative)).is_ok_and(|content| {
            content.contains("Do not restore")
                && content.contains("COMPLETED.md")
                && content.contains("WORKLIST.md")
                && content.contains("ARCHIVED.md")
                && content.contains("REVIEW.md")
        })
    })
}

fn append_reference_repo_clause(prompt: String, reference_repos: &[PathBuf]) -> String {
    if reference_repos.is_empty() {
        return prompt;
    }

    let listing = reference_repos
        .iter()
        .map(|path| format!("- `{}`", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{prompt}\n\nAdditional repositories you may inspect as read-only context:\n{listing}\n\nRepository-crossing rules:\n- Treat every additional repository as read-only. Do not edit, format, stage, commit, push, or run mutating generators in those repos.\n- Implement only the assigned repo's owned surfaces from this lane. If the current task needs code changes in a reference repo, leave a precise follow-up plan item or blocker instead of writing through another repo's canonical worktree.\n- You may read a reference repo's `AGENTS.md`, tests, and operational docs to verify contracts and shape local adapters or fixtures.\n"
    )
}

fn resolve_reference_repos(
    repo_root: &Path,
    paths: &[PathBuf],
    include_siblings: bool,
) -> Result<Vec<PathBuf>> {
    let mut resolved = if include_siblings {
        discover_sibling_git_repos(repo_root)?
    } else {
        Vec::new()
    };
    for path in paths {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            repo_root.join(path)
        };
        let canonical = absolute
            .canonicalize()
            .with_context(|| format!("failed to resolve reference repo {}", absolute.display()))?;
        if !canonical.is_dir() {
            bail!("reference repo {} is not a directory", canonical.display());
        }

        let git_root =
            git_stdout(&canonical, ["rev-parse", "--show-toplevel"]).with_context(|| {
                format!(
                    "reference repo {} is not a git repository",
                    canonical.display()
                )
            })?;
        let git_root = PathBuf::from(git_root.trim())
            .canonicalize()
            .with_context(|| {
                format!(
                    "failed to canonicalize git root for {}",
                    canonical.display()
                )
            })?;
        if git_root != repo_root {
            resolved.push(git_root);
        }
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

fn discover_sibling_git_repos(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let Some(parent) = repo_root.parent() else {
        return Ok(Vec::new());
    };

    let mut siblings = Vec::new();
    for entry in fs::read_dir(parent).with_context(|| {
        format!(
            "failed to read sibling directories under {}",
            parent.display()
        )
    })? {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", parent.display()))?;
        let candidate = entry.path();
        if !candidate.is_dir() {
            continue;
        }

        let canonical = candidate.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize sibling directory {}",
                candidate.display()
            )
        })?;
        if canonical == repo_root {
            continue;
        }

        let Ok(git_root) = git_stdout(&canonical, ["rev-parse", "--show-toplevel"]) else {
            continue;
        };
        let git_root = PathBuf::from(git_root.trim())
            .canonicalize()
            .with_context(|| {
                format!(
                    "failed to canonicalize git root for {}",
                    canonical.display()
                )
            })?;
        if git_root == canonical {
            siblings.push(git_root);
        }
    }

    siblings.sort();
    siblings.dedup();
    Ok(siblings)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrackedRepoState {
    name: String,
    path: PathBuf,
    head: String,
    status: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum RepoProgress {
    None,
    NewCommits,
    DirtyChanges(Vec<String>),
}

fn collect_tracked_repo_states(
    repo_root: &Path,
    reference_repos: &[PathBuf],
) -> Result<Vec<TrackedRepoState>> {
    let mut repos = Vec::with_capacity(reference_repos.len() + 1);
    repos.push(repo_root.to_path_buf());
    repos.extend(reference_repos.iter().cloned());

    let mut states = Vec::with_capacity(repos.len());
    for path in repos {
        let Ok(head) = git_stdout(&path, ["rev-parse", "HEAD"]) else {
            continue;
        };
        let status = git_status_short_filtered(&path).unwrap_or_default();
        states.push(TrackedRepoState {
            name: repo_name(&path),
            path,
            head: head.trim().to_string(),
            status: status.trim().to_string(),
        });
    }
    Ok(states)
}

fn summarize_repo_progress(
    before: &[TrackedRepoState],
    after: &[TrackedRepoState],
) -> RepoProgress {
    let mut dirty_repos = Vec::new();
    for after_state in after {
        let Some(before_state) = before.iter().find(|state| state.path == after_state.path) else {
            return RepoProgress::NewCommits;
        };
        if before_state.head != after_state.head {
            return RepoProgress::NewCommits;
        }
        if before_state.status != after_state.status {
            dirty_repos.push(after_state.name.clone());
        }
    }

    if dirty_repos.is_empty() {
        RepoProgress::None
    } else {
        dirty_repos.sort();
        dirty_repos.dedup();
        RepoProgress::DirtyChanges(dirty_repos)
    }
}

fn resolve_loop_branch(
    repo_root: &Path,
    requested_branch: Option<&str>,
    current_branch: &str,
) -> Result<String> {
    let origin_head = git_stdout(
        repo_root,
        [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok();
    let available = KNOWN_PRIMARY_BRANCHES
        .into_iter()
        .filter(|candidate| git_branch_exists(repo_root, candidate))
        .collect::<Vec<_>>();
    pick_loop_branch(
        requested_branch,
        current_branch,
        origin_head.as_deref(),
        &available,
    )
}

fn pick_loop_branch(
    requested_branch: Option<&str>,
    current_branch: &str,
    origin_head: Option<&str>,
    available_primary_branches: &[&str],
) -> Result<String> {
    if let Some(branch) = requested_branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
    {
        return Ok(branch.to_string());
    }

    if is_primary_branch_name(current_branch) {
        return Ok(current_branch.to_string());
    }

    if let Some(branch) = origin_head.and_then(parse_origin_head_branch) {
        return Ok(branch);
    }

    if let Some(branch) = KNOWN_PRIMARY_BRANCHES
        .into_iter()
        .find(|candidate| available_primary_branches.contains(candidate))
    {
        return Ok(branch.to_string());
    }

    bail!(
        "auto parallel could not resolve the repo's primary branch; pass `--branch <name>` explicitly"
    );
}

fn parse_origin_head_branch(origin_head: &str) -> Option<String> {
    let trimmed = origin_head.trim();
    let branch = trimmed.strip_prefix("origin/").unwrap_or(trimmed).trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

fn is_primary_branch_name(branch: &str) -> bool {
    KNOWN_PRIMARY_BRANCHES.contains(&branch.trim())
}

fn git_branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_ref_exists(repo_root, &format!("refs/heads/{branch}"))
        || git_ref_exists(repo_root, &format!("refs/remotes/origin/{branch}"))
}

fn git_ref_exists(repo_root: &Path, git_ref: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show-ref", "--verify", "--quiet", git_ref])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use anyhow::anyhow;

    use crate::{ParallelArgs, ParallelCargoTarget};

    use super::{
        audit_parallel_completion_drift, build_iteration_prompt, build_parallel_lane_prompt,
        cherry_pick_lane_range, classify_parallel_preflight_needs,
        clear_partial_follow_up_tracking, default_cargo_build_jobs_for,
        dirty_worktree_recovery_note, discover_sibling_git_repos,
        effective_parallel_claude_max_turns, environment_blocker_reason,
        host_queue_state_files_for_repo, inspect_lane_repo_progress, is_linear_usage_limit_error,
        is_verification_only_task, landing_error_suggests_dirty_canonical_worktree,
        landing_recovery_note, lane_repo_has_active_cherry_pick, lane_repo_has_rebase_recovery,
        lane_repo_recovery_note, lane_repo_status_summary, lane_scope_budget, lane_status_task_id,
        last_parallel_stop_state, maybe_disable_linear_auto_sync_for_run,
        next_parallel_unblock_candidate, no_dependency_ready_stop_message,
        parallel_blocker_frontier, parallel_run_root, parallel_tmux_command,
        parallel_tmux_session_name, parse_loop_plan, parse_parallel_stop_ids,
        preflight_warning_names, prepare_lane_landing_recovery, prepare_parallel_startup,
        prepared_landing_recovery_note, preserve_resume_recovery_notes,
        prioritize_ready_parallel_tasks, read_lane_task_id, ready_parallel_tasks,
        recent_parallel_host_warnings, record_partial_follow_up, render_default_parallel_prompt,
        render_parallel_health_summary, repo_forbids_legacy_review_trackers,
        reset_parallel_lane_root, resolve_loop_worker_env, resolve_reference_repos,
        salvage_recovery_note, take_resume_candidate_for_task, task_id_from_prompt_filename,
        tmux_status_line_has_live_worker, try_checkpoint_parallel_host_queue_changes,
        update_task_completion_in_plan_text, ActiveLaneAssignment, CherryPickFailurePolicy,
        LaneLandingRecoveryPrep, LaneRepoProgress, LaneResumeCandidate, LinearAutoSyncState,
        LoopQueueSnapshot, LoopTask, LoopTaskStatus, ParallelBlockerKind, ParallelEventLogger,
        ParallelPreflightNeeds, ParallelStartupPrep, ParallelUnblockCandidateKind,
        PartialFollowUpDisposition,
    };

    #[test]
    fn default_prompt_uses_resolved_branch() {
        let prompt = render_default_parallel_prompt("trunk", &[]);
        assert!(prompt.contains("branch `trunk`"));
        assert!(!prompt.contains("origin/main"));
        assert!(prompt.contains("Study `AGENTS.md` for repo-specific build"));
        assert!(prompt.contains("RED/GREEN/REFACTOR"));
        assert!(prompt.contains("failing test"));
        assert!(prompt
            .contains("identify the first actionable unfinished task marked `- [ ]` or `- [~]`"));
        assert!(prompt.contains("historical context only"));
        assert!(prompt.contains("first actionable unfinished `- [ ]` or `- [~]` task"));
        assert!(prompt.contains("Completion path: <TASK-ID>"));
        assert!(prompt.contains("mark it `- [x]` only when local verification, review handoff, and required completion artifacts are actually in place"));
    }

    #[test]
    fn default_prompt_lists_reference_repos_when_declared() {
        let prompt =
            render_default_parallel_prompt("main", &[PathBuf::from("/tmp/robopokermulti")]);
        assert!(prompt.contains("Additional repositories you may inspect as read-only context"));
        assert!(prompt.contains("/tmp/robopokermulti"));
        assert!(prompt.contains("Do not edit, format, stage, commit, push"));
        assert!(prompt.contains("leave a precise follow-up plan item or blocker"));
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
        let command = parallel_tmux_command(&PathBuf::from("/tmp/auto-parallel"))
            .expect("tmux command should render");

        assert!(command.contains("host.stdout.log"));
        assert!(command.contains("host.stderr.log"));
        assert!(command.contains("tee -a"));
        assert!(command.contains("exec bash"));
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

    #[test]
    fn host_queue_sync_failures_are_logged_without_aborting() {
        let run_root = unique_temp_dir("parallel-host-queue-warning");
        let repo_root = unique_temp_dir("parallel-host-queue-warning-repo");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        fs::create_dir_all(&repo_root).expect("failed to create repo root");
        fs::write(repo_root.join("IMPLEMENTATION_PLAN.md"), "# plan\n")
            .expect("failed to write queue file");

        let logger = ParallelEventLogger::new(&run_root).expect("parallel logger should init");
        try_checkpoint_parallel_host_queue_changes(&repo_root, "main", &logger);

        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("live log should be readable");
        assert!(live_log.contains("failed syncing host-owned queue state"));
        assert!(live_log.contains("continuing without a host queue commit"));

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
        fs::remove_dir_all(&repo_root).expect("failed to remove repo root");
    }

    #[test]
    fn lane_prompt_requires_clean_committed_finish_and_can_include_recovery_context() {
        let snapshot = parse_loop_plan(
            r#"- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
"#,
        );
        let task = snapshot.tasks.first().expect("task should parse");
        let prompt = build_parallel_lane_prompt(
            "base prompt",
            &snapshot,
            task,
            "trunk",
            "Use the host-provided `CARGO_TARGET_DIR`; this run gives each lane its own target directory.",
            "- warn agent-browser: daemon missing",
            Some("Resolve the previous landing conflict."),
        );

        assert!(prompt.contains("run `git status --short`"));
        assert!(prompt.contains("at least one local commit for this task and a clean worktree"));
        assert!(prompt.contains("reports `0 tests`"));
        assert!(prompt.contains("direct target-dir test binaries"));
        assert!(prompt.contains("AUTO_ENV_BLOCKER"));
        assert!(prompt.contains("Host-parsed executable verification commands"));
        assert!(
            prompt.contains("Do not treat narrative `Verification:` prose as literal shell input")
        );
        assert!(prompt.contains("Host preflight report:"));
        assert!(prompt.contains("Host recovery context:"));
        assert!(prompt.contains("Resolve the previous landing conflict."));
    }

    #[test]
    fn lane_status_task_id_reports_idle_when_latest_log_is_idle() {
        assert_eq!(
            lane_status_task_id(
                "OLD-TASK",
                false,
                Some("[auto parallel host lane-5 [idle]] idle: waiting on dependencies"),
            ),
            "[idle]"
        );
        assert_eq!(
            lane_status_task_id("OLD-TASK", true, Some("anything")),
            "OLD-TASK"
        );
    }

    #[test]
    fn recovery_notes_explain_semantic_merge_and_dirty_cleanup_contracts() {
        let landing = landing_recovery_note("trunk", "conflict in src/lib.rs");
        assert!(landing.contains("Resolve conflicts semantically"));
        assert!(landing.contains("GIT_EDITOR=true git rebase --continue"));
        assert!(landing.contains("based on the latest `trunk`"));
        assert!(landing.contains("conflict in src/lib.rs"));

        let prepared = prepared_landing_recovery_note(
            "trunk",
            "git cherry-pick failed",
            "git cherry-pick stopped at src/lib.rs",
        );
        assert!(prepared.contains("landing-recovery mode"));
        assert!(prepared.contains("git cherry-pick"));
        assert!(prepared.contains("cherry-pick --continue"));
        assert!(prepared.contains("git cherry-pick stopped at src/lib.rs"));

        let dirty = dirty_worktree_recovery_note("M src/lib.rs");
        assert!(dirty.contains("Run `git status --short`"));
        assert!(dirty.contains("include it in a local task commit"));
        assert!(dirty.contains("unrelated formatter spillover"));
        assert!(dirty.contains("revert just that file"));
        assert!(dirty.contains("M src/lib.rs"));
    }

    #[test]
    fn stale_rebase_merge_state_is_reported_with_cleanup_recipe() {
        let repo = unique_temp_dir("parallel-stale-rebase-merge");
        fs::create_dir_all(&repo).expect("failed to create temp repo");
        run_git_in(&repo, ["init", "-b", "main"]);
        run_git_in(&repo, ["config", "user.name", "autodev tests"]);
        run_git_in(&repo, ["config", "user.email", "autodev@example.com"]);
        fs::write(repo.join("README.md"), "init\n").expect("failed to write readme");
        run_git_in(&repo, ["add", "README.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);

        let rebase_merge = repo.join(".git").join("rebase-merge");
        fs::create_dir_all(&rebase_merge).expect("failed to create stale rebase dir");
        fs::write(rebase_merge.join("autostash"), "deadbeef\n")
            .expect("failed to write stale autostash");

        assert!(lane_repo_has_rebase_recovery(&repo));
        let summary = lane_repo_status_summary(&repo);
        assert!(summary.contains("stale rebase-merge"));
        let note = lane_repo_recovery_note(&repo, "main", " M README.md");
        assert!(note.contains("git rebase --abort"));
        assert!(note.contains("rebase-merge"));
        assert!(note.contains("autostash"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn environment_blocker_detection_prefers_explicit_marker() {
        let log = "some output\nAUTO_ENV_BLOCKER: regtest RPC is down\nmore output";
        assert_eq!(
            environment_blocker_reason(log),
            Some("regtest RPC is down".to_string())
        );

        assert_eq!(
            environment_blocker_reason(
                "Daemon failed to start (socket: /run/user/1000/agent-browser/default.sock)"
            ),
            Some("agent-browser daemon failed to start".to_string())
        );
    }

    #[test]
    fn detects_direct_review_queue_policy() {
        let temp = unique_temp_dir("loop-direct-review-policy");
        fs::create_dir_all(&temp).expect("failed to create temp dir");
        fs::write(
            temp.join("WORKFLOW.md"),
            "Do not restore `COMPLETED.md`, `WORKLIST.md`, or `ARCHIVED.md`; use `REVIEW.md`.",
        )
        .expect("failed to write policy");

        assert!(repo_forbids_legacy_review_trackers(&temp));

        fs::remove_dir_all(&temp).expect("failed to remove temp dir");
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

        let message =
            no_dependency_ready_stop_message(&plan, &BTreeSet::new(), &queue, &shelved, &deferred);
        assert!(message.contains("stopping with unresolved shelved tasks"));
        assert!(message.contains("pending: TASK-1, TASK-2"));
        assert!(message.contains("blocked: TASK-9"));
        assert!(message.contains("shelved: TASK-3, TASK-4"));
        assert!(message.contains("deferred: TASK-5"));
        assert!(message.contains("frontier: TASK-3 [shelved]"));
    }

    #[test]
    fn parallel_blocker_frontier_classifies_shelved_and_deferred_dependencies() {
        let plan = parse_loop_plan(
            r#"
- [ ] `TASK-A` waits on shelved
  Dependencies: `TASK-S`
- [ ] `TASK-B` waits on deferred
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
        assert_eq!(frontier[0].task_id, "TASK-P");
        assert_eq!(frontier[0].kind, ParallelBlockerKind::DeferredPartial);
        assert_eq!(frontier[1].task_id, "TASK-S");
        assert_eq!(frontier[1].kind, ParallelBlockerKind::Shelved);
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
            &BTreeSet::new(),
        )
        .expect("expected an unblock candidate");
        assert_eq!(candidate.task.id, "TASK-S");
        assert_eq!(candidate.kind, ParallelUnblockCandidateKind::ShelvedResume);
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
    fn salvage_recovery_note_reuses_saved_landing_error() {
        let run_root = unique_temp_dir("parallel-salvage-note");
        let lane_root = run_root.join("lanes").join("lane-3");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        fs::create_dir_all(run_root.join("salvage")).expect("failed to create salvage dir");
        fs::write(
            run_root.join("salvage").join("lane-3-TASK-1.md"),
            "# auto parallel salvage\n\n## Landing Error\n\n```text\ngit cherry-pick failed in /tmp/repo: conflict\n```\n\n## Recovery\n\nReconcile it.\n",
        )
        .expect("failed to write salvage note");

        let note = salvage_recovery_note(&lane_root, 3, "TASK-1", "main").expect("expected note");
        assert!(note.contains("git cherry-pick failed in /tmp/repo: conflict"));
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn dirty_canonical_landing_errors_are_detected() {
        let err = anyhow!(
            "git cherry-pick failed in /tmp/repo: error: Your local changes to the following files would be overwritten by merge:\n  src/lib.rs\nPlease commit your changes or stash them before you merge.\nAborting\nfatal: cherry-pick failed"
        );
        assert!(landing_error_suggests_dirty_canonical_worktree(&err));
    }

    #[test]
    fn host_queue_state_files_skip_missing_untracked_docs() {
        let repo = unique_temp_dir("parallel-host-queue-files");
        init_git_repo(&repo);
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), "# plan\n").expect("failed to write plan");
        fs::write(repo.join("COMPLETED.md"), "# completed\n").expect("failed to write completed");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md", "COMPLETED.md"]);
        run_git_in(&repo, ["commit", "-m", "queue docs"]);
        fs::remove_file(repo.join("COMPLETED.md")).expect("failed to remove completed");

        let files = host_queue_state_files_for_repo(&repo);
        assert!(files.contains(&"IMPLEMENTATION_PLAN.md"));
        assert!(files.contains(&"COMPLETED.md"));
        assert!(!files.contains(&"WORKLIST.md"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
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
        assert!(summary.contains("active recovery lanes: lane-1 TASK-1, lane-3 TASK-3"));
        assert!(summary.contains("stale recovery lanes: lane-2 TASK-2"));

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn preflight_classification_does_not_treat_rbtc_mainnet_as_regtest() {
        let repo = unique_temp_dir("parallel-preflight-mainnet");
        fs::create_dir_all(&repo).expect("failed to create temp repo");

        let needs = classify_parallel_preflight_needs(
            "bitino rbtc mainnet settlement proof and wallet signing",
            &repo,
        );
        assert_eq!(
            needs,
            ParallelPreflightNeeds {
                browser: false,
                docker: false,
                regtest: false,
            }
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn preflight_classification_detects_explicit_regtest_and_browser_infra() {
        let repo = unique_temp_dir("parallel-preflight-regtest");
        fs::create_dir_all(&repo).expect("failed to create temp repo");

        let needs = classify_parallel_preflight_needs(
            "browser smoke against rbtc-regtest with docker compose",
            &repo,
        );
        assert_eq!(
            needs,
            ParallelPreflightNeeds {
                browser: true,
                docker: true,
                regtest: true,
            }
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn preflight_classification_does_not_treat_compose_prose_as_docker() {
        let repo = unique_temp_dir("parallel-preflight-compose-prose");
        fs::create_dir_all(&repo).expect("failed to create temp repo");

        let needs = classify_parallel_preflight_needs(
            "per-game canvases compose via canvas_prelude rather than re-implementing edge iteration",
            &repo,
        );
        assert_eq!(
            needs,
            ParallelPreflightNeeds {
                browser: false,
                docker: false,
                regtest: false,
            }
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn linear_usage_limit_error_detection_matches_linear_graphql_payloads() {
        let usage_limit = anyhow!(
            "Linear GraphQL returned errors: [{{\"extensions\":{{\"code\":\"USAGE_LIMIT_EXCEEDED\",\"meta\":{{\"usageMetric\":\"activeIssueCount\"}}}},\"message\":\"usage limit exceeded\"}}]"
        );
        let unrelated = anyhow!("Linear project `demo` not found");

        assert!(is_linear_usage_limit_error(&usage_limit));
        assert!(!is_linear_usage_limit_error(&unrelated));
    }

    #[test]
    fn linear_usage_limit_disables_auto_sync_for_the_rest_of_the_run() {
        let run_root = unique_temp_dir("parallel-linear-usage-limit");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("failed to create logger");
        let err = anyhow!(
            "Linear GraphQL returned errors: [{{\"extensions\":{{\"code\":\"USAGE_LIMIT_EXCEEDED\",\"meta\":{{\"usageMetric\":\"activeIssueCount\"}}}},\"message\":\"You've exceeded the free issue limit for this workspace.\"}}]"
        );
        let mut state = LinearAutoSyncState::default();

        assert!(maybe_disable_linear_auto_sync_for_run(
            &err,
            &mut state,
            &logger,
            "automatic `auto symphony sync --no-ai-planner`",
        ));
        assert!(state.is_disabled());
        assert!(maybe_disable_linear_auto_sync_for_run(
            &err,
            &mut state,
            &logger,
            "automatic `auto symphony sync --no-ai-planner`",
        ));

        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("failed to read live log");
        assert_eq!(
            live_log
                .matches("disabling further automatic Linear sync for this run")
                .count(),
            1
        );

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn prepare_lane_landing_recovery_rebases_cleanly_when_possible() {
        let (root, remote, upstream, _worker) =
            init_remote_and_clones("parallel-landing-recovery-clean", "main");
        let lane = root.join("lane-clean");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);
        run_git_in(&lane, ["remote", "rename", "origin", "canonical"]);

        let base_commit = git_output(&lane, ["rev-parse", "HEAD"]);
        fs::write(lane.join("lane.txt"), "lane change\n").expect("failed to write lane file");
        run_git_in(&lane, ["add", "lane.txt"]);
        run_git_in(&lane, ["commit", "-m", "lane change"]);

        fs::write(upstream.join("main.txt"), "main change\n").expect("failed to write main file");
        run_git_in(&upstream, ["add", "main.txt"]);
        run_git_in(&upstream, ["commit", "-m", "main change"]);
        run_git_in(&upstream, ["push", "origin", "main"]);
        let remote_head = git_output(&upstream, ["rev-parse", "HEAD"]);

        let mut assignment = ActiveLaneAssignment {
            lane_index: 1,
            attempts: 1,
            task: LoopTask {
                id: "TASK-CLEAN".to_string(),
                title: "clean recovery".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                markdown: "- [ ] `TASK-CLEAN` clean recovery\n".to_string(),
            },
            resumed: false,
            lane_root: root.join("lane-clean-root"),
            lane_repo_root: lane.clone(),
            base_commit: base_commit.clone(),
            stdout_log_path: root.join("lane-clean.stdout.log"),
            stderr_log_path: root.join("lane-clean.stderr.log"),
            worker_pid_path: root.join("lane-clean.worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };

        let prep = prepare_lane_landing_recovery(
            &mut assignment,
            "main",
            &base_commit,
            "git cherry-pick failed",
        )
        .expect("landing recovery should prepare");
        assert_eq!(prep, LaneLandingRecoveryPrep::RebasedCleanly);
        assert_eq!(assignment.base_commit, remote_head);
        assert_eq!(run_git_in(&lane, ["status", "--short"]), "");
        assert!(!lane_repo_has_active_cherry_pick(&lane));
        let log = run_git_in(&lane, ["log", "--format=%s", "-2"]);
        assert_eq!(log, "lane change\nmain change\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn cherry_pick_lane_range_treats_empty_tree_diff_as_already_applied() {
        let (root, remote, _upstream, worker) =
            init_remote_and_clones("parallel-empty-lane-commit", "main");
        let lane = root.join("lane-empty");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);

        let base_commit = git_output(&lane, ["rev-parse", "HEAD"]);
        run_git_in(
            &lane,
            ["commit", "--allow-empty", "-m", "verification-only marker"],
        );
        let lane_head = git_output(&lane, ["rev-parse", "HEAD"]);
        run_git_in(
            &worker,
            [
                "fetch",
                lane.to_str().expect("lane path should be utf-8"),
                lane_head.as_str(),
            ],
        );

        cherry_pick_lane_range(
            &worker,
            &base_commit,
            "FETCH_HEAD",
            CherryPickFailurePolicy::Abort,
        )
        .expect("empty tree-diff lane commit should be treated as already applied");

        assert_eq!(git_output(&worker, ["rev-parse", "HEAD"]), base_commit);
        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        assert!(!lane_repo_has_active_cherry_pick(&worker));

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn prepare_lane_landing_recovery_leaves_conflict_for_worker() {
        let (root, remote, upstream, _worker) =
            init_remote_and_clones("parallel-landing-recovery-conflict", "main");
        let lane = root.join("lane-conflict");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);
        run_git_in(&lane, ["remote", "rename", "origin", "canonical"]);

        let base_commit = git_output(&lane, ["rev-parse", "HEAD"]);
        fs::write(lane.join("shared.txt"), "lane version\n").expect("failed to write lane file");
        run_git_in(&lane, ["add", "shared.txt"]);
        run_git_in(&lane, ["commit", "-m", "lane conflict"]);

        fs::write(upstream.join("shared.txt"), "main version\n")
            .expect("failed to write upstream file");
        run_git_in(&upstream, ["add", "shared.txt"]);
        run_git_in(&upstream, ["commit", "-m", "main conflict"]);
        run_git_in(&upstream, ["push", "origin", "main"]);
        let remote_head = git_output(&upstream, ["rev-parse", "HEAD"]);

        let mut assignment = ActiveLaneAssignment {
            lane_index: 2,
            attempts: 1,
            task: LoopTask {
                id: "TASK-CONFLICT".to_string(),
                title: "conflict recovery".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                markdown: "- [ ] `TASK-CONFLICT` conflict recovery\n".to_string(),
            },
            resumed: false,
            lane_root: root.join("lane-conflict-root"),
            lane_repo_root: lane.clone(),
            base_commit: base_commit.clone(),
            stdout_log_path: root.join("lane-conflict.stdout.log"),
            stderr_log_path: root.join("lane-conflict.stderr.log"),
            worker_pid_path: root.join("lane-conflict.worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };

        let prep = prepare_lane_landing_recovery(
            &mut assignment,
            "main",
            &base_commit,
            "git cherry-pick failed",
        )
        .expect("landing recovery should prepare");
        let note = match prep {
            LaneLandingRecoveryPrep::NeedsWorkerResolution(note) => note,
            other => panic!("expected worker-resolution prep, got {other:?}"),
        };
        assert_eq!(assignment.base_commit, remote_head);
        assert!(lane_repo_has_active_cherry_pick(&lane));
        let status = run_git_in(&lane, ["status", "--short"]);
        assert!(status.contains("shared.txt"));
        assert!(lane_repo_status_summary(&lane).contains("cherry-pick recovery"));
        assert!(note.contains("landing-recovery mode"));
        assert!(note.contains("cherry-pick --continue"));

        let resumed = lane_repo_recovery_note(&lane, "main", status.trim());
        assert!(resumed.contains("in-progress landing-recovery cherry-pick"));
        assert!(resumed.contains("shared.txt"));

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
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

    #[test]
    fn prompt_filename_task_id_round_trips() {
        assert_eq!(
            task_id_from_prompt_filename("P-029C-attempt-03-prompt.md"),
            Some("P-029C".to_string())
        );
        assert_eq!(
            task_id_from_prompt_filename("WEB-CRAPS-D-attempt-1-prompt.md"),
            Some("WEB-CRAPS-D".to_string())
        );
        assert_eq!(task_id_from_prompt_filename("stderr.log"), None);
    }

    #[test]
    fn lane_task_id_prefers_metadata_and_falls_back_to_latest_prompt() {
        let lane_root = unique_temp_dir("parallel-lane-task-id");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        fs::write(lane_root.join("P-018B-attempt-01-prompt.md"), "")
            .expect("failed to write prompt");
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(lane_root.join("P-021-attempt-02-prompt.md"), "")
            .expect("failed to write prompt");

        assert_eq!(
            read_lane_task_id(&lane_root).expect("lane task id should read"),
            Some("P-021".to_string())
        );

        fs::write(lane_root.join(super::LANE_TASK_ID_FILE), "P-029C\n")
            .expect("failed to write metadata");
        assert_eq!(
            read_lane_task_id(&lane_root).expect("lane task id should read"),
            Some("P-029C".to_string())
        );

        fs::remove_dir_all(&lane_root).expect("failed to remove lane root");
    }

    #[test]
    fn lane_repo_progress_reports_commits_and_dirty_state_independently() {
        let repo = unique_temp_dir("parallel-lane-progress");
        init_git_repo(&repo);
        fs::write(repo.join("file.txt"), "base\n").expect("failed to write base file");
        git_ok(&repo, ["add", "file.txt"]);
        git_ok(&repo, ["commit", "-m", "base"]);
        let base = git_output(&repo, ["rev-parse", "HEAD"]);

        fs::write(repo.join("file.txt"), "dirty\n").expect("failed to dirty file");
        assert_eq!(
            inspect_lane_repo_progress(&repo, &base).expect("progress should inspect"),
            LaneRepoProgress::Dirty("M file.txt".to_string())
        );

        git_ok(&repo, ["add", "file.txt"]);
        git_ok(&repo, ["commit", "-m", "task"]);
        assert_eq!(
            inspect_lane_repo_progress(&repo, &base).expect("progress should inspect"),
            LaneRepoProgress::NewCommits
        );

        fs::write(repo.join("file.txt"), "dirty again\n").expect("failed to dirty file again");
        assert_eq!(
            inspect_lane_repo_progress(&repo, &base).expect("progress should inspect"),
            LaneRepoProgress::NewCommitsWithDirty("M file.txt".to_string())
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn reset_parallel_lane_root_rehomes_existing_contents() {
        let lane_root = unique_temp_dir("parallel-lane-reset");
        fs::create_dir_all(lane_root.join("repo")).expect("failed to create lane repo");
        fs::write(lane_root.join("repo").join("stale.txt"), "stale")
            .expect("failed to write stale file");

        reset_parallel_lane_root(&lane_root).expect("lane root should reset");

        assert!(lane_root.exists(), "lane root should exist after reset");
        assert!(
            fs::read_dir(&lane_root)
                .expect("lane root should be readable")
                .next()
                .is_none(),
            "lane root should be recreated empty"
        );

        let parent = lane_root.parent().expect("lane root should have parent");
        let prefix = format!(
            "{}.stale-",
            lane_root
                .file_name()
                .expect("lane root should have file name")
                .to_string_lossy()
        );
        let stale_dirs = fs::read_dir(parent)
            .expect("parent should be readable")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().starts_with(&prefix))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        assert!(
            stale_dirs.is_empty(),
            "stale lane roots should be pruned after reset"
        );

        fs::remove_dir_all(&lane_root).expect("failed to remove lane root");
    }

    #[test]
    fn resume_candidate_matches_requested_task() {
        let ready_tasks = [
            LoopTask {
                id: "P-019D".to_string(),
                title: "first".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                markdown: String::new(),
            },
            LoopTask {
                id: "P-021".to_string(),
                title: "second".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                markdown: String::new(),
            },
        ];
        let mut resumable = BTreeMap::new();
        resumable.insert(
            2,
            LaneResumeCandidate {
                lane_index: 2,
                task: ready_tasks[1].clone(),
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: None,
            },
        );
        resumable.insert(
            5,
            LaneResumeCandidate {
                lane_index: 5,
                task: ready_tasks[0].clone(),
                lane_root: PathBuf::from("/tmp/lane-5"),
                lane_repo_root: PathBuf::from("/tmp/lane-5/repo"),
                base_commit: "def456".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-5/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-5/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-5/worker.pid"),
                host_recovery_note: Some("recover this lane".to_string()),
            },
        );

        let matched = take_resume_candidate_for_task(
            &mut resumable,
            &ready_tasks[0].id,
            &BTreeMap::<usize, ActiveLaneAssignment>::new(),
        )
        .expect("expected a matching resumable lane");
        assert_eq!(matched.0, 5);
        assert_eq!(matched.1.task.id, "P-019D");
        assert_eq!(
            matched.1.host_recovery_note.as_deref(),
            Some("recover this lane")
        );
        assert!(resumable.contains_key(&2));
        assert!(!resumable.contains_key(&5));

        let mut rediscovered = BTreeMap::new();
        rediscovered.insert(
            2,
            LaneResumeCandidate {
                lane_index: 2,
                task: ready_tasks[1].clone(),
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: None,
            },
        );
        resumable
            .get_mut(&2)
            .expect("lane-2 should remain resumable")
            .host_recovery_note = Some("preserve this note".to_string());
        preserve_resume_recovery_notes(&mut rediscovered, &resumable);
        assert_eq!(
            rediscovered
                .get(&2)
                .and_then(|candidate| candidate.host_recovery_note.as_deref()),
            Some("preserve this note")
        );

        let mut active = BTreeMap::new();
        active.insert(
            2,
            ActiveLaneAssignment {
                lane_index: 2,
                attempts: 1,
                task: ready_tasks[1].clone(),
                resumed: true,
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                clean_commit_since: None,
                terminate_requested_at: None,
                host_recovery_note: None,
            },
        );
        assert!(
            take_resume_candidate_for_task(&mut resumable, &ready_tasks[1].id, &active).is_none()
        );
    }

    #[test]
    fn lane_scope_budget_tracks_plan_scope() {
        let xs = LoopTask {
            id: "TASK-XS".to_string(),
            title: "tiny".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("XS".to_string()),
            completion_path_target: None,
            markdown: String::new(),
        };
        let medium = LoopTask {
            id: "TASK-M".to_string(),
            title: "medium".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("M".to_string()),
            completion_path_target: None,
            markdown: String::new(),
        };

        assert_eq!(lane_scope_budget(&xs).max_changed_files, 8);
        assert_eq!(lane_scope_budget(&xs).max_package_roots, 1);
        assert_eq!(lane_scope_budget(&medium).max_changed_files, 28);
        assert_eq!(lane_scope_budget(&medium).max_package_roots, 3);
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
            markdown: "- [ ] `WEB-CRAPS-C` Checkpoint\n  Scope boundary: verification only.\n  Acceptance criteria:\n    - pass".to_string(),
        };
        let normal = LoopTask {
            id: "WEB-CRAPS-D".to_string(),
            title: "real work".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec!["WEB-CRAPS-C".to_string()],
            estimated_scope: Some("M".to_string()),
            completion_path_target: None,
            markdown: "- [ ] `WEB-CRAPS-D` Real work\n  Scope boundary: state source only.\n  Acceptance criteria:\n    - ship".to_string(),
        };

        assert!(is_verification_only_task(&verification_only));
        assert!(!is_verification_only_task(&normal));
    }

    #[test]
    fn parse_loop_plan_tracks_ready_and_blocked_dependencies() {
        let plan = r#"
- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
- [ ] `TASK-002` Second task
  Dependencies:
  - `TASK-001`
  Estimated scope: medium
- [!] `TASK-003` Blocked task
  Dependencies:
  - `TASK-999`
  Estimated scope: large
- [x] `TASK-004` Completed task
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-001", "TASK-002"]);
        assert_eq!(queue.blocked_ids, vec!["TASK-003"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-001"]
        );
    }

    #[test]
    fn parse_loop_plan_skips_merged_placeholder_tasks() {
        let plan = r#"
- [ ] `WEB-CODEGEN-A` Real tranche head
  Dependencies:
  - None
- [ ] `WEB-PAYOUT-TRUTH` Merged into WEB-CODEGEN-A
  Status: This standalone item is kept as a checkbox placeholder for traceability but its work is now folded into WEB-CODEGEN-A above.
  Dependencies:
  - `WEB-CODEGEN-A`
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["WEB-CODEGEN-A"]);
        assert!(queue.blocked_ids.is_empty());
        assert_eq!(snapshot.tasks.len(), 2);
        assert_eq!(snapshot.tasks[1].status, LoopTaskStatus::Done);
    }

    #[test]
    fn parse_loop_plan_blocks_deferred_not_shipped_rows() {
        let plan = r#"
- [ ] `TASK-A` Implement deferred queue handling
  Dependencies:
  - None
- [ ] `TASK-D` Future feature — **DEFERRED, not shipped**
  Dependencies:
  - None
- [ ] `TASK-E` Depends on deferred feature
  Dependencies:
  - `TASK-D`
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-A", "TASK-E"]);
        assert_eq!(queue.blocked_ids, vec!["TASK-D"]);
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == "TASK-D")
                .map(|task| task.status),
            Some(LoopTaskStatus::Blocked)
        );
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-A"]
        );
    }

    #[test]
    fn parse_loop_plan_treats_none_dependencies_as_empty() {
        let plan = r#"
- [ ] `WEB-HOUSE-AUDIT` Audit
  Dependencies: none (parallel with `WEB-CODEGEN-A`)
  Estimated scope: S
- [ ] `WEB-CODEGEN-A` Real tranche head
  Dependencies: `WEB-HOUSE-AUDIT`
  Estimated scope: L
"#;

        let snapshot = parse_loop_plan(plan);
        assert!(snapshot.tasks[0].dependencies.is_empty());
        assert_eq!(snapshot.tasks[1].dependencies, vec!["WEB-HOUSE-AUDIT"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["WEB-HOUSE-AUDIT"]
        );
    }

    #[test]
    fn parse_loop_plan_ignores_parallelism_notes_in_dependency_lines() {
        let plan = r#"
- [x] `WEB-HOUSE-AUDIT` Audit
  Dependencies: none
  Estimated scope: S
- [x] `WEB-CHANNEL-COVERAGE` Coverage
  Dependencies: none
  Estimated scope: S
- [ ] `WEB-CODEGEN-A` Codegen
  Dependencies: `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE`
  Estimated scope: L
- [ ] `WEB-CLIENT-BUILD` Build
  Dependencies: `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE` (Wave 0 gate — finding #3; parallel with `WEB-CODEGEN-A` + `WEB-DESIGN-SYSTEM`)
  Estimated scope: M
- [ ] `WEB-DESIGN-SYSTEM` Design
  Dependencies: `WEB-CLIENT-BUILD` (need bundle for shell exports), `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE` (Wave 0 gate — finding #3). Parallel with `WEB-CODEGEN-A`.
  Estimated scope: L
"#;

        let snapshot = parse_loop_plan(plan);
        let codegen = snapshot
            .tasks
            .iter()
            .find(|task| task.id == "WEB-CODEGEN-A")
            .expect("WEB-CODEGEN-A present");
        let build = snapshot
            .tasks
            .iter()
            .find(|task| task.id == "WEB-CLIENT-BUILD")
            .expect("WEB-CLIENT-BUILD present");
        let design = snapshot
            .tasks
            .iter()
            .find(|task| task.id == "WEB-DESIGN-SYSTEM")
            .expect("WEB-DESIGN-SYSTEM present");

        assert_eq!(
            codegen.dependencies,
            vec!["WEB-HOUSE-AUDIT", "WEB-CHANNEL-COVERAGE"]
        );
        assert_eq!(
            build.dependencies,
            vec!["WEB-HOUSE-AUDIT", "WEB-CHANNEL-COVERAGE"]
        );
        assert_eq!(
            design.dependencies,
            vec![
                "WEB-CLIENT-BUILD",
                "WEB-HOUSE-AUDIT",
                "WEB-CHANNEL-COVERAGE"
            ]
        );
    }

    #[test]
    fn parse_loop_plan_treats_partial_tasks_as_unfinished_dependencies() {
        let plan = r#"
- [~] `TASK-001` Evidence gap
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-002` Depends on partial
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-001", "TASK-002"]);
        assert!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>()
                == vec!["TASK-001"]
        );
    }

    #[test]
    fn parse_loop_plan_skips_partial_completion_path_placeholders() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Completion path: `TASK-010`.
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
- [ ] `TASK-020` Depends on placeholder alias
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-010", "TASK-020"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-010"]
        );
    }

    #[test]
    fn parse_loop_plan_skips_partial_prose_completion_path_placeholders() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Reconciled via `TASK-099` (see `TASK-010` for the completion path).
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
- [ ] `TASK-020` Depends on placeholder alias
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-010", "TASK-020"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-010"]
        );
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
                markdown: "- [ ] `TASK-001`\n  Owns: `src.txt`\n".to_string(),
            },
            LoopTask {
                id: "TASK-002".to_string(),
                title: "clean task".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
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
        let mut attempted = BTreeSet::new();
        let mut deferred = BTreeSet::new();

        assert_eq!(
            record_partial_follow_up("TASK-001", &mut attempted, &mut deferred),
            PartialFollowUpDisposition::RetryLaterThisRun
        );
        assert!(attempted.contains("TASK-001"));
        assert!(!deferred.contains("TASK-001"));

        assert_eq!(
            record_partial_follow_up("TASK-001", &mut attempted, &mut deferred),
            PartialFollowUpDisposition::ParkForRestOfRun
        );
        assert!(attempted.contains("TASK-001"));
        assert!(deferred.contains("TASK-001"));

        clear_partial_follow_up_tracking("TASK-001", &mut attempted, &mut deferred);
        assert!(!attempted.contains("TASK-001"));
        assert!(!deferred.contains("TASK-001"));
    }

    #[test]
    fn completion_path_alias_resolves_once_follow_on_is_done() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Completion path: `TASK-010`.
  Dependencies: none
  Estimated scope: S
- [x] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
- [ ] `TASK-020` Depends on placeholder alias
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-020"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-020"]
        );
    }

    #[test]
    fn update_task_completion_in_plan_text_marks_partial_instead_of_dropping_block() {
        let plan = r#"- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
- [ ] `TASK-002` Second task
  Dependencies:
  - `TASK-001`
  Estimated scope: medium
"#;

        let updated =
            update_task_completion_in_plan_text(plan, "TASK-001", LoopTaskStatus::Partial);

        assert!(updated.contains("- [~] `TASK-001` First task"));
        assert!(updated.contains("TASK-002"));
        assert!(updated.starts_with("- [~] `TASK-001`"));
    }

    #[test]
    fn audit_parallel_completion_drift_demotes_done_without_review_handoff() {
        let repo = unique_temp_dir("parallel-drift-audit");
        let run_root = unique_temp_dir("parallel-drift-audit-run");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n",
        )
        .expect("failed to write plan");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let updated = audit_parallel_completion_drift(
            &repo,
            &fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should exist"),
            &logger,
        )
        .expect("drift audit should succeed");

        assert!(updated.contains("- [~] `TASK-001` First task"));
        let persisted =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should persist");
        assert!(persisted.contains("- [~] `TASK-001` First task"));
    }

    #[test]
    fn iteration_prompt_injects_actionable_and_blocked_tasks() {
        let queue = LoopQueueSnapshot {
            pending_ids: vec!["META-001".to_string(), "GATE-P4".to_string()],
            blocked_ids: vec!["DEC-001".to_string()],
        };
        let prompt = build_iteration_prompt("base prompt", &queue);

        assert!(prompt.contains("First actionable unfinished task: `META-001`"));
        assert!(prompt.contains("Unfinished task count: 2"));
        assert!(prompt.contains("Blocked tasks marked `- [!]` to skip this iteration: DEC-001"));
    }

    #[test]
    fn discovers_sibling_git_repos_by_default() {
        let workspace = unique_temp_dir("loop-siblings");
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let non_repo = workspace.join("notes");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        fs::create_dir_all(&non_repo).expect("failed to create non-repo dir");

        let discovered = discover_sibling_git_repos(&repo_root).expect("should discover siblings");

        assert_eq!(
            discovered,
            vec![sibling_repo.canonicalize().expect("canonical sibling")]
        );

        fs::remove_dir_all(&workspace).expect("failed to remove temp workspace");
    }

    #[test]
    fn resolve_reference_repos_merges_siblings_and_explicit_paths() {
        let workspace = unique_temp_dir("loop-reference-merge");
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let explicit_repo = workspace.join("sharedlib");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        init_git_repo(&explicit_repo);

        let resolved = resolve_reference_repos(
            &repo_root,
            &[PathBuf::from("../sharedlib"), sibling_repo.clone()],
            true,
        )
        .expect("should resolve sibling and explicit repos");

        assert_eq!(
            resolved,
            vec![
                sibling_repo.canonicalize().expect("canonical sibling"),
                explicit_repo.canonicalize().expect("canonical explicit"),
            ]
        );

        fs::remove_dir_all(&workspace).expect("failed to remove temp workspace");
    }

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

    fn git_output<const N: usize>(repo: &PathBuf, args: [&str; N]) -> String {
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
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }
}
