//! Professional whole-repo audit pipeline for `auto audit --everything`.
//!
//! The legacy `auto audit` command is a doctrine-driven per-file fixer. This
//! module is deliberately larger: it first builds repository context, then runs
//! one clean model iteration per file, synthesizes crate/module reports, applies
//! bounded crate-by-crate improvements, and finally reviews the diff before an
//! optional merge back to the primary branch.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::codex_exec::run_codex_exec_max_context;
use crate::util::{
    atomic_write, binary_provenance_line, ensure_repo_layout, git_repo_root, git_stdout, run_git,
    timestamp_slug,
};
use crate::{AuditArgs, AuditEverythingPhase};

const PROFESSIONAL_AUDIT_DIR: &str = ".auto/audit-everything";
const LATEST_RUN_FILE: &str = "latest-run";
const MAX_FILE_PROMPT_BYTES: usize = 220_000;
const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["trunk", "main", "master"];
const DEFAULT_EXCLUDE_PREFIXES: [&str; 9] = [
    ".git/",
    ".auto/",
    "target/",
    "node_modules/",
    "dist/",
    "build/",
    "bug/",
    "nemesis/",
    "gen-",
];

const GSTACK_SKILL_POLICY: &str = r#"# GStack Skill Policy

This audit uses gstack skills as deterministic compact lenses unless the phase explicitly asks for live validation. Workers should not bulk-load full skill files by default.

## Always-On Audit Lenses

- review: pre-landing structural review, diff risk, SQL/data safety, LLM trust boundaries, conditional side effects, documentation staleness.
- health: project-native typecheck, lint, tests, dead-code, shell lint, and quality score evidence.
- investigate: root-cause discipline; no fixes or recommendations without evidence and a falsifiable theory.
- cso: secrets archaeology, dependency and CI/CD supply chain, auth/session boundaries, OWASP/STRIDE, LLM/AI security, production safety.
- careful: destructive-command caution, especially for deletion, force pushes, migrations, production, and shared environments.

## Planning And Context Lenses

- autoplan: complete plan gauntlet, represented here by CEO, engineering, design, and developer-experience review lenses.
- plan-ceo-review: product scope, ambition, simplification, and whether the proposed best version is worth building.
- plan-eng-review: architecture, data flow, invariants, edge cases, test plan, rollout risk, and maintainability.
- plan-design-review: UI/UX plan quality, hierarchy, interaction model, accessibility, visual system consistency.
- plan-devex-review: APIs, CLIs, SDKs, docs, onboarding, error messages, and time-to-hello-world.
- design-consultation: creation or repair of DESIGN.md/design-system docs when UI surfaces lack a coherent source of truth.

## Implementation And Remediation Lenses

- qa: test-fix-verify loop for web or interactive surfaces when remediation is allowed.
- qa-only: report-only web/app QA when source edits are disallowed.
- design-review: live visual QA and design polish for implemented UI surfaces.
- benchmark: browser-backed performance, Core Web Vitals, load time, resource and bundle regressions.
- devex-review: live developer-experience audit of docs, CLI help, onboarding, and error messages.
- document-release: post-change documentation synchronization across README, ARCHITECTURE, AGENTS, changelog, and TODOs.
- ship: pre-merge readiness, base-branch sync, validation gate, version/changelog/PR hygiene.
- land-and-deploy: merge/deploy/canary posture; use as a final-review lens, not as an automatic action inside audit workers.
- canary: post-deploy monitoring and visual/console/performance anomaly checks when deployment exists.

## State And Boundary Lenses

- checkpoint, context-save, context-restore: resumability and handoff quality.
- freeze, guard, unfreeze: write-scope control. For this audit, prefer the host runner's group boundaries over ad hoc widening.
- learn, retro: mine previous decisions or trends only when local project artifacts make them relevant.

## Browser And Artifact Tools

- browse/gstack: direct browser inspection for web/app QA, screenshots, responsive checks, forms, dialogs, and state assertions.
- connect-chrome/open-gstack-browser/setup-browser-cookies/pair-agent: direct browser setup only when authenticated or visible-browser QA is explicitly required.
- make-pdf: optional final packaging for markdown reports; never required for merge readiness.

## Usually Excluded From Audit Workers

