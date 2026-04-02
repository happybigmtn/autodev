use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::{atomic_write, ensure_repo_layout};

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct AutoState {
    pub(crate) planning_root: Option<PathBuf>,
    pub(crate) latest_output_dir: Option<PathBuf>,
}

pub(crate) fn state_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".auto").join("state.json")
}

pub(crate) fn load_state(repo_root: &Path) -> Result<AutoState> {
    let path = state_path(repo_root);
    if !path.exists() {
        return Ok(AutoState::default());
    }
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

pub(crate) fn save_state(repo_root: &Path, state: &AutoState) -> Result<()> {
    ensure_repo_layout(repo_root)?;
    let bytes = serde_json::to_vec_pretty(state)?;
    atomic_write(&state_path(repo_root), &bytes)
}
