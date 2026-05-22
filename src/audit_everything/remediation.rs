//! Remediation planning, the dependency-graph scheduler, and isolated lane
//! execution and landing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tokio::task::JoinSet;

use crate::audit_everything::git::{
    audit_lane_changed_files, cherry_pick_lane_range, clone_audit_lane_repo, commit_worktree_changes,
    fetch_lane_commit, git_ref_is_ancestor,
};
use crate::audit_everything::manifest::write_manifest;
use crate::audit_everything::manifest::{EverythingManifest, RemediationTaskState, StageStatus};
use crate::audit_everything::prompts::selected_skill_names_for_file;
use crate::audit_everything::prompts::{
    is_docs_or_devex_path, is_release_or_deploy_path, is_rust_or_backend_path, is_test_or_perf_path,
    push_unique, render_skill_policy,
};
use crate::audit_everything::run_paths::{
    remediation_plan_json_path, remediation_plan_markdown_path, PhaseConfig, RunPaths,
};
use crate::audit_everything::worktree::pause_requested;
use crate::audit_everything::{one_line, path_display};
use crate::codex_exec::run_codex_exec_max_context;
use crate::util::{atomic_write, git_stdout, run_git};
use crate::AuditArgs;

struct RemediationLaneResult {
    task: RemediationTaskState,
    error: Option<String>,
}

pub(crate) struct RemediationSchedulerChoice {
    pub(crate) index: usize,
    pub(crate) unmet_dependencies: Vec<String>,
}

pub(crate) async fn run_remediation_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    reset_interrupted_remediation_tasks(paths, manifest)?;
    let pending_count = manifest
        .remediation_tasks
        .iter()
        .filter(|task| !matches!(task.status, StageStatus::Complete | StageStatus::Skipped))
        .count();
    if pending_count == 0 {
        println!("remediation: complete (resume)");
        return Ok(());
    }
    let config = PhaseConfig {
        model: args.remediation_model.clone(),
        effort: args.remediation_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let workers = args.remediation_threads.clamp(1, 10);
    println!(
        "remediation: {} task(s), {} lane(s)",
        pending_count, workers
    );
    run_remediation_lanes(paths, workers, config, manifest).await
}

pub(crate) fn run_remediation_plan_phase(
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    manifest.remediation_plan.status = StageStatus::Running;
    manifest.remediation_tasks = build_remediation_tasks(paths, manifest)?;
    write_remediation_plan_files(paths, manifest)?;
    manifest.remediation_plan.status = StageStatus::Complete;
    manifest.remediation_plan.artifact = Some(path_display(&remediation_plan_markdown_path(paths)));
    manifest.remediation_plan.note = Some(format!(
        "{} task(s), {} dependency edge(s)",
        manifest.remediation_tasks.len(),
        manifest
            .remediation_tasks
            .iter()
            .map(|task| task.dependencies.len())
            .sum::<usize>()
    ));
    write_manifest(paths, manifest)?;
    commit_worktree_changes(paths, manifest)?;
    Ok(())
}

