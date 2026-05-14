use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::claude_exec::{describe_claude_harness, run_claude_exec_with_env, FUTILITY_EXIT_MARKER};
use crate::codex_exec::run_codex_exec_with_env;
use crate::session_survival::{
    read_lane_checkpoint as read_session_lane_checkpoint,
    write_lane_checkpoint as write_session_lane_checkpoint, LaneCheckpoint as SessionLaneCheckpoint,
};
use crate::completion_artifacts::{
    assess_task_completion_gap, ensure_host_review_handoff, inspect_task_completion_evidence,
    legacy_verification_receipt_backfill_footer, verification_plan,
    verification_receipt_commit_footer, CompletionGapKind,
};
use crate::linear_tracker::LinearTracker;
use crate::symphony_command::run_sync;
use crate::task_parser::{
    parse_task_header as parse_shared_task_header, parse_tasks as parse_shared_tasks, LaneKind,
    PlanTask as SharedPlanTask, TaskStatus as SharedTaskStatus,
};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root,
    git_status_short_filtered, git_stdout, push_branch_with_remote_sync, repo_name, run_git,
    sync_branch_with_remote, timestamp_slug,
};
use crate::{ParallelAction, ParallelArgs, ParallelCargoTarget, SymphonySyncArgs};

#[path = "receipts.rs"]
pub(crate) mod receipts;

const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];

/// Default consecutive cherry-pick failures before we fall back to a
/// rebase + squash merge (Runner-up #90). Override via
/// `AUTODEV_CHERRY_PICK_FALLBACK_THRESHOLD`.
pub(crate) const DEFAULT_CHERRY_PICK_FALLBACK_THRESHOLD: u32 = 3;
const SHARED_QUEUE_FILES: [&str; 6] = [
    "IMPLEMENTATION_PLAN.md",
    "COMPLETED.md",
    "WORKLIST.md",
    "REVIEW.md",
    "AGENTS.md",
    "RECEIPTS-DRIFT.md",
];
const HOST_QUEUE_STATE_FILES: [&str; 6] = [
    "IMPLEMENTATION_PLAN.md",
    "COMPLETED.md",
    "WORKLIST.md",
    "REVIEW.md",
    "ARCHIVED.md",
    "RECEIPTS-DRIFT.md",
];
const LANE_POLL_INTERVAL: Duration = Duration::from_secs(5);
const CLEAN_COMMIT_GRACE: Duration = Duration::from_secs(15);
const CLEAN_COMMIT_KILL_GRACE: Duration = Duration::from_secs(5);
const STALE_GIT_INDEX_LOCK_GRACE: Duration = Duration::from_secs(30);
const MIN_AUTONOMOUS_UNBLOCK_ATTEMPTS: usize = 4;
const SALVAGE_DIR: &str = "salvage";
const DIRECT_REVIEW_QUEUE_PARALLEL_CLAUSE: &str = r#"

Repo-specific direct `REVIEW.md` handoff:
- This repo normally records completion notes in `REVIEW.md`, but `auto parallel` treats queue and review files as host-owned state.
- Do not edit `REVIEW.md`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `WORKLIST.md`, `ARCHIVED.md`, or `RECEIPTS-DRIFT.md` from a lane.
- Preserve blocker or completion evidence in your committed code/tests and command output; the host will reconcile queue and review docs after landing."#;
const LANE_TASK_ID_FILE: &str = "task-id";
const LANE_ASSIGNMENT_FILE: &str = "assignment.json";

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
        match launch_parallel_tmux_session(&session_name, &run_root, &args)? {
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

// Loop-worker environment plumbing: ParallelStartupPrep,
// LoopWorkerEnv, LoopQueueSnapshot, prepare_parallel_startup,
// parallel_run_root, cargo target layout resolution, build-jobs
// caps, claude turn budget, IMPLEMENTATION_PLAN.md reader.
include!("parallel/startup.rs");

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
    lane_kind: LaneKind,
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
        lane_kind,
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
    let inferred_lane_kind = lane_kind.unwrap_or_else(|| infer_lane_kind(&title, &markdown));
    LoopTask {
        id,
        title,
        status,
        dependencies,
        estimated_scope: task_field_line_value(&markdown, "Estimated scope:"),
        completion_path_target,
        lane_kind: inferred_lane_kind,
        markdown,
    }
}

