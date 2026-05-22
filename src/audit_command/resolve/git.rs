//! Git plumbing for finding-resolution lanes: clone, stage, commit, fetch,
//! cherry-pick, and landing lane commits onto the target branch.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::audit_command::resolve::lanes::{FindingResolutionLane, FindingResolutionLaneOutcome};
use crate::util::{git_stdout, push_branch_with_remote_sync, run_git};

pub(crate) fn clone_finding_resolution_lane_repo(
    repo_root: &Path,
    target_branch: &str,
    lane_repo_root: &Path,
) -> Result<()> {
    let parent = lane_repo_root
        .parent()
        .with_context(|| format!("{} has no parent", lane_repo_root.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let output = Command::new("git")
        .args(["clone", "--quiet", "--local"])
        .arg("--branch")
        .arg(target_branch)
        .arg("--single-branch")
        .arg(repo_root)
        .arg(lane_repo_root)
        .output()
        .with_context(|| {
            format!(
                "failed to clone lane repo into {}",
                lane_repo_root.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "git clone failed for finding resolution lane {}: {}",
            lane_repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let remotes = git_stdout(lane_repo_root, ["remote"]).unwrap_or_default();
    if remotes.lines().any(|remote| remote.trim() == "origin") {
        run_git(lane_repo_root, ["remote", "rename", "origin", "canonical"])?;
    }
    run_git(lane_repo_root, ["checkout", "--quiet", "--detach", "HEAD"])
}

pub(crate) fn commit_finding_resolution_lane_changes(
    lane_repo_root: &Path,
    lane: &FindingResolutionLane,
    _base_commit: &str,
) -> Result<()> {
    let status = git_stdout(lane_repo_root, ["status", "--short"])?;
    if status.trim().is_empty() {
        return Ok(());
    }
    stage_finding_resolution_lane_changes(lane_repo_root)?;
    if !finding_resolution_lane_has_staged_changes(lane_repo_root)? {
        return Ok(());
    }
    let message = format!(
        "audit: resolve findings lane {:02} {}",
        lane.id + 1,
        lane.name
    );
    run_git(lane_repo_root, ["commit", "-m", &message])
}

fn stage_finding_resolution_lane_changes(lane_repo_root: &Path) -> Result<()> {
    let excludes = finding_resolution_commit_exclude_pathspecs();
    let mut args = vec!["add", "-A", "--", "."];
    args.extend(excludes.iter().map(String::as_str));
    run_git(lane_repo_root, args)
}

fn finding_resolution_commit_exclude_pathspecs() -> Vec<String> {
    [
        ":(exclude).auto",
        ":(exclude).auto/**",
        ":(exclude)audit/AUDIT-PROGRESS.md",
        ":(exclude)audit/FINDING-RESOLVE-STATUS.json",
        ":(exclude)audit/FINDING-RESOLVE-STATUS.md",
        ":(exclude)audit/FINDING-VERIFY.json",
        ":(exclude)audit/FINDING-VERIFY.md",
        ":(exclude)audit/MANIFEST.json",
        ":(exclude)audit/live.log",
        ":(exclude)audit/files/**",
        ":(exclude)audit/finding-resolution/**",
        ":(exclude)audit/logs/**",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn finding_resolution_lane_has_staged_changes(lane_repo_root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(lane_repo_root)
        .args(["diff", "--cached", "--quiet", "--exit-code"])
        .output()
        .with_context(|| format!("failed to inspect {}", lane_repo_root.display()))?;
    if output.status.success() {
        return Ok(false);
    }
    if output.status.code() == Some(1) {
        return Ok(true);
    }
    bail!(
        "git diff --cached failed in {}: {}",
        lane_repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

pub(crate) fn land_finding_resolution_lane_result(
    repo_root: &Path,
    target_branch: &str,
    outcome: &FindingResolutionLaneOutcome,
) -> Result<String> {
    let lane_head = git_stdout(&outcome.lane_repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    fetch_finding_resolution_lane_commit(repo_root, &outcome.lane_repo_root, &lane_head)?;
    if lane_head != outcome.base_commit && !git_ref_is_ancestor(repo_root, "FETCH_HEAD", "HEAD")? {
        let range_base = git_stdout(repo_root, ["merge-base", "HEAD", "FETCH_HEAD"])
            .map(|value| value.trim().to_string())
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| outcome.base_commit.clone());
        cherry_pick_finding_resolution_lane_range(repo_root, &range_base, "FETCH_HEAD")?;
    }
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        println!("remote sync: rebased onto origin/{target_branch}");
    }
    Ok(git_stdout(repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string())
}

fn fetch_finding_resolution_lane_commit(
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
                "failed to fetch finding resolution lane commit {lane_head} from {}",
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

fn git_ref_is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .with_context(|| format!("failed to inspect git ancestry in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    bail!(
        "git merge-base failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn cherry_pick_finding_resolution_lane_range(
    repo_root: &Path,
    range_base: &str,
    head_ref: &str,
) -> Result<()> {
    let range = format!("{range_base}..{head_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cherry-pick", "--empty=drop"])
        .arg(&range)
        .output()
        .with_context(|| format!("failed to cherry-pick {range} into {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cherry-pick", "--abort"])
        .output();
    bail!(
        "git cherry-pick failed in {} for {range}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

pub(crate) fn prune_completed_finding_resolution_lane(lane_repo_root: &Path) -> Result<()> {
    let lane_root = lane_repo_root.parent().unwrap_or(lane_repo_root);
    if lane_root.exists() {
        fs::remove_dir_all(lane_root)
            .with_context(|| format!("failed to prune {}", lane_root.display()))?;
    }
    Ok(())
}