async fn run_remediation_lanes(
    paths: &RunPaths,
    workers: usize,
    config: PhaseConfig,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    let mut active = BTreeSet::<String>::new();
    let mut active_cycle_breakers = BTreeSet::<String>::new();
    let mut join_set = JoinSet::<RemediationLaneResult>::new();
    let mut failures = Vec::new();

    loop {
        let paused = pause_requested(paths);
        if paused && active.is_empty() {
            println!(
                "professional audit pause request observed at {}; scheduler is idle",
                paths.pause_path.display()
            );
            persist_remediation_progress(paths, manifest)?;
            return Ok(());
        }

        while !paused && active.len() < workers {
            let cycle_breaker_allowed =
                active.is_empty() || active.iter().all(|id| active_cycle_breakers.contains(id));
            let Some(choice) =
                next_remediation_scheduler_choice(manifest, &active, cycle_breaker_allowed)
            else {
                break;
            };
            let index = choice.index;
            match try_harvest_existing_remediation_lane(paths, manifest, index) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(err) => {
                    let task_id = manifest.remediation_tasks[index].id.clone();
                    let error = format!("{err:#}");
                    manifest.remediation_tasks[index].status = StageStatus::Failed;
                    manifest.remediation_tasks[index].note = Some(error.clone());
                    failures.push(format!("{task_id}: {error}"));
                    write_manifest(paths, manifest)?;
                    continue;
                }
            }
            let task_id = manifest.remediation_tasks[index].id.clone();
            manifest.remediation_tasks[index].status = StageStatus::Running;
            manifest.remediation_tasks[index].note =
                Some(if choice.unmet_dependencies.is_empty() {
                    "lane dispatched".to_string()
                } else {
                    format!(
                        "dependency cycle breaker: lane dispatched despite unmet dependencies: {}",
                        choice.unmet_dependencies.join(", ")
                    )
                });
            write_manifest(paths, manifest)?;

            let mut task = manifest.remediation_tasks[index].clone();
            let paths_clone = paths.clone();
            let config_clone = config.clone();
            if !choice.unmet_dependencies.is_empty() {
                active_cycle_breakers.insert(task_id.clone());
            }
            active.insert(task_id);
            join_set.spawn(async move {
                if let Err(err) =
                    run_one_remediation_lane(&paths_clone, &mut task, &config_clone).await
                {
                    return RemediationLaneResult {
                        task,
                        error: Some(format!("{err:#}")),
                    };
                }
                RemediationLaneResult { task, error: None }
            });
        }

        if active.is_empty() {
            break;
        }

        let Some(result) = join_set.join_next().await else {
            bail!("remediation lane scheduler lost all workers while tasks were active");
        };
        let lane_result = match result {
            Ok(result) => result,
            Err(err) => {
                failures.push(format!("lane task panicked: {err}"));
                continue;
            }
        };
        active.remove(&lane_result.task.id);
        active_cycle_breakers.remove(&lane_result.task.id);
        let task_index = manifest
            .remediation_tasks
            .iter()
            .position(|task| task.id == lane_result.task.id)
            .with_context(|| format!("missing remediation task {}", lane_result.task.id))?;
        manifest.remediation_tasks[task_index].base_commit = lane_result.task.base_commit.clone();

        if let Some(error) = lane_result.error {
            manifest.remediation_tasks[task_index].status = StageStatus::Failed;
            manifest.remediation_tasks[task_index].note = Some(error.clone());
            failures.push(format!(
                "{}: {error}",
                manifest.remediation_tasks[task_index].id
            ));
            persist_remediation_progress(paths, manifest)?;
            continue;
        }

        match land_remediation_lane(paths, &lane_result.task) {
            Ok(changed_files) => {
                manifest.remediation_tasks[task_index].status = StageStatus::Complete;
                manifest.remediation_tasks[task_index].note =
                    Some(format!("landed {} changed file(s)", changed_files.len()));
                if let Some(group) = manifest
                    .groups
                    .iter_mut()
                    .find(|group| group.name == lane_result.task.group)
                {
                    group.remediation_status = StageStatus::Complete;
                }
                persist_remediation_progress(paths, manifest)?;
            }
            Err(err) => {
                let error = format!("{err:#}");
                manifest.remediation_tasks[task_index].status = StageStatus::Failed;
                manifest.remediation_tasks[task_index].note = Some(error.clone());
                failures.push(format!(
                    "{}: {error}",
                    manifest.remediation_tasks[task_index].id
                ));
                persist_remediation_progress(paths, manifest)?;
            }
        }
    }

    persist_remediation_progress(paths, manifest)?;
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("remediation failure: {failure}");
        }
        bail!("remediation failed for {} task(s)", failures.len());
    }
    if let Some(failed) = manifest
        .remediation_tasks
        .iter()
        .find(|task| matches!(task.status, StageStatus::Failed))
    {
        let note = failed.note.as_deref().unwrap_or("no failure note recorded");
        bail!(
            "remediation stopped with failed task `{}` (`{}`): {}",
            failed.id,
            failed.group,
            one_line(note)
        );
    }
    if let Some(blocked) = first_blocked_remediation_task(manifest) {
        bail!(
            "remediation stopped with no dependency-ready lane for `{}`; dependencies: {}",
            blocked.id,
            blocked.dependencies.join(", ")
        );
    }
    Ok(())
}

fn persist_remediation_progress(
    paths: &RunPaths,
    manifest: &EverythingManifest,
) -> Result<()> {
    write_remediation_plan_files(paths, manifest)?;
    write_manifest(paths, manifest)?;
    commit_worktree_changes(paths, manifest)
}

