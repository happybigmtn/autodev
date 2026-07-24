use super::*;

pub(crate) fn landing_recovery_note(branch: &str, error: &str) -> String {
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

pub(crate) fn prepared_landing_recovery_note(
    branch: &str,
    error: &str,
    repair_error: &str,
) -> String {
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

pub(crate) fn resumed_landing_recovery_note(branch: &str, status: &str) -> String {
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

pub(crate) fn dirty_worktree_recovery_note(status: &str) -> String {
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

pub(crate) fn lane_repo_recovery_note(lane_repo_root: &Path, branch: &str, status: &str) -> String {
    if let Some(issue) = lane_repo_rebase_recovery_issue(lane_repo_root) {
        stale_rebase_recovery_note(branch, status, &issue)
    } else if lane_repo_has_active_cherry_pick(lane_repo_root) {
        resumed_landing_recovery_note(branch, status)
    } else {
        dirty_worktree_recovery_note(status)
    }
}

pub(crate) fn lane_repo_has_rebase_recovery(lane_repo_root: &Path) -> bool {
    lane_repo_rebase_recovery_issue(lane_repo_root).is_some()
}

pub(crate) fn lane_repo_rebase_recovery_issue(lane_repo_root: &Path) -> Option<String> {
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

pub(crate) fn stale_rebase_recovery_note(branch: &str, status: &str, issue: &str) -> String {
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

pub(crate) fn lane_repo_has_active_cherry_pick(lane_repo_root: &Path) -> bool {
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
pub(crate) struct SupersededLaneRecovery {
    pub(crate) cherry_pick_head: String,
    pub(crate) superseding_commit: String,
}

impl SupersededLaneRecovery {
    pub(crate) fn summary(&self) -> String {
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

pub(crate) fn short_commit(commit: &str) -> String {
    commit.chars().take(10).collect()
}

pub(crate) fn superseded_lane_cherry_pick_recovery(
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

pub(crate) fn retire_superseded_lane_cherry_pick_recovery(
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

pub(crate) fn latest_canonical_task_commit_not_reachable_from(
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

pub(crate) fn environment_blocker_recovery_note(reason: &str, preflight_report: &str) -> String {
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

pub(crate) fn write_parallel_salvage_record(
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

pub(crate) fn parallel_salvage_record_path(
    lane_root: &Path,
    task_id: &str,
    lane_index: usize,
) -> PathBuf {
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

pub(crate) fn salvage_recovery_note(
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

pub(crate) fn sanitize_salvage_filename(raw: &str) -> String {
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

pub(crate) fn detect_lane_environment_blocker(assignment: &ActiveLaneAssignment) -> Option<String> {
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

pub(crate) fn environment_blocker_reason(log_text: &str) -> Option<String> {
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

pub(crate) fn read_recent_log_text(path: &Path, max_lines: usize) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut lines = content.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    Ok(lines.join("\n"))
}

pub(crate) fn prepend_host_recovery_note(assignment: &mut ActiveLaneAssignment, note: &str) {
    assignment.host_recovery_note = Some(match assignment.host_recovery_note.take() {
        Some(existing) if !existing.trim().is_empty() => {
            format!("{}\n\n{}", note.trim(), existing.trim())
        }
        _ => note.trim().to_string(),
    });
}

pub(crate) fn preserve_resume_recovery_notes(
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

/// Bounded failure context threaded into the prompt on a generic non-zero retry.
///
/// Without this, a refreshed-plan retry re-runs BLIND: the next model call sees
/// the same task with no record of why the last attempt failed, so it typically
/// repeats the same mistake. This surfaces the TERMINAL cause of the failed
/// attempt — the exit disposition, the lane repo's git state (what the attempt
/// did or did not commit), and the tails of the worker's own stdout/stderr (where
/// the failing command's output lives). Every section is individually bounded so
/// a runaway log can never bloat the next prompt.
pub(crate) fn retry_failure_recovery_note(
    lane_repo_root: &Path,
    stdout_log_path: &Path,
    stderr_log_path: &Path,
    exit_code: i32,
    is_futility: bool,
) -> String {
    const MAX_TAIL_LINES: usize = 40;
    const MAX_SECTION_CHARS: usize = 4000;

    let exit_desc = if is_futility {
        "the worker gave up in a futility spiral (repeated no-progress turns)".to_string()
    } else {
        format!("the worker exited non-zero (code {exit_code})")
    };

    let status = git_stdout(lane_repo_root, ["status", "--short"]).unwrap_or_default();
    let status = clamp_recovery_section(status.trim(), MAX_SECTION_CHARS);
    let status_block = if status.is_empty() {
        "clean (no uncommitted changes)".to_string()
    } else {
        status
    };

    let diffstat = git_stdout(lane_repo_root, ["diff", "--stat"]).unwrap_or_default();
    let diffstat = clamp_recovery_section(diffstat.trim(), MAX_SECTION_CHARS);
    let diffstat_block = if diffstat.is_empty() {
        "no unstaged changes".to_string()
    } else {
        diffstat
    };

    let stderr_tail = clamp_recovery_section(
        &recovery_log_tail(stderr_log_path, MAX_TAIL_LINES),
        MAX_SECTION_CHARS,
    );
    let stderr_block = if stderr_tail.trim().is_empty() {
        "(empty)".to_string()
    } else {
        stderr_tail
    };

    let stdout_tail = clamp_recovery_section(
        &recovery_log_tail(stdout_log_path, MAX_TAIL_LINES),
        MAX_SECTION_CHARS,
    );
    let stdout_block = if stdout_tail.trim().is_empty() {
        "(empty)".to_string()
    } else {
        stdout_tail
    };

    format!(
        r#"The previous attempt on this task FAILED: {exit_desc}. This is an automatic retry with a refreshed plan. Diagnose and fix the terminal cause shown below instead of repeating the same steps blindly.

Do not treat this as a fresh start: read the failure evidence first, form a hypothesis about why the last attempt failed, and change your approach accordingly. If the real blocker is missing external infrastructure or environment, print `AUTO_ENV_BLOCKER: <short reason>` before exiting non-zero rather than looping on the same failure.

Lane repo `git status --short` after the failed attempt:
{status_block}

Lane repo `git diff --stat` (unstaged) after the failed attempt:
{diffstat_block}

Last {MAX_TAIL_LINES} non-empty stderr lines from the failed attempt:
{stderr_block}

Last {MAX_TAIL_LINES} non-empty stdout lines from the failed attempt:
{stdout_block}"#
    )
}

fn recovery_log_tail(path: &Path, max_lines: usize) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn clamp_recovery_section(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let skip = count - max_chars;
    let tail: String = text.chars().skip(skip).collect();
    format!("[... truncated {skip} chars ...]\n{tail}")
}

#[cfg(test)]
mod tests {
    use crate::parallel_command::*;
    use std::time::UNIX_EPOCH;

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

    #[test]
    fn retry_failure_recovery_note_captures_exit_git_and_log_tails() {
        let dir = unique_temp_dir("retry-failure-note");
        let repo = dir.join("repo");
        fs::create_dir_all(&repo).expect("failed to create repo");
        run_git_in(&repo, ["init", "-q"]);
        run_git_in(&repo, ["config", "user.email", "test@example.com"]);
        run_git_in(&repo, ["config", "user.name", "Autodev Test"]);
        fs::write(repo.join("committed.txt"), "base\n").expect("write base");
        run_git_in(&repo, ["add", "committed.txt"]);
        run_git_in(&repo, ["commit", "-q", "-m", "base"]);
        // Leave the worktree dirty so status/diff have content to report.
        fs::write(repo.join("committed.txt"), "changed\n").expect("dirty file");

        let stdout_log = dir.join("stdout.log");
        let stderr_log = dir.join("stderr.log");
        fs::write(
            &stdout_log,
            "worker stdout line one\nworker stdout line two\n",
        )
        .expect("write stdout");
        fs::write(
            &stderr_log,
            "error[E0433]: failed to resolve: use of undeclared crate `foo`\n",
        )
        .expect("write stderr");

        let note = retry_failure_recovery_note(&repo, &stdout_log, &stderr_log, 101, false);
        assert!(note.contains("exited non-zero (code 101)"), "{note}");
        assert!(note.contains("M committed.txt"), "git status tail: {note}");
        assert!(note.contains("committed.txt"), "diff stat: {note}");
        assert!(note.contains("error[E0433]"), "stderr tail: {note}");
        assert!(
            note.contains("worker stdout line two"),
            "stdout tail: {note}"
        );

        // Futility disposition renders its own description.
        let futile = retry_failure_recovery_note(&repo, &stdout_log, &stderr_log, -9, true);
        assert!(futile.contains("futility spiral"), "{futile}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn retry_failure_recovery_note_bounds_runaway_logs() {
        let dir = unique_temp_dir("retry-failure-note-bounds");
        let repo = dir.join("repo");
        fs::create_dir_all(&repo).expect("failed to create repo");
        run_git_in(&repo, ["init", "-q"]);
        run_git_in(&repo, ["config", "user.email", "test@example.com"]);
        run_git_in(&repo, ["config", "user.name", "Autodev Test"]);

        let stdout_log = dir.join("stdout.log");
        let stderr_log = dir.join("stderr.log");
        // 500 distinct non-empty lines; only the last 40 should survive.
        let big = (0..500)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&stdout_log, &big).expect("write big stdout");
        fs::write(&stderr_log, "").expect("write empty stderr");

        let note = retry_failure_recovery_note(&repo, &stdout_log, &stderr_log, 1, false);
        assert!(note.contains("line-499"), "keeps the most recent line");
        assert!(
            !note.contains("line-400"),
            "drops old lines beyond the tail"
        );
        assert!(
            note.contains("Last 40 non-empty stderr lines from the failed attempt:\n(empty)"),
            "empty stderr renders as (empty): {note}"
        );

        fs::remove_dir_all(&dir).ok();
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
    fn superseded_lane_recovery_is_retired_after_newer_task_commit_lands() {
        let (root, remote, upstream, _worker) =
            init_remote_and_clones("parallel-superseded-recovery", "main");
        let lane = root.join("lane-superseded");
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
        fs::write(lane.join("manifest.json"), "{\"result\":\"old\"}\n")
            .expect("failed to write lane manifest");
        run_git_in(&lane, ["add", "manifest.json"]);
        run_git_in(
            &lane,
            ["commit", "-m", "repo: TASK-001 refresh proof manifest"],
        );

        fs::write(upstream.join("manifest.json"), "{\"result\":\"main\"}\n")
            .expect("failed to write upstream manifest");
        run_git_in(&upstream, ["add", "manifest.json"]);
        run_git_in(&upstream, ["commit", "-m", "main conflicting edit"]);
        run_git_in(&upstream, ["push", "origin", "main"]);
        let recovery_base = git_output(&upstream, ["rev-parse", "HEAD"]);

        let mut assignment = ActiveLaneAssignment {
            lane_index: 4,
            attempts: 1,
            task: LoopTask {
                id: "TASK-001".to_string(),
                title: "superseded proof".to_string(),
                status: LoopTaskStatus::Partial,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [~] `TASK-001` superseded proof\n".to_string(),
            },
            resumed: true,
            lane_root: root.join("lane-superseded-root"),
            lane_repo_root: lane.clone(),
            base_commit: base_commit.clone(),
            stdout_log_path: root.join("lane-superseded.stdout.log"),
            stderr_log_path: root.join("lane-superseded.stderr.log"),
            worker_pid_path: root.join("lane-superseded.worker.pid"),
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
        assert!(matches!(
            prep,
            LaneLandingRecoveryPrep::NeedsWorkerResolution { .. }
        ));
        assert_eq!(assignment.base_commit, recovery_base);
        assert!(lane_repo_has_active_cherry_pick(&lane));

        fs::write(upstream.join("manifest.json"), "{\"result\":\"newer\"}\n")
            .expect("failed to write newer upstream manifest");
        run_git_in(&upstream, ["add", "manifest.json"]);
        run_git_in(
            &upstream,
            ["commit", "-m", "repo: TASK-001 publish newer proof"],
        );
        let newer_commit = git_output(&upstream, ["rev-parse", "HEAD"]);

        let superseded = superseded_lane_cherry_pick_recovery(&upstream, &lane, "TASK-001")
            .expect("superseded check should succeed")
            .expect("expected superseded recovery");
        assert_eq!(superseded.superseding_commit, newer_commit);

        let retired = retire_superseded_lane_cherry_pick_recovery(&upstream, &lane, "TASK-001")
            .expect("retirement should succeed")
            .expect("expected retired recovery");
        assert_eq!(retired.superseding_commit, newer_commit);
        assert!(!lane_repo_has_active_cherry_pick(&lane));
        assert_eq!(run_git_in(&lane, ["status", "--short"]), "");
        assert_eq!(git_output(&lane, ["rev-parse", "HEAD"]), recovery_base);

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }
}
