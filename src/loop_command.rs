use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::claude_exec::{run_claude_exec_with_env, FUTILITY_EXIT_MARKER};
use crate::codex_exec::{
    ensure_tmux_lanes, run_codex_exec_in_tmux_with_env, run_codex_exec_with_env, TmuxCodexRunConfig,
};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, repo_name, sync_branch_with_remote, timestamp_slug,
};
use crate::LoopArgs;

const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];
const BUCKET_PLAN_VERSION: usize = 2;
const LOOP_RUN_STATE_VERSION: usize = 1;
const LOOP_DRAIN_FILE: &str = "drain";
const LOOP_TASK_FIELD_PREFIXES: [&str; 15] = [
    "Spec:",
    "Why now:",
    "Codebase evidence:",
    "Owns:",
    "Integration touchpoints:",
    "Scope boundary:",
    "Acceptance criteria:",
    "Verification:",
    "Required tests:",
    "Dependencies:",
    "Estimated scope:",
    "Completion signal:",
    "Blocker:",
    "Notes:",
    "Decision:",
];
const ROOT_OWNERSHIP_NAMES: [&str; 14] = [
    "docs", "specs", "plans", "scripts", "fixtures", "deploy", "ops", "src", "tests", "xtask",
    "types", "operator", "node", "indexer",
];

const DIRECT_REVIEW_QUEUE_LOOP_CLAUSE: &str = r#"

Repo-specific direct `REVIEW.md` handoff:
- This repo forbids root `COMPLETED.md`, `WORKLIST.md`, and `ARCHIVED.md`.
  These bullets override any generic tracker instructions above.
- When a task finishes, remove it from `IMPLEMENTATION_PLAN.md` and append the
  completion record directly to `REVIEW.md` with task id, changed surfaces,
  validation commands, known failures, and commit sha when available.
- If you find out-of-scope work, add an explicit unchecked
  `IMPLEMENTATION_PLAN.md` item with spec linkage instead of writing
  `WORKLIST.md`.
- Stage `REVIEW.md` with the implementation files and `IMPLEMENTATION_PLAN.md`
  when they changed. Do not create or stage `COMPLETED.md`, `WORKLIST.md`, or
  `ARCHIVED.md`."#;

pub(crate) const DEFAULT_LOOP_PROMPT_TEMPLATE: &str = r#"0a. Read only the minimum `AGENTS.md` content needed for repo-specific build, narrow validation, staging, and branch rules. Do not pull unrelated operational commentary into your working context unless the current task actually touches it.
0b. Study `IMPLEMENTATION_PLAN.md` and identify the first pending task marked `- [ ]` whose explicit dependencies are already satisfied. Treat tasks marked `- [!]` as blocked and skip them unless they are later unblocked.
0c. Study `specs/*` with full repo context, but when multiple dated specs cover the same surface, treat the newest spec referenced by the current unchecked task as authoritative. Older or duplicate specs are historical context only.
0d. Use the specs, plan, and the live codebase as a single contract. If they disagree, treat the code and the current task's authoritative specs as evidence, record the conflict truthfully, and do not bluff your way through it.
0e. For every current-state fact, trust the live codebase over planning artifacts unless the code is plainly stale and the repo includes stronger primary-source evidence.
0f. When additional repositories are listed below, inspect and edit them directly when the current task's owned surfaces, acceptance criteria, or blocker evidence point there. Read each touched repo's own `AGENTS.md` and operational docs before editing it.

1. Your task is to implement functionality per the specifications using the full repository context.
   - Follow `IMPLEMENTATION_PLAN.md` in order and take the next pending `- [ ]` task from top to bottom.
   - Do not reprioritize the queue yourself.
   - Do not stop on earlier `- [!]` tasks; they are blocked and not runnable in this iteration.
   - Before making changes, search the codebase, tests, and planning artifacts. Do not assume a surface is missing until you verify it.
   - If the current task's owned surfaces live in an additional listed repo, do the code change there while keeping this queue repo's planning artifacts truthful.
   - Build a short task brief for yourself before editing: task id, spec refs, owned surfaces, integration touchpoints, scope boundary, acceptance criteria, verification, and any assumptions you are relying on.
   - Restate the task's assumptions and success conditions from repo evidence before editing. If the plan/spec/task contract is ambiguous, resolve the ambiguity in the docs before pretending implementation can start.

2. Implement the task in the smallest truthful slice that fully closes it using a RED/GREEN/REFACTOR cycle by default:
   - Stay within the task contract's owned surfaces plus the minimum adjacent integration edits needed to make the code work.
   - Prefer the simplest solution that matches the existing codebase patterns. Do not add abstractions that are not earning their complexity.
   - Keep the codebase compilable while you work. Do not leave placeholders, TODOs, or half-wired scaffolding.
   - If the repo is still greenfield, perform the bootstrap work the plan requires instead of pretending later tasks are ready.
   - If the task changes behavior or fixes a bug, start by writing or identifying a failing test, failing command, or other executable proof. Confirm the proof fails before claiming the bug or missing behavior is reproduced.
   - Make the minimum code change that turns the proof green.
   - After the proof is green, run a short simplification pass on the touched code: improve names, remove dead paths, reduce unnecessary branching, and collapse unearned abstractions without changing behavior or widening scope.
   - For browser-facing or runtime-sensitive changes, use browser/runtime verification when available instead of relying on static reasoning alone.
   - If the slice needs to land before the full user-facing feature is ready, prefer existing safe-default or feature-gating patterns in the repo. Do not invent a new flag system if the repo has none.

3. When anything breaks, stop the line and debug systematically:
   - Preserve the failing command, output, repro step, or screenshot evidence.
   - Reproduce the failure as narrowly as you can.
   - Fix the root cause, not the nearest symptom.
   - Guard against recurrence with tests or tighter validation when practical.
   - Resume feature work only after the task's verification story is truthful again.

4. Keep the planning artifacts current:
   - When you discover important implementation facts, blockers, or scope corrections, update `IMPLEMENTATION_PLAN.md`.
   - When you finish a task, remove its entry from `IMPLEMENTATION_PLAN.md` so the plan remains an active queue of unfinished work only.
   - When a task is blocked by an external dependency or owner decision, mark it as `- [!]` and record the blocker under that task.
   - Append a concise record to `COMPLETED.md` with task id, what was completed, the validation command(s), and commit sha.
   - If you notice worthwhile out-of-scope work, append a concise item to `WORKLIST.md` instead of quietly broadening scope.
   - Update `AGENTS.md` only when you learn something operational that will help future loops run or validate the repo correctly.

