mod branch;
mod prompt;
mod queue;
mod refs;
#[cfg(test)]
mod testkit;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::claude_exec::{describe_claude_harness, run_claude_exec, FUTILITY_EXIT_MARKER};
use crate::codex_exec::run_codex_exec;
use crate::loop_command::branch::resolve_loop_branch;
use crate::loop_command::prompt::{
    append_reference_repo_clause, build_iteration_prompt, render_default_loop_prompt,
};
pub(crate) use crate::loop_command::prompt::DEFAULT_LOOP_PROMPT_TEMPLATE;
use crate::loop_command::queue::{inspect_loop_queue, reconcile_loop_task_completion_evidence};
use crate::loop_command::refs::resolve_reference_repos;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, repo_name, run_git, sync_branch_with_remote, timestamp_slug,
};
use crate::LoopArgs;

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

    let prompt_template = match &args.prompt_file {
        Some(path) => {
            let prompt = fs::read_to_string(path)
                .with_context(|| format!("failed to read prompt file {}", path.display()))?;
            append_reference_repo_clause(prompt, &reference_repos)
        }
        None => render_default_loop_prompt(&target_branch, &reference_repos),
    };
    let run_root = args
        .run_root
        .unwrap_or_else(|| repo_root.join(".auto").join("loop"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let stderr_log_path = run_root.join("codex.stderr.log");

    let harness = if args.claude { "Claude" } else { "Codex" };

    println!("auto loop");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", target_branch);
    if args.claude {
        println!(
            "harness:     {}",
            describe_claude_harness(&args.model, &args.reasoning_effort)
        );
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

    if sync_branch_with_remote(&repo_root, target_branch.as_str())? {
        println!("remote sync: rebased onto origin/{}", target_branch);
    }

    if let Some(commit) =
        auto_checkpoint_if_needed(&repo_root, target_branch.as_str(), "auto loop checkpoint")?
    {
        println!("checkpoint:  committed pre-existing changes at {commit}");
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
                println!("no unfinished `- [ ]` / `- [~]` tasks remain; stopping.");
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
            run_claude_exec(
                &repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                args.max_turns,
                &stderr_log_path,
                None,
                "auto loop",
            )
            .await?
        } else {
            run_codex_exec(
                &repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                &args.codex_bin,
                &stderr_log_path,
                None,
                "auto loop",
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

        if let Some(reconciliation) =
            reconcile_loop_task_completion_evidence(&repo_root, current_task.as_str())?
        {
            if reconciliation.plan_updated {
                run_git(&repo_root, ["add", "IMPLEMENTATION_PLAN.md"])?;
                let message = format!(
                    "{}: {} completion evidence sync",
                    repo_name(&repo_root),
                    current_task
                );
                run_git(&repo_root, ["commit", "-m", &message])?;
                println!(
                    "completion: {} left `[~]` ({})",
                    reconciliation.task_id,
                    reconciliation.missing_reasons.join("; ")
                );
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

#[cfg(test)]
mod tests {
    use super::{summarize_repo_progress, RepoProgress, TrackedRepoState};

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
}
