use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::task::JoinSet;

use crate::claude_exec::{describe_claude_harness, run_claude_exec_with_env, FUTILITY_EXIT_MARKER};
use crate::codex_exec::run_codex_exec_with_env;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, clear_and_recreate_dir, ensure_repo_layout,
    git_repo_root, git_stdout, push_branch_with_remote_sync, repo_name, run_git,
    sync_branch_with_remote, timestamp_slug,
};
use crate::ParallelArgs;

const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];
const SHARED_QUEUE_FILES: [&str; 5] = [
    "IMPLEMENTATION_PLAN.md",
    "COMPLETED.md",
    "WORKLIST.md",
    "REVIEW.md",
    "AGENTS.md",
];
const LANE_POLL_INTERVAL: Duration = Duration::from_secs(5);
const CLEAN_COMMIT_GRACE: Duration = Duration::from_secs(15);
const CLEAN_COMMIT_KILL_GRACE: Duration = Duration::from_secs(5);
const DIRECT_REVIEW_QUEUE_PARALLEL_CLAUSE: &str = r#"

Repo-specific direct `REVIEW.md` handoff:
- This repo normally records completion notes in `REVIEW.md`, but `auto parallel` treats queue and review files as host-owned state.
- Do not edit `REVIEW.md`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `WORKLIST.md`, or `ARCHIVED.md` from a lane.
- Preserve blocker or completion evidence in your committed code/tests and command output; the host will reconcile queue and review docs after landing."#;
const LANE_TASK_ID_FILE: &str = "task-id";

pub(crate) async fn run_parallel(args: ParallelArgs) -> Result<()> {
    if args.max_concurrent_workers == 0 {
        bail!("--max-concurrent-workers must be greater than 0");
    }
    if args.claude && args.max_turns == Some(0) {
        bail!("--max-turns must be greater than 0");
    }

    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let reference_repos =
        resolve_reference_repos(&repo_root, &args.reference_repos, args.include_siblings)?;
    if args.max_concurrent_workers > 1 && !reference_repos.is_empty() {
        bail!(
            "auto parallel does not yet support additional reference repos; rerun without `--reference-repo` / `--include-siblings`"
        );
    }

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
    let run_root = args
        .run_root
        .clone()
        .unwrap_or_else(|| repo_root.join(".auto").join("parallel"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    if args.max_concurrent_workers > 1 {
        let status = git_stdout(&repo_root, ["status", "--short"])?;
        if !status.trim().is_empty() {
            bail!(
                "auto parallel requires a clean repo; commit, push, or revert pre-existing changes before launch"
            );
        }
        setup_parallel_tmux_windows(&run_root, args.max_concurrent_workers, std::process::id())?;
    }
    let worker_env = build_loop_worker_env(&args, &run_root)?;

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

    if let Some(commit) = auto_checkpoint_if_needed(
        &repo_root,
        target_branch.as_str(),
        "auto parallel checkpoint",
    )? {
        println!("checkpoint:  committed pre-existing changes at {commit}");
    } else if sync_branch_with_remote(&repo_root, target_branch.as_str())? {
        println!("remote sync: rebased onto origin/{}", target_branch);
    }

    if args.max_concurrent_workers > 1 {
        run_parallel_loop(
            &repo_root,
            &args,
            &target_branch,
            &prompt_template,
            &run_root,
            &worker_env,
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoopWorkerEnv {
    extra_env: Vec<(String, String)>,
    cargo_jobs_summary: String,
    cargo_target_summary: Option<String>,
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
        "{prompt_template}\n\nCurrent queue state for this iteration:\n- First actionable task marked `- [ ]`: `{}`\n- Pending task count: {}\n- {}\n\nExecute the instructions above.",
        queue.pending_ids[0],
        queue.pending_ids.len(),
        blocked_clause
    )
}

fn build_loop_worker_env(args: &ParallelArgs, run_root: &Path) -> Result<LoopWorkerEnv> {
    let inherited = std::env::var("CARGO_BUILD_JOBS").ok();
    let inherited_target = std::env::var("CARGO_TARGET_DIR").ok();
    let parallelism = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4);
    resolve_loop_worker_env(
        args.cargo_build_jobs,
        inherited.as_deref(),
        inherited_target.as_deref(),
        parallelism,
        args.max_concurrent_workers,
        run_root,
    )
}

fn resolve_loop_worker_env(
    cargo_build_jobs: Option<usize>,
    inherited_cargo_build_jobs: Option<&str>,
    inherited_cargo_target_dir: Option<&str>,
    available_parallelism: usize,
    max_concurrent_workers: usize,
    run_root: &Path,
) -> Result<LoopWorkerEnv> {
    if let Some(jobs) = cargo_build_jobs {
        if jobs == 0 {
            bail!("--cargo-build-jobs must be greater than 0");
        }
        return Ok(cargo_build_jobs_env(
            jobs,
            format!("override CARGO_BUILD_JOBS={jobs}"),
            inherited_cargo_target_dir,
            max_concurrent_workers,
            run_root,
        ));
    }

    if let Some(value) = inherited_cargo_build_jobs {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(inherited_target_loop_worker_env(
                format!("inherited CARGO_BUILD_JOBS={value}"),
                inherited_cargo_target_dir,
                max_concurrent_workers,
                run_root,
            ));
        }
    }

    let jobs = default_cargo_build_jobs_for(available_parallelism, max_concurrent_workers);
    Ok(cargo_build_jobs_env(
        jobs,
        format!("auto CARGO_BUILD_JOBS={jobs}"),
        inherited_cargo_target_dir,
        max_concurrent_workers,
        run_root,
    ))
}

fn cargo_build_jobs_env(
    jobs: usize,
    cargo_jobs_summary: String,
    inherited_cargo_target_dir: Option<&str>,
    max_concurrent_workers: usize,
    run_root: &Path,
) -> LoopWorkerEnv {
    let mut env = inherited_target_loop_worker_env(
        cargo_jobs_summary,
        inherited_cargo_target_dir,
        max_concurrent_workers,
        run_root,
    );
    env.extra_env
        .push(("CARGO_BUILD_JOBS".to_string(), jobs.to_string()));
    env
}

fn inherited_target_loop_worker_env(
    cargo_jobs_summary: String,
    inherited_cargo_target_dir: Option<&str>,
    max_concurrent_workers: usize,
    run_root: &Path,
) -> LoopWorkerEnv {
    let mut extra_env = Vec::new();
    let cargo_target_summary = resolve_parallel_cargo_target_dir(
        inherited_cargo_target_dir,
        max_concurrent_workers,
        run_root,
    )
    .map(|target_dir| {
        extra_env.push(("CARGO_TARGET_DIR".to_string(), target_dir.clone()));
        target_dir
    });
    LoopWorkerEnv {
        extra_env,
        cargo_jobs_summary,
        cargo_target_summary,
    }
}

fn resolve_parallel_cargo_target_dir(
    inherited_cargo_target_dir: Option<&str>,
    max_concurrent_workers: usize,
    run_root: &Path,
) -> Option<String> {
    let inherited = inherited_cargo_target_dir
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    if inherited.is_some() {
        return inherited;
    }
    if max_concurrent_workers <= 1 {
        return None;
    }
    Some(
        run_root
            .join("shared-cargo-target")
            .to_string_lossy()
            .into_owned(),
    )
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

fn extract_task_id(task_line: &str) -> Option<String> {
    let rest = task_line.strip_prefix('`')?;
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoopTaskStatus {
    Pending,
    Blocked,
    Done,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoopTask {
    id: String,
    title: String,
    status: LoopTaskStatus,
    dependencies: Vec<String>,
    estimated_scope: Option<String>,
    markdown: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LoopPlanSnapshot {
    tasks: Vec<LoopTask>,
}

impl LoopPlanSnapshot {
    fn queue_snapshot(&self) -> LoopQueueSnapshot {
        let mut queue = LoopQueueSnapshot::default();
        for task in &self.tasks {
            match task.status {
                LoopTaskStatus::Pending => queue.pending_ids.push(task.id.clone()),
                LoopTaskStatus::Blocked => queue.blocked_ids.push(task.id.clone()),
                LoopTaskStatus::Done => {}
            }
        }
        queue
    }

    fn ready_tasks(&self, inflight: &BTreeSet<String>) -> Vec<LoopTask> {
        let unresolved = self
            .tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.status,
                    LoopTaskStatus::Pending | LoopTaskStatus::Blocked
                )
            })
            .map(|task| task.id.as_str())
            .chain(inflight.iter().map(String::as_str))
            .collect::<BTreeSet<_>>();

        self.tasks
            .iter()
            .filter(|task| task.status == LoopTaskStatus::Pending)
            .filter(|task| !inflight.contains(&task.id))
            .filter(|task| {
                task.dependencies
                    .iter()
                    .all(|dep| !unresolved.contains(dep.as_str()))
            })
            .cloned()
            .collect()
    }
}

fn parse_loop_plan(plan: &str) -> LoopPlanSnapshot {
    let mut tasks = Vec::new();
    let mut current_header = None::<String>;
    let mut current_lines = Vec::<String>::new();

    for line in plan.lines() {
        if parse_task_header(line).is_some() {
            if let Some(task) = finalize_task(current_header.take(), &current_lines) {
                tasks.push(task);
            }
            current_header = Some(line.to_string());
            current_lines = vec![line.to_string()];
            continue;
        }

        if current_header.is_some() {
            current_lines.push(line.to_string());
        }
    }

    if let Some(task) = finalize_task(current_header, &current_lines) {
        tasks.push(task);
    }

    LoopPlanSnapshot { tasks }
}

fn finalize_task(header: Option<String>, lines: &[String]) -> Option<LoopTask> {
    let header = header?;
    let (status, id, title) = parse_task_header(&header)?;
    let markdown = lines.join("\n");
    Some(LoopTask {
        id,
        title,
        status,
        dependencies: parse_task_dependencies(&markdown),
        estimated_scope: task_field_line_value(&markdown, "Estimated scope:"),
        markdown,
    })
}

fn parse_task_header(line: &str) -> Option<(LoopTaskStatus, String, String)> {
    let trimmed = line.trim_start();
    let (status, rest) = if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
        (LoopTaskStatus::Pending, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [!] ") {
        (LoopTaskStatus::Blocked, rest)
    } else if let Some(rest) = trimmed
        .strip_prefix("- [x] ")
        .or_else(|| trimmed.strip_prefix("- [X] "))
    {
        (LoopTaskStatus::Done, rest)
    } else {
        return None;
    };
    let id = extract_task_id(rest)?;
    let title = rest
        .trim_start_matches('`')
        .trim_start_matches(&id)
        .trim_start_matches('`')
        .trim()
        .to_string();
    Some((status, id, title))
}

fn parse_task_dependencies(markdown: &str) -> Vec<String> {
    task_field_body(markdown, "Dependencies:", "Estimated scope:")
        .map(|body| collect_task_refs(&body))
        .unwrap_or_default()
}

fn task_field_line_value(markdown: &str, field: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        let trimmed = line.trim_start();
        trimmed
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
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(field) {
            collecting = true;
            if !rest.trim().is_empty() {
                body.push(rest.trim().to_string());
            }
            continue;
        }
        if collecting && trimmed.starts_with(next_field) {
            break;
        }
        if collecting {
            body.push(line.to_string());
        }
    }
    collecting.then(|| body.join("\n"))
}

fn collect_task_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else {
            break;
        };
        let candidate = &rest[..end];
        if candidate
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            refs.push(candidate.to_string());
        }
        rest = &rest[end + 1..];
    }
    dedup_task_refs(refs)
}

