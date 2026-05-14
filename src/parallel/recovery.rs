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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SupersededLaneRecovery {
    cherry_pick_head: String,
    superseding_commit: String,
}

impl SupersededLaneRecovery {
    fn summary(&self) -> String {
        if self.cherry_pick_head == self.superseding_commit {
            format!(
                "cherry-pick commit {} is already reachable from canonical HEAD",
                short_commit(&self.cherry_pick_head)
            )
        } else {
            format!(
                "cherry-pick commit {} is superseded by canonical task commit {}",
                short_commit(&self.cherry_pick_head),
                short_commit(&self.superseding_commit)
            )
        }
    }
}

fn short_commit(commit: &str) -> String {
    commit.chars().take(10).collect()
}

fn superseded_lane_cherry_pick_recovery(
    repo_root: &Path,
    lane_repo_root: &Path,
    task_id: &str,
) -> Result<Option<SupersededLaneRecovery>> {
    if !lane_repo_has_active_cherry_pick(lane_repo_root) || task_id.trim().is_empty() {
        return Ok(None);
    }
    let cherry_pick_head = git_stdout(lane_repo_root, ["rev-parse", "CHERRY_PICK_HEAD"])?;
    let cherry_pick_head = cherry_pick_head.trim().to_string();
    if cherry_pick_head.is_empty() {
        return Ok(None);
    }
    let cherry_message = git_stdout(
        lane_repo_root,
        ["show", "-s", "--format=%B", "CHERRY_PICK_HEAD"],
    )?;
    if !cherry_message.contains(task_id) {
        return Ok(None);
    }

    if git_commit_exists(repo_root, &cherry_pick_head)
        && git_ref_is_ancestor(repo_root, &cherry_pick_head, "HEAD")?
    {
        return Ok(Some(SupersededLaneRecovery {
            cherry_pick_head: cherry_pick_head.clone(),
            superseding_commit: cherry_pick_head,
        }));
    }

    let lane_recovery_base = git_stdout(lane_repo_root, ["rev-parse", "HEAD"])?;
    let lane_recovery_base = lane_recovery_base.trim().to_string();
    if lane_recovery_base.is_empty() {
        return Ok(None);
    }
    let Some(superseding_commit) =
        latest_canonical_task_commit_not_reachable_from(repo_root, task_id, &lane_recovery_base)?
    else {
        return Ok(None);
    };
    if superseding_commit == cherry_pick_head {
        return Ok(None);
    }
    Ok(Some(SupersededLaneRecovery {
        cherry_pick_head,
        superseding_commit,
    }))
}

fn retire_superseded_lane_cherry_pick_recovery(
    repo_root: &Path,
    lane_repo_root: &Path,
    task_id: &str,
) -> Result<Option<SupersededLaneRecovery>> {
    let Some(superseded) =
        superseded_lane_cherry_pick_recovery(repo_root, lane_repo_root, task_id)?
    else {
        return Ok(None);
    };
    run_git(lane_repo_root, ["cherry-pick", "--abort"])?;
    let status = git_stdout(lane_repo_root, ["status", "--short"])?;
    if !status.trim().is_empty() {
        bail!(
            "retired superseded cherry-pick recovery in {} but lane is still dirty:\n{}",
            lane_repo_root.display(),
            status.trim()
        );
    }
    Ok(Some(superseded))
}