fn build_remediation_tasks(
    paths: &RunPaths,
    manifest: &EverythingManifest,
) -> Result<Vec<RemediationTaskState>> {
    let old_by_group = manifest
        .remediation_tasks
        .iter()
        .map(|task| (task.group.clone(), task.clone()))
        .collect::<BTreeMap<_, _>>();
    let dependency_groups = remediation_dependency_groups(&paths.worktree_root, manifest)?;
    let group_to_id = manifest
        .groups
        .iter()
        .enumerate()
        .map(|(index, group)| (group.name.clone(), format!("AUD-REM-{:03}", index + 1)))
        .collect::<BTreeMap<_, _>>();
    let mut tasks = Vec::new();
    for (index, group) in manifest.groups.iter().enumerate() {
        let id = group_to_id
            .get(&group.name)
            .cloned()
            .unwrap_or_else(|| format!("AUD-REM-{:03}", index + 1));
        let lane_index = index + 1;
        let lane_root = paths
            .host_root
            .join("remediation-lanes")
            .join(format!("lane-{lane_index}"));
        let dependencies = dependency_groups
            .get(&group.name)
            .into_iter()
            .flat_map(|groups| groups.iter())
            .filter_map(|group_name| group_to_id.get(group_name))
            .filter(|dependency_id| *dependency_id != &id)
            .cloned()
            .collect::<Vec<_>>();
        let previous = old_by_group.get(&group.name);
        let status = match previous.map(|task| task.status) {
            Some(StageStatus::Complete) => StageStatus::Complete,
            Some(StageStatus::Skipped) => StageStatus::Skipped,
            Some(StageStatus::Failed) => StageStatus::Failed,
            _ if matches!(group.remediation_status, StageStatus::Complete) => StageStatus::Complete,
            _ => StageStatus::Pending,
        };
        tasks.push(RemediationTaskState {
            id,
            group: group.name.clone(),
            slug: group.slug.clone(),
            report_path: group.report_path.clone(),
            owned_paths: group.files.clone(),
            dependencies,
            lane_index,
            lane_root: path_display(&lane_root),
            lane_repo_root: path_display(&lane_root.join("repo")),
            base_commit: previous.and_then(|task| task.base_commit.clone()),
            status,
            note: previous.and_then(|task| task.note.clone()),
        });
    }
    Ok(tasks)
}

pub(crate) fn remediation_dependency_groups(
    repo_root: &Path,
    manifest: &EverythingManifest,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut dependencies = manifest
        .groups
        .iter()
        .map(|group| (group.name.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let source_groups = manifest
        .groups
        .iter()
        .filter(|group| group.files.iter().any(|path| is_rust_or_backend_path(path)))
        .map(|group| group.name.clone())
        .collect::<Vec<_>>();
    let test_groups = manifest
        .groups
        .iter()
        .filter(|group| group.files.iter().any(|path| is_test_or_perf_path(path)))
        .map(|group| group.name.clone())
        .collect::<Vec<_>>();

    for group in &manifest.groups {
        if group
            .files
            .iter()
            .any(|path| is_docs_or_devex_path(path) || is_context_path(path))
        {
            extend_group_dependencies(&mut dependencies, &group.name, &source_groups);
            extend_group_dependencies(&mut dependencies, &group.name, &test_groups);
        }
        if group.files.iter().any(|path| is_test_or_perf_path(path)) {
            extend_group_dependencies(&mut dependencies, &group.name, &source_groups);
        }
        if group
            .files
            .iter()
            .any(|path| is_release_or_deploy_path(path))
        {
            extend_group_dependencies(&mut dependencies, &group.name, &source_groups);
            extend_group_dependencies(&mut dependencies, &group.name, &test_groups);
        }
    }

    for (group, deps) in cargo_group_dependencies(repo_root, manifest)? {
        extend_group_dependencies(&mut dependencies, &group, &deps);
    }
    for (group, deps) in &mut dependencies {
        deps.remove(group);
    }
    Ok(dependencies)
}

fn is_context_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path == "agents.md"
        || path == "architecture.md"
        || path == "claude.md"
        || path.starts_with("doctrine/")
        || path.starts_with("specs/")
        || path.starts_with("plans/")
        || path.contains("architecture")
}

fn extend_group_dependencies(
    dependencies: &mut BTreeMap<String, BTreeSet<String>>,
    group: &str,
    deps: &[String],
) {
    if let Some(existing) = dependencies.get_mut(group) {
        existing.extend(deps.iter().filter(|dep| dep.as_str() != group).cloned());
    }
}

fn cargo_group_dependencies(
    repo_root: &Path,
    manifest: &EverythingManifest,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut package_to_group = BTreeMap::new();
    let mut group_to_manifest = BTreeMap::new();
    for group in &manifest.groups {
        let root = if group.name == "." {
            repo_root.to_path_buf()
        } else {
            repo_root.join(&group.name)
        };
        let cargo = root.join("Cargo.toml");
        if let Ok(raw) = fs::read_to_string(&cargo) {
            if let Ok(value) = raw.parse::<toml::Value>() {
                if let Some(name) = value
                    .get("package")
                    .and_then(|pkg| pkg.get("name"))
                    .and_then(|name| name.as_str())
                {
                    package_to_group.insert(name.to_string(), group.name.clone());
                    group_to_manifest.insert(group.name.clone(), value);
                }
            }
        }
    }

    let mut dependencies = BTreeMap::new();
    for (group, manifest_value) in group_to_manifest {
        let mut deps = Vec::new();
        for table_name in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(table) = manifest_value
                .get(table_name)
                .and_then(|value| value.as_table())
            {
                for package in table.keys() {
                    if let Some(dep_group) = package_to_group.get(package) {
                        if dep_group != &group && !deps.contains(dep_group) {
                            deps.push(dep_group.clone());
                        }
                    }
                }
            }
        }
        dependencies.insert(group, deps);
    }
    Ok(dependencies)
}