fn infer_lane_kind(title: &str, markdown: &str) -> LaneKind {
    let text = format!("{title}\n{markdown}").to_ascii_lowercase();
    if text.contains("operator action")
        || text.contains("operator-action")
        || text.contains("operator queue")
        || text.contains("operator must")
        || text.contains("operator approval")
        || text.contains("human approval")
        || text.contains("real human")
    {
        LaneKind::Operator
    } else if text.contains("evidence only")
        || text.contains("evidence-only")
        || text.contains("verification only")
        || text.contains("receipt refresh")
        || text.contains("review handoff")
        || text.contains("proof-only")
    {
        LaneKind::Evidence
    } else {
        LaneKind::Code
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LaneAssignmentMetadata {
    task_id: String,
    target_branch: String,
    base_commit: String,
    task_hash: u64,
    dependency_hash: u64,
    verification_hash: u64,
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

// Parallel-host preflight: ParallelPreflightReport,
// ParallelPreflightCheck, PreflightStatus, run_parallel_preflight,
// classify_parallel_preflight_needs, command discovery,
// agent-browser daemon warmup.
include!("parallel/preflight.rs");
// Status reporting: run_parallel_status,
// parallel_status_safety_verdict, run_parallel_inline, host
// process discovery, lane log inspection, parallel host
// warnings, render_parallel_health_summary, receipt drift
// summary, last_parallel_stop_state, format_system_time_age.
include!("parallel/status.rs");

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
    repair_parallel_canonical_before_dispatch(repo_root, target_branch, parallel_logger)?;
    let mut join_set = JoinSet::<LaneAttemptResult>::new();
    let mut active_lanes = BTreeMap::<usize, ActiveLaneAssignment>::new();
    let mut active_tasks = BTreeSet::<String>::new();
    let mut shelved_tasks = BTreeMap::<String, String>::new();
    let mut attempted_partial_followups = BTreeSet::<String>::new();
    let mut deferred_partial_tasks = BTreeSet::<String>::new();
    let mut unblock_attempt_counts = BTreeMap::<String, usize>::new();
    let max_autonomous_unblock_attempts = autonomous_unblock_attempt_limit(args.max_retries);
    let mut linear_auto_sync_state = LinearAutoSyncState::default();
    let mut landed = 0usize;
    let mut plan = refresh_parallel_plan(
        repo_root,
        target_branch,
        linear_tracker,
        &mut linear_auto_sync_state,
        parallel_logger,
    )
    .await?;
    let preflight_report = run_parallel_preflight(repo_root, &plan, run_root, parallel_logger)?;
    let lane_config = LaneRunConfig::new(args, worker_env, preflight_report.prompt_clause());
    try_checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger);
    let mut resumable_lanes =
        discover_resume_candidates(repo_root, run_root, target_branch, &plan, parallel_logger)?;
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
        target_branch,
        linear_tracker,
        &mut linear_auto_sync_state,
        &plan,
        parallel_logger,
    )
    .await?;
    try_checkpoint_parallel_host_queue_changes(repo_root, target_branch, parallel_logger);
    let mut rediscovered_lanes =
        discover_resume_candidates(repo_root, run_root, target_branch, &plan, parallel_logger)?;
    preserve_resume_recovery_notes(&mut rediscovered_lanes, &resumable_lanes);
    resumable_lanes = rediscovered_lanes;
    let mut last_idle_summary = None::<String>;

    loop {
        nudge_lingering_committed_lanes(&mut active_lanes);
        if active_lanes.is_empty() {
            repair_parallel_canonical_before_dispatch(repo_root, target_branch, parallel_logger)?;
        }
        plan = refresh_parallel_plan_or_last_good(
            repo_root,
            target_branch,
            linear_tracker,
            &mut linear_auto_sync_state,
            &plan,
            parallel_logger,
        )
        .await?;
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
        unblock_attempt_counts.retain(|task_id, _| {
            plan.tasks
                .iter()
                .find(|task| task.id == *task_id)
                .is_some_and(|task| task.status != LoopTaskStatus::Done)
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
                    &unblock_attempt_counts,
                    max_autonomous_unblock_attempts,
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
                    let attempt_count = unblock_attempt_counts
                        .entry(candidate.task.id.clone())
                        .or_insert(0);
                    *attempt_count += 1;
                    parallel_logger.info(format!(
                        "unblock:     lane-{} -> {} [{} attempt {}/{}] because the normal ready queue is empty; downstream: {}",
                        lane_index,
                        candidate.task.id,
                        candidate.kind.label(),
                        *attempt_count,
                        max_autonomous_unblock_attempts,
                        if candidate.downstream.is_empty() {
                            "none".to_string()
                        } else {
                            candidate.downstream.join(", ")
                        }
                    ));
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
            let mut operator_ready = Vec::new();
            let mut evidence_ready = Vec::new();
            let mut executable_ready = Vec::new();
            for task in ready {
                if is_operator_task(&task) {
                    operator_ready.push(task);
                } else if is_evidence_lane_task(&task) {
                    evidence_ready.push(task);
                } else {
                    executable_ready.push(task);
                }
            }
            if !operator_ready.is_empty() {
                match write_operator_actions_for_ready_tasks(run_root, &operator_ready) {
                    Ok(path) => parallel_logger.info(format!(
                        "operator-queue: {} item(s) require operator action before code lanes can unblock; see {}",
                        operator_ready.len(),
                        path.display()
                    )),
                    Err(err) => parallel_logger.warn(format!(
                        "warning: failed writing operator action queue: {err:#}"
                    )),
                }
            } else {
                clear_stale_operator_actions(run_root, parallel_logger);
            }
            if executable_ready.is_empty() {
                let message = format!(
                    "no executable dependency-ready code tasks remain; evidence queue: {} operator queue: {}",
                    evidence_ready
                        .iter()
                        .map(|task| task.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    operator_ready
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

            let recovered = recover_shelved_tasks_from_canonical_evidence(
                repo_root,
                target_branch,
                &mut shelved_tasks,
                parallel_logger,
            )?;
            if recovered > 0 {
                plan = refresh_parallel_plan_or_last_good(
                    repo_root,
                    target_branch,
                    linear_tracker,
                    &mut linear_auto_sync_state,
                    &plan,
                    parallel_logger,
                )
                .await?;
                last_idle_summary = None;
                continue;
            }

            parallel_logger.info(no_dependency_ready_stop_message(
                &plan,
                &active_tasks,
                &queue,
                &shelved_tasks,
                &deferred_partial_tasks,
                &unblock_attempt_counts,
                max_autonomous_unblock_attempts,
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
                            if completion_status == LoopTaskStatus::Done {
                                unblock_attempt_counts.remove(&assignment.task.id);
                            }
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
                target_branch,
                linear_tracker,
                &mut linear_auto_sync_state,
                &plan,
                parallel_logger,
            )
            .await?;
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
                        unblock_attempt_counts.remove(&assignment.task.id);
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
                        if completion_status == LoopTaskStatus::Done {
                            unblock_attempt_counts.remove(&assignment.task.id);
                        }
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

// Lane recovery + repair: try_spawn_lane_recovery_attempt,
// recovery-note builders (landing/prepared/resumed/dirty-worktree/
// lane-repo/stale-rebase), SupersededLaneRecovery + helpers,
// git_commit_exists/git_path,
// repair_parallel_canonical_before_dispatch,
// repair_stale_git_index_lock, checkpoint_parallel_dispatch_paths,
// environment_blocker_recovery_note.
include!("parallel/recovery.rs");
// Frontier stop messages, autonomous unblock attempt limits,
// operator-actions JSON, parallel salvage records + recovery
// notes, lane environment-blocker detection + reasons,
// read_recent_log_text.
include!("parallel/operator_salvage.rs");
// Host queue mutation + receipts drift triage + plan refresh.
// checkpoint_parallel_host_queue_changes + try variant,
// host_queue_state_files_for_repo, lane-progress shelving,
// inspect_loop_plan, ReceiptDriftTriageEntry + audit drift +
// backfill receipt footer + drift writers, refresh_parallel_plan,
// linear usage-limit + auto-sync disabler.
include!("parallel/queue.rs");
// tmux session and window lifecycle: setup_parallel_tmux_windows,
// TmuxLaunchStatus, launch_parallel_tmux_session, session helpers,
// command builder, log paths, tmux IPC wrappers, shell_quote +
// live-log normalize/redact.
include!("parallel/tmux.rs");
// Lane dispatch + scheduling: classify_task_execution_kind,
// is_operator_task / is_evidence_lane_task,
// describe_parallel_idle_state, ready-task selection +
// dirty-path partitioning, frontier helpers, partial follow-up
// tracking, ParallelUnblockCandidate.
include!("parallel/dispatch.rs");
// Lane slot assignment + resume: next_free_lane_index,
// prepare_parallel_lane_assignment + fallback, reset/reserve lane
// root, discover_resume_candidates, harvest_resumable_lane_results,
// take_resume_candidate_for_task, preserve_resume_recovery_notes,
// clone_loop_lane_repo.
include!("parallel/lane_assignment.rs");

// Lane worker spawn: spawn_parallel_lane_attempt,
// refresh_assignment_task_from_plan, nudge_lingering_committed_lanes.
include!("parallel/lane_worker.rs");
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

fn write_lane_assignment_metadata(
    lane_root: &Path,
    target_branch: &str,
    base_commit: &str,
    task: &LoopTask,
) -> Result<()> {
    let metadata = LaneAssignmentMetadata {
        task_id: task.id.clone(),
        target_branch: target_branch.to_string(),
        base_commit: base_commit.to_string(),
        task_hash: hash_stable(&task.markdown),
        dependency_hash: hash_stable(&task.dependencies),
        verification_hash: hash_stable(&task_field_body(
            &task.markdown,
            "Verification:",
            "Required tests:",
        )),
    };
    let json = serde_json::to_vec_pretty(&metadata)?;
    atomic_write(&lane_root.join(LANE_ASSIGNMENT_FILE), &json).with_context(|| {
        format!(
            "failed to write {}",
            lane_root.join(LANE_ASSIGNMENT_FILE).display()
        )
    })
}

fn validate_lane_assignment_metadata(
    lane_root: &Path,
    target_branch: &str,
    task: &LoopTask,
) -> Result<LaneAssignmentMetadata> {
    let metadata_path = lane_root.join(LANE_ASSIGNMENT_FILE);
    let text = fs::read_to_string(&metadata_path)
        .with_context(|| format!("failed to read {}", metadata_path.display()))?;
    let metadata: LaneAssignmentMetadata = serde_json::from_str(&text)
        .with_context(|| format!("invalid {}", metadata_path.display()))?;
    if metadata.task_id != task.id {
        bail!(
            "task id changed from `{}` to `{}`",
            metadata.task_id,
            task.id
        );
    }
    if metadata.target_branch != target_branch {
        bail!(
            "target branch changed from `{}` to `{target_branch}`",
            metadata.target_branch
        );
    }
    if metadata.verification_hash
        != hash_stable(&task_field_body(
            &task.markdown,
            "Verification:",
            "Required tests:",
        ))
    {
        bail!("verification text hash changed");
    }
    if metadata.task_hash != hash_stable(&task.markdown) {
        bail!("task body hash changed");
    }
    if metadata.dependency_hash != hash_stable(&task.dependencies) {
        bail!("dependency hash changed");
    }
    Ok(metadata)
}

fn hash_stable<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
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

fn lane_worker_status(lane_root: &Path, lane_repo_root: &Path) -> Result<(bool, String)> {
    let pid_path = lane_root.join("worker.pid");
    let pid_state = match read_worker_pid(&pid_path) {
        Ok(Some(pid)) => match worker_pid_is_alive(pid) {
            Ok(true) => return Ok((true, format!("running pid {pid}"))),
            Ok(false) => Some(format!("stale pid {pid}")),
            Err(err) => Some(format!("pid liveness unknown: {err:#}")),
        },
        Ok(None) => None,
        Err(err) => Some(format!("worker pid unreadable: {err:#}")),
    };

    let descendant_pids = lane_repo_process_pids(lane_repo_root)?;
    if !descendant_pids.is_empty() {
        return Ok((
            true,
            format!(
                "running descendant pid(s) {}{}",
                descendant_pids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                pid_state
                    .map(|state| format!(" ({state})"))
                    .unwrap_or_default()
            ),
        ));
    }

    Ok((
        false,
        pid_state.unwrap_or_else(|| "no worker pid".to_string()),
    ))
}

fn lane_repo_process_pids(lane_repo_root: &Path) -> Result<Vec<u32>> {
    if !lane_repo_root.exists() {
        return Ok(Vec::new());
    }
    let output = Command::new("ps")
        .args(["-eo", "pid=,command="])
        .output()
        .context("failed to inspect process table")?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(parse_lane_repo_process_pids(
        lane_repo_root,
        &String::from_utf8_lossy(&output.stdout),
    ))
}

fn parse_lane_repo_process_pids(lane_repo_root: &Path, ps_output: &str) -> Vec<u32> {
    let needle = lane_repo_root.display().to_string();
    ps_output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let (pid, command) = trimmed.split_once(char::is_whitespace)?;
            if !command.contains(&needle) {
                return None;
            }
            let command = command.trim_start();
            let executable = command
                .split_whitespace()
                .next()
                .and_then(|word| Path::new(word).file_name())
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if matches!(executable, "rg" | "grep")
                || command.starts_with("auto parallel status")
                || command.contains(" auto parallel status")
                || command.contains("/auto parallel status")
            {
                return None;
            }
            pid.parse::<u32>().ok()
        })
        .collect()
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
        "{prompt_template}\n\nParallel assignment for this worker:\n- Assigned task for this lane: `{task_id}` {title}\n- This task is already dependency-ready for this run: {dependency_clause}\n- The host owns queue reconciliation and branch landing in parallel mode.\n- Do not push to `origin/{branch}` or any other remote. Create local commit(s) only; the host will land them onto `{branch}`.\n- Before finishing, run `git status --short`. Finish only with at least one local commit for this task and a clean worktree. If files are still dirty, either commit task-owned leftovers or revert unrelated/formatter spillover before exiting.\n- {protected_clause}\n- {cargo_target_clause}\n- If the repo contains `scripts/run-task-verification.sh`, run the host-parsed executable verification commands through that wrapper instead of invoking them bare. Do not treat narrative `Verification:` prose as literal shell input.\n- Host-parsed executable verification commands: {verification_commands_clause}\n- Narrative verification guidance preserved from the task: {verification_guidance_clause}\n- Source-of-truth discipline: runtime/engine/API owners define facts; UI/presentation code renders those facts. Do not duplicate runtime-owned catalogs, constants, settlement math, risk classifications, eligibility rules, balances, or status derivations in UI code.\n- Runtime-first order: when the task touches both runtime and UI, implement or confirm the runtime/API contract first, regenerate/check generated bindings or schemas second, then update UI consumers.\n- Fixture boundary: production code must not import fixture/demo/sample data as fallback truth. Fixture data belongs in tests, stories, demos, or explicit dev-only harnesses.\n- Contract generation: if the task names generated artifacts or changes runtime/API shapes, run the named generator/check or record `AUTO_ENV_BLOCKER`/`AUTO_VERIFICATION_BLOCKER` with the exact reason it could not run.\n- Cross-surface proof: if UI consumers are named, include at least one runtime-output-to-UI/readback proof or a clear blocker. Component-only tests are insufficient when the original risk is runtime/UI drift.\n- Retire-first cleanup: if the task names retired or superseded surfaces, delete/archive/tombstone them and clean callers/indexes in the same lane when in scope. Do not leave stale active doctrine as a TODO unless the task explicitly gates it.\n- Independent closeout: before your final answer, re-check the original task fields (`Source of truth`, `Runtime owner`, `UI consumers`, `Generated artifacts`, `Fixture boundary`, `Retired surfaces`, and `Review/closeout`) and state how each was satisfied or blocked.\n- If no executable verification commands were parsed, derive the narrowest truthful proof yourself and record blockers honestly instead of patching the wrapper to accept prose.\n- If a proof command exits successfully but reports `0 tests`, treat that proof as not run. Find the exact test/package target or report the verification blocker; do not count zero-test output as passing evidence.\n- Do not use direct target-dir test binaries as final proof unless you built that exact artifact from this lane's current source tree in the immediately preceding command. Prefer `cargo test` or the repo's verification wrapper.\n- If missing external infrastructure blocks verification or runtime smoke tests, print `AUTO_ENV_BLOCKER: <short reason>` before exiting non-zero. Do not present an environment blocker as a code proof failure.\n- Never hand-edit or commit `.auto/symphony/verification-receipts/*.json`. Receipt JSON is staging evidence; the host embeds durable proof in closeout commit footers.\n- The host marks this task `- [x]` only when local review handoff, verification evidence, and declared completion artifacts are present. Otherwise it leaves the task `- [~]` for follow-up instead of bluffing completion.\n{preflight_clause}{recovery_clause}\nCanonical queue snapshot when this lane started:\n- Unfinished task count: {pending_count}\n- Currently blocked tasks: {blocked_clause}\n\nAssigned task markdown:\n{markdown}\n",
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
            if let Err(err) = cherry_pick_lane_range_with_fallback(
                repo_root,
                target_branch,
                &range_base,
                "FETCH_HEAD",
                cherry_pick_fallback_threshold(),
            )
            .map(|_| ())
            {
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
        commit_task_closeout(repo_root, &assignment.task.id, &message, false)?;
    }
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        println!("remote sync: rebased onto origin/{}", target_branch);
    }
    let landed_head = git_stdout(repo_root, ["rev-parse", "HEAD"])
        .map(|sha| sha.trim().to_string())
        .unwrap_or_default();
    record_lane_checkpoint(
        &assignment.lane_root,
        "commit",
        serde_json::json!({
            "task_id": assignment.task.id,
            "target_branch": target_branch,
            "lane_head": final_lane_head,
            "range_base": final_range_base,
            "landed_head": landed_head,
            "completion_status": format!("{completion_status:?}"),
            "auto_repaired": auto_repaired,
            "changed_files": changed_files,
        }),
    );
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
    write_clean_no_commit_verdict(
        assignment,
        "needs-human-triage",
        "lane exited cleanly without a local commit; canonical evidence will be inspected before shelving",
    )?;
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
            commit_task_closeout(repo_root, &assignment.task.id, &message, false)?;
            if push_branch_with_remote_sync(repo_root, target_branch)? {
                parallel_logger.info(format!(
                    "remote sync: rebased onto origin/{} after evidence self-heal",
                    target_branch
                ));
            }
        }
    } else {
        let message = format!(
            "{}: {} evidence closeout",
            repo_name(repo_root),
            assignment.task.id
        );
        commit_task_closeout(repo_root, &assignment.task.id, &message, true)?;
        if push_branch_with_remote_sync(repo_root, target_branch)? {
            parallel_logger.info(format!(
                "remote sync: rebased onto origin/{} after empty evidence closeout",
                target_branch
            ));
        }
    }
    write_clean_no_commit_verdict(
        assignment,
        "task-already-done",
        "canonical review, receipt, and declared artifact evidence are complete; host created an evidence closeout",
    )?;

    Ok(true)
}

fn recover_shelved_tasks_from_canonical_evidence(
    repo_root: &Path,
    target_branch: &str,
    shelved_tasks: &mut BTreeMap<String, String>,
    parallel_logger: &ParallelEventLogger,
) -> Result<usize> {
    let mut recovered = Vec::new();
    for (task_id, markdown) in shelved_tasks.clone() {
        let evidence = inspect_task_completion_evidence(repo_root, &task_id, &markdown);
        if !evidence.is_fully_evidenced() {
            continue;
        }
        let review_added = ensure_host_review_handoff(repo_root, &task_id, &[], &evidence)?;
        let plan_updated =
            update_task_completion_in_plan(repo_root, &task_id, LoopTaskStatus::Done)?;
        if review_added {
            run_git(repo_root, ["add", "REVIEW.md"])?;
        }
        if plan_updated {
            run_git(repo_root, ["add", "IMPLEMENTATION_PLAN.md"])?;
        }
        let message = format!("{}: {} evidence recovery", repo_name(repo_root), task_id);
        if repo_has_staged_queue_updates(repo_root)? {
            commit_task_closeout(repo_root, &task_id, &message, false)?;
        } else {
            commit_task_closeout(repo_root, &task_id, &message, true)?;
        }
        recovered.push(task_id);
    }

    if recovered.is_empty() {
        return Ok(0);
    }
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        parallel_logger.info(format!(
            "remote sync: rebased onto origin/{} after shelved evidence recovery",
            target_branch
        ));
    }
    for task_id in &recovered {
        shelved_tasks.remove(task_id);
    }
    parallel_logger.info(format!(
        "self-heal: recovered {} shelved task(s) from canonical evidence before NO-GO ({})",
        recovered.len(),
        recovered.join(", ")
    ));
    Ok(recovered.len())
}

fn write_clean_no_commit_verdict(
    assignment: &ActiveLaneAssignment,
    verdict: &str,
    reason: &str,
) -> Result<()> {
    let path = assignment.lane_root.join("clean-no-commit-verdict.json");
    let payload = serde_json::json!({
        "task_id": assignment.task.id,
        "lane_index": assignment.lane_index,
        "verdict": verdict,
        "reason": reason,
    });
    let text = serde_json::to_vec_pretty(&payload)?;
    atomic_write(&path, &text).with_context(|| format!("failed to write {}", path.display()))
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

fn commit_task_closeout(
    repo_root: &Path,
    task_id: &str,
    message: &str,
    allow_empty: bool,
) -> Result<()> {
    let footer = verification_receipt_commit_footer(repo_root, task_id)?;
    let mut command = Command::new("git");
    command.arg("-C").arg(repo_root).arg("commit");
    if allow_empty {
        command.arg("--allow-empty");
    }
    command.arg("-m").arg(message);
    if let Some(footer) = footer {
        command.arg("-m").arg(footer);
    }
    let output = command
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git command failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
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
    assignment.base_commit = recovery_base.clone();
    match cherry_pick_lane_range_with_fallback(
        &assignment.lane_repo_root,
        &recovery_base,
        range_base,
        &original_lane_head,
        cherry_pick_fallback_threshold(),
    ) {
        Ok(_) => Ok(LaneLandingRecoveryPrep::RebasedCleanly),
        Err(err) => Ok(LaneLandingRecoveryPrep::NeedsWorkerResolution(
            prepared_landing_recovery_note(target_branch, landing_error, &format!("{err:#}")),
        )),
    }
}

// Cherry-pick fallback, lane checkpoints, structural patch fallback,
// plan-integrity demotion guard, receipts rehash, plan-text
// completion mutators.
include!("parallel/landing_primitives.rs");
// Default-prompt rendering, reference-repo discovery, branch
// resolution, and tracked sibling-repo helpers.
include!("parallel/reference_repos.rs");

#[cfg(test)]
#[path = "parallel/tests.rs"]
mod tests;
