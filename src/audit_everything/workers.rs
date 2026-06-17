//! Work-stealing worker pools and the Codex/Claude phase runners shared by
//! every audit phase.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tokio::task::JoinSet;

use crate::audit_everything::inventory::prompt_file_body;
use crate::audit_everything::manifest::write_manifest;
use crate::audit_everything::manifest::{EverythingManifest, FileState, GroupState, StageStatus};
use crate::audit_everything::prompts::{build_file_prompt, build_synthesis_prompt};
use crate::audit_everything::require_nonempty_file;
use crate::audit_everything::run_paths::{PhaseConfig, RunPaths};
use crate::codex_exec::run_codex_exec_max_context;
use crate::util::atomic_write;

pub(crate) async fn run_group_workers(
    paths: &RunPaths,
    pending: Vec<GroupState>,
    workers: usize,
    config: PhaseConfig,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    let mut join_set = JoinSet::new();
    let mut pending_iter = pending.into_iter();
    let mut active = 0usize;
    for _ in 0..workers {
        if let Some(group) = pending_iter.next() {
            spawn_group_worker(&mut join_set, paths, group, &config);
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
            Ok(Ok(slug)) => {
                if let Some(group) = manifest.groups.iter_mut().find(|group| group.slug == slug) {
                    group.synthesis_status = StageStatus::Complete;
                }
                write_manifest(paths, manifest)?;
            }
            Ok(Err(err)) => failures.push(format!("{err:#}")),
            Err(err) => failures.push(format!("group worker task panicked: {err}")),
        }
        if let Some(group) = pending_iter.next() {
            spawn_group_worker(&mut join_set, paths, group, &config);
            active += 1;
        }
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("group phase failure: {failure}");
        }
        bail!("group phase failed for {} group(s)", failures.len());
    }
    write_manifest(paths, manifest)?;
    Ok(())
}

fn spawn_group_worker(
    join_set: &mut JoinSet<Result<String>>,
    paths: &RunPaths,
    group: GroupState,
    config: &PhaseConfig,
) {
    let paths_clone = paths.clone();
    let config_clone = config.clone();
    join_set.spawn(async move { run_one_group_phase(&paths_clone, &group, &config_clone).await });
}

async fn run_one_file_analysis(
    paths: &RunPaths,
    file: &FileState,
    context: &str,
    config: &PhaseConfig,
) -> Result<String> {
    let artifact_dir = PathBuf::from(&file.artifact_dir);
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;
    let file_path = paths.worktree_root.join(&file.path);
    let file_body = prompt_file_body(&file_path)?;
    let prompt = build_file_prompt(file, context, &file_body);
    run_codex_phase_for_artifact(paths, &artifact_dir, "first-pass", &prompt, config).await?;
    require_nonempty_file(&artifact_dir.join("analysis.md"))?;
    Ok(file.path.clone())
}

pub(crate) fn spawn_file_worker(
    join_set: &mut JoinSet<Result<String>>,
    paths: &RunPaths,
    file: FileState,
    context: &str,
    config: &PhaseConfig,
) {
    let paths_clone = paths.clone();
    let context_clone = context.to_string();
    let config_clone = config.clone();
    join_set.spawn(async move {
        run_one_file_analysis(&paths_clone, &file, &context_clone, &config_clone).await
    });
}

async fn run_one_group_phase(
    paths: &RunPaths,
    group: &GroupState,
    config: &PhaseConfig,
) -> Result<String> {
    let report_path = PathBuf::from(&group.report_path);
    require_nonempty_file(&report_path)?;
    let prompt = build_synthesis_prompt(paths, group);
    run_codex_phase_for_artifact(
        paths,
        report_path.parent().unwrap_or(&paths.report_root),
        "synthesis",
        &prompt,
        config,
    )
    .await?;
    require_nonempty_file(&report_path)?;
    Ok(group.slug.clone())
}

pub(crate) async fn run_codex_phase(
    paths: &RunPaths,
    phase_slug: &str,
    prompt: &str,
    config: &PhaseConfig,
) -> Result<()> {
    run_codex_phase_for_artifact(
        paths,
        &paths.host_root.join("logs"),
        phase_slug,
        prompt,
        config,
    )
    .await
}

pub(crate) async fn run_codex_phase_for_artifact(
    paths: &RunPaths,
    artifact_dir: &Path,
    phase_slug: &str,
    prompt: &str,
    config: &PhaseConfig,
) -> Result<()> {
    fs::create_dir_all(artifact_dir)
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;
    let prompt_path = artifact_dir.join(format!("{phase_slug}-prompt.md"));
    let stderr_path = artifact_dir.join(format!("{phase_slug}-stderr.log"));
    let stdout_path = artifact_dir.join(format!("{phase_slug}-stdout.log"));
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    let claude_route = crate::claude_exec::looks_like_claude_model(&config.model);
    println!(
        "phase:       {phase_slug} | backend: {} | model: {} | effort: {} | prompt: {}",
        if claude_route { "claude" } else { "codex" },
        config.model,
        config.effort,
        prompt_path.display()
    );
    let status = if claude_route {
        crate::claude_exec::run_claude_exec(
            &paths.worktree_root,
            prompt,
            &config.model,
            &config.effort,
            None,
            &stderr_path,
            Some(&stdout_path),
            phase_slug,
        )
        .await?
    } else {
        run_codex_exec_max_context(
            &paths.worktree_root,
            prompt,
            &config.model,
            &config.effort,
            &config.codex_bin,
            &stderr_path,
            Some(&stdout_path),
            phase_slug,
        )
        .await?
    };
    if !status.success() {
        bail!(
            "professional audit phase `{phase_slug}` failed with status {status}; see {}",
            stderr_path.display()
        );
    }
    Ok(())
}
