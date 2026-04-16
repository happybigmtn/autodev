use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use regex::Regex;
use tokio::task::JoinSet;

use crate::claude_exec::{describe_claude_harness, run_claude_exec_with_env, FUTILITY_EXIT_MARKER};
use crate::codex_exec::run_codex_exec_with_env;
use crate::linear_tracker::LinearTracker;
use crate::symphony_command::run_sync;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, clear_and_recreate_dir, ensure_repo_layout,
    git_repo_root, git_stdout, push_branch_with_remote_sync, repo_name, run_git,
    sync_branch_with_remote, timestamp_slug,
};
use crate::{ParallelArgs, SymphonySyncArgs};

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
    let parallel_logger = ParallelEventLogger::new(&run_root)?;
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
    let (mut status, id, title) = parse_task_header(&header)?;
    let markdown = lines.join("\n");
    if matches!(status, LoopTaskStatus::Pending | LoopTaskStatus::Blocked)
        && task_is_non_actionable_placeholder(&title, &markdown)
    {
        status = LoopTaskStatus::Done;
    }
    Some(LoopTask {
        id,
        title,
        status,
        dependencies: parse_task_dependencies(&markdown),
        estimated_scope: task_field_line_value(&markdown, "Estimated scope:"),
        markdown,
    })
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
    let Some(body) = task_field_body(markdown, "Dependencies:", "Estimated scope:") else {
        return Vec::new();
    };

    let first_meaningful = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.trim_start_matches('-').trim().to_ascii_lowercase());
    if first_meaningful
        .as_deref()
        .is_some_and(|line| line.starts_with("none"))
    {
        return Vec::new();
    }

    dedup_task_refs(
        body.lines()
            .flat_map(task_dependency_refs_from_line)
            .collect::<Vec<_>>(),
    )
}

fn task_dependency_refs_from_line(line: &str) -> Vec<String> {
    let without_parens = strip_parenthetical_groups(line);
    let narrative_cut = without_parens.split(['.', ';']).next().unwrap_or("").trim();
    collect_task_refs(narrative_cut)
}