fn dedup_task_refs(refs: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for task_id in refs {
        if seen.insert(task_id.clone()) {
            ordered.push(task_id);
        }
    }
    ordered
}

#[derive(Clone, Debug)]
struct LaneRunConfig {
    claude: bool,
    max_turns: Option<usize>,
    model: String,
    reasoning_effort: String,
    codex_bin: PathBuf,
    extra_env: Vec<(String, String)>,
}

impl LaneRunConfig {
    fn new(args: &ParallelArgs, worker_env: &LoopWorkerEnv) -> Self {
        Self {
            claude: args.claude,
            max_turns: effective_parallel_claude_max_turns(args),
            model: args.model.clone(),
            reasoning_effort: args.reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            extra_env: worker_env.extra_env.clone(),
        }
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
    stderr_log_path: PathBuf,
    worker_pid_path: PathBuf,
    clean_commit_since: Option<Instant>,
    terminate_requested_at: Option<Instant>,
}

#[derive(Clone, Debug)]
struct LaneResumeCandidate {
    lane_index: usize,
    task: LoopTask,
    lane_root: PathBuf,
    lane_repo_root: PathBuf,
    base_commit: String,
    stderr_log_path: PathBuf,
    worker_pid_path: PathBuf,
}

#[derive(Debug)]
struct LaneAttemptResult {
    lane_index: usize,
    exit_status: ExitStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LaneRepoProgress {
    None,
    Dirty(String),
    NewCommits,
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
                println!("no pending `- [ ]` tasks remain; stopping.");
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
                "auto parallel",
                &worker_env.extra_env,
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
                "auto parallel",
                &worker_env.extra_env,
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

async fn run_parallel_loop(
    repo_root: &Path,
    args: &ParallelArgs,
    target_branch: &str,
    prompt_template: &str,
    run_root: &Path,
    worker_env: &LoopWorkerEnv,
) -> Result<()> {
    let harness = if args.claude { "Claude" } else { "Codex" };
    let lane_config = LaneRunConfig::new(args, worker_env);
    let mut join_set = JoinSet::<Result<LaneAttemptResult>>::new();
    let mut active_lanes = BTreeMap::<usize, ActiveLaneAssignment>::new();
    let mut active_tasks = BTreeSet::<String>::new();
    let mut landed = 0usize;
    let mut resumable_lanes =
        discover_resume_candidates(run_root, target_branch, &inspect_loop_plan(repo_root)?)?;
    landed += harvest_resumable_lane_results(repo_root, target_branch, &mut resumable_lanes)?;
    resumable_lanes =
        discover_resume_candidates(run_root, target_branch, &inspect_loop_plan(repo_root)?)?;

    loop {
        nudge_lingering_committed_lanes(&mut active_lanes)?;

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

            let plan = inspect_loop_plan(repo_root)?;
            let queue = plan.queue_snapshot();
            if queue.pending_ids.is_empty() {
                break;
            }

            let ready = plan.ready_tasks(&active_tasks);
            if ready.is_empty() {
                break;
            }
            let (verification_only, executable_ready): (Vec<_>, Vec<_>) =
                ready.into_iter().partition(is_verification_only_task);
            if executable_ready.is_empty() {
                println!(
                    "no executable dependency-ready tasks remain; manual verification-only checkpoints must be cleared before continuing: {}",
                    verification_only
                        .iter()
                        .map(|task| task.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                break;
            }

            let (task, lane_index, resume_candidate) = if let Some((lane_index, candidate)) =
                take_matching_resume_candidate(
                    &mut resumable_lanes,
                    &executable_ready,
                    &active_lanes,
                ) {
                (candidate.task.clone(), lane_index, Some(candidate))
            } else {
                (
                    executable_ready[0].clone(),
                    next_free_lane_index(args.max_concurrent_workers, &active_lanes)
                        .context("failed to find a free loop lane")?,
                    None,
                )
            };
            let mut assignment = prepare_parallel_lane_assignment(
                repo_root,
                run_root,
                target_branch,
                lane_index,
                task,
                resume_candidate,
            )?;
            let plan_for_prompt = inspect_loop_plan(repo_root)?;
            spawn_parallel_lane_attempt(
                &mut join_set,
                &lane_config,
                prompt_template,
                &plan_for_prompt,
                &mut assignment,
                target_branch,
            )?;
            println!(
                "dispatch:    lane-{} -> {} {}{}",
                lane_index,
                assignment.task.id,
                assignment.task.title,
                if assignment.resumed { " [resume]" } else { "" }
            );
            active_tasks.insert(assignment.task.id.clone());
            active_lanes.insert(lane_index, assignment);
        }

        if active_lanes.is_empty() {
            let plan = inspect_loop_plan(repo_root)?;
            let queue = plan.queue_snapshot();
            if queue.pending_ids.is_empty() {
                if queue.blocked_ids.is_empty() {
                    println!("no pending `- [ ]` tasks remain; stopping.");
                } else {
                    println!(
                        "all remaining tasks are blocked `[!]`; stopping. blocked: {}",
                        queue.blocked_ids.join(", ")
                    );
                }
                break;
            }

            println!(
                "no dependency-ready tasks remain to dispatch; stopping. pending: {} blocked: {}",
                queue.pending_ids.join(", "),
                if queue.blocked_ids.is_empty() {
                    "none".to_string()
                } else {
                    queue.blocked_ids.join(", ")
                }
            );
            break;
        }

        let joined = match tokio::time::timeout(LANE_POLL_INTERVAL, join_set.join_next()).await {
            Ok(result) => result.context("parallel lane join set unexpectedly empty")?,
            Err(_) => continue,
        };
        let lane_result = joined.context("parallel lane task panicked")??;
        let mut assignment = active_lanes
            .remove(&lane_result.lane_index)
            .with_context(|| format!("missing active state for lane-{}", lane_result.lane_index))?;
        active_tasks.remove(&assignment.task.id);

        if !lane_result.exit_status.success() {
            match inspect_lane_repo_progress(&assignment.lane_repo_root, &assignment.base_commit)? {
                LaneRepoProgress::NewCommits => {
                    land_parallel_lane_result(repo_root, target_branch, &assignment)?;
                    landed += 1;
                    println!(
                        "landed:      {} via lane-{} after non-zero worker exit (total landed: {})",
                        assignment.task.id, assignment.lane_index, landed
                    );
                    continue;
                }
                LaneRepoProgress::Dirty(_) | LaneRepoProgress::None => {}
            }
            let exit_code = lane_result.exit_status.code().unwrap_or(-1);
            let is_futility = exit_code == FUTILITY_EXIT_MARKER;
            if assignment.attempts > args.max_retries {
                bail!(
                    "{} lane-{} (`{}`) exited with status {} after {} attempts; see {}",
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
                );
            }

            println!(
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
            );
            let plan_for_prompt = inspect_loop_plan(repo_root)?;
            spawn_parallel_lane_attempt(
                &mut join_set,
                &lane_config,
                prompt_template,
                &plan_for_prompt,
                &mut assignment,
                target_branch,
            )?;
            active_tasks.insert(assignment.task.id.clone());
            active_lanes.insert(assignment.lane_index, assignment);
            continue;
        }

        match inspect_lane_repo_progress(&assignment.lane_repo_root, &assignment.base_commit)? {
            LaneRepoProgress::Dirty(status) => {
                bail!(
                    "parallel lane-{} (`{}`) exited cleanly but left uncommitted changes:\n{}",
                    assignment.lane_index,
                    assignment.task.id,
                    status
                );
            }
            LaneRepoProgress::None => {
                bail!(
                    "parallel lane-{} (`{}`) exited cleanly without producing a local commit; see {}",
                    assignment.lane_index,
                    assignment.task.id,
                    assignment.stderr_log_path.display()
                );
            }
            LaneRepoProgress::NewCommits => {
                land_parallel_lane_result(repo_root, target_branch, &assignment)?;
                landed += 1;
                println!(
                    "landed:      {} via lane-{} (total landed: {})",
                    assignment.task.id, assignment.lane_index, landed
                );
            }
        }
    }

    Ok(())
}

fn inspect_loop_plan(repo_root: &Path) -> Result<LoopPlanSnapshot> {
    let plan = read_loop_plan(repo_root)?;
    Ok(parse_loop_plan(&plan))
}

fn setup_parallel_tmux_windows(run_root: &Path, lanes: usize, host_pid: u32) -> Result<()> {
    let Some(tmux_pane) = env::var_os("TMUX_PANE") else {
        return Ok(());
    };
    if tmux_pane.is_empty() {
        return Ok(());
    }

    let live_log_path = run_root.join("live.log");
    fs::write(&live_log_path, b"")
        .with_context(|| format!("failed to initialize {}", live_log_path.display()))?;

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

    run_tmux([
        "pipe-pane",
        "-t",
        &pane_target,
        &format!(
            "cat >> {}",
            shell_quote(&live_log_path.display().to_string())
        ),
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

    let live_log = shell_quote(&live_log_path.display().to_string());
    for lane in 1..=lanes {
        let window_name = format!("parallel-lane-{lane}");
        let filter = format!("^\\[auto parallel lane-{lane} ");
        let script = format!(
            "mkdir -p {run_root}; touch {live_log}; tail --pid={host_pid} -n +1 -F {live_log} | grep --line-buffered -E {filter} || true; printf '\\n[auto parallel lane-{lane}] host process {host_pid} exited; log tail stopped.\\n'; exec bash",
            run_root = shell_quote(&run_root.display().to_string()),
            live_log = live_log,
            filter = shell_quote(&filter),
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
            stderr_log_path: candidate.stderr_log_path,
            worker_pid_path: candidate.worker_pid_path,
            clean_commit_since: None,
            terminate_requested_at: None,
        });
    }

    let lane_root = run_root.join("lanes").join(format!("lane-{lane_index}"));
    clear_and_recreate_dir(&lane_root)?;
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
        stderr_log_path: lane_root.join("stderr.log"),
        worker_pid_path: lane_root.join("worker.pid"),
        clean_commit_since: None,
        terminate_requested_at: None,
    })
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
        .filter(|task| task.status == LoopTaskStatus::Pending)
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

        let stderr_log_path = lane_root.join("stderr.log");
        let worker_pid_path = lane_root.join("worker.pid");
        clear_stale_worker_pid(&worker_pid_path)?;
        if let Some(pid) = read_worker_pid(&worker_pid_path)? {
            if worker_pid_is_alive(pid)? {
                bail!(
                    "lane-{} still has a live worker pid {} in {}; stop the previous auto parallel run before restarting",
                    lane_index,
                    pid,
                    lane_root.display()
                );
            }
            fs::remove_file(&worker_pid_path)
                .with_context(|| format!("failed to remove {}", worker_pid_path.display()))?;
        }

        let base_commit = infer_lane_base_commit(&lane_repo_root, target_branch)?;
        if matches!(
            inspect_lane_repo_progress(&lane_repo_root, &base_commit)?,
            LaneRepoProgress::None
        ) {
            continue;
        }

        candidates.insert(
            lane_index,
            LaneResumeCandidate {
                lane_index,
                task,
                lane_root,
                lane_repo_root,
                base_commit,
                stderr_log_path,
                worker_pid_path,
            },
        );
    }

    Ok(candidates)
}

fn harvest_resumable_lane_results(
    repo_root: &Path,
    target_branch: &str,
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
) -> Result<usize> {
    let mut landed = 0usize;
    let lane_indexes = resumable_lanes.keys().copied().collect::<Vec<_>>();
    for lane_index in lane_indexes {
        let should_land = {
            let candidate = resumable_lanes
                .get(&lane_index)
                .with_context(|| format!("missing resumable lane-{lane_index}"))?;
            matches!(
                inspect_lane_repo_progress(&candidate.lane_repo_root, &candidate.base_commit)?,
                LaneRepoProgress::NewCommits
            )
        };
        if !should_land {
            continue;
        }
        let candidate = resumable_lanes
            .remove(&lane_index)
            .with_context(|| format!("missing resumable lane-{lane_index}"))?;
        let assignment = ActiveLaneAssignment {
            lane_index: candidate.lane_index,
            attempts: 0,
            task: candidate.task,
            resumed: true,
            lane_root: candidate.lane_root,
            lane_repo_root: candidate.lane_repo_root,
            base_commit: candidate.base_commit,
            stderr_log_path: candidate.stderr_log_path,
            worker_pid_path: candidate.worker_pid_path,
            clean_commit_since: None,
            terminate_requested_at: None,
        };
        match land_parallel_lane_result(repo_root, target_branch, &assignment) {
            Ok(()) => {
                landed += 1;
                println!(
                    "resumed:     landed {} from lane-{} before dispatch (total landed: {})",
                    assignment.task.id, assignment.lane_index, landed
                );
            }
            Err(error) => {
                println!(
                    "warning: resume harvest for lane-{} `{}` failed; keeping lane resumable instead: {error:#}",
                    assignment.lane_index, assignment.task.id
                );
                resumable_lanes.insert(
                    lane_index,
                    LaneResumeCandidate {
                        lane_index: assignment.lane_index,
                        task: assignment.task,
                        lane_root: assignment.lane_root,
                        lane_repo_root: assignment.lane_repo_root,
                        base_commit: assignment.base_commit,
                        stderr_log_path: assignment.stderr_log_path,
                        worker_pid_path: assignment.worker_pid_path,
                    },
                );
            }
        }
    }
    Ok(landed)
}

fn take_matching_resume_candidate(
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
    ready_tasks: &[LoopTask],
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
) -> Option<(usize, LaneResumeCandidate)> {
    for task in ready_tasks {
        let lane_index = resumable_lanes
            .iter()
            .find(|(lane_index, candidate)| {
                !active_lanes.contains_key(lane_index) && candidate.task.id == task.id
            })
            .map(|(lane_index, _)| *lane_index);
        let Some(lane_index) = lane_index else {
            continue;
        };
        let candidate = resumable_lanes.remove(&lane_index)?;
        return Some((lane_index, candidate));
    }
    None
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
    if lane_repo_root.join(".githooks").exists() {
        run_git(lane_repo_root, ["config", "core.hooksPath", ".githooks"])?;
    }
    Ok(())
}

fn spawn_parallel_lane_attempt(
    join_set: &mut JoinSet<Result<LaneAttemptResult>>,
    lane_config: &LaneRunConfig,
    prompt_template: &str,
    plan: &LoopPlanSnapshot,
    assignment: &mut ActiveLaneAssignment,
    target_branch: &str,
) -> Result<()> {
    assignment.attempts += 1;
    assignment.clean_commit_since = None;
    assignment.terminate_requested_at = None;
    let full_prompt =
        build_parallel_lane_prompt(prompt_template, plan, &assignment.task, target_branch);
    let prompt_path = assignment.lane_root.join(format!(
        "{}-attempt-{:02}-prompt.md",
        assignment.task.id, assignment.attempts
    ));
    let repo_root = assignment.lane_repo_root.clone();
    let stderr_log_path = assignment.stderr_log_path.clone();
    let worker_pid_path = assignment.worker_pid_path.clone();
    let lane_index = assignment.lane_index;
    let task_id = assignment.task.id.clone();
    let lane_config = lane_config.clone();

    join_set.spawn(async move {
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        let context_label = format!("auto parallel lane-{lane_index} {task_id}");
        let exit_status = if lane_config.claude {
            run_claude_exec_with_env(
                &repo_root,
                &full_prompt,
                &lane_config.model,
                &lane_config.reasoning_effort,
                lane_config.max_turns,
                &stderr_log_path,
                &context_label,
                &lane_config.extra_env,
                Some(&worker_pid_path),
            )
            .await?
        } else {
            run_codex_exec_with_env(
                &repo_root,
                &full_prompt,
                &lane_config.model,
                &lane_config.reasoning_effort,
                &lane_config.codex_bin,
                &stderr_log_path,
                &context_label,
                &lane_config.extra_env,
                Some(&worker_pid_path),
            )
            .await?
        };
        Ok(LaneAttemptResult {
            lane_index,
            exit_status,
        })
    });
    Ok(())
}

fn nudge_lingering_committed_lanes(
    active_lanes: &mut BTreeMap<usize, ActiveLaneAssignment>,
) -> Result<()> {
    for assignment in active_lanes.values_mut() {
        match inspect_lane_repo_progress(&assignment.lane_repo_root, &assignment.base_commit)? {
            LaneRepoProgress::NewCommits => {
                let Some(pid) = read_worker_pid(&assignment.worker_pid_path)? else {
                    assignment.clean_commit_since = None;
                    assignment.terminate_requested_at = None;
                    continue;
                };
                if !worker_pid_is_alive(pid)? {
                    assignment.clean_commit_since = None;
                    assignment.terminate_requested_at = None;
                    continue;
                }

                let commit_since = assignment
                    .clean_commit_since
                    .get_or_insert_with(Instant::now);
                if let Some(requested_at) = assignment.terminate_requested_at {
                    if requested_at.elapsed() >= CLEAN_COMMIT_KILL_GRACE {
                        signal_worker(pid, "KILL")?;
                        println!(
                            "harvest:     lane-{} `{}` still lingered after clean commit; sent SIGKILL to pid {}",
                            assignment.lane_index, assignment.task.id, pid
                        );
                        assignment.terminate_requested_at = None;
                    }
                    continue;
                }

                if commit_since.elapsed() >= CLEAN_COMMIT_GRACE {
                    signal_worker(pid, "TERM")?;
                    println!(
                        "harvest:     lane-{} `{}` has a clean local commit; sent SIGTERM to lingering pid {}",
                        assignment.lane_index, assignment.task.id, pid
                    );
                    assignment.terminate_requested_at = Some(Instant::now());
                }
            }
            LaneRepoProgress::Dirty(_) | LaneRepoProgress::None => {
                assignment.clean_commit_since = None;
                assignment.terminate_requested_at = None;
            }
        }
    }
    Ok(())
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
    let scope_budget = render_lane_scope_budget(task);
    let allowed_surfaces = render_task_surface_summary(task);

    format!(
        "{prompt_template}\n\nParallel assignment for this worker:\n- Assigned task for this lane: `{task_id}` {title}\n- This task is already dependency-ready for this run: {dependency_clause}\n- The host owns queue reconciliation and branch landing in parallel mode.\n- Do not push to `origin/{branch}` or any other remote. Create local commit(s) only; the host will land them onto `{branch}`.\n- {protected_clause}\n- Keep the final diff within this task's scope budget ({scope_budget}) and declared file surfaces ({allowed_surfaces}). If the real fix needs more than that, stop and report the blocker instead of widening scope.\n- Do not override the host-provided `CARGO_TARGET_DIR`. Shared build cache is part of the execution contract for this run; if Cargo is busy, wait or narrow the proof instead of switching to a lane-local target dir.\n- If the repo contains `scripts/run-task-verification.sh`, run every command from the task's `Verification:` block through that wrapper instead of invoking the command bare. Use the exact command text from the `Verification:` block so the verification receipt matches the task contract.\n- Never hand-edit verification receipt files. They are execution evidence, not notes.\n- If the lane repo contains `.githooks/`, pre-commit enforcement is active in this clone via `core.hooksPath=.githooks`; do not bypass it.\n\nCanonical queue snapshot when this lane started:\n- Pending task count: {pending_count}\n- Currently blocked tasks: {blocked_clause}\n\nAssigned task markdown:\n{markdown}\n",
        task_id = task.id,
        title = task.title,
        dependency_clause = dependency_clause,
        branch = branch,
        protected_clause = protected_clause,
        scope_budget = scope_budget,
        allowed_surfaces = allowed_surfaces,
        pending_count = queue.pending_ids.len(),
        blocked_clause = blocked_clause,
        markdown = task.markdown
    )
}

fn inspect_lane_repo_progress(repo_root: &Path, base_commit: &str) -> Result<LaneRepoProgress> {
    let status = git_stdout(repo_root, ["status", "--short"])?;
    if !status.trim().is_empty() {
        return Ok(LaneRepoProgress::Dirty(status.trim().to_string()));
    }

    let head = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    if head.trim() == base_commit {
        Ok(LaneRepoProgress::None)
    } else {
        Ok(LaneRepoProgress::NewCommits)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfacePatternKind {
    Exact,
    Prefix,
    Glob,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SurfacePattern {
    kind: SurfacePatternKind,
    value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LaneScopeBudget {
    max_changed_files: usize,
    max_package_roots: usize,
    max_area_roots: usize,
}

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

fn render_task_surface_summary(task: &LoopTask) -> String {
    let tokens = collect_task_surface_tokens(task);
    if tokens.is_empty() {
        return "no explicit repo paths; host falls back to scope budget + queue-file bans"
            .to_string();
    }
    let mut rendered = tokens
        .into_iter()
        .take(8)
        .map(|token| format!("`{token}`"))
        .collect::<Vec<_>>();
    if rendered.len() == 8 {
        rendered.push("...".to_string());
    }
    rendered.join(", ")
}

fn is_verification_only_task(task: &LoopTask) -> bool {
    task_field_body(&task.markdown, "Scope boundary:", "Acceptance criteria:")
        .map(|body| body.to_ascii_lowercase().contains("verification only"))
        .unwrap_or(false)
}

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

fn collect_task_surface_tokens(task: &LoopTask) -> Vec<String> {
    let mut tokens = Vec::new();
    if let Some(body) = task_field_body(&task.markdown, "Owns:", "Integration touchpoints:") {
        tokens.extend(extract_backtick_tokens(&body));
    }
    if let Some(body) = task_field_body(
        &task.markdown,
        "Integration touchpoints:",
        "Scope boundary:",
    ) {
        tokens.extend(extract_backtick_tokens(&body));
    }
    dedup_tokens(tokens)
}

fn extract_backtick_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else {
            break;
        };
        let token = rest[..end].trim();
        if !token.is_empty() {
            tokens.push(token.to_string());
        }
        rest = &rest[end + 1..];
    }
    tokens
}

fn dedup_tokens(tokens: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for token in tokens {
        if seen.insert(token.clone()) {
            deduped.push(token);
        }
    }
    deduped
}

fn validate_parallel_lane_result(
    lane_repo_root: &Path,
    assignment: &ActiveLaneAssignment,
    lane_head: &str,
) -> Result<()> {
    validate_lane_commit_subjects(
        lane_repo_root,
        &assignment.task.id,
        &assignment.base_commit,
        lane_head,
    )?;
    let changed_files = changed_files_between(lane_repo_root, &assignment.base_commit, lane_head)?;
    let budget = lane_scope_budget(&assignment.task);
    if changed_files.is_empty() {
        bail!(
            "parallel lane-{} (`{}`) produced a commit with no file-level diff",
            assignment.lane_index,
            assignment.task.id
        );
    }

    let shared_queue_edits = changed_files
        .iter()
        .filter(|path| SHARED_QUEUE_FILES.contains(&path.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !shared_queue_edits.is_empty() {
        bail!(
            "parallel lane-{} (`{}`) edited host-owned queue files: {}",
            assignment.lane_index,
            assignment.task.id,
            shared_queue_edits.join(", ")
        );
    }

    if changed_files.len() > budget.max_changed_files {
        bail!(
            "parallel lane-{} (`{}`) exceeded scope budget: {} changed files (budget {})",
            assignment.lane_index,
            assignment.task.id,
            changed_files.len(),
            budget.max_changed_files
        );
    }

    let package_roots = changed_files
        .iter()
        .filter_map(|path| cargo_package_root(lane_repo_root, path))
        .collect::<BTreeSet<_>>();
    if package_roots.len() > budget.max_package_roots {
        bail!(
            "parallel lane-{} (`{}`) touched too many Rust packages: {} (budget {}) [{}]",
            assignment.lane_index,
            assignment.task.id,
            package_roots.len(),
            budget.max_package_roots,
            package_roots.into_iter().collect::<Vec<_>>().join(", ")
        );
    }

    let top_level_areas = changed_files
        .iter()
        .map(|path| top_level_area(path))
        .collect::<BTreeSet<_>>();
    if top_level_areas.len() > budget.max_area_roots {
        bail!(
            "parallel lane-{} (`{}`) touched too many top-level areas: {} (budget {}) [{}]",
            assignment.lane_index,
            assignment.task.id,
            top_level_areas.len(),
            budget.max_area_roots,
            top_level_areas.into_iter().collect::<Vec<_>>().join(", ")
        );
    }

    let allowed_patterns = allowed_patterns_for_task(lane_repo_root, &assignment.task)?;
    if !allowed_patterns.is_empty() {
        let matched_package_roots = changed_files
            .iter()
            .filter(|path| matches_allowed(path, &allowed_patterns))
            .filter_map(|path| cargo_package_root(lane_repo_root, path))
            .collect::<BTreeSet<_>>();
        let out_of_scope = changed_files
            .iter()
            .filter(|path| !matches_allowed(path, &allowed_patterns))
            .filter(|path| {
                !is_adjacent_rust_integration_path(lane_repo_root, path, &matched_package_roots)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !out_of_scope.is_empty() {
            bail!(
                "parallel lane-{} (`{}`) changed files outside the task contract: {}",
                assignment.lane_index,
                assignment.task.id,
                out_of_scope.join(", ")
            );
        }
    }

    Ok(())
}

fn validate_lane_commit_subjects(
    repo_root: &Path,
    task_id: &str,
    base_commit: &str,
    head_ref: &str,
) -> Result<()> {
    let range = format!("{base_commit}..{head_ref}");
    let subjects = git_stdout(repo_root, ["log", "--format=%s", &range])?;
    let mismatches = subjects
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.contains(task_id))
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    if !mismatches.is_empty() {
        bail!(
            "parallel lane commit subjects must include assigned task id `{task_id}`; offending subjects: {}",
            mismatches.join(" | ")
        );
    }
    Ok(())
}

fn changed_files_between(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
) -> Result<Vec<String>> {
    let range = format!("{base_commit}..{head_ref}");
    let output = git_stdout(repo_root, ["diff", "--name-only", &range])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect())
}

fn top_level_area(path: &str) -> String {
    path.split('/').next().unwrap_or(path).to_string()
}

fn cargo_package_root(repo_root: &Path, path: &str) -> Option<String> {
    let relative = Path::new(path);
    let mut current = relative
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    loop {
        let manifest = if current.as_os_str().is_empty() {
            repo_root.join("Cargo.toml")
        } else {
            repo_root.join(&current).join("Cargo.toml")
        };
        if manifest.is_file() {
            return Some(if current.as_os_str().is_empty() {
                ".".to_string()
            } else {
                current.to_string_lossy().into_owned()
            });
        }
        if current.as_os_str().is_empty() {
            return None;
        }
        current.pop();
    }
}

fn allowed_patterns_for_task(repo_root: &Path, task: &LoopTask) -> Result<Vec<SurfacePattern>> {
    let tracked = git_stdout(repo_root, ["ls-files"])?;
    let tracked = tracked
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let mut patterns = Vec::new();
    for token in collect_task_surface_tokens(task) {
        patterns.extend(resolve_surface_patterns(repo_root, &tracked, &token));
    }
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for pattern in patterns {
        if seen.insert((pattern.kind as u8, pattern.value.clone())) {
            deduped.push(pattern);
        }
    }
    Ok(deduped)
}

fn resolve_surface_patterns(
    repo_root: &Path,
    tracked: &[String],
    token: &str,
) -> Vec<SurfacePattern> {
    let Some(normalized) = normalize_surface_token(token) else {
        return Vec::new();
    };
    if normalized.contains('*') || normalized.contains('?') || normalized.contains('[') {
        return vec![SurfacePattern {
            kind: SurfacePatternKind::Glob,
            value: normalized,
        }];
    }

    let candidate_rel = normalized.trim_start_matches('/');
    if candidate_rel.is_empty() {
        return Vec::new();
    }
    let candidate = repo_root.join(candidate_rel);
    if candidate.is_dir() {
        return vec![SurfacePattern {
            kind: SurfacePatternKind::Prefix,
            value: format!("{}/", candidate_rel.trim_end_matches('/')),
        }];
    }
    if candidate.is_file() || tracked.iter().any(|path| path == candidate_rel) {
        return vec![SurfacePattern {
            kind: SurfacePatternKind::Exact,
            value: candidate_rel.to_string(),
        }];
    }

    let suffix_matches = tracked
        .iter()
        .filter(|path| path.ends_with(candidate_rel))
        .cloned()
        .collect::<Vec<_>>();
    if suffix_matches.len() == 1 {
        return vec![SurfacePattern {
            kind: SurfacePatternKind::Exact,
            value: suffix_matches[0].clone(),
        }];
    }

    Vec::new()
}

fn normalize_surface_token(token: &str) -> Option<String> {
    let mut value = token.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("none") {
        return None;
    }
    if let Some(stripped) = value.strip_prefix("./") {
        value = stripped;
    }
    if let Some((prefix, suffix)) = value.rsplit_once(':') {
        if prefix.contains('/')
            && suffix
                .chars()
                .all(|ch| ch.is_ascii_digit() || ch == ',' || ch == '-')
        {
            value = prefix;
        }
    }
    Some(value.trim_end_matches('/').to_string())
}

fn matches_allowed(path: &str, patterns: &[SurfacePattern]) -> bool {
    patterns.iter().any(|pattern| match pattern.kind {
        SurfacePatternKind::Exact => path == pattern.value,
        SurfacePatternKind::Prefix => path.starts_with(&pattern.value),
        SurfacePatternKind::Glob => glob_match(path, &pattern.value),
    })
}

fn glob_match(path: &str, pattern: &str) -> bool {
    let path = path.as_bytes();
    let pattern = pattern.as_bytes();
    let mut path_index = 0usize;
    let mut pattern_index = 0usize;
    let mut star_index = None;
    let mut match_index = 0usize;

    while path_index < path.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == path[path_index])
        {
            path_index += 1;
            pattern_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            match_index = path_index;
            pattern_index += 1;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            match_index += 1;
            path_index = match_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }

    pattern_index == pattern.len()
}

fn is_adjacent_rust_integration_path(
    repo_root: &Path,
    path: &str,
    matched_package_roots: &BTreeSet<String>,
) -> bool {
    if matched_package_roots.is_empty() {
        return false;
    }
    if path == "Cargo.lock" {
        return true;
    }
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !matches!(
        file_name,
        "Cargo.toml" | "build.rs" | "lib.rs" | "main.rs" | "mod.rs"
    ) {
        return false;
    }
    cargo_package_root(repo_root, path)
        .map(|root| matched_package_roots.contains(&root))
        .unwrap_or(false)
}

fn land_parallel_lane_result(
    repo_root: &Path,
    target_branch: &str,
    assignment: &ActiveLaneAssignment,
) -> Result<()> {
    let lane_head = git_stdout(&assignment.lane_repo_root, ["rev-parse", "HEAD"])?;
    let lane_head = lane_head.trim().to_string();
    validate_parallel_lane_result(&assignment.lane_repo_root, assignment, &lane_head)?;
    fetch_lane_commit(repo_root, &assignment.lane_repo_root, &lane_head)?;
    cherry_pick_lane_range(repo_root, &assignment.base_commit, "FETCH_HEAD").with_context(
        || {
            format!(
                "failed landing lane-{} task `{}` from {}",
                assignment.lane_index,
                assignment.task.id,
                assignment.lane_repo_root.display()
            )
        },
    )?;
    if remove_task_from_plan(repo_root, &assignment.task.id)? {
        let message = format!(
            "{}: {} queue sync",
            repo_name(repo_root),
            assignment.task.id
        );
        run_git(repo_root, ["add", "IMPLEMENTATION_PLAN.md"])?;
        run_git(repo_root, ["commit", "-m", &message])?;
    }
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        println!("remote sync: rebased onto origin/{}", target_branch);
    }
    Ok(())
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

fn cherry_pick_lane_range(repo_root: &Path, base_commit: &str, head_ref: &str) -> Result<()> {
    let range = format!("{base_commit}..{head_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cherry-pick")
        .arg(&range)
        .output()
        .with_context(|| format!("failed to cherry-pick {range} in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }

    let _ = run_git(repo_root, ["cherry-pick", "--abort"]);
    bail!(
        "git cherry-pick failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn remove_task_from_plan(repo_root: &Path, task_id: &str) -> Result<bool> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(false);
    }

    let plan = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let updated = remove_task_from_plan_text(&plan, task_id);
    if updated == plan {
        return Ok(false);
    }

    atomic_write(&plan_path, updated.as_bytes())
        .with_context(|| format!("failed to write {}", plan_path.display()))?;
    Ok(true)
}

fn remove_task_from_plan_text(plan: &str, task_id: &str) -> String {
    let mut updated = String::new();
    let mut skipping = false;

    for chunk in plan.split_inclusive('\n') {
        let line = chunk.trim_end_matches('\n').trim_end_matches('\r');
        let task_header = parse_task_header(line);
        if let Some((_, current_task_id, _)) = task_header {
            if current_task_id == task_id {
                skipping = true;
                continue;
            }
            if skipping {
                skipping = false;
            }
        }

        if !skipping {
            updated.push_str(chunk);
        }
    }

    updated
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
        "{prompt}\n\nAdditional repositories you may inspect or edit when the task contract points there:\n{listing}\n\nRepository-crossing rules:\n- If the current task's owned surfaces live in one of these repos, implement the code change there instead of pretending the queue repo should own it.\n- Keep `IMPLEMENTATION_PLAN.md` truthful as the active queue for this run even when code lands in another repo.\n- Read each touched repo's `AGENTS.md`, tests, and operational docs before editing it.\n- Commit and push each touched repo separately.\n"
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
        let status = git_stdout(&path, ["status", "--short"]).unwrap_or_default();
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::ParallelArgs;

    use super::{
        build_iteration_prompt, default_cargo_build_jobs_for, discover_sibling_git_repos,
        effective_parallel_claude_max_turns, is_verification_only_task, lane_scope_budget,
        parse_loop_plan, read_lane_task_id, remove_task_from_plan_text,
        render_default_parallel_prompt, repo_forbids_legacy_review_trackers,
        resolve_loop_worker_env, resolve_reference_repos, take_matching_resume_candidate,
        task_id_from_prompt_filename, ActiveLaneAssignment, LaneResumeCandidate, LoopQueueSnapshot,
        LoopTask, LoopTaskStatus,
    };

    #[test]
    fn default_prompt_uses_resolved_branch() {
        let prompt = render_default_parallel_prompt("trunk", &[]);
        assert!(prompt.contains("branch `trunk`"));
        assert!(!prompt.contains("origin/main"));
        assert!(prompt.contains("Study `AGENTS.md` for repo-specific build"));
        assert!(prompt.contains("RED/GREEN/REFACTOR"));
        assert!(prompt.contains("failing test"));
        assert!(prompt.contains("identify the first pending task marked `- [ ]`"));
        assert!(prompt.contains("historical context only"));
        assert!(prompt.contains("next pending `- [ ]` task"));
        assert!(prompt.contains("remove its entry from `IMPLEMENTATION_PLAN.md`"));
    }

    #[test]
    fn default_prompt_lists_reference_repos_when_declared() {
        let prompt = render_default_parallel_prompt(
            "main",
            &[PathBuf::from("/home/r/coding/robopokermulti")],
        );
        assert!(prompt.contains("Additional repositories you may inspect or edit"));
        assert!(prompt.contains("/home/r/coding/robopokermulti"));
        assert!(prompt.contains("owned surfaces live in one of these repos"));
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
    fn loop_worker_env_respects_override_and_inherited_cargo_jobs() {
        let run_root = unique_temp_dir("loop-worker-env");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let shared_target = run_root
            .join("shared-cargo-target")
            .to_string_lossy()
            .into_owned();

        let inherited = resolve_loop_worker_env(None, Some("8"), None, 22, 5, &run_root).unwrap();
        assert_eq!(
            inherited.extra_env,
            vec![("CARGO_TARGET_DIR".to_string(), shared_target.clone())]
        );
        assert_eq!(inherited.cargo_jobs_summary, "inherited CARGO_BUILD_JOBS=8");
        assert_eq!(inherited.cargo_target_summary, Some(shared_target.clone()));

        let overridden =
            resolve_loop_worker_env(Some(3), Some("8"), None, 22, 5, &run_root).unwrap();
        assert_eq!(
            overridden.extra_env,
            vec![
                ("CARGO_TARGET_DIR".to_string(), shared_target.clone()),
                ("CARGO_BUILD_JOBS".to_string(), "3".to_string())
            ]
        );
        assert_eq!(overridden.cargo_jobs_summary, "override CARGO_BUILD_JOBS=3");

        let automatic = resolve_loop_worker_env(None, None, None, 22, 5, &run_root).unwrap();
        assert_eq!(
            automatic.extra_env,
            vec![
                ("CARGO_TARGET_DIR".to_string(), shared_target),
                ("CARGO_BUILD_JOBS".to_string(), "3".to_string())
            ]
        );
        assert_eq!(automatic.cargo_jobs_summary, "auto CARGO_BUILD_JOBS=3");

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn loop_worker_env_rejects_zero_cargo_jobs_override() {
        let run_root = unique_temp_dir("loop-worker-env-error");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let err = resolve_loop_worker_env(Some(0), None, None, 22, 5, &run_root).unwrap_err();
        assert!(err.to_string().contains("--cargo-build-jobs"));
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn loop_worker_env_respects_inherited_cargo_target_dir() {
        let run_root = unique_temp_dir("loop-worker-env-inherited-target");
        fs::create_dir_all(&run_root).expect("failed to create run root");

        let env = resolve_loop_worker_env(None, None, Some("/tmp/shared-target"), 22, 5, &run_root)
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

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn parallel_claude_has_no_implicit_turn_budget() {
        let args = ParallelArgs {
            max_iterations: None,
            max_concurrent_workers: 5,
            cargo_build_jobs: None,
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
    fn matching_resume_candidate_uses_ready_task_order() {
        let ready_tasks = vec![
            LoopTask {
                id: "P-019D".to_string(),
                title: "first".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                markdown: String::new(),
            },
            LoopTask {
                id: "P-021".to_string(),
                title: "second".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
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
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
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
                stderr_log_path: PathBuf::from("/tmp/lane-5/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-5/worker.pid"),
            },
        );

        let matched = take_matching_resume_candidate(
            &mut resumable,
            &ready_tasks,
            &BTreeMap::<usize, ActiveLaneAssignment>::new(),
        )
        .expect("expected a matching resumable lane");
        assert_eq!(matched.0, 5);
        assert_eq!(matched.1.task.id, "P-019D");
        assert!(resumable.contains_key(&2));
        assert!(!resumable.contains_key(&5));

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
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                clean_commit_since: None,
                terminate_requested_at: None,
            },
        );
        assert!(take_matching_resume_candidate(&mut resumable, &ready_tasks, &active).is_none());
    }

    #[test]
    fn lane_scope_budget_tracks_plan_scope() {
        let xs = LoopTask {
            id: "TASK-XS".to_string(),
            title: "tiny".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("XS".to_string()),
            markdown: String::new(),
        };
        let medium = LoopTask {
            id: "TASK-M".to_string(),
            title: "medium".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("M".to_string()),
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
            markdown: "- [ ] `WEB-CRAPS-C` Checkpoint\n  Scope boundary: verification only.\n  Acceptance criteria:\n    - pass".to_string(),
        };
        let normal = LoopTask {
            id: "WEB-CRAPS-D".to_string(),
            title: "real work".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec!["WEB-CRAPS-C".to_string()],
            estimated_scope: Some("M".to_string()),
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
    fn remove_task_from_plan_text_drops_entire_task_block() {
        let plan = r#"- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
- [ ] `TASK-002` Second task
  Dependencies:
  - `TASK-001`
  Estimated scope: medium
"#;

        let updated = remove_task_from_plan_text(plan, "TASK-001");

        assert!(!updated.contains("- [ ] `TASK-001` First task"));
        assert!(updated.contains("TASK-002"));
        assert!(updated.starts_with("- [ ] `TASK-002`"));
    }

    #[test]
    fn iteration_prompt_injects_actionable_and_blocked_tasks() {
        let queue = LoopQueueSnapshot {
            pending_ids: vec!["META-001".to_string(), "GATE-P4".to_string()],
            blocked_ids: vec!["DEC-001".to_string()],
        };
        let prompt = build_iteration_prompt("base prompt", &queue);

        assert!(prompt.contains("First actionable task marked `- [ ]`: `META-001`"));
        assert!(prompt.contains("Pending task count: 2"));
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
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }
}
