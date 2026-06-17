//! Worktree provisioning and pause-request handling for the audit pipeline.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::audit_everything::git::{git_ref_exists, remote_branch_exists, run_git_dynamic};
use crate::audit_everything::manifest::EverythingManifest;
use crate::audit_everything::path_str;
use crate::audit_everything::run_paths::RunPaths;
use crate::audit_everything::status::write_run_status_if_possible;
use crate::util::{atomic_write, git_stdout, run_git, timestamp_slug};

pub(crate) fn ensure_clean_in_place_start(repo_root: &Path) -> Result<()> {
    let status = git_stdout(repo_root, ["status", "--short"])?;
    if !status.trim().is_empty() {
        bail!(
            "--everything-in-place requires a clean checkout for a new run; existing changes would be committed with audit artifacts:\n{}",
            status
        );
    }
    Ok(())
}

pub(crate) fn request_pause(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    let body = format!(
        "pause requested at {}\nrun: {}\n",
        timestamp_slug(),
        manifest.run_id
    );
    atomic_write(&paths.pause_path, body.as_bytes())
        .with_context(|| format!("failed to write {}", paths.pause_path.display()))?;
    write_run_status_if_possible(paths, manifest)?;
    println!("pause requested: {}", paths.pause_path.display());
    println!("active remediation lanes will drain; no new lanes will be dispatched");
    Ok(())
}

pub(crate) fn clear_pause(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    if paths.pause_path.exists() {
        fs::remove_file(&paths.pause_path)
            .with_context(|| format!("failed to remove {}", paths.pause_path.display()))?;
        println!("pause cleared: {}", paths.pause_path.display());
    } else {
        println!("pause was not active: {}", paths.pause_path.display());
    }
    write_run_status_if_possible(paths, manifest)?;
    Ok(())
}

pub(crate) fn pause_requested(paths: &RunPaths) -> bool {
    paths.pause_path.exists()
}

pub(crate) fn ensure_worktree(
    repo_root: &Path,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if manifest.in_place {
        fs::create_dir_all(&paths.report_root)
            .with_context(|| format!("failed to create {}", paths.report_root.display()))?;
        manifest.worktree_root = paths.worktree_root.display().to_string();
        manifest.report_root = paths.report_root.display().to_string();
        return Ok(());
    }
    if paths.worktree_root.join(".git").exists() || paths.worktree_root.join("Cargo.toml").exists()
    {
        return Ok(());
    }
    if paths.worktree_root.exists() {
        fs::remove_dir_all(&paths.worktree_root).with_context(|| {
            format!(
                "failed to remove incomplete worktree {}",
                paths.worktree_root.display()
            )
        })?;
    }
    if remote_branch_exists(repo_root, &manifest.branch) {
        let _ = run_git(repo_root, ["fetch", "origin", &manifest.branch]);
    }
    let branch_ref = if git_ref_exists(repo_root, &format!("refs/heads/{}", manifest.audit_branch))
    {
        manifest.audit_branch.clone()
    } else if remote_branch_exists(repo_root, &manifest.branch) {
        format!("origin/{}", manifest.branch)
    } else {
        manifest.branch.clone()
    };
    if git_ref_exists(repo_root, &format!("refs/heads/{}", manifest.audit_branch)) {
        run_git_dynamic(
            repo_root,
            &[
                "worktree",
                "add",
                path_str(&paths.worktree_root)?,
                &manifest.audit_branch,
            ],
        )?;
    } else {
        run_git_dynamic(
            repo_root,
            &[
                "worktree",
                "add",
                "-b",
                &manifest.audit_branch,
                path_str(&paths.worktree_root)?,
                &branch_ref,
            ],
        )?;
    }
    fs::create_dir_all(&paths.report_root)
        .with_context(|| format!("failed to create {}", paths.report_root.display()))?;
    Ok(())
}
