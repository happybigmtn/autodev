//! Super-run manifest: persistence, resume hydration, stage bookkeeping, sidecar reports.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::super_command::gate::DeterministicGateSummary;
use crate::super_command::{SUPER_GENERATION_MODE_SNAPSHOT_ONLY, SUPER_ROOT_PLAN_STATUS_UNCHANGED};
use crate::util::{atomic_write, binary_provenance_line, timestamp_slug};
use crate::{GenerationArgs, SuperArgs};

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct SuperManifest {
    run_id: String,
    repo_root: String,
    planning_root: String,
    pub(crate) output_dir: Option<String>,
    super_root: String,
    prompt: Option<String>,
    focus: Option<String>,
    model: String,
    reasoning_effort: String,
    worker_model: String,
    worker_reasoning_effort: String,
    max_concurrent_workers: usize,
    max_iterations: Option<usize>,
    execute: bool,
    design_enabled: bool,
    #[serde(default)]
    super_review_skipped: bool,
    design_resolve_passes: usize,
    #[serde(default)]
    with_audit: bool,
    #[serde(default)]
    audit_threads: usize,
    #[serde(default)]
    audit_first_pass_retries: usize,
    #[serde(default)]
    pub(crate) audit_run_id: Option<String>,
    branch: Option<String>,
    reference_repos: Vec<String>,
    binary: String,
    #[serde(default = "default_super_generation_mode")]
    generation_mode: String,
    #[serde(default = "default_super_root_plan_status")]
    root_plan_status: String,
    #[serde(default)]
    pub(crate) promotion_command: Option<String>,
    stages: Vec<SuperStage>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct SuperStage {
    name: String,
    status: String,
    artifact: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct SuperRepoRecord {
    role: String,
    path: String,
    branch: String,
    head: String,
    status: String,
}

pub(crate) fn default_super_generation_mode() -> String {
    SUPER_GENERATION_MODE_SNAPSHOT_ONLY.to_string()
}

pub(crate) fn default_super_root_plan_status() -> String {
    SUPER_ROOT_PLAN_STATUS_UNCHANGED.to_string()
}

pub(crate) fn prepare_super_run(
    repo_root: &Path,
    args: &mut SuperArgs,
) -> Result<(PathBuf, SuperManifest)> {
    if let Some(resume_root) = args.resume.clone() {
        let super_root = absolutize_super_path(repo_root, &resume_root);
        let manifest = load_super_manifest(&super_root)?;
        if manifest.repo_root != repo_root.display().to_string() {
            bail!(
                "refusing to resume auto super run rooted at `{}` from repo `{}`; manifest belongs to `{}`",
                super_root.display(),
                repo_root.display(),
                manifest.repo_root
            );
        }
        hydrate_super_args_from_manifest(args, &manifest);
        return Ok((super_root, manifest));
    }

    let run_id = timestamp_slug();
    let super_root = repo_root.join(".auto").join("super").join(&run_id);
    let planning_root = args
        .planning_root
        .clone()
        .unwrap_or_else(|| repo_root.join("genesis"));
    let manifest = SuperManifest {
        run_id,
        repo_root: repo_root.display().to_string(),
        planning_root: planning_root.display().to_string(),
        output_dir: args
            .output_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        super_root: super_root.display().to_string(),
        prompt: args.prompt.clone(),
        focus: args.focus.clone(),
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        worker_model: args.worker_model.clone(),
        worker_reasoning_effort: args.worker_reasoning_effort.clone(),
        max_concurrent_workers: args.max_concurrent_workers.max(1),
        max_iterations: args.max_iterations,
        execute: !args.no_execute,
        design_enabled: !args.skip_design,
        super_review_skipped: args.skip_super_review,
        design_resolve_passes: if args.no_execute || args.skip_design {
            0
        } else {
            args.design_resolve_passes.max(1)
        },
        with_audit: args.with_audit,
        audit_threads: args.audit_threads.max(1),
        audit_first_pass_retries: args.audit_first_pass_retries,
        audit_run_id: args.audit_run_id.clone(),
        branch: args.branch.clone(),
        reference_repos: args
            .reference_repos
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        binary: binary_provenance_line(),
        generation_mode: SUPER_GENERATION_MODE_SNAPSHOT_ONLY.to_string(),
        root_plan_status: SUPER_ROOT_PLAN_STATUS_UNCHANGED.to_string(),
        promotion_command: args
            .output_dir
            .as_ref()
            .map(|path| super_snapshot_promotion_command(path)),
        stages: Vec::new(),
    };
    Ok((super_root, manifest))
}

pub(crate) fn build_super_generation_args(
    args: &SuperArgs,
    planning_root: &Path,
) -> GenerationArgs {
    GenerationArgs {
        planning_root: Some(planning_root.to_path_buf()),
        output_dir: args.output_dir.clone(),
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        codex_review_model: args.model.clone(),
        codex_review_effort: args.reasoning_effort.clone(),
        codex_bin: args.codex_bin.clone(),
        gbrain_bin: std::path::PathBuf::from("gbrain"),
        no_gbrain_context: false,
        skip_codex_review: false,
        max_turns: args.max_turns,
        parallelism: args.planning_parallelism,
        plan_only: false,
        snapshot_only: true,
        sync_only: false,
    }
}

pub(crate) fn super_snapshot_promotion_command(output_dir: &Path) -> String {
    format!("auto gen --sync-only --output-dir {}", output_dir.display())
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SuperParallelDecision {
    pub(crate) launch: bool,
    pub(crate) skip_reason: Option<String>,
    pub(crate) promotion_command: Option<String>,
}

pub(crate) fn super_snapshot_parallel_decision(
    output_dir: Option<&Path>,
) -> Result<SuperParallelDecision> {
    let output_dir = output_dir.context(
        "auto super generated snapshot path is unavailable; cannot decide promotion-gated parallel launch",
    )?;
    Ok(SuperParallelDecision {
        launch: false,
        skip_reason: Some("snapshot requires explicit promotion".to_string()),
        promotion_command: Some(super_snapshot_promotion_command(output_dir)),
    })
}

pub(crate) fn absolutize_super_path(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

pub(crate) fn load_super_manifest(super_root: &Path) -> Result<SuperManifest> {
    let path = super_root.join("manifest.json");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

pub(crate) fn hydrate_super_args_from_manifest(args: &mut SuperArgs, manifest: &SuperManifest) {
    if args.prompt.is_none() {
        args.prompt = manifest.prompt.clone();
    }
    if args.focus.is_none() {
        args.focus = manifest.focus.clone();
    }
    if args.planning_root.is_none() {
        args.planning_root = Some(PathBuf::from(&manifest.planning_root));
    }
    if args.output_dir.is_none() {
        args.output_dir = manifest.output_dir.as_ref().map(PathBuf::from);
    }
    if args.branch.is_none() {
        args.branch = manifest.branch.clone();
    }
    if args.reference_repos.is_empty() {
        args.reference_repos = manifest.reference_repos.iter().map(PathBuf::from).collect();
    }
    args.model = manifest.model.clone();
    args.reasoning_effort = manifest.reasoning_effort.clone();
    args.worker_model = manifest.worker_model.clone();
    args.worker_reasoning_effort = manifest.worker_reasoning_effort.clone();
    args.max_concurrent_workers = manifest.max_concurrent_workers.max(1);
    args.max_iterations = manifest.max_iterations;
    args.no_execute = !manifest.execute;
    args.skip_design = !manifest.design_enabled;
    args.skip_super_review = manifest.super_review_skipped;
    if manifest.design_resolve_passes > 0 {
        args.design_resolve_passes = manifest.design_resolve_passes;
    }
    args.with_audit = manifest.with_audit;
    if manifest.audit_threads > 0 {
        args.audit_threads = manifest.audit_threads;
    }
    if manifest.audit_first_pass_retries > 0 {
        args.audit_first_pass_retries = manifest.audit_first_pass_retries;
    }
    if args.audit_run_id.is_none() && manifest.audit_run_id.is_some() {
        args.audit_run_id = manifest.audit_run_id.clone();
    }
}

pub(crate) fn super_stage_terminal(manifest: &SuperManifest, name: &str) -> bool {
    // "launched" was previously treated as terminal so the parallel stage would
    // be skipped on resume even when it exited with everything shelved. That
    // turned resume into a footgun: a ctrl-c or shelved-everything exit left
    // the run looking "done" and forced operators to bypass super entirely.
    // Only true completion ("complete") or explicit skip ("skipped") now count
    // as terminal. Resume re-enters any stage that didn't write "complete".
    manifest
        .stages
        .iter()
        .rev()
        .find(|stage| stage.name == name)
        .is_some_and(|stage| matches!(stage.status.as_str(), "complete" | "skipped"))
}

pub(crate) fn super_stage_terminal_any(manifest: &SuperManifest, names: &[&str]) -> bool {
    names
        .iter()
        .any(|name| super_stage_terminal(manifest, name))
}

pub(crate) fn super_stage_artifact(manifest: &SuperManifest, name: &str) -> Option<PathBuf> {
    manifest
        .stages
        .iter()
        .rev()
        .find(|stage| stage.name == name)
        .and_then(|stage| stage.artifact.as_ref())
        .map(PathBuf::from)
}

pub(crate) fn push_skipped_stage_if_needed(
    super_root: &Path,
    manifest: &mut SuperManifest,
    name: &str,
) -> Result<()> {
    if super_stage_terminal(manifest, name) {
        println!("stage:       {name} (resume skip)");
        return Ok(());
    }
    push_stage(super_root, manifest, name, "skipped", None)
}

pub(crate) fn read_deterministic_gate(super_root: &Path) -> Result<DeterministicGateSummary> {
    let path = super_root.join("DETERMINISTIC-GATE.json");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

pub(crate) fn write_super_cross_repo_manifest(
    super_root: &Path,
    repo_root: &Path,
    planning_root: &Path,
    args: &SuperArgs,
) -> Result<()> {
    #[derive(Serialize)]
    struct CrossRepoManifest {
        primary: SuperRepoRecord,
        references: Vec<SuperRepoRecord>,
        autodev_binary: String,
        planning_root: String,
        worker_model: String,
        worker_reasoning_effort: String,
    }

    let manifest = CrossRepoManifest {
        primary: repo_record("primary", repo_root),
        references: args
            .reference_repos
            .iter()
            .map(|path| repo_record("reference", path))
            .collect(),
        autodev_binary: binary_provenance_line(),
        planning_root: planning_root.display().to_string(),
        worker_model: args.worker_model.clone(),
        worker_reasoning_effort: args.worker_reasoning_effort.clone(),
    };
    let path = super_root.join("CROSS-REPO-MANIFEST.json");
    atomic_write(&path, &serde_json::to_vec_pretty(&manifest)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn repo_record(role: &str, path: &Path) -> SuperRepoRecord {
    SuperRepoRecord {
        role: role.to_string(),
        path: path.display().to_string(),
        branch: git_text(path, ["branch", "--show-current"])
            .unwrap_or_else(|| "unknown".to_string()),
        head: git_text(path, ["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string()),
        status: git_text(path, ["status", "--short", "--branch"])
            .unwrap_or_else(|| "not a readable git repo".to_string()),
    }
}

pub(crate) fn write_super_branch_reconciliation_plan(
    super_root: &Path,
    repo_root: &Path,
    args: &SuperArgs,
    phase: &str,
) -> Result<()> {
    let branch =
        git_text(repo_root, ["branch", "--show-current"]).unwrap_or_else(|| "unknown".to_string());
    let head = git_text(repo_root, ["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let status = git_text(repo_root, ["status", "--short", "--branch"])
        .unwrap_or_else(|| "git status unavailable".to_string());
    let target = args.branch.as_deref().unwrap_or(branch.as_str());
    let content = format!(
        "# Auto Super Branch Reconciliation\n\n\
Phase: `{phase}`\n\
Primary repo: `{}`\n\
Active branch: `{branch}`\n\
Parallel target branch: `{target}`\n\
HEAD: `{head}`\n\n\
## Current Status\n\n```text\n{}\n```\n\n\
## Reconciliation Doctrine\n\n\
1. Do not merge this branch into trunk while auto super or auto parallel is still mutating it.\n\
2. Preserve dirty operator/audit artifacts on trunk before updating trunk from origin.\n\
3. After the run is complete, merge or intentionally cherry-pick this branch into trunk, then run the gate commands named in `FINAL-SANITY.md`.\n\
4. Push trunk only after queue truth, receipts, branch head, and remote head agree.\n",
        repo_root.display(),
        status.trim()
    );
    let path = super_root.join("BRANCH-RECONCILIATION.md");
    atomic_write(&path, content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn write_super_final_sanity(
    super_root: &Path,
    repo_root: &Path,
    gate: &DeterministicGateSummary,
    args: &SuperArgs,
    phase: &str,
) -> Result<()> {
    let branch =
        git_text(repo_root, ["branch", "--show-current"]).unwrap_or_else(|| "unknown".to_string());
    let head = git_text(repo_root, ["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let remote =
        git_text(repo_root, ["ls-remote", "--heads", "origin", &branch]).unwrap_or_default();
    let remote_head = remote
        .split_whitespace()
        .next()
        .unwrap_or("unavailable")
        .to_string();
    let content = format!(
        "# Auto Super Final Sanity\n\n\
Phase: `{phase}`\n\
Branch: `{branch}`\n\
HEAD: `{head}`\n\
Remote HEAD: `{remote_head}`\n\
Execute: `{}`\n\
Ready tasks at deterministic gate: `{}`\n\
Priority tasks: `{}`\n\
Follow-on tasks: `{}`\n\
Worker model: `{}`\n\
Worker reasoning effort: `{}`\n\n\
## Required Closeout Checks\n\n\
- Root queue has no accidental empty or malformed executable rows.\n\
- Every landed implementation item has a `REVIEW.md` handoff or repo-native completion artifact.\n\
- Verification receipts exist for executable `Verification:` commands where the repo requires the wrapper.\n\
- No lane repo remains in cherry-pick, rebase, or stale `rebase-merge` recovery.\n\
- Branch reconciliation is recorded in `BRANCH-RECONCILIATION.md` before trunk is pushed.\n",
        !args.no_execute,
        gate.unchecked_tasks,
        gate.priority_tasks,
        gate.follow_on_tasks,
        args.worker_model,
        args.worker_reasoning_effort,
    );
    let path = super_root.join("FINAL-SANITY.md");
    atomic_write(&path, content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn git_text<const N: usize>(repo_root: &Path, args: [&str; N]) -> Option<String> {
    crate::util::git_stdout(repo_root, args)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

pub(crate) fn push_stage(
    super_root: &Path,
    manifest: &mut SuperManifest,
    name: &str,
    status: &str,
    artifact: Option<&Path>,
) -> Result<()> {
    manifest.stages.push(SuperStage {
        name: name.to_string(),
        status: status.to_string(),
        artifact: artifact.map(|path| path.display().to_string()),
    });
    append_status_log(super_root, name, status);
    write_manifest(super_root, manifest)
}

pub(crate) fn append_status_log(super_root: &Path, name: &str, status: &str) {
    // Tail-friendly stage-only log so operators can `tail -f status.log`
    // without wading through codex narrative. Best-effort: never fails the
    // surrounding push_stage even if the write errors.
    let path = super_root.join("status.log");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let pid = std::process::id();
    let line = format!("{now} pid={pid} stage={name} status={status}\n");
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

pub(crate) fn write_manifest(super_root: &Path, manifest: &SuperManifest) -> Result<()> {
    let path = super_root.join("manifest.json");
    atomic_write(&path, &serde_json::to_vec_pretty(manifest)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::super_command::gate::DeterministicGateSummary;
    use crate::super_command::{
        IMPLEMENTATION_PLAN, SUPER_GENERATION_MODE_SNAPSHOT_ONLY,
        SUPER_PLAN_SOURCE_GENERATED_SNAPSHOT, SUPER_ROOT_PLAN_STATUS_UNCHANGED,
    };
    use crate::SuperArgs;

    use super::{
        build_super_generation_args, load_super_manifest, read_deterministic_gate,
        super_snapshot_parallel_decision, super_snapshot_promotion_command, super_stage_artifact,
        super_stage_terminal, SuperManifest, SuperStage,
    };

    #[test]
    fn resume_helpers_skip_terminal_stages_and_restore_gate_artifact() {
        let root = temp_dir("super-resume-manifest");
        let artifact = root.join("gen-output");
        let gate = DeterministicGateSummary {
            unchecked_tasks: 3,
            priority_tasks: 2,
            follow_on_tasks: 1,
            plan_path: Some(root.join(IMPLEMENTATION_PLAN).display().to_string()),
            plan_source: SUPER_PLAN_SOURCE_GENERATED_SNAPSHOT.to_string(),
            generation_mode: SUPER_GENERATION_MODE_SNAPSHOT_ONLY.to_string(),
            root_plan_status: SUPER_ROOT_PLAN_STATUS_UNCHANGED.to_string(),
            promotion_required: true,
            promotion_command: Some(super_snapshot_promotion_command(&artifact)),
        };
        fs::write(
            root.join("DETERMINISTIC-GATE.json"),
            serde_json::to_vec_pretty(&gate).unwrap(),
        )
        .unwrap();

        let manifest = SuperManifest {
            run_id: "run-1".to_string(),
            repo_root: "/repo".to_string(),
            planning_root: "/repo/genesis".to_string(),
            output_dir: Some("/repo/gen-out".to_string()),
            super_root: root.display().to_string(),
            prompt: Some("ship it".to_string()),
            focus: Some("market drama".to_string()),
            model: "gpt-5.5".to_string(),
            reasoning_effort: "xhigh".to_string(),
            worker_model: "gpt-5.5".to_string(),
            worker_reasoning_effort: "high".to_string(),
            max_concurrent_workers: 5,
            max_iterations: None,
            execute: true,
            design_enabled: true,
            super_review_skipped: false,
            design_resolve_passes: 3,
            with_audit: false,
            audit_threads: 0,
            audit_first_pass_retries: 0,
            audit_run_id: None,
            branch: Some("main".to_string()),
            reference_repos: vec!["/ref".to_string()],
            binary: "auto test".to_string(),
            generation_mode: SUPER_GENERATION_MODE_SNAPSHOT_ONLY.to_string(),
            root_plan_status: SUPER_ROOT_PLAN_STATUS_UNCHANGED.to_string(),
            promotion_command: Some(super_snapshot_promotion_command(&artifact)),
            stages: vec![
                SuperStage {
                    name: "gen".to_string(),
                    status: "complete".to_string(),
                    artifact: Some(artifact.display().to_string()),
                },
                SuperStage {
                    name: "parallel".to_string(),
                    status: "launched".to_string(),
                    artifact: Some(root.join("parallel").display().to_string()),
                },
            ],
        };
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let loaded = load_super_manifest(&root).unwrap();

        assert!(super_stage_terminal(&loaded, "gen"));
        // "launched" is intentionally NOT terminal so resume re-enters parallel
        // when an earlier run exited cleanly with everything shelved.
        assert!(!super_stage_terminal(&loaded, "parallel"));
        assert_eq!(super_stage_artifact(&loaded, "gen"), Some(artifact));
        assert_eq!(read_deterministic_gate(&root).unwrap(), gate);
    }

    #[test]
    fn super_default_generation_is_snapshot_only() {
        let args = super_args();
        let planning_root = PathBuf::from("/repo/genesis");

        let generation_args = build_super_generation_args(&args, &planning_root);

        assert_eq!(generation_args.planning_root, Some(planning_root));
        assert!(generation_args.snapshot_only);
        assert!(!generation_args.sync_only);
        assert!(!generation_args.plan_only);
    }

    #[test]
    fn super_skips_parallel_until_snapshot_is_promoted() {
        let gen_dir = PathBuf::from("/repo/gen-review");

        let decision = super_snapshot_parallel_decision(Some(&gen_dir)).unwrap();

        assert!(!decision.launch);
        assert_eq!(
            decision.skip_reason.as_deref(),
            Some("snapshot requires explicit promotion")
        );
        assert_eq!(
            decision.promotion_command,
            Some("auto gen --sync-only --output-dir /repo/gen-review".to_string())
        );
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn super_args() -> SuperArgs {
        SuperArgs {
            prompt: Some("snapshot proof".to_string()),
            planning_root: None,
            output_dir: None,
            resume: None,
            idea: None,
            focus: None,
            reference_repos: Vec::new(),
            model: "gpt-5.5".to_string(),
            reasoning_effort: "xhigh".to_string(),
            codex_bin: PathBuf::from("codex"),
            max_turns: 200,
            planning_parallelism: 8,
            max_concurrent_workers: 5,
            max_iterations: None,
            worker_model: "gpt-5.5".to_string(),
            worker_reasoning_effort: "high".to_string(),
            branch: None,
            no_execute: false,
            skip_super_review: false,
            skip_design: false,
            design_resolve_passes: 3,
            with_audit: false,
            audit_threads: 10,
            audit_first_pass_retries: 3,
            audit_run_id: None,
            dry_run: false,
        }
    }
}