5. When validation passes, commit the increment:
   - Stage only the files relevant to the completed task plus `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `WORKLIST.md`, and `AGENTS.md` when they changed.
   - Do not sweep unrelated pre-existing churn into the commit.
   - If you touch multiple repositories, commit and push each repository separately. Never try to mix files from different git repos into one commit.
   - Commit with a message like `repo-name: TASK-ID short description` using the actual repository name for each touched repo.
   - Before committing, rerun the task's direct proof plus only the narrow additional regression commands explicitly required by the task contract or needed for the touched surfaces.
   - Do not default to workspace-wide or package-wide validation suites. Run them only when the current task explicitly names them or there is no narrower truthful proof.
   - After committing, run `git status` in every touched repo to verify no implementation files were left unstaged. If any were, amend the relevant commit.
   - Push the queue repo directly to `origin/{branch}` after the commit. For additional listed repos, push the currently checked-out branch unless that repo's own instructions require something else.

6. If you hit a real blocker after genuine debugging:
   - Convert the task marker from `- [ ]` to `- [!]` and record the blocker under the task in `IMPLEMENTATION_PLAN.md`.
   - Commit the planning update if it materially changes the execution record.
   - Move to the next pending `- [ ]` task instead of repeating the same failed attempt.

7. Task-order rule:
   - Treat the order in `IMPLEMENTATION_PLAN.md` as authoritative.
   - Work on the first pending `- [ ]` task unless its explicit dependencies are still unchecked.
   - Treat `- [!]` tasks as blocked and skip them while selecting work.
   - If the current task is already satisfied, remove it from `IMPLEMENTATION_PLAN.md`, append a truthful note to `COMPLETED.md`, and continue downward.

8. Branch rule:
   - Work only on branch `{branch}`.
   - Do not create or push feature branches, lane branches, or topic branches.

99999. Important: keep `AGENTS.md` operational only.
999999. Important: prefer complete working increments over placeholders.
9999999. Important: if unrelated tests fail and they prevent a truthful green result, fix them as part of the increment.
99999999. CRITICAL: Do not assume functionality is missing — search the codebase to confirm before implementing anything new.
999999999. Every new module must be importable and wired into the package. Dead code that isn't reachable from any entry point is an island — wire it before committing.
9999999999. When you learn something new about how to build, run, or validate the repo, update `AGENTS.md` — but keep it brief and operational only.
99999999999. A task is not done because the code looks right. It is done when the acceptance criteria are satisfied and the verification evidence is real.
999999999999. Shell safety: never pass file contents or large strings (>50KB) as inline shell command arguments — write them to a temp file instead. Narrow glob patterns with directory prefixes so they cannot expand to thousands of paths and hit the OS argument limit.
9999999999999. Search resilience: treat empty Grep/Glob/Find results as evidence, not proof a surface is missing. If an exact symbol search misses, inspect the containing enum/struct/module, nearby tests, and the latest compiler/test errors before retrying the search.
99999999999999. Search futility: if the same search tool returns empty results 3 times in a row, stop and re-evaluate your approach. The thing you are looking for may not exist, may be named differently, or may live in a different location. Prefer behavior-level searches and current code definitions over stale symbol names."#;

pub(crate) async fn run_loop(args: LoopArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let reference_repos =
        resolve_reference_repos(&repo_root, &args.reference_repos, args.include_siblings)?;

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    let target_branch = resolve_loop_branch(&repo_root, args.branch.as_deref(), &current_branch)?;
    if current_branch != target_branch {
        bail!(
            "auto loop must run on branch `{}` (current: `{}`)",
            target_branch,
            current_branch
        );
    }

    let using_default_prompt = args.prompt_file.is_none();
    let mut prompt_template = match &args.prompt_file {
        Some(path) => {
            let prompt = fs::read_to_string(path)
                .with_context(|| format!("failed to read prompt file {}", path.display()))?;
            append_reference_repo_clause(prompt, &reference_repos)
        }
        None => render_default_loop_prompt(&target_branch, &reference_repos),
    };
    if using_default_prompt && repo_forbids_legacy_review_trackers(&repo_root) {
        prompt_template.push_str(DIRECT_REVIEW_QUEUE_LOOP_CLAUSE);
    }
    let run_root = args
        .run_root
        .clone()
        .unwrap_or_else(|| repo_root.join(".auto").join("loop"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let worker_env = build_loop_worker_env(&args)?;
    let stderr_log_path = run_root.join("codex.stderr.log");

    let harness = if args.claude { "Claude" } else { "Codex" };

    println!("auto loop");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", target_branch);
    if args.claude {
        println!("harness:     Claude (Opus 4.6 high)");
        println!(
            "max turns:   {}",
            args.max_turns
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unlimited".to_string())
        );
        println!("max retries: {}", args.max_retries);
    } else {
        println!("model:       {}", args.model);
        println!("reasoning:   {}", args.reasoning_effort);
    }
    println!("run root:    {}", run_root.display());
    println!("threads:     {}", args.threads.max(1));
    println!("cargo jobs:  {}", worker_env.cargo_jobs_summary);
    if args.drain_after_current_wave {
        println!("drain mode:  after initial dispatch");
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

    if let Some(commit) =
        auto_checkpoint_if_needed(&repo_root, target_branch.as_str(), "auto loop checkpoint")?
    {
        println!("checkpoint:  committed pre-existing changes at {commit}");
    } else if sync_branch_with_remote(&repo_root, target_branch.as_str())? {
        println!("remote sync: rebased onto origin/{}", target_branch);
    }

    if args.threads.max(1) > 1 {
        return run_parallel_loop(
            &args,
            &repo_root,
            &target_branch,
            &reference_repos,
            &prompt_template,
            &run_root,
            &worker_env,
        )
        .await;
    }

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

        let queue = inspect_loop_queue(&repo_root)?;
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

        let current_task = queue.pending_ids[0].clone();
        println!("next task:   {}", current_task);
        if !queue.blocked_ids.is_empty() {
            println!("blocked:     {}", queue.blocked_ids.join(", "));
        }

        let full_prompt = build_iteration_prompt(&prompt_template, &queue);

        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("loop-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        println!("prompt log:  {}", prompt_path.display());

        let state_before = collect_tracked_repo_states(&repo_root, &reference_repos)?;
        println!();
        println!("running {harness} iteration {}", iteration + 1);

        let exit_status = if args.claude {
            run_claude_exec_with_env(
                &repo_root,
                &full_prompt,
                args.max_turns,
                &stderr_log_path,
                "auto loop",
                &worker_env.extra_env,
            )
            .await?
        } else {
            run_codex_exec_with_env(
                &repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                &args.codex_bin,
                &stderr_log_path,
                "auto loop",
                &worker_env.extra_env,
            )
            .await?
        };
        if !exit_status.success() {
            let exit_code = exit_status.code().unwrap_or(-1);
            let is_futility = exit_code == FUTILITY_EXIT_MARKER;
            consecutive_failures += 1;

            // Checkpoint any partial progress before potentially bailing
            if let Some(commit) = auto_checkpoint_if_needed(
                &repo_root,
                target_branch.as_str(),
                &format!("auto loop checkpoint (pre-retry {})", consecutive_failures),
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

        let state_after = collect_tracked_repo_states(&repo_root, &reference_repos)?;
        match summarize_repo_progress(&state_before, &state_after) {
            RepoProgress::NewCommits => {}
            RepoProgress::DirtyChanges(repos) => {
                bail!(
                    "tracked repo changes were left uncommitted in: {}; commit or revert them before continuing",
                    repos.join(", ")
                );
            }
            RepoProgress::None => {
                if let Some(commit) = auto_checkpoint_if_needed(
                    &repo_root,
                    target_branch.as_str(),
                    "auto loop checkpoint",
                )? {
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

        if push_branch_with_remote_sync(&repo_root, target_branch.as_str())? {
            println!("remote sync: rebased onto origin/{}", target_branch);
        }
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, target_branch.as_str(), "auto loop checkpoint")?
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
    args: &LoopArgs,
    repo_root: &Path,
    target_branch: &str,
    reference_repos: &[PathBuf],
    prompt_template: &str,
    run_root: &Path,
    worker_env: &LoopWorkerEnv,
) -> Result<()> {
    if args.claude {
        bail!("parallel auto loop currently supports Codex workers only; rerun without --claude");
    }
    if !reference_repos.is_empty() {
        bail!(
            "parallel auto loop does not isolate reference repositories yet; rerun without --threads or without --reference-repo/--include-siblings"
        );
    }

    let threads = args.threads.max(1);
    let plan = read_loop_plan(repo_root)?;
    let fingerprint = stable_plan_fingerprint(&plan);
    let tasks = parse_loop_tasks(&plan);
    let bucket_plan =
        load_or_create_bucket_plan(run_root, target_branch, threads, &fingerprint, &tasks)?;
    let tmux_session = build_tmux_session_name(repo_root, &fingerprint);
    ensure_tmux_lanes(&tmux_session, threads, repo_root)?;
    let state_path = run_root.join("state.json");
    let drain_path = run_root.join(LOOP_DRAIN_FILE);
    let mut run_state = new_loop_run_state(
        target_branch,
        &fingerprint,
        threads,
        &tmux_session,
        &bucket_plan.markdown_path,
    );
    write_loop_run_state(&state_path, &mut run_state)?;
    println!("bucket plan: {}", bucket_plan.markdown_path.display());
    println!(
        "bucket reuse: {}",
        if bucket_plan.reused { "yes" } else { "no" }
    );
    println!("{}", render_bucket_summary(&bucket_plan.plan));
    println!("state:      {}", state_path.display());
    println!(
        "drain file: touch {} to stop new dispatches",
        drain_path.display()
    );
    println!("tmux:       tmux attach -t {tmux_session}");

    let (tx, mut rx) = mpsc::unbounded_channel::<ParallelWorkerResult>();
    let mut active = BTreeMap::<usize, ActiveParallelWorker>::new();
    let mut completed_task_ids = BTreeSet::<String>::new();
    let mut failed_task_ids = BTreeSet::<String>::new();
    let mut completed = 0usize;
    let mut first_error = None::<String>;
    let mut draining = false;
    let mut drain_after_initial_dispatch = args.drain_after_current_wave;

    loop {
        if drain_path.exists() && !draining {
            println!("drain requested by {}", drain_path.display());
            draining = true;
            run_state.drain_requested = true;
            write_loop_run_state(&state_path, &mut run_state)?;
        }

        let current_plan = read_loop_plan(repo_root)?;
        let current_tasks = parse_loop_tasks(&current_plan);
        let pending_task_ids = current_tasks
            .iter()
            .filter(|task| task.status == LoopTaskStatus::Pending)
            .map(|task| task.id.clone())
            .collect::<BTreeSet<_>>();
        let task_map = current_tasks
            .iter()
            .cloned()
            .map(|task| (task.id.clone(), task))
            .collect::<BTreeMap<_, _>>();

        let mut dispatched = 0usize;
        if first_error.is_none() && !draining {
            loop {
                if args
                    .max_iterations
                    .is_some_and(|limit| completed + active.len() >= limit)
                {
                    break;
                }
                let active_task_ids = active
                    .values()
                    .map(|worker| worker.task.id.clone())
                    .collect::<BTreeSet<_>>();
                let active_owned_paths = active
                    .values()
                    .flat_map(|worker| worker.task.owned_paths.iter().cloned())
                    .collect::<Vec<_>>();
                let Some(slot) = next_dispatchable_bucket_slot(
                    &bucket_plan.plan,
                    &current_tasks,
                    &active,
                    &active_task_ids,
                    &completed_task_ids,
                    &failed_task_ids,
                    &active_owned_paths,
                ) else {
                    break;
                };
                let task = task_map
                    .get(&slot.task_id)
                    .with_context(|| format!("bucket references missing task `{}`", slot.task_id))?
                    .clone();
                println!(
                    "dispatch:    lane-{} wave {} `{}`",
                    slot.thread, slot.wave, task.id
                );
                let active_worker = dispatch_parallel_worker(
                    args,
                    repo_root,
                    run_root,
                    target_branch,
                    prompt_template,
                    worker_env,
                    &tmux_session,
                    &fingerprint,
                    slot,
                    task,
                    tx.clone(),
                )?;
                set_loop_lane_running(&mut run_state, &active_worker);
                write_loop_run_state(&state_path, &mut run_state)?;
                active.insert(active_worker.slot.thread, active_worker);
                dispatched += 1;
            }
        }

        if drain_after_initial_dispatch && dispatched > 0 {
            draining = true;
            run_state.drain_requested = true;
            drain_after_initial_dispatch = false;
            println!("drain mode: no new tasks will be dispatched after current lanes finish");
            write_loop_run_state(&state_path, &mut run_state)?;
        }

        if active.is_empty() {
            if let Some(error) = first_error {
                bail!("{error}");
            }
            if pending_task_ids.is_empty() {
                println!("no pending `- [ ]` tasks remain; stopping.");
            } else if args.max_iterations.is_some_and(|limit| completed >= limit) {
                println!(
                    "reached max iterations/tasks: {}",
                    args.max_iterations.unwrap_or_default()
                );
            } else if draining {
                println!("drain complete; no active lanes remain.");
            } else {
                println!("no bucketed tasks are currently ready; stopping.");
            }
            break;
        }

        let result = rx
            .recv()
            .await
            .context("parallel worker channel closed while lanes were active")?;
        active.remove(&result.slot.thread);
        set_loop_lane_integrating(&mut run_state, &result);
        write_loop_run_state(&state_path, &mut run_state)?;

        if let Some(error) = result.error.as_deref() {
            let message = format!(
                "parallel worker `{}` failed before returning; worktree left at {}: {error}",
                result.task.id,
                result.worker.worktree.display()
            );
            println!("worker failed: {message}");
            failed_task_ids.insert(result.task.id.clone());
            first_error.get_or_insert(message.clone());
            draining = true;
            run_state.drain_requested = true;
            set_loop_lane_failed(
                &mut run_state,
                &result,
                result.exit_status.code().unwrap_or(-1),
                &message,
            );
            write_loop_run_state(&state_path, &mut run_state)?;
            continue;
        }

        if !result.exit_status.success() {
            let exit_code = result.exit_status.code().unwrap_or(-1);
            let message = format!(
                "parallel worker `{}` exited with status {}; worktree left at {}",
                result.task.id,
                exit_code,
                result.worker.worktree.display()
            );
            println!("worker failed: {message}");
            failed_task_ids.insert(result.task.id.clone());
            first_error.get_or_insert(message.clone());
            draining = true;
            run_state.drain_requested = true;
            set_loop_lane_failed(&mut run_state, &result, exit_code, &message);
            write_loop_run_state(&state_path, &mut run_state)?;
            continue;
        }

        if let Err(err) = integrate_parallel_worker_result(
            repo_root,
            target_branch,
            reference_repos,
            &result.task,
            &result.worker,
        ) {
            let message = format!(
                "failed integrating `{}` from {}: {err:#}",
                result.task.id,
                result.worker.worktree.display()
            );
            println!("integration failed: {message}");
            failed_task_ids.insert(result.task.id.clone());
            first_error.get_or_insert(message.clone());
            draining = true;
            run_state.drain_requested = true;
            set_loop_lane_failed(
                &mut run_state,
                &result,
                result.exit_status.code().unwrap_or(-1),
                &message,
            );
            write_loop_run_state(&state_path, &mut run_state)?;
            continue;
        }

        completed += 1;
        completed_task_ids.insert(result.task.id.clone());
        set_loop_lane_completed(&mut run_state, &result);
        write_loop_run_state(&state_path, &mut run_state)?;
        if push_branch_with_remote_sync(repo_root, target_branch)? {
            println!("remote sync: rebased onto origin/{}", target_branch);
        }
    }

    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoopTask {
    id: String,
    title: String,
    status: LoopTaskStatus,
    dependencies: Vec<String>,
    owned_paths: Vec<String>,
    markdown: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoopTaskStatus {
    Pending,
    Blocked,
    Done,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredBucketPlan {
    #[serde(default)]
    version: usize,
    branch: String,
    threads: usize,
    fingerprint: String,
    waves: Vec<BucketWave>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct BucketWave {
    wave: usize,
    slots: Vec<BucketSlot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct BucketSlot {
    wave: usize,
    thread: usize,
    task_id: String,
    title: String,
    dependencies: Vec<String>,
    owned_paths: Vec<String>,
}

#[derive(Clone, Debug)]
struct LoadedBucketPlan {
    plan: StoredBucketPlan,
    reused: bool,
    markdown_path: PathBuf,
}

#[derive(Clone, Debug)]
struct ParallelWorker {
    branch: String,
    worktree: PathBuf,
}

#[derive(Clone, Debug)]
struct ActiveParallelWorker {
    slot: BucketSlot,
    task: LoopTask,
    worker: ParallelWorker,
    run_dir: PathBuf,
    started_at: String,
}

#[derive(Debug)]
struct ParallelWorkerResult {
    slot: BucketSlot,
    task: LoopTask,
    worker: ParallelWorker,
    run_dir: PathBuf,
    exit_status: std::process::ExitStatus,
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredLoopRunState {
    version: usize,
    branch: String,
    fingerprint: String,
    threads: usize,
    tmux_session: String,
    bucket_plan: String,
    updated_at: String,
    drain_requested: bool,
    lanes: Vec<StoredLaneState>,
    completed: Vec<StoredTaskOutcome>,
    failed: Vec<StoredTaskOutcome>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredLaneState {
    thread: usize,
    status: StoredLaneStatus,
    task_id: Option<String>,
    branch: Option<String>,
    worktree: Option<String>,
    run_dir: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    exit_code: Option<i32>,
    message: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredLaneStatus {
    Idle,
    Running,
    Integrating,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredTaskOutcome {
    task_id: String,
    thread: usize,
    at: String,
    exit_code: Option<i32>,
    message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoopWorkerEnv {
    extra_env: Vec<(String, String)>,
    cargo_jobs_summary: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LoopQueueSnapshot {
    pending_ids: Vec<String>,
    blocked_ids: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn dispatch_parallel_worker(
    args: &LoopArgs,
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    prompt_template: &str,
    worker_env: &LoopWorkerEnv,
    tmux_session: &str,
    fingerprint: &str,
    slot: BucketSlot,
    task: LoopTask,
    tx: mpsc::UnboundedSender<ParallelWorkerResult>,
) -> Result<ActiveParallelWorker> {
    let worker = prepare_parallel_worker(repo_root, run_root, target_branch, fingerprint, &task)?;
    let prompt = build_parallel_worker_prompt(prompt_template, &task, &worker.branch);
    let prompt_path = worker
        .worktree
        .join(".auto")
        .join("parallel-worker-prompt.md");
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;

    let run_dir = worker
        .worktree
        .join(".auto")
        .join("tmux")
        .join(format!("lane-{}-{}", slot.thread, task.id));
    let active_worker = ActiveParallelWorker {
        slot: slot.clone(),
        task: task.clone(),
        worker: worker.clone(),
        run_dir: run_dir.clone(),
        started_at: Utc::now().to_rfc3339(),
    };

    let model = args.model.clone();
    let reasoning_effort = args.reasoning_effort.clone();
    let codex_bin = args.codex_bin.clone();
    let lane_label = format!("lane-{} {}", slot.thread, task.id);
    let tmux = TmuxCodexRunConfig {
        session_name: tmux_session.to_string(),
        window_name: format!("lane-{}", slot.thread),
        run_dir: run_dir.clone(),
        lane_label,
    };
    let stderr_log_path = worker.worktree.join(".auto").join("codex.stderr.log");
    let context_label = format!("auto loop {}", task.id);
    let extra_env = worker_env.extra_env.clone();
    tokio::spawn(async move {
        let (exit_status, error) = match run_codex_exec_in_tmux_with_env(
            &worker.worktree,
            &prompt,
            &model,
            &reasoning_effort,
            &codex_bin,
            &stderr_log_path,
            &context_label,
            &extra_env,
            &tmux,
        )
        .await
        {
            Ok(status) => (status, None),
            Err(err) => (
                std::process::ExitStatus::from_raw(1 << 8),
                Some(format!("{err:#}")),
            ),
        };
        let _ = tx.send(ParallelWorkerResult {
            slot,
            task,
            worker,
            run_dir,
            exit_status,
            error,
        });
    });

    Ok(active_worker)
}

fn next_dispatchable_bucket_slot(
    bucket_plan: &StoredBucketPlan,
    current_tasks: &[LoopTask],
    active: &BTreeMap<usize, ActiveParallelWorker>,
    active_task_ids: &BTreeSet<String>,
    completed_task_ids: &BTreeSet<String>,
    failed_task_ids: &BTreeSet<String>,
    active_owned_paths: &[String],
) -> Option<BucketSlot> {
    (1..=bucket_plan.threads)
        .filter(|thread| !active.contains_key(thread))
        .find_map(|thread| {
            next_ready_bucket_slot_for_lane(
                bucket_plan,
                current_tasks,
                thread,
                active_task_ids,
                completed_task_ids,
                failed_task_ids,
                active_owned_paths,
            )
        })
}

fn next_ready_bucket_slot_for_lane(
    bucket_plan: &StoredBucketPlan,
    current_tasks: &[LoopTask],
    thread: usize,
    active_task_ids: &BTreeSet<String>,
    completed_task_ids: &BTreeSet<String>,
    failed_task_ids: &BTreeSet<String>,
    active_owned_paths: &[String],
) -> Option<BucketSlot> {
    let statuses = current_tasks
        .iter()
        .map(|task| (task.id.as_str(), task.status))
        .collect::<BTreeMap<_, _>>();
    for wave in &bucket_plan.waves {
        for slot in wave.slots.iter().filter(|slot| slot.thread == thread) {
            if statuses.get(slot.task_id.as_str()) != Some(&LoopTaskStatus::Pending) {
                continue;
            }
            if active_task_ids.contains(&slot.task_id)
                || completed_task_ids.contains(&slot.task_id)
                || failed_task_ids.contains(&slot.task_id)
            {
                continue;
            }
            let dependencies_ready = slot.dependencies.iter().all(|dep| {
                !matches!(
                    statuses.get(dep.as_str()),
                    Some(LoopTaskStatus::Pending | LoopTaskStatus::Blocked)
                )
            });
            if !dependencies_ready {
                continue;
            }
            if owned_paths_conflict_any(&slot.owned_paths, active_owned_paths) {
                continue;
            }
            return Some(slot.clone());
        }
    }
    None
}

fn new_loop_run_state(
    branch: &str,
    fingerprint: &str,
    threads: usize,
    tmux_session: &str,
    bucket_plan_path: &Path,
) -> StoredLoopRunState {
    let now = Utc::now().to_rfc3339();
    StoredLoopRunState {
        version: LOOP_RUN_STATE_VERSION,
        branch: branch.to_string(),
        fingerprint: fingerprint.to_string(),
        threads,
        tmux_session: tmux_session.to_string(),
        bucket_plan: bucket_plan_path.display().to_string(),
        updated_at: now,
        drain_requested: false,
        lanes: (1..=threads)
            .map(|thread| StoredLaneState {
                thread,
                status: StoredLaneStatus::Idle,
                task_id: None,
                branch: None,
                worktree: None,
                run_dir: None,
                started_at: None,
                finished_at: None,
                exit_code: None,
                message: None,
            })
            .collect(),
        completed: Vec::new(),
        failed: Vec::new(),
    }
}

fn write_loop_run_state(path: &Path, state: &mut StoredLoopRunState) -> Result<()> {
    state.updated_at = Utc::now().to_rfc3339();
    let json = serde_json::to_vec_pretty(state).context("failed to serialize loop run state")?;
    atomic_write(path, &json)
}

fn set_loop_lane_running(state: &mut StoredLoopRunState, active_worker: &ActiveParallelWorker) {
    if let Some(lane) = state
        .lanes
        .iter_mut()
        .find(|lane| lane.thread == active_worker.slot.thread)
    {
        lane.status = StoredLaneStatus::Running;
        lane.task_id = Some(active_worker.task.id.clone());
        lane.branch = Some(active_worker.worker.branch.clone());
        lane.worktree = Some(active_worker.worker.worktree.display().to_string());
        lane.run_dir = Some(active_worker.run_dir.display().to_string());
        lane.started_at = Some(active_worker.started_at.clone());
        lane.finished_at = None;
        lane.exit_code = None;
        lane.message = Some(format!("wave {}", active_worker.slot.wave));
    }
}

fn set_loop_lane_integrating(state: &mut StoredLoopRunState, result: &ParallelWorkerResult) {
    if let Some(lane) = state
        .lanes
        .iter_mut()
        .find(|lane| lane.thread == result.slot.thread)
    {
        lane.status = StoredLaneStatus::Integrating;
        lane.task_id = Some(result.task.id.clone());
        lane.branch = Some(result.worker.branch.clone());
        lane.worktree = Some(result.worker.worktree.display().to_string());
        lane.run_dir = Some(result.run_dir.display().to_string());
        lane.finished_at = Some(Utc::now().to_rfc3339());
        lane.exit_code = result.exit_status.code();
        lane.message = Some("worker finished; coordinator integrating branch".to_string());
    }
}

fn set_loop_lane_completed(state: &mut StoredLoopRunState, result: &ParallelWorkerResult) {
    let now = Utc::now().to_rfc3339();
    if let Some(lane) = state
        .lanes
        .iter_mut()
        .find(|lane| lane.thread == result.slot.thread)
    {
        lane.status = StoredLaneStatus::Completed;
        lane.task_id = Some(result.task.id.clone());
        lane.branch = Some(result.worker.branch.clone());
        lane.worktree = Some(result.worker.worktree.display().to_string());
        lane.run_dir = Some(result.run_dir.display().to_string());
        lane.finished_at = Some(now.clone());
        lane.exit_code = result.exit_status.code();
        lane.message = Some("merged into coordinator branch".to_string());
    }
    state.completed.push(StoredTaskOutcome {
        task_id: result.task.id.clone(),
        thread: result.slot.thread,
        at: now,
        exit_code: result.exit_status.code(),
        message: Some("merged".to_string()),
    });
}

fn set_loop_lane_failed(
    state: &mut StoredLoopRunState,
    result: &ParallelWorkerResult,
    exit_code: i32,
    message: &str,
) {
    let now = Utc::now().to_rfc3339();
    if let Some(lane) = state
        .lanes
        .iter_mut()
        .find(|lane| lane.thread == result.slot.thread)
    {
        lane.status = StoredLaneStatus::Failed;
        lane.task_id = Some(result.task.id.clone());
        lane.branch = Some(result.worker.branch.clone());
        lane.worktree = Some(result.worker.worktree.display().to_string());
        lane.run_dir = Some(result.run_dir.display().to_string());
        lane.finished_at = Some(now.clone());
        lane.exit_code = Some(exit_code);
        lane.message = Some(message.to_string());
    }
    state.failed.push(StoredTaskOutcome {
        task_id: result.task.id.clone(),
        thread: result.slot.thread,
        at: now,
        exit_code: Some(exit_code),
        message: Some(message.to_string()),
    });
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

fn build_loop_worker_env(args: &LoopArgs) -> Result<LoopWorkerEnv> {
    let inherited = std::env::var("CARGO_BUILD_JOBS").ok();
    let parallelism = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4);
    resolve_loop_worker_env(
        args.threads.max(1),
        args.cargo_build_jobs,
        inherited.as_deref(),
        parallelism,
    )
}

fn resolve_loop_worker_env(
    threads: usize,
    cargo_build_jobs: Option<usize>,
    inherited_cargo_build_jobs: Option<&str>,
    available_parallelism: usize,
) -> Result<LoopWorkerEnv> {
    if let Some(jobs) = cargo_build_jobs {
        if jobs == 0 {
            bail!("--cargo-build-jobs must be greater than 0");
        }
        return Ok(cargo_build_jobs_env(
            jobs,
            format!("override CARGO_BUILD_JOBS={jobs}"),
        ));
    }

    if let Some(value) = inherited_cargo_build_jobs {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(LoopWorkerEnv {
                extra_env: Vec::new(),
                cargo_jobs_summary: format!("inherited CARGO_BUILD_JOBS={value}"),
            });
        }
    }

    let jobs = default_cargo_build_jobs_for(threads, available_parallelism);
    Ok(cargo_build_jobs_env(
        jobs,
        format!("auto CARGO_BUILD_JOBS={jobs}"),
    ))
}

fn cargo_build_jobs_env(jobs: usize, cargo_jobs_summary: String) -> LoopWorkerEnv {
    LoopWorkerEnv {
        extra_env: vec![("CARGO_BUILD_JOBS".to_string(), jobs.to_string())],
        cargo_jobs_summary,
    }
}

fn default_cargo_build_jobs_for(threads: usize, available_parallelism: usize) -> usize {
    let threads = threads.max(1);
    let available_parallelism = available_parallelism.max(1);
    (available_parallelism / (threads + 2)).clamp(1, 4)
}

fn inspect_loop_queue(repo_root: &Path) -> Result<LoopQueueSnapshot> {
    let plan = read_loop_plan(repo_root)?;
    Ok(parse_loop_queue(&plan))
}

fn read_loop_plan(repo_root: &Path) -> Result<String> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))
}

fn parse_loop_queue(plan: &str) -> LoopQueueSnapshot {
    let mut queue = LoopQueueSnapshot::default();
    for line in plan.lines() {
        let trimmed = line.trim_start();
        if let Some(task) = trimmed.strip_prefix("- [ ] ") {
            if let Some(task_id) = extract_task_id(task) {
                queue.pending_ids.push(task_id);
            }
        } else if let Some(task) = trimmed.strip_prefix("- [!] ") {
            if let Some(task_id) = extract_task_id(task) {
                queue.blocked_ids.push(task_id);
            }
        }
    }
    queue
}

fn parse_loop_tasks(plan: &str) -> Vec<LoopTask> {
    let mut tasks = Vec::new();
    let mut current_header = None::<String>;
    let mut current_lines = Vec::<String>::new();

    for line in plan.lines() {
        let trimmed = line.trim_start();
        let is_task = trimmed.starts_with("- [ ] ")
            || trimmed.starts_with("- [!] ")
            || trimmed.starts_with("- [x] ")
            || trimmed.starts_with("- [X] ");
        if is_task {
            if let Some(task) = finalize_loop_task(current_header.take(), &current_lines) {
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

    if let Some(task) = finalize_loop_task(current_header, &current_lines) {
        tasks.push(task);
    }
    tasks
}

fn finalize_loop_task(header: Option<String>, lines: &[String]) -> Option<LoopTask> {
    let header = header?;
    let (status, id, title) = parse_loop_task_header(&header)?;
    let markdown = lines.join("\n");
    let dependencies = parse_task_dependencies(&markdown);
    let owned_paths = parse_task_owned_paths(&markdown);
    Some(LoopTask {
        id,
        title,
        status,
        dependencies,
        owned_paths,
        markdown,
    })
}

fn parse_loop_task_header(line: &str) -> Option<(LoopTaskStatus, String, String)> {
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
    let rest = rest.strip_prefix('`')?;
    let end = rest.find('`')?;
    let id = rest[..end].to_string();
    let title = rest[end + 1..].trim().to_string();
    Some((status, id, title))
}

