//! The post-remediation file-quality gate: rerating passes plus per-file
//! deliverable passes that drive every tracked file to the acceptance floor.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tokio::task::JoinSet;

use crate::audit_everything::git::commit_worktree_changes;
use crate::audit_everything::inventory::file_artifact_slug;
use crate::audit_everything::manifest::write_manifest;
use crate::audit_everything::manifest::{
    EverythingManifest, FileQualityPassState, FileQualityRatingState, FileState, StageStatus,
};
use crate::audit_everything::prompts::{
    build_file_quality_deliverables_prompt, build_file_quality_rerate_prompt,
};
use crate::audit_everything::require_nonempty_file;
use crate::audit_everything::run_paths::{
    file_quality_file_path, file_quality_pass_path, file_quality_root_path, PhaseConfig, RunPaths,
};
use crate::audit_everything::workers::run_codex_phase_for_artifact;
use crate::AuditArgs;

pub(crate) const DEFAULT_FILE_QUALITY_PASS_LIMIT: usize = 10;
pub(crate) const FILE_QUALITY_ACCEPT_SCORE: f64 = 9.0;
pub(crate) const FILE_QUALITY_TARGET_SCORE: f64 = 10.0;

pub(crate) async fn run_file_quality_gate_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<bool> {
    if matches!(manifest.file_quality.status, StageStatus::Complete) {
        println!("file quality: complete (resume)");
        return Ok(false);
    }

    let limit = args
        .file_quality_passes
        .clamp(1, DEFAULT_FILE_QUALITY_PASS_LIMIT);
    manifest.file_quality.status = StageStatus::Running;
    manifest.file_quality.artifact = Some(crate::audit_everything::path_display(
        &file_quality_root_path(paths),
    ));
    write_manifest(paths, manifest)?;

    let mut changed = false;
    let rerate_config = PhaseConfig {
        model: args.first_pass_model.clone(),
        effort: args.first_pass_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let deliverable_config = PhaseConfig {
        model: args.remediation_model.clone(),
        effort: args.remediation_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let rerate_workers = args.everything_threads.clamp(1, 15);
    let deliverable_workers = args.remediation_threads.clamp(1, 10);

    for pass_index in next_file_quality_pass_index(manifest)..=limit {
        let pass =
            run_one_file_quality_pass(paths, manifest, pass_index, &rerate_config, rerate_workers)
                .await?;
        let below_threshold = pass
            .ratings
            .iter()
            .filter(|rating| rating.score_out_of_10.unwrap_or(0.0) < FILE_QUALITY_ACCEPT_SCORE)
            .cloned()
            .collect::<Vec<_>>();

        manifest.file_quality_passes.push(pass);
        if below_threshold.is_empty() {
            manifest.file_quality.status = StageStatus::Complete;
            manifest.file_quality.note = Some(format!(
                "all files rerated at least {FILE_QUALITY_ACCEPT_SCORE:.0}/10 after pass {pass_index}"
            ));
            write_manifest(paths, manifest)?;
            return Ok(changed);
        }

        println!(
            "file quality: pass {pass_index} found {} file(s) below {FILE_QUALITY_ACCEPT_SCORE:.0}/10",
            below_threshold.len()
        );
        run_file_quality_deliverables(
            paths,
            manifest,
            pass_index,
            &below_threshold,
            &deliverable_config,
            deliverable_workers,
        )
        .await?;
        changed = true;
        manifest.file_quality.note = Some(format!(
            "pass {pass_index} ran {} per-file deliverable set(s); next pass will rerate all files",
            below_threshold.len()
        ));
        write_manifest(paths, manifest)?;
        commit_worktree_changes(paths, manifest)?;
    }

    manifest.file_quality.status = StageStatus::Failed;
    manifest.file_quality.note = Some(format!(
        "files remained below {FILE_QUALITY_ACCEPT_SCORE:.0}/10 after {limit} quality pass(es)"
    ));
    write_manifest(paths, manifest)?;
    bail!(
        "file-quality gate failed: at least one file remained below {FILE_QUALITY_ACCEPT_SCORE:.0}/10 after {limit} pass(es)"
    )
}

async fn run_one_file_quality_pass(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    pass_index: usize,
    config: &PhaseConfig,
    workers: usize,
) -> Result<FileQualityPassState> {
    let pass_dir = file_quality_pass_path(paths, pass_index);
    fs::create_dir_all(&pass_dir)
        .with_context(|| format!("failed to create {}", pass_dir.display()))?;
    let pending = manifest
        .files
        .iter()
        .filter(|file| {
            !file_quality_file_path(paths, pass_index, file)
                .join("rating.json")
                .exists()
        })
        .cloned()
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        println!(
            "file quality: pass {pass_index} rerating {} file(s), {} worker(s)",
            pending.len(),
            workers
        );
        let mut join_set = JoinSet::new();
        let mut pending_iter = pending.into_iter();
        let mut active = 0usize;
        for _ in 0..workers {
            if let Some(file) = pending_iter.next() {
                spawn_file_quality_rerate_worker(
                    &mut join_set,
                    paths.clone(),
                    manifest.clone(),
                    file,
                    pass_index,
                    config.clone(),
                );
                active += 1;
            }
        }
        let mut failures = Vec::new();
        while active > 0 {
            let Some(result) = join_set.join_next().await else {
                break;
            };
            active -= 1;
            match result {
                Ok(Ok(path)) => println!("file quality rating: {}", path.display()),
                Ok(Err(err)) => failures.push(format!("{err:#}")),
                Err(err) => failures.push(format!("file-quality rerate task panicked: {err}")),
            }
            if let Some(file) = pending_iter.next() {
                spawn_file_quality_rerate_worker(
                    &mut join_set,
                    paths.clone(),
                    manifest.clone(),
                    file,
                    pass_index,
                    config.clone(),
                );
                active += 1;
            }
        }
        if !failures.is_empty() {
            for failure in &failures {
                eprintln!("file quality rerate failure: {failure}");
            }
            bail!("file quality rerate failed for {} file(s)", failures.len());
        }
    }
    let mut ratings = Vec::new();
    for file in &manifest.files {
        let artifact_dir = file_quality_file_path(paths, pass_index, file);
        let rating_json = artifact_dir.join("rating.json");
        require_nonempty_file(&rating_json)?;
        let score = read_file_quality_score(&rating_json)?;
        ratings.push(FileQualityRatingState {
            path: file.path.clone(),
            score_out_of_10: score,
            status: StageStatus::Complete,
            artifact_dir: crate::audit_everything::path_display(&artifact_dir),
            note: score.map(|score| format!("rerated {score:.1}/10")),
        });
    }

    let below = ratings
        .iter()
        .filter(|rating| rating.score_out_of_10.unwrap_or(0.0) < FILE_QUALITY_ACCEPT_SCORE)
        .count();
    Ok(FileQualityPassState {
        pass_index,
        status: if below == 0 {
            StageStatus::Complete
        } else {
            StageStatus::Running
        },
        artifact_dir: crate::audit_everything::path_display(&pass_dir),
        ratings,
        note: Some(format!(
            "{below} file(s) below {FILE_QUALITY_ACCEPT_SCORE:.0}/10 on rerating"
        )),
    })
}

fn spawn_file_quality_rerate_worker(
    join_set: &mut JoinSet<Result<PathBuf>>,
    paths: RunPaths,
    manifest: EverythingManifest,
    file: FileState,
    pass_index: usize,
    config: PhaseConfig,
) {
    join_set.spawn(async move {
        let artifact_dir = file_quality_file_path(&paths, pass_index, &file);
        let prompt = build_file_quality_rerate_prompt(&paths, &manifest, &file, pass_index);
        let phase_slug = format!(
            "file-quality-rerate-{pass_index}-{}",
            file_artifact_slug(&file.path, &file.content_hash)
        );
        run_codex_phase_for_artifact(&paths, &artifact_dir, &phase_slug, &prompt, &config).await?;
        let rating_json = artifact_dir.join("rating.json");
        require_nonempty_file(&rating_json)?;
        Ok(rating_json)
    });
}

async fn run_file_quality_deliverables(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    pass_index: usize,
    ratings: &[FileQualityRatingState],
    config: &PhaseConfig,
    workers: usize,
) -> Result<()> {
    println!(
        "file quality: pass {pass_index} deliverables {} file(s), {} worker(s)",
        ratings.len(),
        workers
    );
    let mut pending = ratings.iter().cloned();
    let mut join_set = JoinSet::new();
    let mut active = 0usize;
    for _ in 0..workers {
        if let Some(rating) = pending.next() {
            spawn_file_quality_deliverable_worker(
                &mut join_set,
                paths.clone(),
                manifest.clone(),
                rating,
                pass_index,
                config.clone(),
            );
            active += 1;
        }
    }
    let mut failures = Vec::new();
    while active > 0 {
        let Some(result) = join_set.join_next().await else {
            break;
        };
        active -= 1;
        match result {
            Ok(Ok(path)) => println!("file quality deliverable: {}", path.display()),
            Ok(Err(err)) => failures.push(format!("{err:#}")),
            Err(err) => failures.push(format!("file-quality deliverable task panicked: {err}")),
        }
        if let Some(rating) = pending.next() {
            spawn_file_quality_deliverable_worker(
                &mut join_set,
                paths.clone(),
                manifest.clone(),
                rating,
                pass_index,
                config.clone(),
            );
            active += 1;
        }
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("file quality deliverable failure: {failure}");
        }
        bail!(
            "file quality deliverables failed for {} file(s)",
            failures.len()
        );
    }
    Ok(())
}

