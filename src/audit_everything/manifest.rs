//! The audit-run data model: the manifest, its nested state records, and the
//! `StageStatus` lifecycle enum.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audit_everything::run_paths::RunPaths;
use crate::audit_everything::status::write_run_status_if_possible;
use crate::util::atomic_write;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EverythingManifest {
    pub(crate) run_id: String,
    pub(crate) repo_root: String,
    pub(crate) worktree_root: String,
    pub(crate) report_root: String,
    #[serde(default)]
    pub(crate) in_place: bool,
    pub(crate) branch: String,
    pub(crate) audit_branch: String,
    pub(crate) base_commit: String,
    pub(crate) created_at: String,
    pub(crate) context: ContextState,
    pub(crate) files: Vec<FileState>,
    pub(crate) groups: Vec<GroupState>,
    #[serde(default)]
    pub(crate) remediation_plan: StageState,
    #[serde(default)]
    pub(crate) remediation_tasks: Vec<RemediationTaskState>,
    #[serde(default)]
    pub(crate) final_review_repairs: Vec<StageState>,
    #[serde(default)]
    pub(crate) file_quality: StageState,
    #[serde(default)]
    pub(crate) file_quality_passes: Vec<FileQualityPassState>,
    #[serde(default)]
    pub(crate) change_summary: StageState,
    pub(crate) final_review: StageState,
    pub(crate) merge: StageState,
    #[serde(default)]
    pub(crate) final_status: StageState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ContextState {
    pub(crate) status: StageStatus,
    pub(crate) agents_hash: Option<String>,
    pub(crate) architecture_hash: Option<String>,
    pub(crate) doctrine_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FileState {
    pub(crate) path: String,
    pub(crate) group: String,
    pub(crate) content_hash: String,
    pub(crate) artifact_dir: String,
    pub(crate) status: StageStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct GroupState {
    pub(crate) name: String,
    pub(crate) slug: String,
    pub(crate) files: Vec<String>,
    pub(crate) report_path: String,
    pub(crate) synthesis_status: StageStatus,
    pub(crate) remediation_status: StageStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RemediationTaskState {
    pub(crate) id: String,
    pub(crate) group: String,
    pub(crate) slug: String,
    pub(crate) report_path: String,
    pub(crate) owned_paths: Vec<String>,
    pub(crate) dependencies: Vec<String>,
    pub(crate) lane_index: usize,
    pub(crate) lane_root: String,
    pub(crate) lane_repo_root: String,
    pub(crate) base_commit: Option<String>,
    pub(crate) status: StageStatus,
    pub(crate) note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FileQualityPassState {
    pub(crate) pass_index: usize,
    pub(crate) status: StageStatus,
    pub(crate) artifact_dir: String,
    pub(crate) ratings: Vec<FileQualityRatingState>,
    pub(crate) note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FileQualityRatingState {
    pub(crate) path: String,
    pub(crate) score_out_of_10: Option<f64>,
    pub(crate) status: StageStatus,
    pub(crate) artifact_dir: String,
    pub(crate) note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct StageState {
    pub(crate) status: StageStatus,
    pub(crate) artifact: Option<String>,
    pub(crate) note: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StageStatus {
    #[default]
    Pending,
    Running,
    Complete,
    Failed,
    Skipped,
}

pub(crate) fn write_manifest(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    atomic_write(&paths.manifest_path, &serde_json::to_vec_pretty(manifest)?)
        .with_context(|| format!("failed to write {}", paths.manifest_path.display()))?;
    write_run_status_if_possible(paths, manifest)
}
