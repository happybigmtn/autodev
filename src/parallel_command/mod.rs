pub(crate) use std::collections::{BTreeMap, BTreeSet};
pub(crate) use std::env;
pub(crate) use std::fs;
pub(crate) use std::fs::OpenOptions;
pub(crate) use std::hash::{Hash, Hasher};
pub(crate) use std::io::Write;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::{Command, ExitStatus};
pub(crate) use std::sync::OnceLock;
pub(crate) use std::time::{Duration, Instant, SystemTime};

pub(crate) use anyhow::{bail, Context, Result};
pub(crate) use regex::Regex;
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use tokio::task::JoinSet;

pub(crate) use crate::claude_exec::{
    describe_claude_harness, run_claude_exec_with_env, FUTILITY_EXIT_MARKER,
};
pub(crate) use crate::codex_exec::run_codex_exec_with_env;
pub(crate) use crate::completion_artifacts::{
    assess_task_completion_gap, clear_verified_source_attestation,
    commit_message_has_reserved_verification_receipt_footer, compute_task_owned_inputs_fingerprint,
    current_dirty_state_fingerprint, direct_verification_receipt_problem,
    ensure_host_review_handoff, footer_task_owned_inputs, git_verification_receipt_footers,
    inspect_task_completion_evidence, inspect_task_completion_evidence_with_owned_inputs,
    record_verified_source_attestation, unresolved_review_findings_for_task,
    unresolved_workspace_review_findings_for_task, verification_plan,
    verification_receipt_commit_footer, CompletionGapKind, VerificationReceiptFooter,
};
pub(crate) use crate::linear_tracker::LinearTracker;
pub(crate) use crate::symphony_command::run_sync;
pub(crate) use crate::task_parser::{
    parse_tasks as parse_shared_tasks,
    parse_top_level_task_header as parse_shared_top_level_task_header, validate_execution_rows,
    LaneKind, PlanTask as SharedPlanTask, TaskStatus as SharedTaskStatus,
};
pub(crate) use crate::util::{
    active_plan_path, active_plan_relative, atomic_write, auto_checkpoint_if_needed,
    capture_validated_task_closeout_tree, commit_staged_checkpoint_cas,
    commit_staged_queue_checkpoint_cas, commit_validated_task_closeout_tree_cas,
    ensure_repo_layout, ensure_writable_run_root, git_cherry_pick_empty_arg, git_repo_root,
    git_stdout, push_branch_with_remote_sync, refuse_unsealed_task_completion_checkpoint,
    refuse_unsealed_task_completion_transitions_except, refuse_worktree_paths_outside, repo_name,
    run_git, sync_branch_with_remote, timestamp_slug, unsealed_task_completion_ids,
};
pub(crate) use crate::{ParallelAction, ParallelArgs, ParallelCargoTarget, SymphonySyncArgs};

mod assignment;
mod landing;
mod lane_repo;
mod orchestrator;
mod plan;
mod preflight;
mod prompt;
mod purge;
mod receipt_backfill;
mod recovery_notes;
mod review_gate;
mod run_state;
mod scheduling;
mod status;
mod tmux;
mod validation_lease;
mod verify_gate;
mod worker_env;

pub(crate) use assignment::*;
pub(crate) use landing::*;
pub(crate) use lane_repo::*;
pub(crate) use orchestrator::*;
pub(crate) use plan::*;
pub(crate) use preflight::*;
pub(crate) use prompt::*;
pub(crate) use purge::*;
pub(crate) use receipt_backfill::*;
pub(crate) use recovery_notes::*;
pub(crate) use review_gate::*;
pub(crate) use run_state::*;
pub(crate) use scheduling::*;
pub(crate) use status::*;
pub(crate) use tmux::*;
pub(crate) use validation_lease::*;
pub(crate) use verify_gate::*;
pub(crate) use worker_env::*;

pub(crate) const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];

pub(crate) const HOST_QUEUE_STATE_FILES: [&str; 8] = [
    "PLAN.md",
    "IMPLEMENTATION_PLAN.md",
    "COMPLETED.md",
    "WORKLIST.md",
    "REVIEW.md",
    "AGENTS.md",
    "ARCHIVED.md",
    "RECEIPTS-DRIFT.md",
];