- benchmark-models, plan-tune, gstack-upgrade, design-shotgun, design-html, office-hours: meta/tooling/ideation skills. Mention only if the file itself implements those workflows or the user explicitly requested that surface.
"#;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EverythingManifest {
    run_id: String,
    repo_root: String,
    worktree_root: String,
    report_root: String,
    branch: String,
    audit_branch: String,
    base_commit: String,
    created_at: String,
    context: ContextState,
    files: Vec<FileState>,
    groups: Vec<GroupState>,
    final_review: StageState,
    merge: StageState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ContextState {
    status: StageStatus,
    agents_hash: Option<String>,
    architecture_hash: Option<String>,
    doctrine_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileState {
    path: String,
    group: String,
    content_hash: String,
    artifact_dir: String,
    status: StageStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GroupState {
    name: String,
    slug: String,
    files: Vec<String>,
    report_path: String,
    synthesis_status: StageStatus,
    remediation_status: StageStatus,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StageState {
    status: StageStatus,
    artifact: Option<String>,
    note: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StageStatus {
    #[default]
    Pending,
    Running,
    Complete,
    Failed,
    Skipped,
}

struct RunPaths {
    host_root: PathBuf,
    manifest_path: PathBuf,
    latest_path: PathBuf,
    worktree_root: PathBuf,
    report_root: PathBuf,
}

#[derive(Clone)]
struct PhaseConfig {
    model: String,
    effort: String,
    codex_bin: PathBuf,
}

pub(crate) async fn run_audit_everything(args: AuditArgs) -> Result<()> {
    if args.everything_threads == 0 {
        bail!("--everything-threads must be greater than 0");
    }
    if args.remediation_threads == 0 {
        bail!("--remediation-threads must be greater than 0");
    }

    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string();
    let branch = resolve_primary_branch(&repo_root, args.branch.as_deref(), &current_branch)?;
    let run_root_base = resolve_run_root_base(&repo_root, args.everything_run_root.as_deref());

    let (mut manifest, paths) = load_or_create_run(&repo_root, &run_root_base, &branch, &args)?;

    println!("auto audit --everything");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", manifest.branch);
    println!("audit branch: {}", manifest.audit_branch);
    println!("run id:      {}", manifest.run_id);
    println!("run root:    {}", paths.host_root.display());
    println!("worktree:    {}", paths.worktree_root.display());
    println!("reports:     {}", paths.report_root.display());
    println!("phase:       {:?}", args.everything_phase);

    if matches!(args.everything_phase, AuditEverythingPhase::Status) {
        print_status(&manifest);
        return Ok(());
    }

    ensure_worktree(&repo_root, &paths, &mut manifest)?;
    write_manifest(&paths, &manifest)?;

    match args.everything_phase {
        AuditEverythingPhase::All => {
            run_context_phase(&args, &paths, &mut manifest).await?;
            run_first_pass_phase(&args, &paths, &mut manifest).await?;
            run_synthesis_phase(&args, &paths, &mut manifest).await?;
            if args.report_only {
                mark_remediation_skipped(&paths, &mut manifest, "--report-only")?;
                run_final_review_phase(&args, &paths, &mut manifest).await?;
                mark_merge_skipped(&paths, &mut manifest, "--report-only")?;
            } else {
                run_remediation_phase(&args, &paths, &mut manifest).await?;
                run_final_review_phase(&args, &paths, &mut manifest).await?;
                if args.no_everything_merge {
                    mark_merge_skipped(&paths, &mut manifest, "--no-everything-merge")?;
                } else {
                    attempt_merge(&repo_root, &paths, &mut manifest)?;
                }
            }
        }
        AuditEverythingPhase::InitContext => {
            run_context_phase(&args, &paths, &mut manifest).await?;
        }
        AuditEverythingPhase::FirstPass => {
            require_context_complete(&manifest)?;
            run_first_pass_phase(&args, &paths, &mut manifest).await?;
        }
        AuditEverythingPhase::Synthesize => {
            require_first_pass_complete(&manifest)?;
            run_synthesis_phase(&args, &paths, &mut manifest).await?;
        }
        AuditEverythingPhase::Remediate => {
            require_synthesis_complete(&manifest)?;
            if args.report_only {
                mark_remediation_skipped(&paths, &mut manifest, "--report-only")?;
            } else {
                run_remediation_phase(&args, &paths, &mut manifest).await?;
            }
        }
        AuditEverythingPhase::FinalReview => {
            run_final_review_phase(&args, &paths, &mut manifest).await?;
        }
        AuditEverythingPhase::Merge => {
            attempt_merge(&repo_root, &paths, &mut manifest)?;
        }
        AuditEverythingPhase::Status => unreachable!("handled above"),
    }

    print_status(&manifest);
    Ok(())
}

fn resolve_run_root_base(repo_root: &Path, override_root: Option<&Path>) -> PathBuf {
    match override_root {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => repo_root.join(path),
        None => repo_root.join(PROFESSIONAL_AUDIT_DIR),
    }
}

fn load_or_create_run(
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
    let worktree_root = host_root.join("worktree");
    let report_root = worktree_root.join("audit").join("everything").join(&run_id);
    let paths = RunPaths {
        host_root: host_root.clone(),
        manifest_path: manifest_path.clone(),
        latest_path,
        worktree_root: worktree_root.clone(),
        report_root: report_root.clone(),
    };

    if manifest_path.exists() {
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let mut manifest: EverythingManifest = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        reconcile_file_inventory(&worktree_root, &report_root, &mut manifest).ok();
        return Ok((manifest, paths));
    }

    let base_commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let audit_branch = format!(
        "auto-audit/{repo}-{run_id}",
        repo = slugify(
            repo_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("repo")
        )
    );
    let manifest = EverythingManifest {
        run_id: run_id.clone(),
        repo_root: repo_root.display().to_string(),
        worktree_root: worktree_root.display().to_string(),
        report_root: report_root.display().to_string(),
        branch: branch.to_string(),
        audit_branch,
        base_commit,
        created_at: timestamp_slug(),
        context: ContextState::default(),
        files: Vec::new(),
        groups: Vec::new(),
        final_review: StageState::default(),
        merge: StageState::default(),
    };
    atomic_write(&paths.latest_path, run_id.as_bytes())
        .with_context(|| format!("failed to write {}", paths.latest_path.display()))?;
    write_manifest(&paths, &manifest)?;
    Ok((manifest, paths))
}

fn ensure_worktree(
    repo_root: &Path,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
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

async fn run_context_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if matches!(manifest.context.status, StageStatus::Complete) {
        println!("context:     complete (resume)");
        return Ok(());
    }
    manifest.context.status = StageStatus::Running;
    write_manifest(paths, manifest)?;
    fs::create_dir_all(&paths.report_root)
        .with_context(|| format!("failed to create {}", paths.report_root.display()))?;
    write_skill_policy_reference(paths)?;

    let prompt = build_context_prompt(&paths.worktree_root, &paths.report_root);
    let config = PhaseConfig {
        model: args.synthesis_model.clone(),
        effort: args.synthesis_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    run_codex_phase(paths, "init-context", &prompt, &config).await?;

    require_nonempty_file(&paths.worktree_root.join("AGENTS.md"))?;
    require_nonempty_file(&paths.worktree_root.join("ARCHITECTURE.md"))?;
    write_skill_policy_reference(paths)?;
    write_context_bundle(paths)?;

    manifest.context.status = StageStatus::Complete;
    manifest.context.agents_hash = hash_file_if_exists(&paths.worktree_root.join("AGENTS.md"))?;
    manifest.context.architecture_hash =
        hash_file_if_exists(&paths.worktree_root.join("ARCHITECTURE.md"))?;
    manifest.context.doctrine_hash = Some(hash_doctrine(&paths.worktree_root)?);
    reconcile_file_inventory(&paths.worktree_root, &paths.report_root, manifest)?;
    write_manifest(paths, manifest)?;
    Ok(())
}

async fn run_first_pass_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    reconcile_file_inventory(&paths.worktree_root, &paths.report_root, manifest)?;
    let pending = manifest
        .files
        .iter()
        .filter(|file| !matches!(file.status, StageStatus::Complete))
        .cloned()
        .collect::<Vec<_>>();
    if pending.is_empty() {
        println!("first pass:  complete (resume)");
        return Ok(());
    }

    let context = read_context_bundle(paths)?;
    let config = PhaseConfig {
        model: args.first_pass_model.clone(),
        effort: args.first_pass_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let workers = args.everything_threads.clamp(1, 15);
    println!(
        "first pass:  {} file(s), {} worker(s)",
        pending.len(),
        workers
    );
    let semaphore = Arc::new(Semaphore::new(workers));
    let mut join_set = JoinSet::new();
    for file in pending {
        let permit = semaphore.clone().acquire_owned().await?;
        let paths_clone = RunPaths {
            host_root: paths.host_root.clone(),
            manifest_path: paths.manifest_path.clone(),
            latest_path: paths.latest_path.clone(),
            worktree_root: paths.worktree_root.clone(),
            report_root: paths.report_root.clone(),
        };
        let context_clone = context.clone();
        let config_clone = config.clone();
        join_set.spawn(async move {
            let _permit = permit;
            run_one_file_analysis(&paths_clone, &file, &context_clone, &config_clone).await
        });
    }

    let mut failures = Vec::new();
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(path)) => {
                if let Some(file) = manifest.files.iter_mut().find(|file| file.path == path) {
                    file.status = StageStatus::Complete;
                }
                write_manifest(paths, manifest)?;
            }
            Ok(Err(err)) => failures.push(format!("{err:#}")),
            Err(err) => failures.push(format!("worker task panicked: {err}")),
        }
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("first pass failure: {failure}");
        }
        bail!("first pass failed for {} file(s)", failures.len());
    }
    write_manifest(paths, manifest)?;
    Ok(())
}

async fn run_synthesis_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    build_initial_group_reports(paths, manifest)?;
    let pending = manifest
        .groups
        .iter()
        .filter(|group| !matches!(group.synthesis_status, StageStatus::Complete))
        .cloned()
        .collect::<Vec<_>>();
    if pending.is_empty() {
        println!("synthesis:   complete (resume)");
        return Ok(());
    }

    let config = PhaseConfig {
        model: args.synthesis_model.clone(),
        effort: args.synthesis_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let workers = args.everything_threads.clamp(1, 15);
    println!(
        "synthesis:   {} group(s), {} worker(s)",
        pending.len(),
        workers
    );
    run_group_workers(
        paths,
        pending,
        workers,
        config,
        GroupPhase::Synthesis,
        manifest,
    )
    .await
}

async fn run_remediation_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    let pending = manifest
        .groups
        .iter()
        .filter(|group| !matches!(group.remediation_status, StageStatus::Complete))
        .cloned()
        .collect::<Vec<_>>();
    if pending.is_empty() {
        println!("remediation: complete (resume)");
        return Ok(());
    }
    let config = PhaseConfig {
        model: args.remediation_model.clone(),
        effort: args.remediation_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let workers = args.remediation_threads.clamp(1, 15);
    println!(
        "remediation: {} group(s), {} worker(s)",
        pending.len(),
        workers
    );
    run_group_workers(
        paths,
        pending,
        workers,
        config,
        GroupPhase::Remediation,
        manifest,
    )
    .await
}

async fn run_final_review_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if matches!(manifest.final_review.status, StageStatus::Complete) {
        println!("final review: complete (resume)");
        return Ok(());
    }
    manifest.final_review.status = StageStatus::Running;
    write_manifest(paths, manifest)?;
    let prompt = build_final_review_prompt(paths, manifest);
    let config = PhaseConfig {
        model: args.final_review_model.clone(),
        effort: args.final_review_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    run_codex_phase(paths, "final-review", &prompt, &config).await?;
    let review_path = paths.report_root.join("FINAL-REVIEW.md");
    require_nonempty_file(&review_path)?;
    let review = fs::read_to_string(&review_path)
        .with_context(|| format!("failed to read {}", review_path.display()))?;
    manifest.final_review.artifact = Some(path_display(&review_path));
    manifest.final_review.note = first_verdict_line(&review);
    manifest.final_review.status = StageStatus::Complete;
    write_manifest(paths, manifest)?;
    Ok(())
}

fn attempt_merge(
    repo_root: &Path,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if matches!(manifest.merge.status, StageStatus::Complete) {
        println!("merge:       complete (resume)");
        return Ok(());
    }
    if !final_review_is_go(paths) {
        manifest.merge.status = StageStatus::Skipped;
        manifest.merge.note = Some("final review did not contain `Verdict: GO`".to_string());
        write_manifest(paths, manifest)?;
        bail!("final review is not GO; not attempting merge");
    }

    commit_worktree_changes(paths, manifest)?;

    let current_branch = git_stdout(repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string();
    if current_branch != manifest.branch {
        bail!(
            "merge requires canonical checkout on `{}` (current: `{}`)",
            manifest.branch,
            current_branch
        );
    }
    let status = git_stdout(repo_root, ["status", "--short"])?;
    if !status.trim().is_empty() {
        bail!(
            "canonical checkout is dirty; clean it before merging professional audit branch:\n{}",
            status
        );
    }
    if remote_branch_exists(repo_root, &manifest.branch) {
        let _ = run_git(repo_root, ["pull", "--rebase", "origin", &manifest.branch]);
    }
    run_git(repo_root, ["merge", "--ff-only", &manifest.audit_branch])?;
    if remote_branch_exists(repo_root, &manifest.branch) {
        run_git(repo_root, ["push", "origin", &manifest.branch])?;
    }
    manifest.merge.status = StageStatus::Complete;
    manifest.merge.note = Some(format!("merged {}", manifest.audit_branch));
    write_manifest(paths, manifest)?;
    Ok(())
}

fn commit_worktree_changes(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    let status = git_stdout(&paths.worktree_root, ["status", "--short"])?;
    if status.trim().is_empty() {
        return Ok(());
    }
    run_git(&paths.worktree_root, ["add", "--", "."])?;
    let staged = command_status(
        &paths.worktree_root,
        ["diff", "--cached", "--quiet", "--exit-code"],
    )?;
    if staged.success() {
        return Ok(());
    }
    let message = format!("audit: professional whole-repo audit {}", manifest.run_id);
    run_git(&paths.worktree_root, ["commit", "-m", &message])?;
    Ok(())
}

#[derive(Clone, Copy)]
enum GroupPhase {
    Synthesis,
    Remediation,
}

async fn run_group_workers(
    paths: &RunPaths,
    pending: Vec<GroupState>,
    workers: usize,
    config: PhaseConfig,
    phase: GroupPhase,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    let semaphore = Arc::new(Semaphore::new(workers));
    let mut join_set = JoinSet::new();
    for group in pending {
        let permit = semaphore.clone().acquire_owned().await?;
        let paths_clone = RunPaths {
            host_root: paths.host_root.clone(),
            manifest_path: paths.manifest_path.clone(),
            latest_path: paths.latest_path.clone(),
            worktree_root: paths.worktree_root.clone(),
            report_root: paths.report_root.clone(),
        };
        let config_clone = config.clone();
        join_set.spawn(async move {
            let _permit = permit;
            run_one_group_phase(&paths_clone, &group, phase, &config_clone).await
        });
    }

    let mut failures = Vec::new();
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(slug)) => {
                if let Some(group) = manifest.groups.iter_mut().find(|group| group.slug == slug) {
                    match phase {
                        GroupPhase::Synthesis => group.synthesis_status = StageStatus::Complete,
                        GroupPhase::Remediation => group.remediation_status = StageStatus::Complete,
                    }
                }
                write_manifest(paths, manifest)?;
            }
            Ok(Err(err)) => failures.push(format!("{err:#}")),
            Err(err) => failures.push(format!("group worker task panicked: {err}")),
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

async fn run_one_group_phase(
    paths: &RunPaths,
    group: &GroupState,
    phase: GroupPhase,
    config: &PhaseConfig,
) -> Result<String> {
    let report_path = PathBuf::from(&group.report_path);
    require_nonempty_file(&report_path)?;
    let slug = match phase {
        GroupPhase::Synthesis => "synthesis",
        GroupPhase::Remediation => "remediation",
    };
    let prompt = match phase {
        GroupPhase::Synthesis => build_synthesis_prompt(paths, group),
        GroupPhase::Remediation => build_remediation_prompt(paths, group),
    };
    run_codex_phase_for_artifact(
        paths,
        report_path.parent().unwrap_or(&paths.report_root),
        slug,
        &prompt,
        config,
    )
    .await?;
    require_nonempty_file(&report_path)?;
    Ok(group.slug.clone())
}

async fn run_codex_phase(
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

async fn run_codex_phase_for_artifact(
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
    println!(
        "phase:       {phase_slug} | model: {} | effort: {} | prompt: {}",
        config.model,
        config.effort,
        prompt_path.display()
    );
    let status = run_codex_exec_max_context(
        &paths.worktree_root,
        prompt,
        &config.model,
        &config.effort,
        &config.codex_bin,
        &stderr_path,
        Some(&stdout_path),
        phase_slug,
    )
    .await?;
    if !status.success() {
        bail!(
            "professional audit phase `{phase_slug}` failed with status {status}; see {}",
            stderr_path.display()
        );
    }
    Ok(())
}

fn write_skill_policy_reference(paths: &RunPaths) -> Result<()> {
    fs::create_dir_all(&paths.report_root)
        .with_context(|| format!("failed to create {}", paths.report_root.display()))?;
    atomic_write(
        &paths.report_root.join("GSTACK-SKILL-POLICY.md"),
        GSTACK_SKILL_POLICY.as_bytes(),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            paths.report_root.join("GSTACK-SKILL-POLICY.md").display()
        )
    })
}

fn selected_skill_policy_for_file(path: &str) -> String {
    render_skill_policy(&selected_skill_names_for_file(path))
}

fn selected_skill_policy_for_group(group: &GroupState) -> String {
    let mut selected = Vec::new();
    push_unique(&mut selected, "review");
    push_unique(&mut selected, "health");
    push_unique(&mut selected, "investigate");
    push_unique(&mut selected, "plan-eng-review");
    for path in &group.files {
        for skill in selected_skill_names_for_file(path) {
            push_unique(&mut selected, skill);
        }
    }
    render_skill_policy(&selected)
}

fn selected_skill_policy_for_final_review() -> String {
    render_skill_policy(&[
        "review",
        "cso",
        "health",
        "investigate",
        "careful",
        "qa-only",
        "benchmark",
        "devex-review",
        "document-release",
        "ship",
        "land-and-deploy",
        "canary",
        "checkpoint",
    ])
}

fn selected_skill_names_for_file(path: &str) -> Vec<&'static str> {
    let lower = path.to_ascii_lowercase();
    let mut selected = Vec::new();
    push_unique(&mut selected, "review");
    push_unique(&mut selected, "health");
    push_unique(&mut selected, "investigate");

    if is_context_path(&lower) {
        push_unique(&mut selected, "plan-ceo-review");
        push_unique(&mut selected, "plan-eng-review");
        push_unique(&mut selected, "plan-devex-review");
        push_unique(&mut selected, "plan-design-review");
        push_unique(&mut selected, "document-release");
        push_unique(&mut selected, "checkpoint");
        push_unique(&mut selected, "context-save");
        push_unique(&mut selected, "context-restore");
    }
    if is_rust_or_backend_path(&lower) {
        push_unique(&mut selected, "plan-eng-review");
        push_unique(&mut selected, "cso");
    }
    if is_security_or_ops_path(&lower) {
        push_unique(&mut selected, "cso");
        push_unique(&mut selected, "careful");
        push_unique(&mut selected, "ship");
    }
    if is_ui_path(&lower) {
        push_unique(&mut selected, "plan-design-review");
        push_unique(&mut selected, "design-review");
        push_unique(&mut selected, "qa");
        push_unique(&mut selected, "qa-only");
        push_unique(&mut selected, "browse");
        push_unique(&mut selected, "benchmark");
    }
    if is_docs_or_devex_path(&lower) {
        push_unique(&mut selected, "plan-devex-review");
        push_unique(&mut selected, "devex-review");
        push_unique(&mut selected, "document-release");
    }
    if is_test_or_perf_path(&lower) {
        push_unique(&mut selected, "qa");
        push_unique(&mut selected, "qa-only");
        push_unique(&mut selected, "benchmark");
    }
    if is_release_or_deploy_path(&lower) {
        push_unique(&mut selected, "ship");
        push_unique(&mut selected, "land-and-deploy");
        push_unique(&mut selected, "canary");
        push_unique(&mut selected, "setup-deploy");
    }

    selected
}