pub(crate) fn write_remediation_plan_files(
    paths: &RunPaths,
    manifest: &EverythingManifest,
) -> Result<()> {
    let mut body = String::new();
    body.push_str("# Remediation Plan\n\n");
    body.push_str("Generated from synthesized audit reports. The host scheduler owns this file; remediation lanes update their assigned group report and commit source/doc/test changes in isolated worktrees.\n\n");
    body.push_str("## Debt And Architecture Contract\n\n");
    body.push_str("Remediation is allowed to remove proved-dead code, retire deprecated paths, consolidate duplicates, simplify agent-written filler, and deepen module boundaries when the group report records repository evidence. Follow `CODEBASE-IMPROVEMENT-POLICY.md` for proof requirements and debt classes.\n\n");
    body.push_str("## Tasks\n\n");
    for task in &manifest.remediation_tasks {
        let deps = if task.dependencies.is_empty() {
            "none".to_string()
        } else {
            task.dependencies.join(", ")
        };
        body.push_str(&format!(
            "### {} `{}`\n\n- Status: {:?}\n- Group: `{}`\n- Report: `{}`\n- Lane: `{}`\n- Dependencies: {}\n",
            task.id, task.slug, task.status, task.group, task.report_path, task.lane_root, deps
        ));
        if let Some(note) = task.note.as_deref().filter(|note| !note.trim().is_empty()) {
            body.push_str(&format!("- Note: {}\n", note.trim().replace('\n', " ")));
        }
        body.push_str("- Owned paths:\n");
        for path in task.owned_paths.iter().take(200) {
            body.push_str(&format!("  - `{path}`\n"));
        }
        if task.owned_paths.len() > 200 {
            body.push_str(&format!(
                "  - _{} additional paths omitted from this summary_\n",
                task.owned_paths.len() - 200
            ));
        }
        body.push('\n');
    }
    atomic_write(&remediation_plan_markdown_path(paths), body.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            remediation_plan_markdown_path(paths).display()
        )
    })?;
    atomic_write(
        &remediation_plan_json_path(paths),
        &serde_json::to_vec_pretty(&manifest.remediation_tasks)?,
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            remediation_plan_json_path(paths).display()
        )
    })
}