pub(crate) const LANE_POLL_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) const CLEAN_COMMIT_GRACE: Duration = Duration::from_secs(30);

pub(crate) const CLEAN_COMMIT_QUIET_GRACE: Duration = Duration::from_secs(120);

pub(crate) const CLEAN_COMMIT_KILL_GRACE: Duration = Duration::from_secs(5);

pub(crate) const STALE_GIT_INDEX_LOCK_GRACE: Duration = Duration::from_secs(30);

pub(crate) const MIN_AUTONOMOUS_UNBLOCK_ATTEMPTS: usize = 4;

pub(crate) const SALVAGE_DIR: &str = "salvage";

pub(crate) const DIRECT_REVIEW_QUEUE_PARALLEL_CLAUSE: &str = r#"

Repo-specific direct `REVIEW.md` handoff:
- This repo normally records completion notes in `REVIEW.md`, but `auto parallel` treats queue and review files as host-owned state.
- Do not edit `REVIEW.md`, `PLAN.md`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `WORKLIST.md`, `AGENTS.md`, `ARCHIVED.md`, or `RECEIPTS-DRIFT.md` from a lane.
- Preserve blocker or completion evidence in your committed code/tests and command output; the host will reconcile queue and review docs after landing."#;

pub(crate) const LANE_TASK_ID_FILE: &str = "task-id";

/// Run identity, written once per host at startup to `<run_root>/.current-run-id`
/// and copied into each lane's `.run-id` at assignment. `auto parallel status`
/// treats a lane whose `.run-id` differs from (or is missing against) the
/// current run as an artifact of a previous run: shown as stale and excluded
/// from health, so a dead run's lanes never masquerade as live work.
pub(crate) const CURRENT_RUN_ID_FILE: &str = ".current-run-id";
pub(crate) const LANE_RUN_ID_FILE: &str = ".run-id";

pub(crate) const LANE_ASSIGNMENT_FILE: &str = "assignment.json";

pub(crate) const LANE_HOST_PENDING_FILE: &str = "host-pending.json";
pub(crate) const LANE_HOST_PENDING_VERSION: u32 = 1;

pub(crate) async fn run_parallel(args: ParallelArgs) -> Result<()> {
    if args.action == Some(ParallelAction::Status) {
        return run_parallel_status(&args);
    }
    if args.action == Some(ParallelAction::PlanCheck) {
        let repo_root = git_repo_root()?;
        let plan = inspect_loop_plan(&repo_root)?;
        println!(
            "{}: {} task(s), {} actionable",
            active_plan_relative(&repo_root),
            plan.tasks.len(),
            plan.queue_snapshot().pending_ids.len()
        );
        return Ok(());
    }
    if args.action == Some(ParallelAction::ReceiptBackfill) {
        return run_parallel_receipt_backfill(&args);
    }
    if args.action == Some(ParallelAction::Prune) {
        return run_parallel_prune(&args);
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
        ensure_writable_run_root(&run_root)?;
        let startup_lease = acquire_parallel_host_lease(&run_root, "parallel tmux startup")?;
        purge_previous_parallel_run_artifacts(&repo_root, &run_root);
        drop(startup_lease);
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
    ensure_writable_run_root(&run_root)?;
    let _parallel_host_lease = acquire_parallel_host_lease(&run_root, "parallel host startup")?;
    purge_previous_parallel_run_artifacts(&repo_root, &run_root);
    // Reclaim persistent lane-caches for lane indices this run will never use
    // (e.g. after dialing lanes down). Safe at startup: those indices are never
    // active this run. A purge above (clean run) already removed lane-caches
    // wholesale; this catches the resuming-run case where the purge is skipped.
    prune_orphan_lane_caches(&run_root, args.max_concurrent_workers);
    stamp_current_parallel_run_id(&run_root);
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

    // Every worker count, including one, uses an isolated lane and the same
    // host-owned verify -> workspace -> independent-review -> closeout gates.
    // A direct-in-canonical serial worker could otherwise bypass those gates
    // and mint commits that the host never adjudicated.
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