fn parse_task_dependencies(markdown: &str) -> Vec<String> {
    let Some(line) = markdown
        .lines()
        .find(|line| line.trim_start().starts_with("Dependencies:"))
    else {
        return Vec::new();
    };
    let mut deps = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else {
            break;
        };
        let candidate = &rest[..end];
        if candidate.starts_with("P-")
            && candidate
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            deps.push(candidate.to_string());
        }
        rest = &rest[end + 1..];
    }
    deps.sort();
    deps.dedup();
    deps
}

fn parse_task_owned_paths(markdown: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for field in ["Owns:", "Integration touchpoints:"] {
        let Some(body) = task_field_body(markdown, field) else {
            continue;
        };
        paths.extend(parse_owned_paths_from_body(&body));
    }
    paths.sort();
    paths.dedup();
    paths
}

fn parse_owned_paths_from_body(body: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else {
            break;
        };
        let candidate = normalize_owned_path(&rest[..end]);
        if let Some(path) = candidate {
            paths.push(path);
        }
        rest = &rest[end + 1..];
    }
    if paths.is_empty() {
        for token in
            body.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | '(' | ')'))
        {
            if let Some(path) = normalize_owned_path(token) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn task_field_body(markdown: &str, field: &str) -> Option<String> {
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
        if collecting && is_loop_task_field_start(trimmed) {
            break;
        }
        if collecting {
            body.push(line.to_string());
        }
    }
    collecting.then(|| body.join("\n"))
}