fn git_commit_exists(repo_root: &Path, commit: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn latest_canonical_task_commit_not_reachable_from(
    repo_root: &Path,
    task_id: &str,
    base_ref: &str,
) -> Result<Option<String>> {
    let commits = git_stdout(
        repo_root,
        [
            "log",
            "--fixed-strings",
            "--format=%H",
            "--grep",
            task_id,
            "-n",
            "20",
            "HEAD",
        ],
    )?;
    for commit in commits
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !git_ref_is_ancestor(repo_root, commit, base_ref)? {
            return Ok(Some(commit.to_string()));
        }
    }
    Ok(None)
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

fn repair_parallel_canonical_before_dispatch(
    repo_root: &Path,
    target_branch: &str,
    parallel_logger: &ParallelEventLogger,
) -> Result<()> {
    let rebase_merge = git_path(repo_root, "rebase-merge");
    if let Some(path) = rebase_merge.as_ref().filter(|path| path.exists()) {
        let issue = lane_repo_rebase_recovery_issue(repo_root);
        if issue.is_some() {
            let _ = run_git(repo_root, ["rebase", "--abort"]);
            if path.exists() {
                fs::remove_dir_all(path)
                    .with_context(|| format!("failed to remove stale {}", path.display()))?;
            }
            parallel_logger.warn(format!(
                "repair: removed stale canonical rebase metadata before dispatch ({})",
                issue.unwrap_or_else(|| "rebase-merge".to_string())
            ));
        } else {
            bail!(
                "canonical repo has active rebase metadata at {}; resolve it before dispatch",
                path.display()
            );
        }
    }
    repair_stale_git_index_lock(repo_root, parallel_logger, "before dispatch")?;
    let dirty = git_stdout(repo_root, ["status", "--short", "--untracked-files=all"])?;
    let dirty_paths = dirty
        .lines()
        .filter_map(parse_parallel_status_path)
        .filter(|path| !parallel_dispatch_path_is_ignored(path))
        .collect::<Vec<_>>();
    if !dirty_paths.is_empty() {
        let dirty_summary = dirty_paths.join(", ");
        if let Some(commit) = checkpoint_parallel_dispatch_paths(
            repo_root,
            target_branch,
            &dirty_paths,
            "auto parallel checkpoint",
        )? {
            parallel_logger.info(format!(
                "checkpoint: committed dirty canonical dispatch paths at {commit} before dispatch ({dirty_summary})"
            ));
        }

        let remaining_dirty =
            git_stdout(repo_root, ["status", "--short", "--untracked-files=all"])?;
        let remaining_dirty_paths = remaining_dirty
            .lines()
            .filter_map(parse_parallel_status_path)
            .filter(|path| !parallel_dispatch_path_is_ignored(path))
            .collect::<Vec<_>>();
        if !remaining_dirty_paths.is_empty() {
            bail!(
                "canonical repo has dirty tracked dispatch paths before auto parallel dispatch and automatic checkpointing did not clear them: {}. Commit, stash, or revert them before launching lanes",
                remaining_dirty_paths.join(", ")
            );
        }
    }
    Ok(())
}

fn repair_stale_git_index_lock(
    repo_root: &Path,
    parallel_logger: &ParallelEventLogger,
    context: &str,
) -> Result<()> {
    let Some(path) = git_path(repo_root, "index.lock").filter(|path| path.exists()) else {
        return Ok(());
    };
    let metadata =
        fs::metadata(&path).with_context(|| format!("failed to stat {}", path.display()))?;
    let age = metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok());
    let safe_to_remove =
        metadata.len() == 0 && age.is_some_and(|age| age >= STALE_GIT_INDEX_LOCK_GRACE);
    if !safe_to_remove {
        bail!(
            "canonical repo has an active git index lock at {}; size={} age_secs={} context={}; remove it only after confirming no git process is using it",
            path.display(),
            metadata.len(),
            age.map(|age| age.as_secs().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            context,
        );
    }
    fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    parallel_logger.warn(format!(
        "repair: removed stale canonical git index lock {} ({context})",
        path.display()
    ));
    Ok(())
}

fn checkpoint_parallel_dispatch_paths(
    repo_root: &Path,
    target_branch: &str,
    dirty_paths: &[String],
    message_suffix: &str,
) -> Result<Option<String>> {
    if dirty_paths.is_empty() {
        return Ok(None);
    }
    let current_branch = git_stdout(repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim();
    if current_branch.is_empty() {
        bail!("refusing to checkpoint dirty dispatch paths from detached HEAD");
    }
    if current_branch != target_branch {
        bail!(
            "refusing to checkpoint branch `{target_branch}` while checked out on `{current_branch}`; checkout `{target_branch}` or pass the current branch explicitly"
        );
    }
    let mut add_args = vec!["add".to_string(), "--all".to_string(), "--".to_string()];
    add_args.extend(dirty_paths.iter().cloned());
    run_git(repo_root, add_args.iter().map(|arg| arg.as_str()))?;
    let staged = git_stdout(repo_root, ["diff", "--cached", "--name-only"])?;
    if staged.trim().is_empty() {
        return Ok(None);
    }
    let message = format!("{}: {message_suffix}", repo_name(repo_root));
    run_git(repo_root, ["commit", "-m", &message])?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    let commit = commit.trim().to_string();
    if let Err(err) = push_branch_with_remote_sync(repo_root, target_branch) {
        bail!(
            "created checkpoint commit {} but failed to sync/push: {err}",
            commit
        );
    }
    Ok(Some(commit))
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