fn push_unique<'a>(items: &mut Vec<&'a str>, item: &'a str) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn render_skill_policy(skills: &[&str]) -> String {
    skills
        .iter()
        .map(|skill| format!("- `{skill}`: {}", skill_summary(skill)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn skill_summary(skill: &str) -> &'static str {
    match skill {
        "autoplan" => "run the CEO, engineering, design, and DX review gauntlet as one planning lens.",
        "benchmark" => "check page speed, Core Web Vitals, resource size, and bundle/performance regressions.",
        "browse" => "use browser evidence for UI state, screenshots, responsive behavior, forms, dialogs, and flows.",
        "canary" => "use post-deploy health, console, screenshot, and performance anomaly checks as release criteria.",
        "careful" => "treat destructive commands, deletions, force pushes, production, and shared resources as gated risks.",
        "checkpoint" => "preserve resumability: decisions, git state, remaining work, and handoff clarity.",
        "context-restore" => "verify restored context is sufficient before resuming interrupted work.",
        "context-save" => "capture progress and remaining work in durable, resume-friendly artifacts.",
        "cso" => "audit secrets, auth boundaries, supply chain, CI/CD, LLM trust boundaries, OWASP, and STRIDE risks.",
        "design-consultation" => "create or repair design-system source-of-truth docs when UI lacks coherent direction.",
        "design-review" => "judge implemented UI for visual hierarchy, spacing, consistency, accessibility, and interaction polish.",
        "devex-review" => "test docs, CLI/API ergonomics, onboarding, error messages, and time-to-hello-world.",
        "document-release" => "keep README, AGENTS, ARCHITECTURE, changelog, specs, and TODOs aligned with shipped behavior.",
        "freeze" => "hold remediation to the intended directory or module boundary.",
        "guard" => "combine destructive-command caution with strict write-scope discipline.",
        "health" => "prefer project-native check, lint, test, dead-code, and shell-lint evidence over guesswork.",
        "investigate" => "insist on root cause, falsifiable hypotheses, and direct evidence before proposing fixes.",
        "land-and-deploy" => "judge merge/deploy/canary readiness; do not perform deployment from an audit worker.",
        "plan-ceo-review" => "challenge scope, ambition, product value, and whether the best-version recommendation is worthwhile.",
        "plan-design-review" => "score UI/UX plans for interaction model, accessibility, visual system, hierarchy, and polish.",
        "plan-devex-review" => "score developer-facing APIs, CLIs, docs, onboarding, and friction before implementation.",
        "plan-eng-review" => "review architecture, invariants, data flow, edge cases, test plan, performance, and rollout risk.",
        "qa" => "when edits are allowed, run a test-fix-verify loop for app and browser-facing behavior.",
        "qa-only" => "when edits are disallowed, produce report-only QA evidence with repro steps and health score.",
        "review" => "pre-landing code-review lens for structural bugs, behavioral regressions, and stale documentation.",
        "setup-deploy" => "verify deployment configuration, production URL, health checks, and status commands exist and are current.",
        "ship" => "evaluate base-branch sync, validation, version/changelog, diff hygiene, and PR readiness.",
        _ => "use only when the audited surface directly implements or depends on this skill.",
    }
}

fn is_context_path(path: &str) -> bool {
    path == "agents.md"
        || path == "architecture.md"
        || path == "claude.md"
        || path.starts_with("doctrine/")
        || path.starts_with("specs/")
        || path.starts_with("plans/")
        || path.contains("architecture")
}

fn is_rust_or_backend_path(path: &str) -> bool {
    path.ends_with(".rs")
        || path.ends_with(".toml")
        || path.starts_with("src/")
        || path.starts_with("crates/")
        || path.starts_with("packages/")
        || path.contains("/server/")
        || path.contains("/backend/")
        || path.contains("/api/")
}

fn is_security_or_ops_path(path: &str) -> bool {
    path.contains("auth")
        || path.contains("secret")
        || path.contains("credential")
        || path.contains("token")
        || path.contains("session")
        || path.contains("cookie")
        || path.contains("tls")
        || path.contains("security")
        || path.contains("policy")
        || path.starts_with(".github/")
        || path.starts_with("infra/")
        || path.starts_with("ops/")
        || path.starts_with("deploy/")
        || path.contains("docker")
}

fn is_ui_path(path: &str) -> bool {
    path.ends_with(".tsx")
        || path.ends_with(".jsx")
        || path.ends_with(".css")
        || path.ends_with(".scss")
        || path.ends_with(".html")
        || path.contains("/ui/")
        || path.contains("/frontend/")
        || path.contains("/client/")
        || path.contains("/web/")
        || path.contains("/tui/")
        || path.contains("component")
        || path.contains("screen")
        || path.contains("view")
}

fn is_docs_or_devex_path(path: &str) -> bool {
    path.ends_with(".md")
        || path.starts_with("docs/")
        || path.starts_with("examples/")
        || path.starts_with("scripts/")
        || path.contains("readme")
        || path.contains("cli")
        || path.contains("help")
        || path.contains("onboard")
}

fn is_test_or_perf_path(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.contains("test")
        || path.contains("spec")
        || path.contains("bench")
        || path.contains("perf")
        || path.contains("playwright")
}

fn is_release_or_deploy_path(path: &str) -> bool {
    path.contains("release")
        || path.contains("deploy")
        || path.contains("ship")
        || path.contains("version")
        || path.contains("changelog")
        || path.contains("canary")
        || path.starts_with(".github/workflows/")
}

fn build_context_prompt(worktree_root: &Path, report_root: &Path) -> String {
    format!(
        r#"You are preparing the context layer for `auto audit --everything`.

Repository root: `{worktree_root}`
Report root: `{report_root}`
GStack skill policy: `{report_root}/GSTACK-SKILL-POLICY.md`

Edit only repository-local context documents and the report root:
- Create or revise root `AGENTS.md`.
- Create or revise root `ARCHITECTURE.md`.
- Write `{report_root}/CONTEXT.md` summarizing what changed and what remains inferred.

Context engineering requirements:
- Follow the OpenAI harness-engineering posture: `AGENTS.md` is a short map, not a giant manual.
- Keep `AGENTS.md` concise and operational. Point to deeper docs instead of copying them.
- Follow Matklad's `ARCHITECTURE.md` guidance: describe the problem, codemap, module boundaries, invariants, and cross-cutting concerns. Keep details stable and avoid stale links.
- If `doctrine/` exists and contains files, reference it explicitly as doctrine injected into every audit loop. If it does not exist or is empty, ignore it.
- Reference the gstack skill policy as a compact routing artifact for future audit workers. Do not paste the full policy into `AGENTS.md`; point to it and keep `AGENTS.md` short.
- Treat gstack skills as deterministic lenses by phase. Direct tool-like invocation is reserved for remediation/final validation when the selected surface calls for browser, QA, benchmark, deploy, or documentation checks.
- Favor evidence-backed statements. Mark inferred architecture as inferred instead of pretending certainty.
- These first target repos are Bitino and Autonomy, so make the docs useful for Rust workspace/crate-heavy systems, runtime operators, and agent workers.

Do not edit source code in this phase. Do not run formatters across the repo.
"#,
        worktree_root = worktree_root.display(),
        report_root = report_root.display(),
    )
}

fn build_file_prompt(file: &FileState, context: &str, file_body: &str) -> String {
    let skill_policy = selected_skill_policy_for_file(&file.path);
    format!(
        r#"You are running first-pass professional audit analysis for exactly one tracked file.

Hard boundaries:
- Analyze only the file named below.
- Do not edit repository source files.
- Do not read neighboring source files in this first pass.
- The only architectural context you may use is the injected context below.
- Write outputs only in the artifact directory.
- Apply only the selected gstack lenses below for this file's surface. Do not invoke tools in this first pass. Do not discuss unrelated lenses.

Injected context:
{context}

Selected gstack lenses:
{skill_policy}

File under audit:
- Path: `{path}`
- Group: `{group}`
- Content hash: `{hash}`
- Artifact directory: `{artifact_dir}`

Write these files:
1. `{artifact_dir}/analysis.md`
2. `{artifact_dir}/analysis.json`

`analysis.md` must include:
- `# {path}`
- What this file does.
- Important public types/functions/modules/configuration it owns.
- How it appears to fit the architecture.
- Whether it is the best version of itself it could be.
- If not 10/10, list expansions, deletions, revisions, clarifications, tests, code refactors, documentation moves, or retirement steps that would make it an idiomatic 10/10 work product.
- Cross-file questions or likely relationships surfaced by this file, without resolving them from other source files in this pass.

`analysis.json` must be valid JSON with:
`path`, `group`, `score_out_of_10`, `summary`, `best_version_assessment`, `recommended_actions`, `cross_file_questions`, `confidence`.

Target file content:
```text
{file_body}
```
"#,
        context = context,
        skill_policy = skill_policy,
        path = file.path,
        group = file.group,
        hash = file.content_hash,
        artifact_dir = file.artifact_dir,
        file_body = file_body,
    )
}

fn build_synthesis_prompt(paths: &RunPaths, group: &GroupState) -> String {
    let skill_policy = selected_skill_policy_for_group(group);
    format!(
        r#"You are the second-pass cross-file synthesis reviewer for one professional audit group.

Repository root: `{repo}`
Group: `{group}`
Report: `{report}`

Read the group report and the per-file first-pass analyses it references. You may now reason across files in this group and across the concise context docs (`AGENTS.md`, `ARCHITECTURE.md`, and `doctrine/` if present).

Selected gstack lenses for this group:
{skill_policy}

Revise `{report}` in place. Keep every file represented. Tighten or correct the first-pass assessments based on relationships surfaced between files:
- duplicated responsibilities
- unclear ownership or misplaced modules
- missing invariants
- dead code or files that should retire
- test gaps
- docs that should move into `AGENTS.md`, `ARCHITECTURE.md`, doctrine, or inline comments
- cross-crate/API seams

Use the selected lenses as a compact prompt injection, not as permission to bulk-load unrelated skill files. Keep the output grounded in repository evidence.

Do not edit source code in this phase. Only edit `{report}` and optional notes next to it.
"#,
        repo = paths.worktree_root.display(),
        group = group.name,
        report = group.report_path,
        skill_policy = skill_policy,
    )
}

fn build_remediation_prompt(paths: &RunPaths, group: &GroupState) -> String {
    let skill_policy = selected_skill_policy_for_group(group);
    format!(
        r#"You are the crate-by-crate remediation worker for `auto audit --everything`.

Repository root: `{repo}`
Group: `{group}`
Report: `{report}`

You are not alone in the codebase. Other audit phases may have made changes. Preserve existing changes and do not revert unrelated work.

Read `AGENTS.md`, `ARCHITECTURE.md`, doctrine if present, and `{report}`. Apply bounded improvements for this group only:
- code refactors that are clearly supported by the report
- test additions or corrections needed for changed behavior
- documentation clarifications when they make future agent work more legible
- deletion/retirement only when the report confidence is high and the repo evidence confirms it

Selected gstack lenses for this group:
{skill_policy}

Use these lenses prescriptively. Directly run browser/QA/benchmark/devex/documentation checks only when the group surface and report recommendations call for them and the required local services or commands are available.

Keep the write set centered on this group. If a recommendation requires broad cross-group work, leave it in the report as a follow-up instead of expanding scope.

Update `{report}` as you go:
- mark completed recommendations
- record changed files
- record validation commands run and their result
- record remaining blockers honestly

Before finishing, run the narrowest meaningful validation you can derive for this group. If validation is blocked by missing external infrastructure, record that blocker in the report.
"#,
        repo = paths.worktree_root.display(),
        group = group.name,
        report = group.report_path,
        skill_policy = skill_policy,
    )
}

fn build_final_review_prompt(paths: &RunPaths, manifest: &EverythingManifest) -> String {
    let skill_policy = selected_skill_policy_for_final_review();
    format!(
        r#"You are the final professional audit reviewer.

Repository root: `{repo}`
Report root: `{report_root}`
Base commit: `{base}`
Audit branch: `{branch}`

Review all group reports under the report root and the full git diff from `{base}` to HEAD.

Selected gstack lenses for final review:
{skill_policy}

Use `gpt-5.5 xhigh` judgment standards:
- Verify changes correspond to report findings.
- Reject speculative rewrites not grounded in file reports.
- Check for broken architecture docs, stale AGENTS instructions, overbroad edits, missing tests, and merge-risk.
- Run or inspect the narrowest feasible validation for the changed surfaces.

Write `{report_root}/FINAL-REVIEW.md` with:
- `# FINAL REVIEW`
- A line exactly `Verdict: GO` or `Verdict: NO-GO`
- Diff summary
- Report consistency assessment
- Validation run and result
- Required blockers before merge
- Optional follow-ups

Do not merge. The host runner handles merge only after this file says `Verdict: GO`.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        base = manifest.base_commit,
        branch = manifest.audit_branch,
        skill_policy = skill_policy,
    )
}

fn read_context_bundle(paths: &RunPaths) -> Result<String> {
    let bundle = paths.report_root.join("CONTEXT-BUNDLE.md");
    if bundle.exists() {
        return fs::read_to_string(&bundle)
            .with_context(|| format!("failed to read {}", bundle.display()));
    }
    write_context_bundle(paths)?;
    fs::read_to_string(&bundle).with_context(|| format!("failed to read {}", bundle.display()))
}

fn write_context_bundle(paths: &RunPaths) -> Result<()> {
    fs::create_dir_all(&paths.report_root)
        .with_context(|| format!("failed to create {}", paths.report_root.display()))?;
    let mut body = String::new();
    body.push_str("# Context Bundle\n\n");
    append_named_file(
        &mut body,
        "AGENTS.md",
        &paths.worktree_root.join("AGENTS.md"),
        true,
    )?;
    append_named_file(
        &mut body,
        "ARCHITECTURE.md",
        &paths.worktree_root.join("ARCHITECTURE.md"),
        true,
    )?;
    append_named_file(
        &mut body,
        "GSTACK-SKILL-POLICY.md",
        &paths.report_root.join("GSTACK-SKILL-POLICY.md"),
        true,
    )?;
    let doctrine_dir = paths.worktree_root.join("doctrine");
    if doctrine_dir.is_dir() {
        let mut doctrine_files = collect_regular_files(&doctrine_dir)?;
        doctrine_files.sort();
        for path in doctrine_files {
            let rel = path
                .strip_prefix(&paths.worktree_root)
                .unwrap_or(&path)
                .display()
                .to_string();
            append_named_file(&mut body, &rel, &path, false)?;
        }
    }
    atomic_write(
        &paths.report_root.join("CONTEXT-BUNDLE.md"),
        body.as_bytes(),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            paths.report_root.join("CONTEXT-BUNDLE.md").display()
        )
    })
}