fn is_loop_task_field_start(trimmed: &str) -> bool {
    LOOP_TASK_FIELD_PREFIXES
        .iter()
        .any(|field| trimmed.starts_with(field))
}

fn normalize_owned_path(token: &str) -> Option<String> {
    let trimmed = token
        .trim()
        .trim_matches(|ch: char| matches!(ch, '.' | ':' | '"' | '\'' | '`'));
    if trimmed.is_empty()
        || trimmed.contains('*')
        || trimmed.chars().any(char::is_whitespace)
        || trimmed.starts_with('-')
        || trimmed.starts_with('$')
    {
        return None;
    }
    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.contains('/')
        || trimmed.ends_with(".rs")
        || trimmed.ends_with(".toml")
        || trimmed.ends_with(".md")
        || looks_like_root_owned_path(trimmed)
    {
        return Some(trimmed.to_string());
    }
    None
}

fn looks_like_root_owned_path(token: &str) -> bool {
    if ROOT_OWNERSHIP_NAMES.contains(&token) {
        return true;
    }
    token.contains('-')
        && token
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

fn extract_task_id(task_line: &str) -> Option<String> {
    let rest = task_line.strip_prefix('`')?;
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

fn stable_plan_fingerprint(plan: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in plan.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn load_or_create_bucket_plan(
    run_root: &Path,
    branch: &str,
    threads: usize,
    fingerprint: &str,
    tasks: &[LoopTask],
) -> Result<LoadedBucketPlan> {
    let json_path = run_root.join("parallel-buckets.json");
    let markdown_path = run_root.join("parallel-buckets.md");
    if json_path.exists() {
        let text = fs::read_to_string(&json_path)
            .with_context(|| format!("failed to read {}", json_path.display()))?;
        let stored: StoredBucketPlan = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse {}", json_path.display()))?;
        if bucket_plan_reusable(&stored, branch, threads, fingerprint, tasks) {
            atomic_write(
                &markdown_path,
                render_bucket_plan_markdown(&stored).as_bytes(),
            )?;
            return Ok(LoadedBucketPlan {
                plan: stored,
                reused: true,
                markdown_path,
            });
        }
    }

    let plan = build_bucket_plan(branch, threads, fingerprint, tasks);
    let json = serde_json::to_vec_pretty(&plan).context("failed to serialize bucket plan")?;
    atomic_write(&json_path, &json)?;
    atomic_write(
        &markdown_path,
        render_bucket_plan_markdown(&plan).as_bytes(),
    )?;
    Ok(LoadedBucketPlan {
        plan,
        reused: false,
        markdown_path,
    })
}

fn bucket_plan_reusable(
    stored: &StoredBucketPlan,
    branch: &str,
    threads: usize,
    fingerprint: &str,
    current_tasks: &[LoopTask],
) -> bool {
    if stored.branch != branch || stored.threads != threads {
        return false;
    }
    if stored.version != BUCKET_PLAN_VERSION {
        return false;
    }
    if stored.fingerprint == fingerprint {
        return true;
    }

    let stored_slots = stored
        .waves
        .iter()
        .flat_map(|wave| wave.slots.iter())
        .map(|slot| (slot.task_id.as_str(), slot))
        .collect::<BTreeMap<_, _>>();

    current_tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Pending)
        .all(|task| {
            stored_slots.get(task.id.as_str()).is_some_and(|slot| {
                slot.title == task.title
                    && slot.dependencies == task.dependencies
                    && slot.owned_paths == task.owned_paths
            })
        })
}

fn build_bucket_plan(
    branch: &str,
    threads: usize,
    fingerprint: &str,
    tasks: &[LoopTask],
) -> StoredBucketPlan {
    let mut remaining = tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Pending)
        .cloned()
        .collect::<Vec<_>>();
    let active_ids = remaining
        .iter()
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    let blocked_ids = tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Blocked)
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    let mut completed = tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Done || !active_ids.contains(&task.id))
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    let mut waves = Vec::new();
    let mut wave_index = 1usize;

    while !remaining.is_empty() {
        let mut selected = Vec::<LoopTask>::new();
        let mut selected_owned_paths = Vec::<String>::new();
        let mut selected_has_unowned_task = false;
        let mut selected_ids = BTreeSet::<String>::new();
        for task in &remaining {
            if task.dependencies.iter().any(|dep| {
                blocked_ids.contains(dep) || (active_ids.contains(dep) && !completed.contains(dep))
            }) {
                continue;
            }
            if selected.len() >= threads {
                break;
            }
            if task.owned_paths.is_empty() && selected_has_unowned_task {
                continue;
            }
            if owned_paths_conflict_any(&task.owned_paths, &selected_owned_paths) {
                continue;
            }
            if task.owned_paths.is_empty() {
                selected_has_unowned_task = true;
            } else {
                selected_owned_paths.extend(task.owned_paths.iter().cloned());
            }
            selected_ids.insert(task.id.clone());
            selected.push(task.clone());
        }

        if selected.is_empty() {
            selected.push(remaining[0].clone());
            selected_ids.insert(remaining[0].id.clone());
        }

        let slots = selected
            .iter()
            .enumerate()
            .map(|(index, task)| BucketSlot {
                wave: wave_index,
                thread: index + 1,
                task_id: task.id.clone(),
                title: task.title.clone(),
                dependencies: task.dependencies.clone(),
                owned_paths: task.owned_paths.clone(),
            })
            .collect::<Vec<_>>();
        for task in &selected {
            completed.insert(task.id.clone());
        }
        remaining.retain(|task| !selected_ids.contains(&task.id));
        waves.push(BucketWave {
            wave: wave_index,
            slots,
        });
        wave_index += 1;
    }

    StoredBucketPlan {
        version: BUCKET_PLAN_VERSION,
        branch: branch.to_string(),
        threads,
        fingerprint: fingerprint.to_string(),
        waves,
    }
}