fn spawn_file_quality_deliverable_worker(
    join_set: &mut JoinSet<Result<PathBuf>>,
    paths: RunPaths,
    manifest: EverythingManifest,
    rating: FileQualityRatingState,
    pass_index: usize,
    config: PhaseConfig,
) {
    join_set.spawn(async move {
        let Some(file) = manifest.files.iter().find(|file| file.path == rating.path) else {
            bail!("quality rating referenced missing file `{}`", rating.path);
        };
        let artifact_dir = PathBuf::from(&rating.artifact_dir);
        let prompt =
            build_file_quality_deliverables_prompt(&paths, &manifest, file, &rating, pass_index);
        let phase_slug = format!(
            "file-quality-deliverables-{pass_index}-{}",
            file_artifact_slug(&file.path, &file.content_hash)
        );
        run_codex_phase_for_artifact(&paths, &artifact_dir, &phase_slug, &prompt, &config).await?;
        let deliverables = artifact_dir.join("deliverables.md");
        require_nonempty_file(&deliverables)?;
        Ok(deliverables)
    });
}

pub(crate) fn next_file_quality_pass_index(manifest: &EverythingManifest) -> usize {
    manifest
        .file_quality_passes
        .iter()
        .map(|pass| pass.pass_index)
        .max()
        .unwrap_or(0)
        + 1
}

