//! Run-directory layout: [`RunPaths`], the manifest/run loader, and the report
//! artifact path helpers.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::audit_everything::git::{git_ref_exists, remote_branch_exists};
use crate::audit_everything::inventory::reconcile_file_inventory;
use crate::audit_everything::manifest::{
    ContextState, EverythingManifest, StageState, StageStatus,
};
use crate::audit_everything::worktree::ensure_clean_in_place_start;
use crate::audit_everything::{slugify, write_manifest};
use crate::util::{git_stdout, timestamp_slug};
use crate::AuditArgs;

pub(crate) const PROFESSIONAL_AUDIT_DIR: &str = ".auto/audit-everything";
pub(crate) const LATEST_RUN_FILE: &str = "latest-run";
pub(crate) const PAUSE_REQUEST_FILE: &str = "PAUSE";
pub(crate) const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["trunk", "main", "master"];

#[derive(Clone)]
pub(crate) struct RunPaths {
    pub(crate) host_root: PathBuf,
    pub(crate) manifest_path: PathBuf,
    pub(crate) latest_path: PathBuf,
    pub(crate) worktree_root: PathBuf,
    pub(crate) report_root: PathBuf,
    pub(crate) pause_path: PathBuf,
    pub(crate) in_place: bool,
}

#[derive(Clone)]
pub(crate) struct PhaseConfig {
    pub(crate) model: String,
    pub(crate) effort: String,
    pub(crate) codex_bin: PathBuf,
}

pub(crate) fn resolve_run_root_base(repo_root: &Path, override_root: Option<&Path>) -> PathBuf {
    match override_root {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => repo_root.join(path),
        None => repo_root.join(PROFESSIONAL_AUDIT_DIR),
    }
}

pub(crate) fn load_or_create_run(
    repo_root: &Path,
    run_root_base: &Path,
    branch: &str,
    args: &AuditArgs,
) -> Result<(EverythingManifest, RunPaths)> {
    fs::create_dir_all(run_root_base)
        .with_context(|| format!("failed to create {}", run_root_base.display()))?;
    let latest_path = run_root_base.join(LATEST_RUN_FILE);
    let run_id = if let Some(run_id) = args.everything_run_id.as_deref() {
        run_id.to_string()
    } else if latest_path.exists() {
        fs::read_to_string(&latest_path)
            .with_context(|| format!("failed to read {}", latest_path.display()))?
            .trim()
            .to_string()
    } else {
        timestamp_slug()
    };
    if run_id.trim().is_empty() {
        bail!("professional audit run id is empty");
    }

    let host_root = run_root_base.join(&run_id);
    fs::create_dir_all(&host_root)
        .with_context(|| format!("failed to create {}", host_root.display()))?;
    let manifest_path = host_root.join("MANIFEST.json");
    let default_worktree_root = host_root.join("worktree");
    let default_report_root = default_worktree_root
        .join("audit")
        .join("everything")
        .join(&run_id);
    let mut paths = run_paths(
        host_root.clone(),
        manifest_path.clone(),
        latest_path,
        default_worktree_root.clone(),
        default_report_root.clone(),
    );

    if manifest_path.exists() {
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let mut manifest: EverythingManifest = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        paths.worktree_root = PathBuf::from(&manifest.worktree_root);
        paths.report_root = PathBuf::from(&manifest.report_root);
        paths.in_place = manifest.in_place;
        if manifest.files.is_empty() || !matches!(manifest.context.status, StageStatus::Complete) {
            reconcile_file_inventory(&paths.worktree_root, &paths.report_root, &mut manifest).ok();
        }
        return Ok((manifest, paths));
    }

    let (worktree_root, report_root, in_place) = if args.everything_in_place {
        ensure_clean_in_place_start(repo_root)?;
        (
            repo_root.to_path_buf(),
            repo_root.join("audit").join("everything").join(&run_id),
            true,
        )
    } else {
        (default_worktree_root, default_report_root, false)
    };
    paths.worktree_root = worktree_root.clone();
    paths.report_root = report_root.clone();
    paths.in_place = in_place;

    let base_commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let audit_branch = if in_place {
        branch.to_string()
    } else {
        format!(
            "auto-audit/{repo}-{run_id}",
            repo = slugify(
                repo_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("repo")
            )
        )
    };
    let manifest = EverythingManifest {
        run_id: run_id.clone(),
        repo_root: repo_root.display().to_string(),
        worktree_root: worktree_root.display().to_string(),
        report_root: report_root.display().to_string(),
        in_place,
        branch: branch.to_string(),
        audit_branch,
        base_commit,
        created_at: timestamp_slug(),
        context: ContextState::default(),
        files: Vec::new(),
        groups: Vec::new(),
        remediation_plan: StageState::default(),
        remediation_tasks: Vec::new(),
        final_review_repairs: Vec::new(),
        file_quality: StageState::default(),
        file_quality_passes: Vec::new(),
        change_summary: StageState::default(),
        final_review: StageState::default(),
        merge: StageState::default(),
        final_status: StageState::default(),
    };
    crate::util::atomic_write(&paths.latest_path, run_id.as_bytes())
        .with_context(|| format!("failed to write {}", paths.latest_path.display()))?;
    write_manifest(&paths, &manifest)?;
    Ok((manifest, paths))
}