fn owned_paths_conflict_any(candidate: &[String], selected: &[String]) -> bool {
    if candidate.is_empty() || selected.is_empty() {
        return false;
    }
    candidate.iter().any(|left| {
        selected
            .iter()
            .any(|right| owned_paths_conflict(left, right))
    })
}

fn owned_paths_conflict(left: &str, right: &str) -> bool {
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}

fn render_bucket_plan_markdown(plan: &StoredBucketPlan) -> String {
    let mut output = format!(
        "# Auto Loop Parallel Buckets\n\nversion: `{}`\nbranch: `{}`\nthreads: `{}`\nfingerprint: `{}`\n\n",
        plan.version, plan.branch, plan.threads, plan.fingerprint
    );
    for wave in &plan.waves {
        output.push_str(&format!("## Wave {}\n\n", wave.wave));
        for slot in &wave.slots {
            let deps = if slot.dependencies.is_empty() {
                "none".to_string()
            } else {
                slot.dependencies.join(", ")
            };
            let owns = if slot.owned_paths.is_empty() {
                "unspecified".to_string()
            } else {
                slot.owned_paths.join(", ")
            };
            output.push_str(&format!(
                "- Thread {}: `{}` {}  \n  deps: {}  \n  owns: {}\n",
                slot.thread, slot.task_id, slot.title, deps, owns
            ));
        }
        output.push('\n');
    }
    output
}