fn strip_parenthetical_groups(text: &str) -> String {
    let mut depth = 0usize;
    let mut rendered = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => rendered.push(ch),
            _ => {}
        }
    }
    rendered
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
    stdout_log_path: PathBuf,
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
    stdout_log_path: PathBuf,
    stderr_log_path: PathBuf,
    worker_pid_path: PathBuf,
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
        let task_id = read_lane_task_id(&lane_root)
            .ok()
            .flatten()
            .unwrap_or_else(|| "[idle]".to_string());
        append_lane_host_event(
            &lane_root.join("stdout.log"),
            lane_index,
            &task_id,
            &format!("idle: {summary}"),
        );
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
                None,
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
                None,
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
    linear_tracker: &mut Option<LinearTracker>,
    parallel_logger: &ParallelEventLogger,
) -> Result<()> {
    let harness = if args.claude { "Claude" } else { "Codex" };
    let lane_config = LaneRunConfig::new(args, worker_env);
    let mut join_set = JoinSet::<LaneAttemptResult>::new();
    let mut active_lanes = BTreeMap::<usize, ActiveLaneAssignment>::new();
    let mut active_tasks = BTreeSet::<String>::new();
    let mut shelved_tasks = BTreeMap::<String, String>::new();
    let mut landed = 0usize;
    let mut plan = refresh_parallel_plan(repo_root, linear_tracker, parallel_logger).await?;
    let mut resumable_lanes = discover_resume_candidates(run_root, target_branch, &plan)?;
    landed += harvest_resumable_lane_results(
        repo_root,
        target_branch,
        &mut resumable_lanes,
        linear_tracker,
        parallel_logger,
    )
    .await?;
    plan =
        refresh_parallel_plan_or_last_good(repo_root, linear_tracker, &plan, parallel_logger).await;
    resumable_lanes = discover_resume_candidates(run_root, target_branch, &plan)?;
    let mut last_idle_summary = None::<String>;

    loop {
        nudge_lingering_committed_lanes(&mut active_lanes);
        plan =
            refresh_parallel_plan_or_last_good(repo_root, linear_tracker, &plan, parallel_logger)
                .await;
        shelved_tasks.retain(|task_id, markdown| {
            plan.tasks
                .iter()
                .find(|task| task.id == *task_id)
                .is_some_and(|task| task.markdown == *markdown)
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

            let ready = plan
                .ready_tasks(&active_tasks)
                .into_iter()
                .filter(|task| !shelved_tasks.contains_key(&task.id))
                .collect::<Vec<_>>();
            if ready.is_empty() {
                if active_lanes.len() < args.max_concurrent_workers {
                    let idle_summary =
                        describe_parallel_idle_state(&plan, &active_tasks, &shelved_tasks);
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
                    parallel_logger.info("no pending `- [ ]` tasks remain; stopping.");
                } else {
                    parallel_logger.info(format!(
                        "all remaining tasks are blocked `[!]`; stopping. blocked: {}",
                        queue.blocked_ids.join(", ")
                    ));
                }
                break;
            }

            parallel_logger.info(format!(
                "no dependency-ready tasks remain to dispatch; stopping. pending: {} blocked: {}",
                queue.pending_ids.join(", "),
                if queue.blocked_ids.is_empty() {
                    "none".to_string()
                } else {
                    queue.blocked_ids.join(", ")
                }
            ));
            break;
        }

        let joined = match tokio::time::timeout(LANE_POLL_INTERVAL, join_set.join_next()).await {
            Ok(result) => result.context("parallel lane join set unexpectedly empty")?,
            Err(_) => continue,
        };
        let lane_result = joined.context("parallel lane task panicked")?;
        let mut assignment = active_lanes
            .remove(&lane_result.lane_index)
            .with_context(|| format!("missing active state for lane-{}", lane_result.lane_index))?;
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

        let exit_status = lane_result
            .exit_status
            .context("lane attempt completed without an exit status or error")?;

        if !exit_status.success() {
            match inspect_lane_repo_progress(&assignment.lane_repo_root, &assignment.base_commit)? {
                LaneRepoProgress::NewCommits => {
                    if let Err(err) =
                        land_parallel_lane_result(repo_root, target_branch, &assignment)
                    {
                        parallel_logger.warn(format!(
                            "warning: failed landing lane-{} `{}` after non-zero worker exit: {err:#}",
                            assignment.lane_index, assignment.task.id
                        ));
                        shelved_tasks
                            .insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                        continue;
                    }
                    if let Some(tracker) = linear_tracker.as_mut() {
                        if let Err(err) = tracker.note_done(&assignment.task.id).await {
                            eprintln!(
                                "warning: failed to move `{}` to done in Linear: {err:#}",
                                assignment.task.id
                            );
                        }
                    }
                    landed += 1;
                    parallel_logger.info(format!(
                        "landed:      [{}] {} via lane-{} after non-zero worker exit (total landed: {})",
                        classify_task_execution_kind(&assignment.task),
                        assignment.task.id, assignment.lane_index, landed
                    ));
                    append_lane_host_event(
                        &assignment.stdout_log_path,
                        assignment.lane_index,
                        &assignment.task.id,
                        "landed: host harvested committed work after non-zero worker exit",
                    );
                    last_idle_summary = None;
                    continue;
                }
                LaneRepoProgress::Dirty(_) | LaneRepoProgress::None => {}
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

        match inspect_lane_repo_progress(&assignment.lane_repo_root, &assignment.base_commit)? {
            LaneRepoProgress::Dirty(status) => {
                parallel_logger.warn(format!(
                    "warning: parallel lane-{} (`{}`) exited cleanly but left uncommitted changes; shelving for the rest of this run:\n{}",
                    assignment.lane_index,
                    assignment.task.id,
                    status
                ));
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
                if let Err(err) = land_parallel_lane_result(repo_root, target_branch, &assignment) {
                    parallel_logger.warn(format!(
                        "warning: failed landing lane-{} `{}`; shelving for the rest of this run: {err:#}",
                        assignment.lane_index, assignment.task.id
                    ));
                    shelved_tasks
                        .insert(assignment.task.id.clone(), assignment.task.markdown.clone());
                    continue;
                }
                if let Some(tracker) = linear_tracker.as_mut() {
                    if let Err(err) = tracker.note_done(&assignment.task.id).await {
                        eprintln!(
                            "warning: failed to move `{}` to done in Linear: {err:#}",
                            assignment.task.id
                        );
                    }
                }
                landed += 1;
                parallel_logger.info(format!(
                    "landed:      [{}] {} via lane-{} (total landed: {})",
                    classify_task_execution_kind(&assignment.task),
                    assignment.task.id,
                    assignment.lane_index,
                    landed
                ));
                append_lane_host_event(
                    &assignment.stdout_log_path,
                    assignment.lane_index,
                    &assignment.task.id,
                    "landed: host harvested committed work",
                );
                last_idle_summary = None;
            }
        }
    }

    Ok(())
}

fn inspect_loop_plan(repo_root: &Path) -> Result<LoopPlanSnapshot> {
    let plan = read_loop_plan(repo_root)?;
    Ok(parse_loop_plan(&plan))
}

async fn refresh_parallel_plan(
    repo_root: &Path,
    linear_tracker: &mut Option<LinearTracker>,
    parallel_logger: &ParallelEventLogger,
) -> Result<LoopPlanSnapshot> {
    let mut plan_text = read_loop_plan(repo_root)?;
    if let Some(tracker) = linear_tracker.as_mut() {
        if let Err(err) = tracker.refresh_if_plan_changed(&plan_text).await {
            eprintln!("warning: failed to refresh Linear task cache from updated plan: {err:#}");
        } else if tracker.should_attempt_auto_sync(&plan_text) {
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
                parallel_logger.info(format!(
                    "linear drift: {}. running `auto symphony sync --no-ai-planner` before dispatch",
                    reasons.join(" | ")
                ));
                tracker.mark_auto_sync_attempt(&plan_text);
                if let Err(err) = run_sync(SymphonySyncArgs {
                    repo_root: Some(repo_root.to_path_buf()),
                    project_slug: None,
                    todo_state: "Todo".to_string(),
                    planner_model: "gpt-5.4".to_string(),
                    planner_reasoning_effort: "high".to_string(),
                    codex_bin: PathBuf::from("codex"),
                    no_ai_planner: true,
                })
                .await
                {
                    parallel_logger.warn(format!(
                        "warning: automatic `auto symphony sync --no-ai-planner` failed; continuing without refreshed Linear coverage: {err:#}"
                    ));
                } else {
                    plan_text = read_loop_plan(repo_root)?;
                    if let Err(err) = tracker.refresh_after_sync(&plan_text).await {
                        parallel_logger.warn(format!(
                            "warning: failed refreshing Linear cache after automatic sync: {err:#}"
                        ));
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
    last_good_plan: &LoopPlanSnapshot,
    parallel_logger: &ParallelEventLogger,
) -> LoopPlanSnapshot {
    match refresh_parallel_plan(repo_root, linear_tracker, parallel_logger).await {
        Ok(plan) => plan,
        Err(err) => {
            parallel_logger.warn(format!(
                "warning: failed to refresh IMPLEMENTATION_PLAN.md; continuing with the last good queue snapshot: {err:#}"
            ));
            last_good_plan.clone()
        }
    }
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
) -> String {
    let ready = plan
        .ready_tasks(active_tasks)
        .into_iter()
        .filter(|task| !shelved_tasks.contains_key(&task.id))
        .collect::<Vec<_>>();
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

    let unresolved = plan
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                LoopTaskStatus::Pending | LoopTaskStatus::Blocked
            )
        })
        .map(|task| task.id.as_str())
        .chain(active_tasks.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let waiting_on = plan
        .tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Pending)
        .filter(|task| !active_tasks.contains(&task.id))
        .filter(|task| !shelved_tasks.contains_key(&task.id))
        .flat_map(|task| {
            task.dependencies
                .iter()
                .filter(|dep| unresolved.contains(dep.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    if waiting_on.is_empty() {
        "no dependency-ready task is currently available".to_string()
    } else {
        format!(
            "waiting on dependencies: {}",
            waiting_on.into_iter().collect::<Vec<_>>().join(", ")
        )
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
        stdout_log_path: lane_root.join("stdout.log"),
        stderr_log_path: lane_root.join("stderr.log"),
        worker_pid_path: lane_root.join("worker.pid"),
        clean_commit_since: None,
        terminate_requested_at: None,
    })
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
        match inspect_lane_repo_progress(&lane_repo_root, &base_commit) {
            Ok(LaneRepoProgress::None) => continue,
            Ok(LaneRepoProgress::Dirty(_) | LaneRepoProgress::NewCommits) => {}
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because repo progress inspection failed: {err:#}",
                    lane_index
                );
                continue;
            }
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
            },
        );
    }

    Ok(candidates)
}

async fn harvest_resumable_lane_results(
    repo_root: &Path,
    target_branch: &str,
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
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
                    Ok(LaneRepoProgress::Dirty(_) | LaneRepoProgress::None) => false,
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
        let assignment = ActiveLaneAssignment {
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
        };
        match land_parallel_lane_result(repo_root, target_branch, &assignment) {
            Ok(()) => {
                if let Some(tracker) = linear_tracker.as_mut() {
                    if let Err(err) = tracker.note_done(&assignment.task.id).await {
                        eprintln!(
                            "warning: failed to move `{}` to done in Linear: {err:#}",
                            assignment.task.id
                        );
                    }
                }
                landed += 1;
                parallel_logger.info(format!(
                    "resumed:     landed {} from lane-{} before dispatch (total landed: {})",
                    assignment.task.id, assignment.lane_index, landed
                ));
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
    let full_prompt =
        build_parallel_lane_prompt(prompt_template, plan, &assignment.task, target_branch);
    let prompt_path = assignment.lane_root.join(format!(
        "{}-attempt-{:02}-prompt.md",
        assignment.task.id, assignment.attempts
    ));
    let repo_root = assignment.lane_repo_root.clone();
    let stderr_log_path = assignment.stderr_log_path.clone();
    let stdout_log_path = assignment.stdout_log_path.clone();
    let worker_pid_path = assignment.worker_pid_path.clone();
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
                &lane_config.extra_env,
                Some(&worker_pid_path),
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
                &lane_config.extra_env,
                Some(&worker_pid_path),
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
            LaneRepoProgress::Dirty(_) | LaneRepoProgress::None => {
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
    format!(
        "{prompt_template}\n\nParallel assignment for this worker:\n- Assigned task for this lane: `{task_id}` {title}\n- This task is already dependency-ready for this run: {dependency_clause}\n- The host owns queue reconciliation and branch landing in parallel mode.\n- Do not push to `origin/{branch}` or any other remote. Create local commit(s) only; the host will land them onto `{branch}`.\n- {protected_clause}\n- Do not override the host-provided `CARGO_TARGET_DIR`. Shared build cache is part of the execution contract for this run; if Cargo is busy, wait or narrow the proof instead of switching to a lane-local target dir.\n- If the repo contains `scripts/run-task-verification.sh`, run every command from the task's `Verification:` block through that wrapper instead of invoking the command bare. Use the exact command text from the `Verification:` block so the verification receipt matches the task contract.\n- Never hand-edit verification receipt files. They are execution evidence, not notes.\n- If the lane repo contains `.githooks/`, pre-commit enforcement is active in this clone via `core.hooksPath=.githooks`; do not bypass it.\n\nCanonical queue snapshot when this lane started:\n- Pending task count: {pending_count}\n- Currently blocked tasks: {blocked_clause}\n\nAssigned task markdown:\n{markdown}\n",
        task_id = task.id,
        title = task.title,
        dependency_clause = dependency_clause,
        branch = branch,
        protected_clause = protected_clause,
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
    assignment: &ActiveLaneAssignment,
) -> Result<()> {
    let lane_head = git_stdout(&assignment.lane_repo_root, ["rev-parse", "HEAD"])?;
    let lane_head = lane_head.trim().to_string();
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
        resolve_loop_worker_env, resolve_reference_repos, take_resume_candidate_for_task,
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
    fn resume_candidate_matches_requested_task() {
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
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
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
                stdout_log_path: PathBuf::from("/tmp/lane-5/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-5/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-5/worker.pid"),
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
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                clean_commit_since: None,
                terminate_requested_at: None,
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
