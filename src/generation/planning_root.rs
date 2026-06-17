//! Planning-root resolution, corpus staging, and output-directory preparation
//! for the generation pipeline.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::state::AutoState;
use crate::util::{copy_tree, list_markdown_files, timestamp_slug};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlanningRootSource {
    Cli,
    SavedState,
    DefaultGenesis,
}

impl PlanningRootSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::SavedState => "saved state",
            Self::DefaultGenesis => "default genesis",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ResolvedPlanningRoot {
    pub(crate) path: PathBuf,
    pub(crate) source: PlanningRootSource,
}

pub(crate) struct CorpusPreparation {
    pub(crate) authoring_root: PathBuf,
    pub(crate) previous_snapshot: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ActivePlanSurface {
    pub(crate) root_plan_standard_path: Option<String>,
    pub(crate) active_plan_paths: Vec<String>,
}

impl ActivePlanSurface {
    pub(crate) fn has_active_plans(&self) -> bool {
        !self.active_plan_paths.is_empty()
    }

    pub(crate) fn primary_plan_path(&self) -> Option<&str> {
        self.active_plan_paths
            .iter()
            .find(|path| path.ends_with("001-master-plan.md"))
            .or_else(|| self.active_plan_paths.first())
            .map(String::as_str)
    }
}

pub(crate) fn resolve_reference_repos(repo_root: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for path in paths {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            repo_root.join(path)
        };
        let canonical = absolute
            .canonicalize()
            .with_context(|| format!("failed to resolve reference repo {}", absolute.display()))?;
        if !canonical.is_dir() {
            bail!("reference repo {} is not a directory", canonical.display());
        }
        resolved.push(canonical);
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

pub(crate) fn discover_active_plan_surface(repo_root: &Path) -> Result<ActivePlanSurface> {
    let root_plan_standard_path = repo_root
        .join("PLANS.md")
        .exists()
        .then_some("PLANS.md".to_string());
    let plans_dir = repo_root.join("plans");
    let active_plan_paths = if plans_dir.is_dir() {
        list_markdown_files(&plans_dir)?
            .into_iter()
            .map(|path| {
                path.strip_prefix(repo_root)
                    .unwrap_or(&path)
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    Ok(ActivePlanSurface {
        root_plan_standard_path,
        active_plan_paths,
    })
}

pub(crate) fn ensure_planning_root_exists(planning_root: &Path) -> Result<()> {
    if planning_root.exists() {
        return Ok(());
    }
    bail!(
        "planning corpus root {} does not exist; run `auto corpus` first",
        planning_root.display()
    );
}

pub(crate) fn ensure_planning_root_ready_for_corpus(planning_root: &Path) -> Result<()> {
    if !planning_root.exists() || planning_root.is_dir() {
        return Ok(());
    }
    bail!(
        "planning corpus root {} exists but is not a directory",
        planning_root.display()
    );
}

pub(crate) fn resolve_generation_planning_root(
    repo_root: &Path,
    cli_planning_root: Option<&Path>,
    state: &AutoState,
) -> Result<ResolvedPlanningRoot> {
    if let Some(path) = cli_planning_root {
        return Ok(ResolvedPlanningRoot {
            path: normalize_repo_relative_path(repo_root, path),
            source: PlanningRootSource::Cli,
        });
    }

    if let Some(path) = state.planning_root.as_deref() {
        let normalized = normalize_repo_relative_path(repo_root, path);
        if !normalized.starts_with(repo_root) {
            bail!(
                "saved planning root {} is outside repo root {}; pass --planning-root explicitly to use an external corpus",
                normalized.display(),
                repo_root.display()
            );
        }
        return Ok(ResolvedPlanningRoot {
            path: normalized,
            source: PlanningRootSource::SavedState,
        });
    }

    Ok(ResolvedPlanningRoot {
        path: repo_root.join("genesis"),
        source: PlanningRootSource::DefaultGenesis,
    })
}

fn normalize_repo_relative_path(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

pub(crate) fn prepare_planning_root_for_corpus(
    repo_root: &Path,
    planning_root: &Path,
) -> Result<CorpusPreparation> {
    let previous_snapshot = if planning_root.exists() {
        let has_contents = fs::read_dir(planning_root)
            .with_context(|| format!("failed to read {}", planning_root.display()))?
            .next()
            .transpose()?
            .is_some();
        if has_contents {
            let snapshot_root = repo_root.join(".auto").join("fresh-input").join(format!(
                "{}-previous-{}",
                planning_root
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or("planning-root"),
                timestamp_slug()
            ));
            copy_tree(planning_root, &snapshot_root).with_context(|| {
                format!(
                    "failed to archive existing planning corpus from {} into {}",
                    planning_root.display(),
                    snapshot_root.display()
                )
            })?;
            Some(snapshot_root)
        } else {
            None
        }
    } else {
        None
    };
    let staging_root = repo_root.join(".auto").join("corpus-staging").join(format!(
        "{}-{}",
        planning_root
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("planning-root"),
        timestamp_slug()
    ));
    if staging_root.exists() {
        fs::remove_dir_all(&staging_root)
            .with_context(|| format!("failed to clear {}", staging_root.display()))?;
    }
    fs::create_dir_all(&staging_root)
        .with_context(|| format!("failed to create {}", staging_root.display()))?;
    Ok(CorpusPreparation {
        authoring_root: staging_root,
        previous_snapshot,
    })
}

pub(crate) fn promote_staged_planning_root(
    staging_root: &Path,
    planning_root: &Path,
) -> Result<()> {
    if !staging_root.exists() {
        bail!(
            "staged planning root {} does not exist",
            staging_root.display()
        );
    }
    if let Some(parent) = planning_root.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if planning_root.exists() {
        fs::remove_dir_all(planning_root)
            .with_context(|| format!("failed to clear {}", planning_root.display()))?;
    }
    fs::rename(staging_root, planning_root).with_context(|| {
        format!(
            "failed to promote staged corpus {} -> {}",
            staging_root.display(),
            planning_root.display()
        )
    })?;
    Ok(())
}

pub(crate) fn prepare_generation_output_dir(output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    for path in [
        output_dir.join("corpus"),
        output_dir.join("specs"),
        output_dir.join("IMPLEMENTATION_PLAN.md"),
        output_dir.join("COMPLETED.md"),
    ] {
        if path.is_dir() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("failed to clear {}", path.display()))?;
        } else if path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        prepare_planning_root_for_corpus, promote_staged_planning_root,
        resolve_generation_planning_root, PlanningRootSource,
    };
    use crate::generation::tests::{temp_dir, valid_corpus_execplan, write_valid_corpus};
    use crate::state::AutoState;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn saved_outside_repo_planning_root_is_rejected_before_generation() {
        let repo_root = temp_dir("saved-outside-repo");
        let outside = temp_dir("external-corpus");
        let state = AutoState {
            planning_root: Some(outside.clone()),
            latest_output_dir: None,
        };

        let error = resolve_generation_planning_root(&repo_root, None, &state)
            .expect_err("saved outside planning root should fail");

        assert!(error.to_string().contains("outside repo root"));
    }

    #[test]
    fn planning_root_resolution_reports_cli_saved_or_default_source() {
        let repo_root = temp_dir("planning-root-source");
        let mut state = AutoState::default();

        let defaulted = resolve_generation_planning_root(&repo_root, None, &state).unwrap();
        assert_eq!(defaulted.source, PlanningRootSource::DefaultGenesis);
        assert_eq!(defaulted.path, repo_root.join("genesis"));

        state.planning_root = Some(PathBuf::from("genesis"));
        let saved = resolve_generation_planning_root(&repo_root, None, &state).unwrap();
        assert_eq!(saved.source, PlanningRootSource::SavedState);
        assert_eq!(saved.path, repo_root.join("genesis"));

        let cli = resolve_generation_planning_root(
            &repo_root,
            Some(Path::new("../operator-corpus")),
            &state,
        )
        .unwrap();
        assert_eq!(cli.source, PlanningRootSource::Cli);
        assert_eq!(cli.path, repo_root.join("../operator-corpus"));
    }

    #[test]
    fn corpus_refresh_failure_preserves_previous_planning_root() {
        let repo_root = temp_dir("corpus-refresh-preserves");
        let planning_root = repo_root.join("genesis");
        write_valid_corpus(&planning_root);
        let original_plan = fs::read_to_string(planning_root.join("plans/001-build.md")).unwrap();

        let prep = prepare_planning_root_for_corpus(&repo_root, &planning_root).unwrap();
        fs::write(prep.authoring_root.join("BROKEN.md"), "# Broken\n").unwrap();

        assert_eq!(
            fs::read_to_string(planning_root.join("plans/001-build.md")).unwrap(),
            original_plan
        );
        assert!(prep
            .previous_snapshot
            .as_ref()
            .is_some_and(|path| path.join("plans/001-build.md").exists()));
    }

    #[test]
    fn corpus_refresh_promotes_staged_root_after_validation() {
        let repo_root = temp_dir("corpus-refresh-promotes");
        let planning_root = repo_root.join("genesis");
        write_valid_corpus(&planning_root);
        let prep = prepare_planning_root_for_corpus(&repo_root, &planning_root).unwrap();
        write_valid_corpus(&prep.authoring_root);
        fs::write(
            prep.authoring_root.join("plans/001-build.md"),
            valid_corpus_execplan().replace("# Build", "# New Build"),
        )
        .unwrap();

        promote_staged_planning_root(&prep.authoring_root, &planning_root).unwrap();

        assert!(fs::read_to_string(planning_root.join("plans/001-build.md"))
            .unwrap()
            .contains("# New Build"));
        assert!(!prep.authoring_root.exists());
    }
}