fn render_bucket_summary(plan: &StoredBucketPlan) -> String {
    let preview = plan
        .waves
        .iter()
        .take(3)
        .map(|wave| {
            format!(
                "wave {}: {}",
                wave.wave,
                wave.slots
                    .iter()
                    .map(|slot| format!("T{}={}", slot.thread, slot.task_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if plan.waves.len() <= 3 {
        preview
    } else {
        format!("{preview}\n... {} more wave(s)", plan.waves.len() - 3)
    }
}

#[cfg(test)]
fn next_ready_bucket_wave(
    bucket_plan: &StoredBucketPlan,
    current_tasks: &[LoopTask],
) -> Option<Vec<BucketSlot>> {
    let statuses = current_tasks
        .iter()
        .map(|task| (task.id.as_str(), task.status))
        .collect::<BTreeMap<_, _>>();
    for wave in &bucket_plan.waves {
        let slots = wave
            .slots
            .iter()
            .filter(|slot| statuses.get(slot.task_id.as_str()) == Some(&LoopTaskStatus::Pending))
            .filter(|slot| {
                slot.dependencies.iter().all(|dep| {
                    !matches!(
                        statuses.get(dep.as_str()),
                        Some(LoopTaskStatus::Pending | LoopTaskStatus::Blocked)
                    )
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if !slots.is_empty() {
            return Some(slots);
        }
    }
    None
}

fn prepare_parallel_worker(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    fingerprint: &str,
    task: &LoopTask,
) -> Result<ParallelWorker> {
    let worktree_root = run_root.join("worktrees");
    fs::create_dir_all(&worktree_root)
        .with_context(|| format!("failed to create {}", worktree_root.display()))?;
    let short_fingerprint = &fingerprint[..fingerprint.len().min(8)];
    let branch = format!(
        "auto/{}-{short_fingerprint}",
        sanitize_branch_component(&task.id)
    );
    let worktree = worktree_root.join(format!(
        "{}-{short_fingerprint}",
        sanitize_branch_component(&task.id)
    ));
    if worktree.exists() {
        let current_branch = git_stdout(&worktree, ["branch", "--show-current"])
            .unwrap_or_default()
            .trim()
            .to_string();
        if current_branch != branch {
            bail!(
                "existing worker worktree {} is on `{}` but `{}` was expected",
                worktree.display(),
                current_branch,
                branch
            );
        }
        return Ok(ParallelWorker { branch, worktree });
    }
    if git_ref_exists(repo_root, &format!("refs/heads/{branch}")) {
        run_git_owned(
            repo_root,
            vec![
                "worktree".to_string(),
                "add".to_string(),
                worktree.display().to_string(),
                branch.clone(),
            ],
        )?;
    } else {
        run_git_owned(
            repo_root,
            vec![
                "worktree".to_string(),
                "add".to_string(),
                "-b".to_string(),
                branch.clone(),
                worktree.display().to_string(),
                target_branch.to_string(),
            ],
        )?;
    }
    Ok(ParallelWorker { branch, worktree })
}

fn build_tmux_session_name(repo_root: &Path, fingerprint: &str) -> String {
    let short_fingerprint = &fingerprint[..fingerprint.len().min(8)];
    format!(
        "auto-{}-{short_fingerprint}",
        sanitize_branch_component(&repo_name(repo_root))
    )
}

fn sanitize_branch_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn build_parallel_worker_prompt(
    prompt_template: &str,
    task: &LoopTask,
    worker_branch: &str,
) -> String {
    format!(
        r#"{prompt_template}

Parallel auto-loop worker override:
- You are assigned exactly one task: `{task_id}`.
- Ignore generic instructions that say to choose the first pending task. Do not implement any other task.
- You are running on isolated worker branch `{worker_branch}`. Do not push.
- Do not edit `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `COMPLETED.md`, `WORKLIST.md`, or `ARCHIVED.md` in this worker. The coordinator updates shared planning artifacts after merging.
- If the assigned task is complete, commit only the implementation/docs/tests needed for `{task_id}`.
- Before finishing, write `.auto/parallel/handoffs/{task_id}.md` in this worktree with: task id, changed surfaces, validation commands and results, known failures, and the final commit sha if available. Do not stage that handoff file.
- If blocked, make a local commit only if it contains useful code/docs; otherwise leave the worktree clean and explain the blocker in the handoff file.

Assigned task contract:

{task_markdown}

Execute only `{task_id}` now."#,
        task_id = task.id,
        task_markdown = task.markdown,
        worker_branch = worker_branch,
    )
}

fn integrate_parallel_worker_result(
    repo_root: &Path,
    target_branch: &str,
    reference_repos: &[PathBuf],
    task: &LoopTask,
    worker: &ParallelWorker,
) -> Result<()> {
    let status = git_stdout(&worker.worktree, ["status", "--short"])?;
    let non_auto_status = status
        .lines()
        .filter(|line| !line.contains(" .auto/") && !line.contains("?? .auto/"))
        .collect::<Vec<_>>();
    if !non_auto_status.is_empty() {
        bail!(
            "parallel worker `{}` left uncommitted changes in {}; commit or clean them before integration:\n{}",
            task.id,
            worker.worktree.display(),
            non_auto_status.join("\n")
        );
    }

    let before = git_stdout(repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let worker_head = git_stdout(&worker.worktree, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    if before == worker_head {
        bail!(
            "parallel worker `{}` exited successfully but produced no new commit; worktree left at {}",
            task.id,
            worker.worktree.display()
        );
    }

    merge_worker_branch(repo_root, worker).with_context(|| {
        format!(
            "failed to merge worker branch `{}` from {}",
            worker.branch,
            worker.worktree.display()
        )
    })?;
    let after = git_stdout(repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let changed_files = git_stdout(repo_root, ["diff", "--name-only", &before, &after])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    apply_parallel_handoff(repo_root, task, worker, &after, &changed_files)?;

    if let Some(commit) =
        auto_checkpoint_if_needed(repo_root, target_branch, "auto loop parallel handoff")?
    {
        println!("checkpoint:  committed parallel handoff at {commit}");
    }
    let states = collect_tracked_repo_states(repo_root, reference_repos)?;
    let dirty = states
        .iter()
        .filter(|state| !state.status.trim().is_empty())
        .map(|state| state.name.clone())
        .collect::<Vec<_>>();
    if !dirty.is_empty() {
        bail!(
            "tracked repo changes remain after integrating `{}`: {}",
            task.id,
            dirty.join(", ")
        );
    }
    remove_worker_worktree(repo_root, &worker.worktree)?;
    Ok(())
}

fn apply_parallel_handoff(
    repo_root: &Path,
    task: &LoopTask,
    worker: &ParallelWorker,
    commit: &str,
    changed_files: &[String],
) -> Result<()> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    let plan = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let updated_plan = remove_task_block_from_plan(&plan, &task.id);
    if updated_plan != plan {
        atomic_write(&plan_path, updated_plan.as_bytes())?;
    }

    let handoff_path = worker
        .worktree
        .join(".auto")
        .join("parallel")
        .join("handoffs")
        .join(format!("{}.md", task.id));
    let handoff = fs::read_to_string(&handoff_path).unwrap_or_else(|_| {
        "Worker did not write a handoff file; using coordinator-generated summary.".to_string()
    });
    let changed = if changed_files.is_empty() {
        "none reported".to_string()
    } else {
        changed_files.join(", ")
    };
    let entry = format!(
        "\n## `{}` Parallel Implementation Handoff\n\n- Worker branch: `{}`\n- Commit: `{}`\n- Changed surfaces: {}\n\n{}\n",
        task.id,
        worker.branch,
        commit,
        changed,
        handoff.trim()
    );
    append_to_file(&repo_root.join("REVIEW.md"), &entry)?;
    Ok(())
}

fn remove_task_block_from_plan(plan: &str, task_id: &str) -> String {
    let mut output = String::new();
    let mut skipping = false;
    for line in plan.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let starts_task = trimmed.starts_with("- [ ] ")
            || trimmed.starts_with("- [!] ")
            || trimmed.starts_with("- [x] ")
            || trimmed.starts_with("- [X] ");
        if starts_task {
            skipping = parse_loop_task_header(trimmed).is_some_and(|(_, id, _)| id == task_id);
            if skipping {
                continue;
            }
        } else if skipping && trimmed.starts_with("## ") {
            skipping = false;
        }
        if !skipping {
            output.push_str(line);
        }
    }
    output
}

fn append_to_file(path: &Path, addition: &str) -> Result<()> {
    let mut existing = if path.exists() {
        fs::read(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        Vec::new()
    };
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        existing.push(b'\n');
    }
    existing.extend_from_slice(addition.as_bytes());
    atomic_write(path, &existing)
}

fn remove_worker_worktree(repo_root: &Path, worktree: &Path) -> Result<()> {
    run_git_owned(
        repo_root,
        vec![
            "worktree".to_string(),
            "remove".to_string(),
            "--force".to_string(),
            worktree.display().to_string(),
        ],
    )
}

fn merge_worker_branch(repo_root: &Path, worker: &ParallelWorker) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge", "--no-ff", "--no-edit", &worker.branch])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }

    let _ = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge", "--abort"])
        .output();
    bail!(
        "git merge failed and an abort was attempted: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn run_git_owned(repo_root: &Path, args: Vec<String>) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(&args)
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

fn render_default_loop_prompt(branch: &str, reference_repos: &[PathBuf]) -> String {
    append_reference_repo_clause(
        DEFAULT_LOOP_PROMPT_TEMPLATE.replace("{branch}", branch),
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

impl TrackedRepoState {
    #[cfg(test)]
    fn new(name: &str, path: &str, head: &str, status: &str) -> Self {
        Self {
            name: name.to_string(),
            path: PathBuf::from(path),
            head: head.to_string(),
            status: status.to_string(),
        }
    }
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
        "auto loop could not resolve the repo's primary branch; pass `--branch <name>` explicitly"
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
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        bucket_plan_reusable, build_bucket_plan, build_iteration_prompt,
        build_parallel_worker_prompt, build_tmux_session_name, default_cargo_build_jobs_for,
        discover_sibling_git_repos, new_loop_run_state, next_ready_bucket_slot_for_lane,
        next_ready_bucket_wave, parse_loop_queue, parse_loop_tasks, parse_origin_head_branch,
        pick_loop_branch, remove_task_block_from_plan, render_default_loop_prompt,
        repo_forbids_legacy_review_trackers, resolve_loop_worker_env, resolve_reference_repos,
        summarize_repo_progress, write_loop_run_state, LoopQueueSnapshot, LoopTaskStatus,
        RepoProgress, TrackedRepoState,
    };

    #[test]
    fn default_prompt_uses_resolved_branch() {
        let prompt = render_default_loop_prompt("trunk", &[]);
        assert!(prompt.contains("origin/trunk"));
        assert!(prompt.contains("branch `trunk`"));
        assert!(!prompt.contains("origin/main"));
        assert!(prompt.contains("Read only the minimum `AGENTS.md` content needed"));
        assert!(prompt.contains("RED/GREEN/REFACTOR"));
        assert!(prompt.contains("failing test"));
        assert!(prompt.contains("simplification pass"));
        assert!(prompt.contains("newest spec referenced by the current unchecked task"));
        assert!(prompt.contains("historical context only"));
        assert!(prompt.contains("Treat tasks marked `- [!]` as blocked"));
        assert!(prompt.contains("next pending `- [ ]` task"));
        assert!(prompt.contains("Do not default to workspace-wide or package-wide validation suites"));
    }

    #[test]
    fn default_prompt_lists_reference_repos_when_declared() {
        let prompt =
            render_default_loop_prompt("main", &[PathBuf::from("/home/r/coding/robopokermulti")]);
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
        assert_eq!(default_cargo_build_jobs_for(5, 22), 3);
        assert_eq!(default_cargo_build_jobs_for(3, 22), 4);
        assert_eq!(default_cargo_build_jobs_for(1, 22), 4);
        assert_eq!(default_cargo_build_jobs_for(20, 22), 1);
    }

    #[test]
    fn loop_worker_env_respects_override_and_inherited_cargo_jobs() {
        let inherited = resolve_loop_worker_env(5, None, Some("8"), 22).unwrap();
        assert!(inherited.extra_env.is_empty());
        assert_eq!(inherited.cargo_jobs_summary, "inherited CARGO_BUILD_JOBS=8");

        let overridden = resolve_loop_worker_env(5, Some(3), Some("8"), 22).unwrap();
        assert_eq!(
            overridden.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert_eq!(overridden.cargo_jobs_summary, "override CARGO_BUILD_JOBS=3");

        let automatic = resolve_loop_worker_env(5, None, None, 22).unwrap();
        assert_eq!(
            automatic.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert_eq!(automatic.cargo_jobs_summary, "auto CARGO_BUILD_JOBS=3");
    }

    #[test]
    fn loop_worker_env_rejects_zero_cargo_jobs_override() {
        let err = resolve_loop_worker_env(5, Some(0), None, 22).unwrap_err();
        assert!(err.to_string().contains("--cargo-build-jobs"));
    }

    #[test]
    fn tmux_session_name_is_repo_scoped_and_stable() {
        let session =
            build_tmux_session_name(PathBuf::from("/tmp/my repo").as_path(), "abcdef1234");
        assert_eq!(session, "auto-my-repo-abcdef12");
    }

    #[test]
    fn branch_picker_prefers_explicit_branch() {
        let branch =
            pick_loop_branch(Some("release"), "main", Some("origin/trunk"), &["trunk"]).unwrap();
        assert_eq!(branch, "release");
    }

    #[test]
    fn branch_picker_uses_origin_head_when_available() {
        let branch = pick_loop_branch(None, "feature/test", Some("origin/master"), &[]).unwrap();
        assert_eq!(branch, "master");
    }

    #[test]
    fn branch_picker_prefers_current_primary_branch_over_origin_head() {
        let branch =
            pick_loop_branch(None, "main", Some("origin/master"), &["main", "master"]).unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn branch_picker_falls_back_to_current_primary_branch() {
        let branch = pick_loop_branch(None, "trunk", None, &[]).unwrap();
        assert_eq!(branch, "trunk");
    }

    #[test]
    fn branch_picker_falls_back_to_known_available_branch() {
        let branch = pick_loop_branch(None, "feature/test", None, &["master"]).unwrap();
        assert_eq!(branch, "master");
    }

    #[test]
    fn parses_origin_head_branch() {
        assert_eq!(
            parse_origin_head_branch("origin/trunk"),
            Some("trunk".to_string())
        );
    }

    #[test]
    fn repo_progress_detects_reference_repo_commit() {
        let before = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb111", ""),
        ];
        let after = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb222", ""),
        ];

        let progress = summarize_repo_progress(&before, &after);
        assert_eq!(progress, RepoProgress::NewCommits);
    }

    #[test]
    fn repo_progress_flags_dirty_reference_repo_without_commit() {
        let before = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb111", ""),
        ];
        let after = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new(
                "robopokermulti",
                "/tmp/robopokermulti",
                "bbb111",
                " M src/lib.rs",
            ),
        ];

        let progress = summarize_repo_progress(&before, &after);
        assert_eq!(
            progress,
            RepoProgress::DirtyChanges(vec!["robopokermulti".to_string()])
        );
    }

    #[test]
    fn parse_loop_queue_separates_pending_and_blocked_tasks() {
        let queue = parse_loop_queue(
            r#"
- [!] `DEC-001` Choose project license
- [ ] `META-001` Add LICENSE file and Cargo license metadata
- [x] `DONE-001` Finished already
- [ ] `GATE-P4` Phase 4 checkpoint
"#,
        );

        assert_eq!(
            queue,
            LoopQueueSnapshot {
                pending_ids: vec!["META-001".to_string(), "GATE-P4".to_string()],
                blocked_ids: vec!["DEC-001".to_string()],
            }
        );
    }

    #[test]
    fn parse_loop_tasks_extracts_dependencies_and_owned_paths() {
        let tasks = parse_loop_tasks(
            r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `P-001` Foundation

  Spec: `specs/a.md`
  Owns: `src/foundation.rs`; `crates/core/`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-002` Dependent

  Spec: `specs/b.md`
  Owns: `src/dependent.rs`
  Integration touchpoints: none
  Dependencies: `P-001`, `P-999`.
"#,
        );

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "P-001");
        assert_eq!(tasks[0].status, LoopTaskStatus::Pending);
        assert_eq!(
            tasks[0].owned_paths,
            vec!["crates/core".to_string(), "src/foundation.rs".to_string()]
        );
        assert_eq!(
            tasks[1].dependencies,
            vec!["P-001".to_string(), "P-999".to_string()]
        );
    }

    #[test]
    fn owned_path_parser_stops_before_verification_and_uses_integration_paths() {
        let tasks = parse_loop_tasks(
            r#"- [ ] `P-017` Scaffold TUI
  Owns: New crate `observatory-tui` with pane manager.
  Integration touchpoints: `observatory-tui/`, `Cargo.toml`
  Scope boundary: shell only
  Verification:
    scripts/check-harness-engineering-standards.sh
    rg -n "P-017" IMPLEMENTATION_PLAN.md
  Dependencies: none
"#,
        );

        assert_eq!(
            tasks[0].owned_paths,
            vec!["Cargo.toml".to_string(), "observatory-tui".to_string()]
        );
    }

    #[test]
    fn bucket_plan_respects_dependencies_and_owned_path_conflicts() {
        let tasks = parse_loop_tasks(
            r#"- [ ] `P-001` First
  Owns: `src/a.rs`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-002` Conflicts with first
  Owns: `src/a.rs`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-003` Independent
  Owns: `src/b.rs`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-004` Depends on first
  Owns: `src/c.rs`
  Integration touchpoints: none
  Dependencies: `P-001`
"#,
        );

        let plan = build_bucket_plan("main", 2, "abc", &tasks);

        assert_eq!(plan.waves[0].slots[0].task_id, "P-001");
        assert_eq!(plan.waves[0].slots[1].task_id, "P-003");
        assert_eq!(plan.waves[1].slots[0].task_id, "P-002");
        assert_eq!(plan.waves[1].slots[1].task_id, "P-004");
    }

    #[test]
    fn bucket_plan_serializes_tasks_without_owned_paths() {
        let tasks = parse_loop_tasks(
            r#"- [ ] `P-001` First vague task
  Owns: narrative only
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-002` Second vague task
  Owns: another narrative
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-003` Concrete task
  Owns: `src/c.rs`
  Integration touchpoints: none
  Dependencies: none
"#,
        );

        let plan = build_bucket_plan("main", 3, "abc", &tasks);

        assert_eq!(plan.waves[0].slots[0].task_id, "P-001");
        assert_eq!(plan.waves[0].slots[1].task_id, "P-003");
        assert_eq!(plan.waves[1].slots[0].task_id, "P-002");
    }

    #[test]
    fn next_ready_bucket_wave_reuses_persisted_bucket_order() {
        let tasks = parse_loop_tasks(
            r#"- [x] `P-001` First
  Owns: `src/a.rs`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-002` Second
  Owns: `src/b.rs`
  Integration touchpoints: none
  Dependencies: `P-001`
"#,
        );
        let plan = build_bucket_plan("main", 2, "abc", &tasks);
        let wave = next_ready_bucket_wave(&plan, &tasks).expect("second task should be ready");

        assert_eq!(wave.len(), 1);
        assert_eq!(wave[0].task_id, "P-002");
    }

    #[test]
    fn lane_ready_picker_replenishes_idle_lane_without_waiting_for_other_lanes() {
        let original_tasks = parse_loop_tasks(
            r#"- [ ] `P-001` First
  Owns: `src/a.rs`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-002` Second
  Owns: `src/b.rs`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-003` First followup
  Owns: `src/c.rs`
  Integration touchpoints: none
  Dependencies: `P-001`

- [ ] `P-004` Second followup
  Owns: `src/d.rs`
  Integration touchpoints: none
  Dependencies: `P-002`
"#,
        );
        let bucket_plan = build_bucket_plan("main", 2, "abc", &original_tasks);
        let current_tasks = parse_loop_tasks(
            r#"- [x] `P-001` First
  Owns: `src/a.rs`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-002` Second
  Owns: `src/b.rs`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-003` First followup
  Owns: `src/c.rs`
  Integration touchpoints: none
  Dependencies: `P-001`

- [ ] `P-004` Second followup
  Owns: `src/d.rs`
  Integration touchpoints: none
  Dependencies: `P-002`
"#,
        );
        let active_task_ids = BTreeSet::from(["P-002".to_string()]);
        let ready = next_ready_bucket_slot_for_lane(
            &bucket_plan,
            &current_tasks,
            1,
            &active_task_ids,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &["src/b.rs".to_string()],
        )
        .expect("lane 1 should replenish from its next ready bucket slot");

        assert_eq!(ready.task_id, "P-003");
        assert_eq!(ready.thread, 1);
    }

    #[test]
    fn loop_run_state_serializes_lane_snapshot() {
        let temp = unique_temp_dir("loop-run-state");
        fs::create_dir_all(&temp).expect("failed to create temp dir");
        let state_path = temp.join("state.json");
        let mut state = new_loop_run_state("main", "abc123", 2, "auto-test", &temp.join("b.md"));

        write_loop_run_state(&state_path, &mut state).expect("state should write");
        let text = fs::read_to_string(&state_path).expect("state should be readable");

        assert!(text.contains("\"version\": 1"));
        assert!(text.contains("\"tmux_session\": \"auto-test\""));
        assert!(text.contains("\"status\": \"idle\""));
        assert_eq!(state.lanes.len(), 2);

        fs::remove_dir_all(&temp).expect("failed to remove temp dir");
    }

    #[test]
    fn bucket_plan_reuse_survives_completed_task_removal() {
        let original_tasks = parse_loop_tasks(
            r#"- [ ] `P-001` First
  Owns: `src/a.rs`
  Integration touchpoints: none
  Dependencies: none

- [ ] `P-002` Second
  Owns: `src/b.rs`
  Integration touchpoints: none
  Dependencies: `P-001`
"#,
        );
        let stored = build_bucket_plan("main", 2, "old", &original_tasks);
        let resumed_tasks = parse_loop_tasks(
            r#"- [ ] `P-002` Second
  Owns: `src/b.rs`
  Integration touchpoints: none
  Dependencies: `P-001`
"#,
        );

        assert!(bucket_plan_reusable(
            &stored,
            "main",
            2,
            "new",
            &resumed_tasks
        ));
    }

    #[test]
    fn bucket_plan_reuse_rejects_older_bucket_versions() {
        let tasks = parse_loop_tasks(
            r#"- [ ] `P-001` First
  Owns: `src/a.rs`
  Integration touchpoints: none
  Dependencies: none
"#,
        );
        let mut stored = build_bucket_plan("main", 2, "abc", &tasks);
        stored.version = 0;

        assert!(!bucket_plan_reusable(&stored, "main", 2, "abc", &tasks));
    }

    #[test]
    fn parallel_worker_prompt_overrides_shared_plan_edits() {
        let task = parse_loop_tasks(
            r#"- [ ] `P-001` First
  Owns: `src/a.rs`
  Integration touchpoints: none
  Dependencies: none
"#,
        )
        .remove(0);
        let prompt = build_parallel_worker_prompt("base prompt", &task, "auto/P-001-test");

        assert!(prompt.contains("assigned exactly one task: `P-001`"));
        assert!(prompt.contains("Do not push"));
        assert!(prompt.contains("Do not edit `IMPLEMENTATION_PLAN.md`, `REVIEW.md`"));
        assert!(prompt.contains(".auto/parallel/handoffs/P-001.md"));
    }

    #[test]
    fn remove_task_block_removes_only_assigned_task() {
        let plan = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `P-001` First
  Dependencies: none

- [ ] `P-002` Second
  Dependencies: none

## Completed / Already Satisfied
"#;

        let updated = remove_task_block_from_plan(plan, "P-001");

        assert!(!updated.contains("`P-001`"));
        assert!(updated.contains("`P-002`"));
        assert!(updated.contains("## Completed / Already Satisfied"));
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