fn reset_interrupted_remediation_tasks(
    _paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    for task in &mut manifest.remediation_tasks {
        if matches!(task.status, StageStatus::Running) {
            let lane_root = PathBuf::from(&task.lane_root);
            let lane_repo_root = PathBuf::from(&task.lane_repo_root);
            if lane_repo_root.join(".git").exists() {
                let status = git_stdout(&lane_repo_root, ["status", "--short"])?;
                let head = git_stdout(&lane_repo_root, ["rev-parse", "HEAD"])?
                    .trim()
                    .to_string();
                let base_commit = task
                    .base_commit
                    .clone()
                    .or_else(|| infer_existing_remediation_lane_base(&lane_repo_root).ok());
                if status.trim().is_empty() && Some(head.as_str()) != base_commit.as_deref() {
                    task.status = StageStatus::Pending;
                    task.base_commit = base_commit;
                    task.note = Some(
                        "reset from interrupted lane; existing lane commit retained".to_string(),
                    );
                    continue;
                }
            }
            if lane_root.exists() {
                fs::remove_dir_all(&lane_root).with_context(|| {
                    format!("failed to remove interrupted {}", lane_root.display())
                })?;
            }
            task.status = StageStatus::Pending;
            task.base_commit = None;
            task.note = Some("reset from interrupted lane".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
fn next_ready_remediation_task_index(
    manifest: &EverythingManifest,
    active: &BTreeSet<String>,
) -> Option<usize> {
    next_ready_remediation_task_index_with_complete(
        manifest,
        active,
        &complete_remediation_task_ids(manifest),
    )
}

pub(crate) fn next_remediation_scheduler_choice(
    manifest: &EverythingManifest,
    active: &BTreeSet<String>,
    cycle_breaker_allowed: bool,
) -> Option<RemediationSchedulerChoice> {
    let complete = complete_remediation_task_ids(manifest);
    if let Some(index) =
        next_ready_remediation_task_index_with_complete(manifest, active, &complete)
    {
        return Some(RemediationSchedulerChoice {
            index,
            unmet_dependencies: Vec::new(),
        });
    }
    if !cycle_breaker_allowed {
        return None;
    }
    manifest
        .remediation_tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| is_schedulable_remediation_task(task, active))
        .map(|(index, task)| (index, unmet_remediation_dependencies(task, &complete)))
        .min_by(|(left_index, left_unmet), (right_index, right_unmet)| {
            left_unmet
                .len()
                .cmp(&right_unmet.len())
                .then_with(|| left_index.cmp(right_index))
        })
        .map(|(index, unmet_dependencies)| RemediationSchedulerChoice {
            index,
            unmet_dependencies,
        })
}

fn next_ready_remediation_task_index_with_complete(
    manifest: &EverythingManifest,
    active: &BTreeSet<String>,
    complete: &BTreeSet<&str>,
) -> Option<usize> {
    manifest
        .remediation_tasks
        .iter()
        .enumerate()
        .find(|(_, task)| {
            is_schedulable_remediation_task(task, active)
                && task
                    .dependencies
                    .iter()
                    .all(|dependency| complete.contains(dependency.as_str()))
        })
        .map(|(index, _)| index)
}

pub(crate) fn complete_remediation_task_ids(manifest: &EverythingManifest) -> BTreeSet<&str> {
    let complete = manifest
        .remediation_tasks
        .iter()
        .filter(|task| matches!(task.status, StageStatus::Complete | StageStatus::Skipped))
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();
    complete
}

fn is_schedulable_remediation_task(
    task: &RemediationTaskState,
    active: &BTreeSet<String>,
) -> bool {
    !active.contains(&task.id)
        && !matches!(
            task.status,
            StageStatus::Complete
                | StageStatus::Skipped
                | StageStatus::Running
                | StageStatus::Failed
        )
}

pub(crate) fn unmet_remediation_dependencies(
    task: &RemediationTaskState,
    complete: &BTreeSet<&str>,
) -> Vec<String> {
    task.dependencies
        .iter()
        .filter(|dependency| !complete.contains(dependency.as_str()))
        .cloned()
        .collect()
}

fn first_blocked_remediation_task(
    manifest: &EverythingManifest,
) -> Option<&RemediationTaskState> {
    manifest
        .remediation_tasks
        .iter()
        .find(|task| !matches!(task.status, StageStatus::Complete | StageStatus::Skipped))
}

fn try_harvest_existing_remediation_lane(
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
    task_index: usize,
) -> Result<bool> {
    let task = manifest.remediation_tasks[task_index].clone();
    let lane_repo_root = PathBuf::from(&task.lane_repo_root);
    if !lane_repo_root.join(".git").exists() || task.base_commit.is_none() {
        return Ok(false);
    }
    let status = git_stdout(&lane_repo_root, ["status", "--short"])?;
    let head = git_stdout(&lane_repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    if !status.trim().is_empty() || Some(head.as_str()) == task.base_commit.as_deref() {
        return Ok(false);
    }
    let changed_files = land_remediation_lane(paths, &task)?;
    manifest.remediation_tasks[task_index].status = StageStatus::Complete;
    manifest.remediation_tasks[task_index].note = Some(format!(
        "resumed and landed {} changed file(s)",
        changed_files.len()
    ));
    if let Some(group) = manifest
        .groups
        .iter_mut()
        .find(|group| group.name == task.group)
    {
        group.remediation_status = StageStatus::Complete;
    }
    write_manifest(paths, manifest)?;
    Ok(true)
}

async fn run_one_remediation_lane(
    paths: &RunPaths,
    task: &mut RemediationTaskState,
    config: &PhaseConfig,
) -> Result<()> {
    prepare_remediation_lane_repo(paths, task)?;
    let lane_root = PathBuf::from(&task.lane_root);
    let lane_repo_root = PathBuf::from(&task.lane_repo_root);
    let prompt = build_remediation_lane_prompt(paths, task);
    let prompt_path = lane_root.join(format!("{}-prompt.md", task.id));
    let stderr_path = lane_root.join(format!("{}-stderr.log", task.id));
    let stdout_path = lane_root.join(format!("{}-stdout.log", task.id));
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    let claude_route = crate::claude_exec::looks_like_claude_model(&config.model);
    let context_label = format!("auto audit remediation {}", task.id);
    let status = if claude_route {
        crate::claude_exec::run_claude_exec(
            &lane_repo_root,
            &prompt,
            &config.model,
            &config.effort,
            None,
            &stderr_path,
            Some(&stdout_path),
            &context_label,
        )
        .await?
    } else {
        run_codex_exec_max_context(
            &lane_repo_root,
            &prompt,
            &config.model,
            &config.effort,
            &config.codex_bin,
            &stderr_path,
            Some(&stdout_path),
            &context_label,
        )
        .await?
    };
    if !status.success() {
        bail!(
            "remediation lane {} failed with status {status}; see {}",
            task.id,
            stderr_path.display()
        );
    }
    Ok(())
}

fn prepare_remediation_lane_repo(
    paths: &RunPaths,
    task: &mut RemediationTaskState,
) -> Result<()> {
    let lane_root = PathBuf::from(&task.lane_root);
    let lane_repo_root = PathBuf::from(&task.lane_repo_root);
    if lane_repo_root.join(".git").exists() {
        if task.base_commit.is_none() {
            task.base_commit = Some(infer_existing_remediation_lane_base(&lane_repo_root)?);
        }
        return Ok(());
    }
    if lane_root.exists() && !lane_repo_root.exists() {
        fs::remove_dir_all(&lane_root)
            .with_context(|| format!("failed to remove incomplete {}", lane_root.display()))?;
    }
    fs::create_dir_all(&lane_root)
        .with_context(|| format!("failed to create {}", lane_root.display()))?;
    let base_commit = git_stdout(&paths.worktree_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    task.base_commit = Some(base_commit);
    clone_audit_lane_repo(&paths.worktree_root, &lane_repo_root)?;
    Ok(())
}

fn infer_existing_remediation_lane_base(lane_repo_root: &Path) -> Result<String> {
    let branch = git_stdout(lane_repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string();
    if branch.is_empty() {
        return Ok(git_stdout(lane_repo_root, ["rev-parse", "HEAD"])?
            .trim()
            .to_string());
    }
    let remotes = git_stdout(lane_repo_root, ["remote"]).unwrap_or_default();
    let remote = if remotes.lines().any(|remote| remote.trim() == "canonical") {
        "canonical"
    } else {
        "origin"
    };
    let _ = run_git(lane_repo_root, ["fetch", "--quiet", remote, &branch]);
    let base = git_stdout(lane_repo_root, ["merge-base", "HEAD", "FETCH_HEAD"])?
        .trim()
        .to_string();
    if base.is_empty() {
        Ok(git_stdout(lane_repo_root, ["rev-parse", "HEAD"])?
            .trim()
            .to_string())
    } else {
        Ok(base)
    }
}

fn build_remediation_lane_prompt(paths: &RunPaths, task: &RemediationTaskState) -> String {
    let deps = if task.dependencies.is_empty() {
        "none".to_string()
    } else {
        task.dependencies.join(", ")
    };
    let owned_paths = task
        .owned_paths
        .iter()
        .map(|path| format!("- `{path}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let skill_policy = render_skill_policy_for_paths(&task.owned_paths);
    let lane_report = lane_report_path(paths, task);
    format!(
        r#"You are an isolated remediation lane for `auto audit --everything`.

Repository root for this lane: `{repo}`
Canonical audit worktree: `{canonical}` (read-only for this lane)
Task: `{task_id}`
Group: `{group}`
Lane report: `{lane_report}`
Canonical report: `{canonical_report}` (do not edit directly)
Dependencies already satisfied: {deps}

You are not alone in the audit. The host owns the dependency graph, landing, and `REMEDIATION-PLAN.md`.

Hard boundaries:
- Read `AGENTS.md`, `ARCHITECTURE.md`, `audit/everything/*/CONTEXT-BUNDLE.md`, the gstack skill policy, doctrine if present, and the assigned report.
- Read `audit/everything/*/CODEBASE-IMPROVEMENT-POLICY.md` and the assigned report's `## Debt Register` before editing.
- If this lane already contains partial work from an interrupted run, inspect it first and continue from that state instead of discarding it.
- Keep edits centered on the owned paths and directly necessary adjacent tests/docs.
- Do not write into the canonical audit worktree. Edit only files inside this lane repository; the host will cherry-pick your lane commit back.
- Do not edit `REMEDIATION-PLAN.md` or `REMEDIATION-PLAN.json`; the host updates those after landing.
- Do not push to any remote.
- Create one or more local git commits before finishing.
- Finish with `git status --short` clean.
- If validation is blocked by missing external infrastructure, print `AUTO_ENV_BLOCKER: <short reason>` and exit non-zero.
- If a validation command reports `0 tests`, do not count it as passing evidence.

Selected gstack lenses:
{skill_policy}

Owned paths:
{owned_paths}

Required work:
- Apply only recommendations from `{lane_report}` that are supported by repository evidence.
- Systematically address this lane's debt register: delete proved-dead code, remove deprecated paths, consolidate duplicates, simplify AI-slop, or deepen module boundaries when the report supplies enough evidence.
- Before deleting or retiring code, record deletion proof in `{lane_report}`: references/imports/exports checked, public API/CLI/operator/runtime impact, docs/config/generated bindings reviewed, and validation or characterization evidence.
- If proof is incomplete, leave the code in place and update `{lane_report}` with `leave_with_reason` and the exact missing evidence.
- Update `{lane_report}` with completed recommendations, changed files, validation commands, and remaining blockers.
- If your changes alter module responsibilities, runtime flows, user-facing behavior, operator workflows, or durable invariants, update the relevant architecture/docs files in the same lane, especially root `ARCHITECTURE.md` and focused docs under `docs/`.
- Run the narrowest meaningful validation for this group.
- Commit all lane changes locally with a message starting `audit: remediate {task_id}`.
"#,
        repo = task.lane_repo_root,
        canonical = paths.worktree_root.display(),
        task_id = task.id,
        group = task.group,
        lane_report = lane_report.display(),
        canonical_report = task.report_path,
        deps = deps,
        skill_policy = skill_policy,
        owned_paths = owned_paths,
    )
}

fn lane_report_path(paths: &RunPaths, task: &RemediationTaskState) -> PathBuf {
    let report_path = PathBuf::from(&task.report_path);
    match report_path.strip_prefix(&paths.worktree_root) {
        Ok(relative) => PathBuf::from(&task.lane_repo_root).join(relative),
        Err(_) => report_path,
    }
}

fn render_skill_policy_for_paths(paths: &[String]) -> String {
    let mut selected = Vec::new();
    push_unique(&mut selected, "review");
    push_unique(&mut selected, "health");
    push_unique(&mut selected, "investigate");
    push_unique(&mut selected, "careful");
    for path in paths {
        for skill in selected_skill_names_for_file(path) {
            push_unique(&mut selected, skill);
        }
    }
    render_skill_policy(&selected)
}

fn land_remediation_lane(
    paths: &RunPaths,
    task: &RemediationTaskState,
) -> Result<Vec<String>> {
    let lane_repo_root = PathBuf::from(&task.lane_repo_root);
    let base_commit = task
        .base_commit
        .as_deref()
        .context("remediation lane is missing base commit")?;
    let status = git_stdout(&lane_repo_root, ["status", "--short"])?;
    if !status.trim().is_empty() {
        bail!("lane {} left a dirty worktree:\n{}", task.id, status.trim());
    }
    let lane_head = git_stdout(&lane_repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    if lane_head == base_commit {
        bail!("lane {} exited without a local commit", task.id);
    }
    let changed_files = audit_lane_changed_files(&lane_repo_root, base_commit, &lane_head)?;
    restore_dirty_generated_reports(paths, &changed_files)?;
    fetch_lane_commit(&paths.worktree_root, &lane_repo_root, &lane_head)?;
    if !git_ref_is_ancestor(&paths.worktree_root, "FETCH_HEAD", "HEAD")? {
        cherry_pick_lane_range(&paths.worktree_root, base_commit, "FETCH_HEAD", true)?;
    }
    Ok(changed_files)
}

fn restore_dirty_generated_reports(
    paths: &RunPaths,
    changed_files: &[String],
) -> Result<()> {
    let report_prefix = format!("audit/everything/{}/reports/", report_run_id(paths));
    let dirty = git_stdout(&paths.worktree_root, ["status", "--porcelain", "--"])?;
    for path in changed_files
        .iter()
        .filter(|path| path.starts_with(&report_prefix))
    {
        if dirty
            .lines()
            .any(|line| line.get(3..) == Some(path.as_str()))
        {
            run_git(&paths.worktree_root, ["restore", "--", path])?;
        }
    }
    Ok(())
}

fn report_run_id(paths: &RunPaths) -> String {
    paths
        .report_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-run")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        build_remediation_lane_prompt, next_ready_remediation_task_index,
        next_remediation_scheduler_choice, remediation_dependency_groups,
    };
    use crate::audit_everything::manifest::{RemediationTaskState, StageStatus};
    use crate::audit_everything::run_paths::RunPaths;
    use crate::audit_everything::tests::{group_for_test, manifest_with_groups, task_for_test};
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    #[test]
    fn remediation_prompt_uses_lane_local_report_and_readonly_canonical() {
        let paths = RunPaths {
            host_root: PathBuf::from("/tmp/run"),
            manifest_path: PathBuf::from("/tmp/run/manifest.json"),
            latest_path: PathBuf::from("/tmp/run/latest"),
            worktree_root: PathBuf::from("/tmp/run/worktree"),
            report_root: PathBuf::from("/tmp/run/worktree/audit/everything/test-run"),
            pause_path: PathBuf::from("/tmp/run/PAUSE"),
            in_place: false,
        };
        let task = RemediationTaskState {
            id: "AUD-REM-001".to_string(),
            group: ".cargo".to_string(),
            slug: "cargo".to_string(),
            report_path: "/tmp/run/worktree/audit/everything/test-run/reports/cargo.md".to_string(),
            owned_paths: vec![".cargo/config.toml".to_string()],
            dependencies: Vec::new(),
            lane_index: 1,
            lane_root: "/tmp/run/remediation-lanes/lane-1".to_string(),
            lane_repo_root: "/tmp/run/remediation-lanes/lane-1/repo".to_string(),
            base_commit: None,
            status: StageStatus::Pending,
            note: None,
        };

        let prompt = build_remediation_lane_prompt(&paths, &task);

        assert!(prompt.contains(
            "Lane report: `/tmp/run/remediation-lanes/lane-1/repo/audit/everything/test-run/reports/cargo.md`"
        ));
        assert!(prompt
            .contains("Canonical audit worktree: `/tmp/run/worktree` (read-only for this lane)"));
        assert!(prompt.contains("Do not write into the canonical audit worktree"));
        assert!(prompt.contains("Canonical report: `/tmp/run/worktree/audit/everything/test-run/reports/cargo.md` (do not edit directly)"));
        assert!(prompt.contains("update the relevant architecture/docs files"));
        assert!(prompt.contains("CODEBASE-IMPROVEMENT-POLICY.md"));
        assert!(prompt.contains("delete proved-dead code"));
        assert!(prompt.contains("record deletion proof"));
    }

    #[test]
    fn remediation_graph_orders_docs_and_tests_after_sources() {
        let manifest = manifest_with_groups(vec![
            group_for_test("crates/core", &["crates/core/src/lib.rs"]),
            group_for_test("tests", &["tests/core_test.rs"]),
            group_for_test("docs", &["docs/architecture.md"]),
        ]);
        let graph = remediation_dependency_groups(Path::new("."), &manifest)
            .expect("dependency graph should build");
        assert!(graph["tests"].contains("crates/core"));
        assert!(graph["docs"].contains("crates/core"));
        assert!(graph["docs"].contains("tests"));
    }

    #[test]
    fn remediation_scheduler_waits_for_dependencies() {
        let mut manifest = manifest_with_groups(vec![
            group_for_test("crates/core", &["crates/core/src/lib.rs"]),
            group_for_test("docs", &["docs/architecture.md"]),
        ]);
        manifest.remediation_tasks = vec![
            task_for_test("AUD-REM-001", "crates/core", &[]),
            task_for_test("AUD-REM-002", "docs", &["AUD-REM-001"]),
        ];
        assert_eq!(
            next_ready_remediation_task_index(&manifest, &BTreeSet::new()),
            Some(0)
        );
        manifest.remediation_tasks[0].status = StageStatus::Complete;
        assert_eq!(
            next_ready_remediation_task_index(&manifest, &BTreeSet::new()),
            Some(1)
        );
    }

    #[test]
    fn remediation_scheduler_does_not_requeue_failed_tasks() {
        let mut manifest =
            manifest_with_groups(vec![group_for_test("fixtures", &["fixtures/example.toml"])]);
        manifest.remediation_tasks = vec![task_for_test("AUD-REM-001", "fixtures", &[])];
        manifest.remediation_tasks[0].status = StageStatus::Failed;

        assert!(next_remediation_scheduler_choice(&manifest, &BTreeSet::new(), true).is_none());
    }

    #[test]
    fn remediation_scheduler_breaks_dependency_cycle_only_when_idle() {
        let mut manifest = manifest_with_groups(vec![
            group_for_test("crates/core", &["crates/core/src/lib.rs"]),
            group_for_test("docs", &["docs/architecture.md"]),
        ]);
        manifest.remediation_tasks = vec![
            task_for_test("AUD-REM-001", "crates/core", &["AUD-REM-002"]),
            task_for_test("AUD-REM-002", "docs", &["AUD-REM-001"]),
        ];

        let choice = next_remediation_scheduler_choice(&manifest, &BTreeSet::new(), true)
            .expect("idle scheduler should break dependency cycles");
        assert_eq!(choice.index, 0);
        assert_eq!(choice.unmet_dependencies, vec!["AUD-REM-002"]);

        let mut active = BTreeSet::new();
        active.insert("AUD-REM-099".to_string());
        assert!(next_remediation_scheduler_choice(&manifest, &active, false).is_none());
        assert!(next_remediation_scheduler_choice(&manifest, &active, true).is_some());
    }
}