pub(crate) fn require_file_quality_acceptance(manifest: &EverythingManifest) -> Result<()> {
    if manifest.files.is_empty() {
        return Ok(());
    }
    if !matches!(manifest.file_quality.status, StageStatus::Complete) {
        bail!(
            "file-quality gate is not complete; auto audit requires every file to rerate at least {FILE_QUALITY_ACCEPT_SCORE:.0}/10 before successful completion"
        );
    }
    let Some(pass) = latest_file_quality_pass(manifest) else {
        bail!("file-quality gate is marked complete but has no recorded rating pass");
    };
    let ratings_by_path = pass
        .ratings
        .iter()
        .map(|rating| (rating.path.as_str(), rating))
        .collect::<std::collections::BTreeMap<_, _>>();
    let missing = manifest
        .files
        .iter()
        .filter(|file| !ratings_by_path.contains_key(file.path.as_str()))
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "file-quality gate is missing rating(s) for {} file(s): {}",
            missing.len(),
            missing
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let below = pass
        .ratings
        .iter()
        .filter(|rating| rating.score_out_of_10.unwrap_or(0.0) < FILE_QUALITY_ACCEPT_SCORE)
        .map(|rating| {
            format!(
                "{} ({})",
                rating.path,
                rating
                    .score_out_of_10
                    .map(|score| format!("{score:.1}/10"))
                    .unwrap_or_else(|| "unknown".to_string())
            )
        })
        .collect::<Vec<_>>();
    if !below.is_empty() {
        bail!(
            "file-quality gate still has {} file(s) below {FILE_QUALITY_ACCEPT_SCORE:.0}/10: {}",
            below.len(),
            below
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

fn latest_file_quality_pass(manifest: &EverythingManifest) -> Option<&FileQualityPassState> {
    manifest
        .file_quality_passes
        .iter()
        .max_by_key(|pass| pass.pass_index)
}

fn read_file_quality_score(path: &Path) -> Result<Option<f64>> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(value
        .get("score_out_of_10")
        .or_else(|| value.get("rerated_score_out_of_10"))
        .or_else(|| value.get("score"))
        .and_then(json_number_or_string))
}

fn json_number_or_string(value: &serde_json::Value) -> Option<f64> {
    value.as_f64().or_else(|| {
        value
            .as_str()
            .and_then(|score| score.trim().parse::<f64>().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::{read_file_quality_score, require_file_quality_acceptance};
    use crate::audit_everything::manifest::{
        FileQualityPassState, FileQualityRatingState, FileState, StageStatus,
    };
    use crate::audit_everything::tests::{group_for_test, manifest_with_groups};
    use std::fs;

    #[test]
    fn file_quality_score_parser_accepts_number_and_string_scores() {
        let dir =
            std::env::temp_dir().join(format!("auto-audit-quality-score-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let numeric = dir.join("numeric.json");
        fs::write(&numeric, r#"{"score_out_of_10":8.75}"#).expect("failed to write score");
        assert_eq!(read_file_quality_score(&numeric).unwrap(), Some(8.75));

        let string = dir.join("string.json");
        fs::write(&string, r#"{"rerated_score_out_of_10":"9.5"}"#).expect("failed to write score");
        assert_eq!(read_file_quality_score(&string).unwrap(), Some(9.5));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn file_quality_acceptance_requires_complete_nine_plus_ratings() {
        let mut manifest = manifest_with_groups(vec![group_for_test("src", &["src/lib.rs"])]);
        manifest.files = vec![
            FileState {
                path: "src/lib.rs".to_string(),
                group: "src".to_string(),
                content_hash: "hash-a".to_string(),
                artifact_dir: "artifact-a".to_string(),
                status: StageStatus::Complete,
            },
            FileState {
                path: "src/other.rs".to_string(),
                group: "src".to_string(),
                content_hash: "hash-b".to_string(),
                artifact_dir: "artifact-b".to_string(),
                status: StageStatus::Complete,
            },
        ];
        manifest.file_quality.status = StageStatus::Complete;
        manifest.file_quality_passes = vec![FileQualityPassState {
            pass_index: 1,
            status: StageStatus::Complete,
            artifact_dir: "quality/pass-01".to_string(),
            ratings: vec![FileQualityRatingState {
                path: "src/lib.rs".to_string(),
                score_out_of_10: Some(9.0),
                status: StageStatus::Complete,
                artifact_dir: "quality/pass-01/src-lib".to_string(),
                note: None,
            }],
            note: None,
        }];
        let missing = require_file_quality_acceptance(&manifest).unwrap_err();
        assert!(format!("{missing:#}").contains("missing rating"));

        manifest.file_quality_passes[0]
            .ratings
            .push(FileQualityRatingState {
                path: "src/other.rs".to_string(),
                score_out_of_10: Some(8.9),
                status: StageStatus::Complete,
                artifact_dir: "quality/pass-01/src-other".to_string(),
                note: None,
            });
        let below = require_file_quality_acceptance(&manifest).unwrap_err();
        assert!(format!("{below:#}").contains("below 9"));

        manifest.file_quality_passes[0].ratings[1].score_out_of_10 = Some(9.1);
        require_file_quality_acceptance(&manifest).expect("all files should meet quality floor");
    }
}