fn run_paths(
    host_root: PathBuf,
    manifest_path: PathBuf,
    latest_path: PathBuf,
    worktree_root: PathBuf,
    report_root: PathBuf,
) -> RunPaths {
    RunPaths {
        pause_path: host_root.join(PAUSE_REQUEST_FILE),
        host_root,
        manifest_path,
        latest_path,
        worktree_root,
        report_root,
        in_place: false,
    }
}

pub(crate) fn remediation_plan_markdown_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("REMEDIATION-PLAN.md")
}

pub(crate) fn remediation_plan_json_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("REMEDIATION-PLAN.json")
}

pub(crate) fn codebase_improvement_policy_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("CODEBASE-IMPROVEMENT-POLICY.md")
}

pub(crate) fn run_status_markdown_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("RUN-STATUS.md")
}

pub(crate) fn final_review_markdown_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("FINAL-REVIEW.md")
}

pub(crate) fn final_review_shards_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("FINAL-REVIEW-SHARDS")
}

pub(crate) fn change_summary_markdown_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("CHANGE-SUMMARY.md")
}

pub(crate) fn codebase_book_root_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("CODEBASE-BOOK")
}

pub(crate) fn codebase_book_index_path(paths: &RunPaths) -> PathBuf {
    codebase_book_root_path(paths).join("README.md")
}

pub(crate) fn file_quality_root_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("FILE-QUALITY")
}

pub(crate) fn file_quality_pass_path(paths: &RunPaths, pass_index: usize) -> PathBuf {
    file_quality_root_path(paths).join(format!("pass-{pass_index:02}"))
}

pub(crate) fn file_quality_file_path(
    paths: &RunPaths,
    pass_index: usize,
    file: &crate::audit_everything::manifest::FileState,
) -> PathBuf {
    file_quality_pass_path(paths, pass_index).join(
        crate::audit_everything::inventory::file_artifact_slug(&file.path, &file.content_hash),
    )
}

pub(crate) fn resolve_primary_branch(
    repo_root: &Path,
    requested_branch: Option<&str>,
    current_branch: &str,
) -> Result<String> {
    if let Some(branch) = requested_branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
    {
        return Ok(branch.to_string());
    }
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
    if let Some(branch) = origin_head.and_then(|value| parse_origin_head_branch(&value)) {
        return Ok(branch);
    }
    if KNOWN_PRIMARY_BRANCHES.contains(&current_branch) {
        return Ok(current_branch.to_string());
    }
    for branch in KNOWN_PRIMARY_BRANCHES {
        if git_ref_exists(repo_root, &format!("refs/heads/{branch}"))
            || remote_branch_exists(repo_root, branch)
        {
            return Ok(branch.to_string());
        }
    }
    bail!("auto audit --everything could not resolve primary branch; pass --branch <name>");
}

fn parse_origin_head_branch(origin_head: &str) -> Option<String> {
    let trimmed = origin_head.trim();
    let branch = trimmed.strip_prefix("origin/").unwrap_or(trimmed).trim();
    (!branch.is_empty()).then(|| branch.to_string())
}
