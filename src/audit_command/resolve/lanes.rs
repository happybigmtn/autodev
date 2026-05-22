//! Finding-resolution lane construction: lane types, architecture-keyed lane
//! assignment, worktree paths, lane prompt building, and artifact pruning.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::audit_command::files::{file_artifact_dir, slugify};
use crate::audit_command::manifest::ManifestEntry;
use crate::AuditArgs;

#[derive(Clone, Debug)]
pub(crate) struct FindingResolutionLane {
    pub(crate) id: usize,
    pub(crate) name: String,
    pub(crate) entries: Vec<ManifestEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FindingResolutionRunStatus {
    generated_at: String,
    run_id: String,
    phase: String,
    run_dir: String,
    worktree_root: String,
    target_root: String,
    lanes: Vec<FindingResolutionLaneStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FindingResolutionLaneStatus {
    pub(crate) id: usize,
    pub(crate) name: String,
    pub(crate) finding_count: usize,
    pub(crate) state: String,
    pub(crate) repo_dir: String,
    pub(crate) target_dir: String,
    pub(crate) prompt_path: String,
    pub(crate) response_path: String,
    pub(crate) landed_commit: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct FindingResolutionLaneOutcome {
    pub(crate) lane_id: usize,
    pub(crate) lane_repo_root: PathBuf,
    pub(crate) base_commit: String,
}

#[derive(Clone, Debug)]
pub(crate) struct FindingResolutionLaneAssignment {
    pub(crate) lane: FindingResolutionLane,
    pub(crate) lane_root: PathBuf,
    pub(crate) lane_repo_root: PathBuf,
    pub(crate) lane_target_dir: PathBuf,
    pub(crate) base_commit: String,
}

pub(crate) fn build_finding_resolution_lanes(
    findings: Vec<ManifestEntry>,
    max_lanes: usize,
) -> Vec<FindingResolutionLane> {
    let mut by_architecture: HashMap<String, Vec<ManifestEntry>> = HashMap::new();
    for finding in findings {
        by_architecture
            .entry(finding_architecture_key(&finding.path))
            .or_default()
            .push(finding);
    }

    let mut groups = by_architecture.into_iter().collect::<Vec<_>>();
    groups.sort_by(|(left_key, left), (right_key, right)| {
        right
            .len()
            .cmp(&left.len())
            .then_with(|| left_key.cmp(right_key))
    });

    let mut lanes = (0..max_lanes)
        .map(|id| FindingResolutionLane {
            id,
            name: format!("lane-{}", id + 1),
            entries: Vec::new(),
        })
        .collect::<Vec<_>>();
    for (key, mut entries) in groups {
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let target = lanes
            .iter()
            .enumerate()
            .min_by_key(|(_, lane)| lane.entries.len())
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        if lanes[target].entries.is_empty() {
            lanes[target].name = key;
        } else {
            lanes[target].name = format!("{}+{}", lanes[target].name, key);
        }
        lanes[target].entries.extend(entries);
    }
    lanes.retain(|lane| !lane.entries.is_empty());
    lanes
}

fn finding_architecture_key(path: &str) -> String {
    let parts = path.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        ["crates", name, ..] => format!("crates/{name}"),
        ["apps", name, ..] => format!("apps/{name}"),
        ["packages", name, ..] => format!("packages/{name}"),
        ["src", ..] => "src".to_string(),
        ["docs", ..] => "docs".to_string(),
        ["specs", ..] => "specs".to_string(),
        ["scripts", ..] => "scripts".to_string(),
        ["tests", ..] => "tests".to_string(),
        [file] if file.ends_with(".md") => "root-docs".to_string(),
        [first, ..] => (*first).to_string(),
        [] => "root".to_string(),
    }
}

pub(crate) fn finding_resolution_lane_dir(run_dir: &Path, lane: &FindingResolutionLane) -> PathBuf {
    run_dir.join(format!("{:02}-{}", lane.id + 1, slugify(&lane.name)))
}

pub(crate) fn finding_resolution_target_root(repo_root: &Path, run_id: &str) -> PathBuf {
    let _ = run_id;
    repo_root
        .join(".auto")
        .join("audit-resolve-targets")
        .join("shared")
}

pub(crate) fn finding_resolution_worktree_root(repo_root: &Path, run_id: &str) -> PathBuf {
    repo_root
        .join(".auto")
        .join("audit-resolve-worktrees")
        .join(run_id)
}

pub(crate) fn finding_resolution_lane_worktree_dir(
    worktree_root: &Path,
    lane: &FindingResolutionLane,
) -> PathBuf {
    worktree_root.join(format!("{:02}-{}", lane.id + 1, slugify(&lane.name)))
}

pub(crate) fn write_finding_resolution_status(
    output_dir: &Path,
    run_id: &str,
    phase: &str,
    run_dir: &Path,
    worktree_root: &Path,
    target_root: &Path,
    lanes: &[FindingResolutionLaneStatus],
) -> Result<()> {
    let status = FindingResolutionRunStatus {
        generated_at: chrono_like_now(),
        run_id: run_id.to_string(),
        phase: phase.to_string(),
        run_dir: run_dir.display().to_string(),
        worktree_root: worktree_root.display().to_string(),
        target_root: target_root.display().to_string(),
        lanes: lanes.to_vec(),
    };
    let json = serde_json::to_vec_pretty(&status)?;
    crate::util::atomic_write(&output_dir.join("FINDING-RESOLVE-STATUS.json"), &json)?;

    let mut markdown = String::new();
    markdown.push_str("# FINDING-RESOLVE-STATUS\n\n");
    markdown.push_str(&format!("- run id: `{}`\n", status.run_id));
    markdown.push_str(&format!("- phase: `{}`\n", status.phase));
    markdown.push_str(&format!("- run dir: `{}`\n", status.run_dir));
    markdown.push_str(&format!("- worktree root: `{}`\n", status.worktree_root));
    markdown.push_str(&format!("- target root: `{}`\n\n", status.target_root));
    markdown.push_str("| Lane | State | Findings | Repo | Target | Landed Commit |\n");
    markdown.push_str("|---|---|---:|---|---|---|\n");
    for lane in &status.lanes {
        markdown.push_str(&format!(
            "| `{}` | `{}` | {} | `{}` | `{}` | {} |\n",
            lane.name,
            lane.state,
            lane.finding_count,
            lane.repo_dir,
            lane.target_dir,
            lane.landed_commit
                .as_deref()
                .map(|commit| format!("`{commit}`"))
                .unwrap_or_else(|| "".to_string())
        ));
        if let Some(error) = lane.error.as_deref() {
            markdown.push_str(&format!(
                "|  | error |  |  |  | `{}` |\n",
                error.replace('|', "\\|")
            ));
        }
    }
    crate::util::atomic_write(
        &output_dir.join("FINDING-RESOLVE-STATUS.md"),
        markdown.as_bytes(),
    )
}

fn chrono_like_now() -> String {
    crate::util::timestamp_slug()
}

pub(crate) fn prune_finding_resolution_artifacts(
    repo_root: &Path,
    output_dir: &Path,
    current_run_id: &str,
    keep_runs: usize,
    prune_targets: bool,
    include_current_targets: bool,
) -> Result<()> {
    let run_root = output_dir.join("finding-resolution");
    prune_child_dirs_by_name(&run_root, current_run_id, keep_runs, false)?;
    if prune_targets {
        let target_parent = repo_root.join(".auto").join("audit-resolve-targets");
        prune_child_dirs_by_name(
            &target_parent,
            current_run_id,
            keep_runs,
            include_current_targets,
        )?;
        let worktree_parent = repo_root.join(".auto").join("audit-resolve-worktrees");
        prune_child_dirs_by_name(
            &worktree_parent,
            current_run_id,
            keep_runs,
            include_current_targets,
        )?;
    }
    Ok(())
}

fn prune_child_dirs_by_name(
    parent: &Path,
    current_run_id: &str,
    keep_runs: usize,
    include_current: bool,
) -> Result<()> {
    if !parent.exists() {
        return Ok(());
    }
    let mut dirs = fs::read_dir(parent)
        .with_context(|| format!("failed to read {}", parent.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            Some((
                entry.file_name().to_string_lossy().to_string(),
                entry.path(),
            ))
        })
        .collect::<Vec<_>>();
    dirs.sort_by(|left, right| right.0.cmp(&left.0));
    for (idx, (name, path)) in dirs.into_iter().enumerate() {
        let keep_by_count = idx < keep_runs;
        let is_current = name == current_run_id;
        if is_current && include_current {
            fs::remove_dir_all(&path)
                .with_context(|| format!("failed to prune {}", path.display()))?;
            continue;
        }
        if name == "shared" || keep_by_count || (is_current && !include_current) {
            continue;
        }
        fs::remove_dir_all(&path).with_context(|| format!("failed to prune {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn reset_finding_resolution_lane_root(lane_root: &Path) -> Result<()> {
    if lane_root.exists() {
        fs::remove_dir_all(lane_root)
            .with_context(|| format!("failed to reset {}", lane_root.display()))?;
    }
    fs::create_dir_all(lane_root)
        .with_context(|| format!("failed to create {}", lane_root.display()))
}

pub(crate) fn build_finding_resolution_prompt(
    repo_root: &Path,
    output_dir: &Path,
    lane: &FindingResolutionLane,
    lane_target_dir: &Path,
    args: &AuditArgs,
) -> Result<String> {
    let mut body = String::new();
    body.push_str(
        "You are a standalone `auto audit --resolve-findings` remediation lane.\n\
         Work only on the findings listed below. Do not produce a new initial audit report. \
         Resolve the current audit finding in the live codebase.\n\n\
         Required behavior:\n\
         - For RETIRE findings, delete the obsolete file/code if it is truly unused, or simplify it until the retirement finding is no longer valid.\n\
         - For DRIFT-LARGE, DRIFT-SMALL, REFACTOR, ApplyFailed, or Escalated findings, make the smallest code/docs/spec changes needed for the file to pass re-audit.\n\
         - Do not resolve stale current-HEAD, timestamp, run-id, local-checkout, or other volatile proof drift by replacing it with this lane's current value; that self-invalidates after the lane commits. Instead, remove the moving value, point to a stable release identity, or require the command to be run at review time.\n\
         - Keep edits scoped to this lane's paths and direct dependencies.\n\
         - Do not mark implementation-plan rows complete and do not edit audit/MANIFEST.json.\n\
         - Run targeted validation when practical and summarize what changed.\n\
         - Use the provided lane-scoped CARGO_TARGET_DIR. It is intentionally stable across resolve passes for this lane so Cargo can reuse build artifacts; do not override it.\n\
         - Run Cargo validations serially within this lane unless you deliberately give each concurrent command a distinct target directory. Do not start a second `cargo test`, `cargo check`, `cargo clippy`, or `cargo fmt` command while another Cargo command is still running in this lane's target dir; wait for the first command to finish, then run the next proof.\n\
         - Cargo accepts only one test filter per `cargo test` command. Split multiple exact tests into separate commands or use one module-level/common filter.\n\n",
    );
    body.push_str("# Validation Environment\n\n");
    body.push_str(&format!(
        "- `CARGO_TARGET_DIR={}`\n",
        lane_target_dir.display()
    ));
    body.push_str(&format!(
        "- `CARGO_BUILD_JOBS={}`\n",
        args.resolve_validation_threads.max(1)
    ));
    body.push_str(
        "- The lane PATH contains an `auto audit` Cargo guard wrapper that rejects multi-filter `cargo test` invocations before they waste compile time.\n\
         - This target directory may contain artifacts from earlier resolve passes for the same lane. That cache is expected; final proof still must be run with `cargo test`, `cargo check`, `cargo fmt`, or the repo guard from the current source tree.\n\n",
    );
    body.push_str("# Lane Scope\n\n");
    body.push_str(&format!("Lane: `{}`\n\n", lane.name));
    for entry in &lane.entries {
        body.push_str(&format!(
            "## `{}`\n\n- Verdict: `{}`\n- Status: `{:?}`\n",
            entry.path,
            entry.verdict.as_deref().unwrap_or("UNKNOWN"),
            entry.status
        ));
        let artifact_dir = file_artifact_dir(output_dir, &entry.path);
        let artifact_rel = artifact_dir
            .strip_prefix(repo_root)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| artifact_dir.display().to_string());
        body.push_str(&format!("- Artifact directory: `{artifact_rel}`\n"));
        append_optional_artifact(
            &mut body,
            &artifact_dir.join("verdict.json"),
            "verdict.json",
        )?;
        append_optional_artifact(
            &mut body,
            &artifact_dir.join("worklist-entry.md"),
            "worklist-entry.md",
        )?;
        append_optional_artifact(
            &mut body,
            &artifact_dir.join("retire-reason.md"),
            "retire-reason.md",
        )?;
        body.push('\n');
    }
    let retire_batch = output_dir.join("RETIRE-BATCH.md");
    append_optional_artifact(&mut body, &retire_batch, "RETIRE-BATCH.md")?;
    Ok(body)
}

fn append_optional_artifact(body: &mut String, path: &Path, label: &str) -> Result<()> {
    let Ok(mut text) = fs::read_to_string(path) else {
        return Ok(());
    };
    const MAX_ARTIFACT_BYTES: usize = 12_000;
    if text.len() > MAX_ARTIFACT_BYTES {
        text.truncate(MAX_ARTIFACT_BYTES);
        text.push_str("\n[truncated]\n");
    }
    body.push_str(&format!("\n### `{label}`\n\n```text\n{text}\n```\n"));
    Ok(())
}