fn append_named_file(body: &mut String, name: &str, path: &Path, required: bool) -> Result<()> {
    if !path.exists() {
        if required {
            bail!("required context file missing: {}", path.display());
        }
        return Ok(());
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if text.trim().is_empty() {
        if required {
            bail!("required context file is empty: {}", path.display());
        }
        return Ok(());
    }
    body.push_str(&format!("## {name}\n\n```markdown\n{text}\n```\n\n"));
    Ok(())
}

fn reconcile_file_inventory(
    worktree_root: &Path,
    report_root: &Path,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if !worktree_root.exists() {
        return Ok(());
    }
    let tracked = enumerate_tracked_files(worktree_root)?;
    let existing_status = manifest
        .files
        .iter()
        .map(|file| (file.path.clone(), file.status))
        .collect::<BTreeMap<_, _>>();
    let groups = classify_groups(worktree_root, &tracked);
    let mut files = Vec::new();
    for path in tracked {
        let content = fs::read(worktree_root.join(&path))
            .with_context(|| format!("failed to read {}", worktree_root.join(&path).display()))?;
        let hash = sha256_hex(&content);
        let artifact_dir = report_root
            .join("files")
            .join(short_hash(&hash))
            .display()
            .to_string();
        let status = existing_status
            .get(&path)
            .copied()
            .filter(|status| {
                artifact_complete(Path::new(&artifact_dir)) || *status != StageStatus::Complete
            })
            .unwrap_or(StageStatus::Pending);
        files.push(FileState {
            group: groups
                .get(&path)
                .cloned()
                .unwrap_or_else(|| "root".to_string()),
            path,
            content_hash: hash,
            artifact_dir,
            status,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    manifest.files = files;
    rebuild_group_states(report_root, manifest);
    Ok(())
}

fn enumerate_tracked_files(repo_root: &Path) -> Result<Vec<String>> {
    let listing = git_stdout(repo_root, ["ls-files", "-z"])?;
    let mut files = listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| !excluded_path(path))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    for context_file in ["AGENTS.md", "ARCHITECTURE.md"] {
        if repo_root.join(context_file).exists() && !files.iter().any(|path| path == context_file) {
            files.push(context_file.to_string());
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn excluded_path(path: &str) -> bool {
    DEFAULT_EXCLUDE_PREFIXES.iter().any(|prefix| {
        if prefix.ends_with('/') {
            path.starts_with(prefix)
        } else {
            path == *prefix || path.starts_with(prefix)
        }
    })
}

fn classify_groups(repo_root: &Path, files: &[String]) -> BTreeMap<String, String> {
    let crate_roots = cargo_member_roots(repo_root);
    let mut map = BTreeMap::new();
    for path in files {
        let group = crate_roots
            .iter()
            .filter(|root| path == *root || path.starts_with(&format!("{root}/")))
            .max_by_key(|root| root.len())
            .cloned()
            .unwrap_or_else(|| fallback_group(path));
        map.insert(path.clone(), group);
    }
    map
}

fn cargo_member_roots(repo_root: &Path) -> Vec<String> {
    let mut roots = BTreeSet::new();
    let cargo = repo_root.join("Cargo.toml");
    if let Ok(raw) = fs::read_to_string(&cargo) {
        if let Ok(value) = raw.parse::<toml::Value>() {
            if value
                .get("package")
                .and_then(|pkg| pkg.get("name"))
                .is_some()
            {
                roots.insert(".".to_string());
            }
            if let Some(members) = value
                .get("workspace")
                .and_then(|workspace| workspace.get("members"))
                .and_then(|members| members.as_array())
            {
                for member in members.iter().filter_map(|member| member.as_str()) {
                    if !member.contains('*') {
                        roots.insert(member.trim_matches('/').to_string());
                    }
                }
            }
        }
    }
    roots.into_iter().collect()
}

fn fallback_group(path: &str) -> String {
    if path.starts_with("crates/") {
        return path.split('/').take(2).collect::<Vec<_>>().join("/");
    }
    if path.starts_with("src/") {
        return "src".to_string();
    }
    if path.starts_with("tests/") {
        return "tests".to_string();
    }
    if path.starts_with("docs/") {
        return "docs".to_string();
    }
    if path.starts_with("specs/") {
        return "specs".to_string();
    }
    path.split('/').next().unwrap_or("root").to_string()
}

fn rebuild_group_states(report_root: &Path, manifest: &mut EverythingManifest) {
    let old = manifest
        .groups
        .iter()
        .map(|group| (group.name.clone(), group.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in &manifest.files {
        grouped
            .entry(file.group.clone())
            .or_default()
            .push(file.path.clone());
    }
    manifest.groups = grouped
        .into_iter()
        .map(|(name, mut files)| {
            files.sort();
            let slug = slugify(&name);
            let report_path = report_root
                .join("reports")
                .join(format!("{slug}.md"))
                .display()
                .to_string();
            let previous = old.get(&name);
            GroupState {
                name,
                slug,
                files,
                report_path,
                synthesis_status: previous
                    .map(|group| group.synthesis_status)
                    .unwrap_or(StageStatus::Pending),
                remediation_status: previous
                    .map(|group| group.remediation_status)
                    .unwrap_or(StageStatus::Pending),
            }
        })
        .collect();
}

fn build_initial_group_reports(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    fs::create_dir_all(paths.report_root.join("reports")).with_context(|| {
        format!(
            "failed to create {}",
            paths.report_root.join("reports").display()
        )
    })?;
    for group in &manifest.groups {
        let report_path = PathBuf::from(&group.report_path);
        if report_path.exists() {
            continue;
        }
        let mut body = String::new();
        body.push_str(&format!("# Audit Report: {}\n\n", group.name));
        body.push_str("## Scope\n\n");
        body.push_str("This report is assembled from first-pass one-file analyses. The synthesis pass may revise it based on cross-file relationships.\n\n");
        for file_path in &group.files {
            if let Some(file) = manifest.files.iter().find(|file| &file.path == file_path) {
                body.push_str(&format!("## `{}`\n\n", file.path));
                let analysis = Path::new(&file.artifact_dir).join("analysis.md");
                if analysis.exists() {
                    body.push_str(
                        &fs::read_to_string(&analysis)
                            .with_context(|| format!("failed to read {}", analysis.display()))?,
                    );
                    body.push_str("\n\n");
                } else {
                    body.push_str("_First-pass analysis missing._\n\n");
                }
            }
        }
        atomic_write(&report_path, body.as_bytes())
            .with_context(|| format!("failed to write {}", report_path.display()))?;
    }
    Ok(())
}

fn prompt_file_body(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() > MAX_FILE_PROMPT_BYTES {
        return Ok(format!(
            "[file omitted from prompt because it is {} bytes; inspect metadata and path only]",
            bytes.len()
        ));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(err) => Ok(format!(
            "[binary or non-UTF8 file omitted from prompt: {} valid bytes before error]",
            err.utf8_error().valid_up_to()
        )),
    }
}

fn artifact_complete(artifact_dir: &Path) -> bool {
    artifact_dir.join("analysis.md").exists() && artifact_dir.join("analysis.json").exists()
}

fn require_context_complete(manifest: &EverythingManifest) -> Result<()> {
    if !matches!(manifest.context.status, StageStatus::Complete) {
        bail!("context phase is not complete; run --everything-phase init-context first");
    }
    Ok(())
}

fn require_first_pass_complete(manifest: &EverythingManifest) -> Result<()> {
    let incomplete = manifest
        .files
        .iter()
        .filter(|file| !matches!(file.status, StageStatus::Complete))
        .count();
    if incomplete > 0 {
        bail!("first pass has {incomplete} incomplete file(s)");
    }
    Ok(())
}

fn require_synthesis_complete(manifest: &EverythingManifest) -> Result<()> {
    require_first_pass_complete(manifest)?;
    let incomplete = manifest
        .groups
        .iter()
        .filter(|group| !matches!(group.synthesis_status, StageStatus::Complete))
        .count();
    if incomplete > 0 {
        bail!("synthesis has {incomplete} incomplete group(s)");
    }
    Ok(())
}

fn mark_remediation_skipped(
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
    reason: &str,
) -> Result<()> {
    for group in &mut manifest.groups {
        if !matches!(group.remediation_status, StageStatus::Complete) {
            group.remediation_status = StageStatus::Skipped;
        }
    }
    manifest.merge.status = StageStatus::Skipped;
    manifest.merge.note = Some(format!("remediation skipped: {reason}"));
    write_manifest(paths, manifest)
}

fn mark_merge_skipped(
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
    reason: &str,
) -> Result<()> {
    manifest.merge.status = StageStatus::Skipped;
    manifest.merge.note = Some(reason.to_string());
    write_manifest(paths, manifest)
}

fn final_review_is_go(paths: &RunPaths) -> bool {
    let path = paths.report_root.join("FINAL-REVIEW.md");
    fs::read_to_string(path)
        .map(|text| text.lines().any(|line| line.trim() == "Verdict: GO"))
        .unwrap_or(false)
}

fn first_verdict_line(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.trim().starts_with("Verdict:"))
        .map(|line| line.trim().to_string())
}

fn print_status(manifest: &EverythingManifest) {
    let files_done = manifest
        .files
        .iter()
        .filter(|file| matches!(file.status, StageStatus::Complete))
        .count();
    let synthesis_done = manifest
        .groups
        .iter()
        .filter(|group| matches!(group.synthesis_status, StageStatus::Complete))
        .count();
    let remediation_done = manifest
        .groups
        .iter()
        .filter(|group| matches!(group.remediation_status, StageStatus::Complete))
        .count();
    println!("status");
    println!("context:     {:?}", manifest.context.status);
    println!("files:       {files_done}/{}", manifest.files.len());
    println!("synthesis:   {synthesis_done}/{}", manifest.groups.len());
    println!("remediation: {remediation_done}/{}", manifest.groups.len());
    println!("final review:{:?}", manifest.final_review.status);
    println!("merge:       {:?}", manifest.merge.status);
}

fn write_manifest(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    atomic_write(&paths.manifest_path, &serde_json::to_vec_pretty(manifest)?)
        .with_context(|| format!("failed to write {}", paths.manifest_path.display()))
}

fn resolve_primary_branch(
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

fn remote_branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_ref_exists(repo_root, &format!("refs/remotes/origin/{branch}"))
}

fn git_ref_exists(repo_root: &Path, reference: &str) -> bool {
    command_status(repo_root, ["show-ref", "--verify", "--quiet", reference])
        .is_ok_and(|status| status.success())
}

fn command_status<'a>(
    repo_root: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<std::process::ExitStatus> {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .status()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))
}

fn run_git_dynamic(repo_root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    bail!(
        "git command failed in {}: {}",
        repo_root.display(),
        if stderr.is_empty() { stdout } else { stderr }
    );
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn require_nonempty_file(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("missing {}", path.display()))?;
    if metadata.len() == 0 {
        bail!("{} is empty", path.display());
    }
    Ok(())
}

fn hash_file_if_exists(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(sha256_hex(&bytes)))
}

fn hash_doctrine(repo_root: &Path) -> Result<String> {
    let doctrine_dir = repo_root.join("doctrine");
    if !doctrine_dir.is_dir() {
        return Ok(sha256_hex(b""));
    }
    let mut files = collect_regular_files(&doctrine_dir)?;
    files.sort();
    let mut bytes = Vec::new();
    for path in files {
        bytes.extend(path.display().to_string().as_bytes());
        bytes.push(0);
        bytes
            .extend(fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?);
        bytes.push(0);
    }
    Ok(sha256_hex(&bytes))
}

fn collect_regular_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("failed to inspect {}", dir.display()))?;
            let path = entry.path();
            let ty = entry
                .file_type()
                .with_context(|| format!("failed to stat {}", path.display()))?;
            if ty.is_dir() {
                stack.push(path);
            } else if ty.is_file() {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn short_hash(hash: &str) -> String {
    hash.chars().take(16).collect()
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "root".to_string()
    } else {
        slug
    }
}

fn path_display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_group_classifies_common_repo_surfaces() {
        assert_eq!(
            fallback_group("crates/bitino-house/src/lib.rs"),
            "crates/bitino-house"
        );
        assert_eq!(fallback_group("src/main.rs"), "src");
        assert_eq!(fallback_group("tests/parallel_status.rs"), "tests");
        assert_eq!(fallback_group("docs/ops/runbook.md"), "docs");
        assert_eq!(fallback_group("Cargo.toml"), "Cargo.toml");
    }

    #[test]
    fn slugify_keeps_group_names_path_safe() {
        assert_eq!(slugify("crates/bitino-house"), "crates-bitino-house");
        assert_eq!(slugify("Autonomy Core!"), "autonomy-core");
    }

    #[test]
    fn excluded_path_skips_generated_and_runtime_state() {
        assert!(excluded_path(".auto/audit/log"));
        assert!(excluded_path("target/debug/app"));
        assert!(excluded_path("gen-20260424/spec.md"));
        assert!(!excluded_path("crates/bitino-house/src/lib.rs"));
    }

    #[test]
    fn selected_skill_policy_matches_ui_surface() {
        let skills = selected_skill_names_for_file("web/client/src/components/Board.tsx");
        assert!(skills.contains(&"plan-design-review"));
        assert!(skills.contains(&"design-review"));
        assert!(skills.contains(&"qa"));
        assert!(skills.contains(&"browse"));
        assert!(skills.contains(&"benchmark"));
    }

    #[test]
    fn selected_skill_policy_matches_security_and_deploy_surface() {
        let skills = selected_skill_names_for_file(".github/workflows/deploy-auth.yml");
        assert!(skills.contains(&"cso"));
        assert!(skills.contains(&"careful"));
        assert!(skills.contains(&"ship"));
        assert!(skills.contains(&"land-and-deploy"));
        assert!(skills.contains(&"setup-deploy"));
    }

    #[test]
    fn selected_skill_policy_matches_docs_and_context_surface() {
        let skills = selected_skill_names_for_file("ARCHITECTURE.md");
        assert!(skills.contains(&"plan-ceo-review"));
        assert!(skills.contains(&"plan-eng-review"));
        assert!(skills.contains(&"plan-devex-review"));
        assert!(skills.contains(&"document-release"));
        assert!(skills.contains(&"checkpoint"));
    }

    #[test]
    fn final_review_policy_is_merge_readiness_oriented() {
        let policy = selected_skill_policy_for_final_review();
        assert!(policy.contains("`review`"));
        assert!(policy.contains("`ship`"));
        assert!(policy.contains("`land-and-deploy`"));
        assert!(policy.contains("`canary`"));
    }
}
