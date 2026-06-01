//! Git plumbing for the audit pipeline: worktree commits, lane fetch and
//! cherry-pick, branch resolution helpers.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::audit_everything::manifest::EverythingManifest;
use crate::audit_everything::run_paths::RunPaths;
use crate::util::{git_cherry_pick_empty_arg, git_stdout, run_git};

pub(crate) fn commit_worktree_changes(
    paths: &RunPaths,
    manifest: &EverythingManifest,
) -> Result<()> {
    let generated_file_artifacts = format!("audit/everything/{}/files", manifest.run_id);
    let _ = run_git(
        &paths.worktree_root,
        [
            "rm",
            "-r",
            "--cached",
            "--ignore-unmatch",
            &generated_file_artifacts,
        ],
    );
    let status = git_stdout(&paths.worktree_root, ["status", "--short"])?;
    if status.trim().is_empty() {
        return Ok(());
    }
    run_git(
        &paths.worktree_root,
        ["add", "--", ".", ":(exclude)audit/everything/*/files/**"],
    )?;
    let _ = run_git(
        &paths.worktree_root,
        [
            "rm",
            "-r",
            "--cached",
            "--ignore-unmatch",
            &generated_file_artifacts,
        ],
    );
    let staged = command_status(
        &paths.worktree_root,
        ["diff", "--cached", "--quiet", "--exit-code"],
    )?;
    if staged.success() {
        return Ok(());
    }
    let message = format!("audit: professional whole-repo audit {}", manifest.run_id);
    run_git(&paths.worktree_root, ["commit", "-m", &message])?;
    Ok(())
}

pub(crate) fn clone_audit_lane_repo(repo_root: &Path, lane_repo_root: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg("--local")
        .arg(repo_root)
        .arg(lane_repo_root)
        .output()
        .with_context(|| {
            format!(
                "failed to clone audit lane repo from {} to {}",
                repo_root.display(),
                lane_repo_root.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "git clone failed for audit lane {}: {}",
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

pub(crate) fn remote_branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_ref_exists(repo_root, &format!("refs/remotes/origin/{branch}"))
}

pub(crate) fn audit_lane_changed_files(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
) -> Result<Vec<String>> {
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

pub(crate) fn fetch_lane_commit(
    repo_root: &Path,
    lane_repo_root: &Path,
    lane_head: &str,
) -> Result<()> {
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

pub(crate) fn git_ref_is_ancestor(
    repo_root: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool> {
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

pub(crate) fn cherry_pick_lane_range(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
    abort_on_failure: bool,
) -> Result<()> {
    if audit_lane_changed_files(repo_root, base_commit, head_ref)?.is_empty() {
        return Ok(());
    }
    let range = format!("{base_commit}..{head_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cherry-pick")
        .arg(git_cherry_pick_empty_arg())
        .arg(&range)
        .output()
        .with_context(|| format!("failed to cherry-pick {range} in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }
    if abort_on_failure {
        let _ = run_git(repo_root, ["cherry-pick", "--abort"]);
    }
    bail!(
        "git cherry-pick failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

pub(crate) fn git_ref_exists(repo_root: &Path, reference: &str) -> bool {
    command_status(repo_root, ["show-ref", "--verify", "--quiet", reference])
        .is_ok_and(|status| status.success())
}

pub(crate) fn command_status<'a>(
    repo_root: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<std::process::ExitStatus> {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .status()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))
}

pub(crate) fn run_git_dynamic(repo_root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    bail!(
        "git command failed in {}: {}",
        repo_root.display(),
        if stderr.is_empty() { stdout } else { stderr }
    );
}

pub(crate) fn git_rev_parse_short(repo_root: &Path, reference: &str) -> Option<String> {
    git_stdout(repo_root, ["rev-parse", "--short=9", reference])
        .ok()
        .map(|head| head.trim().to_string())
        .filter(|head| !head.is_empty())
}

pub(crate) fn git_ahead_behind(
    repo_root: &Path,
    primary_ref: &str,
    audit_ref: &str,
) -> Option<(usize, usize)> {
    let range = format!("{primary_ref}...{audit_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-list", "--left-right", "--count", &range])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut parts = stdout.split_whitespace();
    let primary_only = parts.next()?.parse::<usize>().ok()?;
    let audit_only = parts.next()?.parse::<usize>().ok()?;
    Some((audit_only, primary_only))
}
