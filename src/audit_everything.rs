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

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;

use crate::codex_exec::run_codex_exec_max_context;
use crate::util::{
    atomic_write, binary_provenance_line, ensure_repo_layout, git_repo_root, git_stdout, run_git,
    timestamp_slug,
};
use crate::{AuditArgs, AuditEverythingPhase};

const PROFESSIONAL_AUDIT_DIR: &str = ".auto/audit-everything";
const LATEST_RUN_FILE: &str = "latest-run";
const PAUSE_REQUEST_FILE: &str = "PAUSE";
const MAX_FILE_PROMPT_BYTES: usize = 220_000;
const LEGACY_LARGE_FILE_OMISSION_MARKER: &str = "[file omitted from prompt because";
const DEFAULT_FILE_QUALITY_PASS_LIMIT: usize = 10;
const FILE_QUALITY_ACCEPT_SCORE: f64 = 9.0;
const FILE_QUALITY_TARGET_SCORE: f64 = 10.0;
const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["trunk", "main", "master"];
const DEFAULT_EXCLUDE_PREFIXES: [&str; 16] = [
    ".git/",
    ".auto/",
    ".claude/worktrees/",
    ".gstack/",
    "audit/",
    "target/",
    "node_modules/",
    "dist/",
    "build/",
    "bug/",
    "nemesis/",
    "gen-",
    ".github/ISSUE_TEMPLATE/",
    "docs/ops/operator-evidence/",
    "web/client/dist/",
    "web/play/dist/",
];
const DEFAULT_EXCLUDE_PATH_SEGMENTS: [&str; 3] = ["/node_modules/", "/target/", "/dist/"];
const DEFAULT_EXCLUDE_SUFFIXES: [&str; 12] = [
    ".lock", ".map", ".png", ".jpg", ".jpeg", ".webp", ".gif", ".pdf", ".ico", ".mp4", ".mov",
    ".zip",
];
const DEFAULT_EXCLUDE_FILENAMES: [&str; 4] = [
    "Cargo.lock",
    "pnpm-lock.yaml",
    "package-lock.json",
    "bun.lockb",
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

const CODEBASE_IMPROVEMENT_POLICY: &str = r#"# Codebase Improvement Policy

This audit is allowed to improve architecture, delete proved-dead code, and remove accumulated agent-written filler. It should not degrade product substance in exchange for a cleaner-looking diff.

## Default Posture

- Prefer deletion, consolidation, and simplification when repository evidence proves code is orphaned, deprecated, duplicated, transitional, or hollow.
- Improve module boundaries and dependency direction when a group report shows shallow modules, misplaced ownership, leaked invariants, or domain vocabulary drift.
- Treat "AI slop" as real debt: vague comments, generic wrappers, fake extensibility, repeated boilerplate, overexplained docs, and abstractions that do not protect an invariant or simplify a caller.
- Preserve behavior unless the synthesized report explicitly recommends changing it and the final review can explain why.

## Required Proof Before Removal

A remediation lane may delete or retire code only when it records proof in the lane report:

- reachability evidence from references, imports, exports, entrypoints, commands, config, docs, generated bindings, and tests
- public API, CLI, operator, or runtime behavior either preserved or intentionally updated
- narrow validation or characterization evidence for the affected surface
- confirmation that durable audit evidence, production/mainnet proof, generated runtime state, and operator artifacts were not removed merely because they looked unused

## Debt Register Classes

Use these classes in group reports and remediation notes:

- `safe_delete`: no live references or external contract remain
- `deprecated_remove`: an obsolete path can be removed with compatibility/docs updated
- `consolidate`: duplicated responsibilities should merge behind one owner
- `simplify`: code remains, but ceremony, wrappers, comments, or branches should shrink
- `deepen_module`: boundaries, names, invariants, or dependency direction need architectural repair
- `leave_with_reason`: suspicious code should stay because evidence shows it still carries substance

## Refactor Discipline

- Characterize behavior before risky refactors or deletions.
- Prefer vertical, reviewable changes over broad cosmetic churn.
- Update `ARCHITECTURE.md`, focused docs, or ADR-like notes when ownership, vocabulary, dependency direction, or durable invariants change.
- If proof is incomplete, leave the code in place and document the missing evidence instead of guessing.
"#;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EverythingManifest {
    run_id: String,
    repo_root: String,
    worktree_root: String,
    report_root: String,
    #[serde(default)]
    in_place: bool,
    branch: String,
    audit_branch: String,
    base_commit: String,
    created_at: String,
    context: ContextState,
    files: Vec<FileState>,
    groups: Vec<GroupState>,
    #[serde(default)]
    remediation_plan: StageState,
    #[serde(default)]
    remediation_tasks: Vec<RemediationTaskState>,
    #[serde(default)]
    final_review_repairs: Vec<StageState>,
    #[serde(default)]
    file_quality: StageState,
    #[serde(default)]
    file_quality_passes: Vec<FileQualityPassState>,
    #[serde(default)]
    change_summary: StageState,
    final_review: StageState,
    merge: StageState,
    #[serde(default)]
    final_status: StageState,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RemediationTaskState {
    id: String,
    group: String,
    slug: String,
    report_path: String,
    owned_paths: Vec<String>,
    dependencies: Vec<String>,
    lane_index: usize,
    lane_root: String,
    lane_repo_root: String,
    base_commit: Option<String>,
    status: StageStatus,
    note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileQualityPassState {
    pass_index: usize,
    status: StageStatus,
    artifact_dir: String,
    ratings: Vec<FileQualityRatingState>,
    note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileQualityRatingState {
    path: String,
    score_out_of_10: Option<f64>,
    status: StageStatus,
    artifact_dir: String,
    note: Option<String>,
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

#[derive(Clone)]
struct RunPaths {
    host_root: PathBuf,
    manifest_path: PathBuf,
    latest_path: PathBuf,
    worktree_root: PathBuf,
    report_root: PathBuf,
    pause_path: PathBuf,
    in_place: bool,
}

#[derive(Clone)]
struct PhaseConfig {
    model: String,
    effort: String,
    codex_bin: PathBuf,
}

struct RemediationLaneResult {
    task: RemediationTaskState,
    error: Option<String>,
}

struct RemediationSchedulerChoice {
    index: usize,
    unmet_dependencies: Vec<String>,
}

pub(crate) async fn run_audit_everything(args: AuditArgs) -> Result<()> {
    if args.everything_threads == 0 {
        bail!("--everything-threads must be greater than 0");
    }
    if args.remediation_threads == 0 {
        bail!("--remediation-threads must be greater than 0");
    }
    if args.file_quality_passes == 0 {
        bail!("--file-quality-passes must be greater than 0");
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
    println!(
        "mode:        {}",
        if manifest.in_place {
            "in-place"
        } else {
            "worktree"
        }
    );
    println!("phase:       {:?}", args.everything_phase);

    if matches!(args.everything_phase, AuditEverythingPhase::Pause) {
        request_pause(&paths, &manifest)?;
        print_status(&paths, &manifest);
        return Ok(());
    }

    if matches!(args.everything_phase, AuditEverythingPhase::Unpause) {
        clear_pause(&paths, &manifest)?;
        print_status(&paths, &manifest);
        return Ok(());
    }

    if matches!(args.everything_phase, AuditEverythingPhase::Status) {
        print_status(&paths, &manifest);
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
                run_remediation_plan_phase(&paths, &mut manifest)?;
                run_remediation_phase(&args, &paths, &mut manifest).await?;
                if pause_requested(&paths) {
                    println!("professional audit paused before final review");
                    print_status(&paths, &manifest);
                    return Ok(());
                }
                run_final_review_and_file_quality_phase(&args, &paths, &mut manifest).await?;
                if manifest.in_place {
                    complete_in_place_run(&paths, &mut manifest)?;
                    refresh_final_status_after_merge(&repo_root, &paths, &mut manifest)?;
                } else if args.no_everything_merge {
                    mark_merge_skipped(&paths, &mut manifest, "--no-everything-merge")?;
                } else {
                    attempt_merge(&repo_root, &paths, &mut manifest)?;
                    refresh_final_status_after_merge(&repo_root, &paths, &mut manifest)?;
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
        AuditEverythingPhase::PlanRemediation => {
            require_synthesis_complete(&manifest)?;
            run_remediation_plan_phase(&paths, &mut manifest)?;
        }
        AuditEverythingPhase::Remediate => {
            require_synthesis_complete(&manifest)?;
            if args.report_only {
                mark_remediation_skipped(&paths, &mut manifest, "--report-only")?;
            } else {
                run_remediation_plan_phase(&paths, &mut manifest)?;
                run_remediation_phase(&args, &paths, &mut manifest).await?;
            }
        }
        AuditEverythingPhase::FinalReview => {
            run_final_review_and_file_quality_phase(&args, &paths, &mut manifest).await?;
        }
        AuditEverythingPhase::Merge => {
            run_final_review_and_file_quality_phase(&args, &paths, &mut manifest).await?;
            if manifest.in_place {
                complete_in_place_run(&paths, &mut manifest)?;
                refresh_final_status_after_merge(&repo_root, &paths, &mut manifest)?;
            } else {
                attempt_merge(&repo_root, &paths, &mut manifest)?;
                refresh_final_status_after_merge(&repo_root, &paths, &mut manifest)?;
            }
        }
        AuditEverythingPhase::Pause
        | AuditEverythingPhase::Unpause
        | AuditEverythingPhase::Status => unreachable!("handled above"),
    }

    print_status(&paths, &manifest);
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
    atomic_write(&paths.latest_path, run_id.as_bytes())
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

fn ensure_clean_in_place_start(repo_root: &Path) -> Result<()> {
    let status = git_stdout(repo_root, ["status", "--short"])?;
    if !status.trim().is_empty() {
        bail!(
            "--everything-in-place requires a clean checkout for a new run; existing changes would be committed with audit artifacts:\n{}",
            status
        );
    }
    Ok(())
}

fn request_pause(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    let body = format!(
        "pause requested at {}\nrun: {}\n",
        timestamp_slug(),
        manifest.run_id
    );
    atomic_write(&paths.pause_path, body.as_bytes())
        .with_context(|| format!("failed to write {}", paths.pause_path.display()))?;
    write_run_status_if_possible(paths, manifest)?;
    println!("pause requested: {}", paths.pause_path.display());
    println!("active remediation lanes will drain; no new lanes will be dispatched");
    Ok(())
}

fn clear_pause(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    if paths.pause_path.exists() {
        fs::remove_file(&paths.pause_path)
            .with_context(|| format!("failed to remove {}", paths.pause_path.display()))?;
        println!("pause cleared: {}", paths.pause_path.display());
    } else {
        println!("pause was not active: {}", paths.pause_path.display());
    }
    write_run_status_if_possible(paths, manifest)?;
    Ok(())
}

fn pause_requested(paths: &RunPaths) -> bool {
    paths.pause_path.exists()
}

fn ensure_worktree(
    repo_root: &Path,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if manifest.in_place {
        fs::create_dir_all(&paths.report_root)
            .with_context(|| format!("failed to create {}", paths.report_root.display()))?;
        manifest.worktree_root = paths.worktree_root.display().to_string();
        manifest.report_root = paths.report_root.display().to_string();
        return Ok(());
    }
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
    let mut join_set = JoinSet::new();
    let mut pending_iter = pending.into_iter();
    let mut active = 0usize;
    for _ in 0..workers {
        if let Some(file) = pending_iter.next() {
            spawn_file_worker(&mut join_set, paths, file, &context, &config);
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
            Ok(Ok(path)) => {
                if let Some(file) = manifest.files.iter_mut().find(|file| file.path == path) {
                    file.status = StageStatus::Complete;
                }
                write_manifest(paths, manifest)?;
            }
            Ok(Err(err)) => failures.push(format!("{err:#}")),
            Err(err) => failures.push(format!("worker task panicked: {err}")),
        }
        if let Some(file) = pending_iter.next() {
            spawn_file_worker(&mut join_set, paths, file, &context, &config);
            active += 1;
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

fn run_remediation_plan_phase(paths: &RunPaths, manifest: &mut EverythingManifest) -> Result<()> {
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

fn persist_remediation_progress(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    write_remediation_plan_files(paths, manifest)?;
    write_manifest(paths, manifest)?;
    commit_worktree_changes(paths, manifest)
}

async fn run_final_review_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    let mut attempt = 0usize;
    loop {
        if matches!(manifest.final_review.status, StageStatus::Complete) {
            if final_review_is_go(paths) {
                println!("final review: complete (resume)");
                return Ok(());
            }
            if attempt == 0 {
                println!("final review: NO-GO (resume)");
            }
        } else {
            run_final_review_once(args, paths, manifest).await?;
        }

        if final_review_is_go(paths) {
            return Ok(());
        }
        if attempt >= args.final_review_retries {
            return Ok(());
        }
        if args.report_only {
            return Ok(());
        }
        if !final_review_has_actionable_blockers(paths) {
            return Ok(());
        }

        attempt += 1;
        run_final_review_repair_phase(args, paths, manifest, attempt).await?;
        manifest.final_review.status = StageStatus::Pending;
        manifest.final_review.note = Some(format!(
            "repair attempt {attempt} applied; final review pending rerun"
        ));
        write_manifest(paths, manifest)?;
    }
}

async fn run_final_review_and_file_quality_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    run_final_review_phase(args, paths, manifest).await?;
    if args.report_only {
        finalize_change_summary_phase(paths, manifest, !args.report_only)?;
        return Ok(());
    }
    if !final_review_is_go(paths) {
        manifest.file_quality.status = StageStatus::Failed;
        manifest.file_quality.note = Some(
            "final review is not GO; audit cannot complete until final-review blockers are resolved and every file rerates at least 9/10"
                .to_string(),
        );
        write_manifest(paths, manifest)?;
        finalize_change_summary_phase(paths, manifest, true)?;
        bail!(
            "final review is not GO; auto audit will not exit successfully until final-review blockers are resolved and every file rerates at least {FILE_QUALITY_ACCEPT_SCORE:.0}/10"
        );
    }
    let changed = run_file_quality_gate_phase(args, paths, manifest).await?;
    require_file_quality_acceptance(manifest)?;
    if changed {
        manifest.final_review.status = StageStatus::Pending;
        manifest.final_review.note = Some(
            "file-quality deliverables changed the audit branch; final review must rerun"
                .to_string(),
        );
        write_manifest(paths, manifest)?;
        run_final_review_phase(args, paths, manifest).await?;
        if !final_review_is_go(paths) {
            finalize_change_summary_phase(paths, manifest, true)?;
            bail!(
                "final review is not GO after file-quality deliverables; auto audit will not exit successfully until the rerun is GO and every file remains at least {FILE_QUALITY_ACCEPT_SCORE:.0}/10"
            );
        }
    }
    finalize_change_summary_phase(paths, manifest, true)?;
    Ok(())
}

fn finalize_change_summary_phase(
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
    commit: bool,
) -> Result<()> {
    manifest.change_summary.status = StageStatus::Running;
    manifest.change_summary.artifact = Some(path_display(&change_summary_markdown_path(paths)));
    write_manifest(paths, manifest)?;

    write_change_summary_markdown(paths, manifest)?;

    manifest.change_summary.status = StageStatus::Complete;
    manifest.change_summary.note = Some(
        "engineer-readable audit change summary written from audit branch git history".to_string(),
    );
    write_manifest(paths, manifest)?;
    if commit {
        commit_worktree_changes(paths, manifest)?;
    }
    Ok(())
}

async fn run_final_review_once(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    manifest.final_review.status = StageStatus::Running;
    write_manifest(paths, manifest)?;
    let config = PhaseConfig {
        model: args.final_review_model.clone(),
        effort: args.final_review_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let workers = args.everything_threads.clamp(1, 15);
    let shard_root = if workers > 1 && manifest.groups.len() > 1 {
        Some(run_final_review_shards(args, paths, manifest, &config, workers).await?)
    } else {
        None
    };
    let prompt = build_final_review_synthesis_prompt(paths, manifest, shard_root.as_deref());
    run_codex_phase(paths, "final-review", &prompt, &config).await?;
    let review_path = final_review_markdown_path(paths);
    let book_index_path = codebase_book_index_path(paths);
    require_nonempty_file(&review_path)?;
    require_nonempty_file(&book_index_path)?;
    let review = fs::read_to_string(&review_path)
        .with_context(|| format!("failed to read {}", review_path.display()))?;
    manifest.final_review.artifact = Some(path_display(&review_path));
    manifest.final_review.note = first_verdict_line(&review);
    manifest.final_review.status = StageStatus::Complete;
    write_manifest(paths, manifest)?;
    Ok(())
}

async fn run_final_review_shards(
    _args: &AuditArgs,
    paths: &RunPaths,
    manifest: &EverythingManifest,
    config: &PhaseConfig,
    workers: usize,
) -> Result<PathBuf> {
    let shard_root = final_review_shards_path(paths);
    if shard_root.exists() {
        fs::remove_dir_all(&shard_root)
            .with_context(|| format!("failed to clear {}", shard_root.display()))?;
    }
    fs::create_dir_all(&shard_root)
        .with_context(|| format!("failed to create {}", shard_root.display()))?;
    let shard_count = workers.min(manifest.groups.len()).max(1);
    println!("final review: {} group-shard reviewer(s)", shard_count);
    let mut groups = manifest.groups.clone();
    groups.sort_by(|left, right| left.slug.cmp(&right.slug));
    let mut buckets = vec![Vec::<GroupState>::new(); shard_count];
    for (idx, group) in groups.into_iter().enumerate() {
        buckets[idx % shard_count].push(group);
    }
    let mut join_set = JoinSet::new();
    for (idx, bucket) in buckets.into_iter().enumerate() {
        if bucket.is_empty() {
            continue;
        }
        let paths = paths.clone();
        let config = config.clone();
        let artifact_dir = shard_root.join(format!("shard-{idx:02}"));
        let prompt = build_final_review_shard_prompt(&paths, manifest, idx, &bucket, &artifact_dir);
        join_set.spawn(async move {
            let phase_slug = format!("final-review-shard-{idx:02}");
            run_codex_phase_for_artifact(&paths, &artifact_dir, &phase_slug, &prompt, &config)
                .await?;
            let shard_path = artifact_dir.join("shard.md");
            require_nonempty_file(&shard_path)?;
            Ok::<_, anyhow::Error>(shard_path)
        });
    }
    let mut failures = Vec::new();
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(path)) => println!("final review shard: {}", path.display()),
            Ok(Err(err)) => failures.push(format!("{err:#}")),
            Err(err) => failures.push(format!("final-review shard task panicked: {err}")),
        }
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("final review shard failure: {failure}");
        }
        bail!(
            "final review shard phase failed for {} shard(s)",
            failures.len()
        );
    }
    Ok(shard_root)
}

async fn run_file_quality_gate_phase(
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
    manifest.file_quality.artifact = Some(path_display(&file_quality_root_path(paths)));
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
            artifact_dir: path_display(&artifact_dir),
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
        artifact_dir: path_display(&pass_dir),
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

async fn run_final_review_repair_phase(
    args: &AuditArgs,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
    attempt: usize,
) -> Result<()> {
    let review_path = final_review_markdown_path(paths);
    require_nonempty_file(&review_path)?;
    let archived_path = paths
        .report_root
        .join(format!("FINAL-REVIEW.no-go-attempt-{attempt}.md"));
    fs::copy(&review_path, &archived_path).with_context(|| {
        format!(
            "failed to archive {} to {}",
            review_path.display(),
            archived_path.display()
        )
    })?;
    let prompt = build_final_review_repair_prompt(paths, manifest, attempt, &archived_path);
    let config = PhaseConfig {
        model: args.remediation_model.clone(),
        effort: args.remediation_effort.clone(),
        codex_bin: args.codex_bin.clone(),
    };
    let phase_slug = format!("final-review-repair-{attempt}");
    run_codex_phase(paths, &phase_slug, &prompt, &config).await?;
    let repair_path = paths
        .report_root
        .join(format!("FINAL-REVIEW-REPAIR-{attempt}.md"));
    require_nonempty_file(&repair_path)?;
    manifest.final_review_repairs.push(StageState {
        status: StageStatus::Complete,
        artifact: Some(path_display(&repair_path)),
        note: Some(format!("repaired NO-GO final review attempt {attempt}")),
    });
    write_manifest(paths, manifest)?;
    commit_worktree_changes(paths, manifest)?;
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
    require_file_quality_acceptance(manifest)?;
    if manifest.in_place {
        return complete_in_place_run(paths, manifest);
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

fn complete_in_place_run(paths: &RunPaths, manifest: &mut EverythingManifest) -> Result<()> {
    if matches!(manifest.merge.status, StageStatus::Complete) {
        println!("merge:       complete (in-place resume)");
        return Ok(());
    }
    if !final_review_is_go(paths) {
        manifest.merge.status = StageStatus::Skipped;
        manifest.merge.note =
            Some("in-place run did not reach `Verdict: GO`; changes left for review".to_string());
        write_manifest(paths, manifest)?;
        bail!("final review is not GO; in-place run left changes for review");
    }
    require_file_quality_acceptance(manifest)?;
    commit_worktree_changes(paths, manifest)?;
    manifest.merge.status = StageStatus::Complete;
    manifest.merge.note =
        Some("in-place run: audit changes are committed on the primary checkout".to_string());
    write_manifest(paths, manifest)?;
    Ok(())
}

fn refresh_final_status_after_merge(
    repo_root: &Path,
    paths: &RunPaths,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if !matches!(manifest.merge.status, StageStatus::Complete) {
        return Ok(());
    }
    manifest.final_status.status = StageStatus::Complete;
    manifest.final_status.artifact = Some(path_display(&run_status_markdown_path(paths)));
    manifest.final_status.note = Some(
        "RUN-STATUS refreshed after merge completion; committed status is generated immediately before the final status commit, so use `git rev-parse` for exact post-commit heads"
            .to_string(),
    );
    write_manifest(paths, manifest)?;
    commit_worktree_changes(paths, manifest)?;

    if manifest.in_place {
        return Ok(());
    }

    let current_branch = git_stdout(repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string();
    if current_branch != manifest.branch {
        bail!(
            "final status refresh requires canonical checkout on `{}` (current: `{}`)",
            manifest.branch,
            current_branch
        );
    }
    let status = git_stdout(repo_root, ["status", "--short"])?;
    if !status.trim().is_empty() {
        bail!(
            "canonical checkout is dirty after merge; cannot land final status refresh:\n{}",
            status
        );
    }
    run_git(repo_root, ["merge", "--ff-only", &manifest.audit_branch])?;
    if remote_branch_exists(repo_root, &manifest.branch) {
        run_git(repo_root, ["push", "origin", &manifest.branch])?;
    }
    Ok(())
}

fn commit_worktree_changes(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    let generated_file_artifacts = format!("audit/everything/{}/files", manifest.run_id);
    let _ = run_git(
        &paths.worktree_root,
        [
            "rm",
            "-r",
            "--cached",
            "--ignore-unmatch",
            &generated_file_artifacts,
        ],
    );
    let status = git_stdout(&paths.worktree_root, ["status", "--short"])?;
    if status.trim().is_empty() {
        return Ok(());
    }
    run_git(
        &paths.worktree_root,
        ["add", "--", ".", ":(exclude)audit/everything/*/files/**"],
    )?;
    let _ = run_git(
        &paths.worktree_root,
        [
            "rm",
            "-r",
            "--cached",
            "--ignore-unmatch",
            &generated_file_artifacts,
        ],
    );
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

fn remediation_dependency_groups(
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

fn remediation_plan_markdown_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("REMEDIATION-PLAN.md")
}

fn remediation_plan_json_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("REMEDIATION-PLAN.json")
}

fn codebase_improvement_policy_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("CODEBASE-IMPROVEMENT-POLICY.md")
}

fn write_remediation_plan_files(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
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

fn next_remediation_scheduler_choice(
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

fn complete_remediation_task_ids(manifest: &EverythingManifest) -> BTreeSet<&str> {
    let complete = manifest
        .remediation_tasks
        .iter()
        .filter(|task| matches!(task.status, StageStatus::Complete | StageStatus::Skipped))
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();
    complete
}

fn is_schedulable_remediation_task(task: &RemediationTaskState, active: &BTreeSet<String>) -> bool {
    !active.contains(&task.id)
        && !matches!(
            task.status,
            StageStatus::Complete
                | StageStatus::Skipped
                | StageStatus::Running
                | StageStatus::Failed
        )
}

fn unmet_remediation_dependencies(
    task: &RemediationTaskState,
    complete: &BTreeSet<&str>,
) -> Vec<String> {
    task.dependencies
        .iter()
        .filter(|dependency| !complete.contains(dependency.as_str()))
        .cloned()
        .collect()
}

fn first_blocked_remediation_task(manifest: &EverythingManifest) -> Option<&RemediationTaskState> {
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
    let status = run_codex_exec_max_context(
        &lane_repo_root,
        &prompt,
        &config.model,
        &config.effort,
        &config.codex_bin,
        &stderr_path,
        Some(&stdout_path),
        &format!("auto audit remediation {}", task.id),
    )
    .await?;
    if !status.success() {
        bail!(
            "remediation lane {} failed with status {status}; see {}",
            task.id,
            stderr_path.display()
        );
    }
    Ok(())
}

fn prepare_remediation_lane_repo(paths: &RunPaths, task: &mut RemediationTaskState) -> Result<()> {
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

fn clone_audit_lane_repo(repo_root: &Path, lane_repo_root: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg("--local")
        .arg(repo_root)
        .arg(lane_repo_root)
        .output()
        .with_context(|| {
            format!(
                "failed to clone audit lane repo from {} to {}",
                repo_root.display(),
                lane_repo_root.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "git clone failed for audit lane {}: {}",
            lane_repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let remotes = git_stdout(lane_repo_root, ["remote"]).unwrap_or_default();
    if remotes.lines().any(|remote| remote.trim() == "origin") {
        run_git(lane_repo_root, ["remote", "rename", "origin", "canonical"])?;
    }
    Ok(())
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

fn land_remediation_lane(paths: &RunPaths, task: &RemediationTaskState) -> Result<Vec<String>> {
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

fn restore_dirty_generated_reports(paths: &RunPaths, changed_files: &[String]) -> Result<()> {
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

#[derive(Clone, Copy)]
enum GroupPhase {
    Synthesis,
}

async fn run_group_workers(
    paths: &RunPaths,
    pending: Vec<GroupState>,
    workers: usize,
    config: PhaseConfig,
    phase: GroupPhase,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    let mut join_set = JoinSet::new();
    let mut pending_iter = pending.into_iter();
    let mut active = 0usize;
    for _ in 0..workers {
        if let Some(group) = pending_iter.next() {
            spawn_group_worker(&mut join_set, paths, group, phase, &config);
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
            spawn_group_worker(&mut join_set, paths, group, phase, &config);
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
    phase: GroupPhase,
    config: &PhaseConfig,
) {
    let paths_clone = paths.clone();
    let config_clone = config.clone();
    join_set.spawn(
        async move { run_one_group_phase(&paths_clone, &group, phase, &config_clone).await },
    );
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

fn spawn_file_worker(
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
    phase: GroupPhase,
    config: &PhaseConfig,
) -> Result<String> {
    let report_path = PathBuf::from(&group.report_path);
    require_nonempty_file(&report_path)?;
    let slug = match phase {
        GroupPhase::Synthesis => "synthesis",
    };
    let prompt = match phase {
        GroupPhase::Synthesis => build_synthesis_prompt(paths, group),
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
    })?;
    atomic_write(
        &codebase_improvement_policy_path(paths),
        CODEBASE_IMPROVEMENT_POLICY.as_bytes(),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            codebase_improvement_policy_path(paths).display()
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
    let path = path.to_ascii_lowercase();
    path == "agents.md"
        || path == "architecture.md"
        || path == "claude.md"
        || path.starts_with("doctrine/")
        || path.starts_with("specs/")
        || path.starts_with("plans/")
        || path.contains("architecture")
}

fn is_rust_or_backend_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
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
    let path = path.to_ascii_lowercase();
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
    let path = path.to_ascii_lowercase();
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
    let path = path.to_ascii_lowercase();
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
    let path = path.to_ascii_lowercase();
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.contains("test")
        || path.contains("spec")
        || path.contains("bench")
        || path.contains("perf")
        || path.contains("playwright")
}

fn is_release_or_deploy_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
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
Codebase improvement policy: `{report_root}/CODEBASE-IMPROVEMENT-POLICY.md`

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
- Reference the codebase improvement policy as the audit's default-on debt, deletion, AI-slop, and architecture-deepening contract. Do not paste the whole policy into root docs.
- Treat gstack skills as deterministic lenses by phase. Direct tool-like invocation is reserved for remediation/final validation when the selected surface calls for browser, QA, benchmark, deploy, or documentation checks.
- Add architecture context that helps later workers find domain boundaries, deprecated surfaces, orphan-risk areas, and evidence sources for safe deletion.
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
- If the target file content below says it is omitted because the file is large, you must read the entire target file from its path in ordered chunks before writing artifacts. Do not sample. Do not rely on metadata only. If you cannot inspect every line, fail this file instead of writing artifacts.

Injected context:
{context}

Selected gstack lenses:
{skill_policy}

Default-on codebase improvement policy:
- Look for orphaned, deprecated, duplicated, transitional, overabstracted, or agent-generated filler in this file.
- Apply the deletion test even in first pass: what references, exports, config, docs, generated bindings, tests, or runtime entrypoints would prove this file or part of it is still live?
- Prefer architectural depth over micro-edits: identify whether this file owns a real invariant, belongs in this module, leaks responsibilities, or should consolidate with another owner.
- Do not recommend deletion unless you can name the proof still needed or already visible from this file plus injected context.

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
- Orphan/deprecation signals, AI-slop signals, and simplification/deletion candidates with the evidence needed before removal.
- Architecture/debt assessment: ownership, module depth, domain vocabulary, dependency direction, duplicated responsibilities, and whether this file appears to carry real substance.
- A coverage note stating whether the full file content was provided inline or reviewed from disk in chunks.
- If not 10/10, list expansions, deletions, revisions, clarifications, tests, code refactors, documentation moves, or retirement steps that would make it an idiomatic 10/10 work product.
- Cross-file questions or likely relationships surfaced by this file, without resolving them from other source files in this pass.

`analysis.json` must be valid JSON with:
`path`, `group`, `score_out_of_10`, `summary`, `best_version_assessment`, `orphaned_or_deprecated_signals`, `ai_slop_signals`, `deletion_candidates`, `architecture_smells`, `behavior_preservation_needs`, `recommended_actions`, `cross_file_questions`, `coverage`, `confidence`.

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

The authoritative input set is the report plus the exact first-pass artifact paths referenced inside it. Do not glob or enumerate `{report_root}/files`; unreferenced artifact directories may be stale leftovers from interrupted or upgraded runs.

Selected gstack lenses for this group:
{skill_policy}

Default-on codebase improvement policy:
- Build or update a debt register for this group. Use the classes `safe_delete`, `deprecated_remove`, `consolidate`, `simplify`, `deepen_module`, and `leave_with_reason`.
- Treat orphaned/deprecated code and AI-slop as first-class audit findings, not optional polish.
- Prefer cross-file architecture fixes over isolated micro-edits when duplicated responsibility, shallow modules, or weak domain boundaries are the real problem.
- Require proof before deletion: references/imports/exports, entrypoints, config, docs, generated bindings, tests, runtime paths, and behavior characterization where needed.
- If proof is missing, record exactly what evidence would be needed instead of guessing.

Revise `{report}` in place. Keep every file represented. Tighten or correct the first-pass assessments based on relationships surfaced between files:
- duplicated responsibilities
- unclear ownership or misplaced modules
- missing invariants
- dead code or files that should retire
- deprecated paths, transitional scaffolding, and orphaned exports
- AI-slop: generic wrappers, hollow abstractions, vague comments, repeated boilerplate, or docs that add words without operational value
- test gaps
- docs that should move into `AGENTS.md`, `ARCHITECTURE.md`, doctrine, or inline comments
- cross-crate/API seams

`{report}` must include a `## Debt Register` section. For each candidate, include path(s), class, recommended action, deletion/refactor proof found, proof still missing, behavior-preservation needs, and risk.

Use the selected lenses as a compact prompt injection, not as permission to bulk-load unrelated skill files. Keep the output grounded in repository evidence.

Do not edit source code in this phase. Only edit `{report}` and optional notes next to it.
"#,
        repo = paths.worktree_root.display(),
        group = group.name,
        report = group.report_path,
        report_root = paths.report_root.display(),
        skill_policy = skill_policy,
    )
}

#[allow(dead_code)]
fn build_final_review_prompt(paths: &RunPaths, manifest: &EverythingManifest) -> String {
    build_final_review_synthesis_prompt(paths, manifest, None)
}

fn build_final_review_synthesis_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    shard_root: Option<&Path>,
) -> String {
    let skill_policy = selected_skill_policy_for_final_review();
    let shard_instruction = shard_root
        .map(|path| {
            format!(
                "Parallel reviewer shard reports are available under `{}`. Read every `shard.md` there and synthesize them with your own full-diff review. Treat shards as advisory evidence, not as a substitute for final judgment.",
                path.display()
            )
        })
        .unwrap_or_else(|| {
            "No parallel reviewer shard reports were generated for this run.".to_string()
        });
    format!(
        r#"You are the final professional audit reviewer.

Repository root: `{repo}`
Report root: `{report_root}`
Base commit: `{base}`
Audit branch: `{branch}`
Reviewer shards: {shard_instruction}

Review all group reports under the report root and the full git diff from `{base}` to HEAD.

Selected gstack lenses for final review:
{skill_policy}

Use `gpt-5.5 xhigh` judgment standards:
- Verify changes correspond to report findings.
- Reject speculative rewrites not grounded in file reports.
- Check for broken architecture docs, stale AGENTS instructions, overbroad edits, missing tests, and merge-risk.
- Verify debt-removal and architecture-deepening work: deletion candidates are evidence-backed, deprecated paths are intentionally retired, AI-slop was removed where safe, and refactors preserve product substance.
- Run or inspect the narrowest feasible validation for the changed surfaces.

Write `{report_root}/FINAL-REVIEW.md` with:
- `# FINAL REVIEW`
- A line exactly `Verdict: GO` or `Verdict: NO-GO`
- Diff summary
- Report consistency assessment
- Validation run and result
- Evidence class checklist
- Deletion and refactor proof checklist
- Required blockers before merge
- Optional follow-ups

The evidence class checklist must classify each evidence class as `pass`, `not run`, `blocked`, or `not applicable`; cite the exact artifact, command, or report path for each non-`not applicable` row; and state what claims the evidence does and does not support. Include at least these classes:
- local static/build/unit validation
- generated contract/binding validation
- browser QA or visual/product workflow validation
- deployment/canary/health validation
- live production or mainnet/on-chain validation
- external-owner or cross-repo validation
- documentation/status artifact integrity

The deletion and refactor proof checklist must classify each debt-removal claim as `pass`, `blocked`, or `not applicable`; cite the group report, debt-register item, diff path, and validation/characterization proof. Reject the audit with `Verdict: NO-GO` if code was deleted or behavior was refactored without enough evidence to show no substance was lost.

Do not count local, fixture, regtest, or synthetic proof as live production proof. Do not merge bulky first-pass mirrors such as `audit/everything/<run-id>/files/**` unless the host explicitly requested them; they should remain transient evidence caches by default.

Also write a chaptered codebase book under `{report_root}/CODEBASE-BOOK/`. This is the final explanatory artifact for a human who wants to understand the audited codebase without rereading every source file. It must not be a single giant markdown file. Write it in a Feynman-style teaching voice: clear first principles, concrete examples, patient logical order, and plain-spoken explanations. Avoid hype and vague praise.

The book standard is intentionally higher than an executive audit summary. A smart junior developer who is otherwise unfamiliar with this repository should be able to read the book and gain a deep technical understanding of the important crates/files, runtime flows, state boundaries, validation posture, and production risks before opening the source code.

`CODEBASE-BOOK/` must include:
- `README.md` with `# CODEBASE BOOK`, the table of contents, the recommended reading path, and links to all chapter files.
- Numbered chapter files organized by the repository's logical architecture and conceptual flow, not by incidental filesystem order. Example shape: `01-problem-and-mental-model.md`, `02-runtime-or-control-flow.md`, `03-data-model-and-storage.md`, followed by subsystem chapters that match this repo.
- File-catalog chapters split by subsystem/group so the catalog is readable as a book appendix, not as one enormous list.
- A documentation and architecture chapter that says what changed in `ARCHITECTURE.md`, `AGENTS.md`, and focused docs, or explicitly says no changes were made.
- A validation and residual-risk chapter.
- Pointers back to the group reports and first-pass artifacts for readers who want evidence.

The book must cover every tracked file included in this audit. Every file needs a reasonably detailed explanation of what it does, why it exists, what owns it or calls it, and how it fits into the surrounding subsystem. Do not use empty boilerplate like "utility file" without explanation. For files changed by this audit, include `changed:` in that file's entry and summarize the substance of the change.

For key files and key sections, include narrative code walkthroughs: name the important modules, functions, types, tests, configuration, and command paths; explain why each matters and how control or data moves through it. If the first draft becomes too high-level, prefer fewer but deeper narrative chapters plus appendix links over shallow coverage everywhere. The standalone `auto book` command can later rewrite these narrative chapters from the completed audit corpus using Codex's maximum context window while preserving appendix/catalog files.

Do not merge. The host runner handles merge only after this file says `Verdict: GO`.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        base = manifest.base_commit,
        branch = manifest.audit_branch,
        shard_instruction = shard_instruction,
        skill_policy = skill_policy,
    )
}

fn build_final_review_shard_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    shard_index: usize,
    groups: &[GroupState],
    artifact_dir: &Path,
) -> String {
    let skill_policy = selected_skill_policy_for_final_review();
    let group_list = groups
        .iter()
        .map(|group| {
            format!(
                "- `{}` report `{}` ({} file(s))",
                group.name,
                group.report_path,
                group.files.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"You are a parallel final-review shard for `auto audit --everything`.

Repository root: `{repo}`
Report root: `{report_root}`
Base commit: `{base}`
Audit branch: `{branch}`
Shard index: {shard_index}
Output file: `{artifact_dir}/shard.md`

Assigned group reports:
{group_list}

Selected gstack lenses for final review:
{skill_policy}

Review the assigned group reports, their debt registers, first-pass evidence when needed, and the git diff from `{base}` to HEAD for paths owned by these groups. You are not the final synthesizer; do not write `FINAL-REVIEW.md` and do not write `CODEBASE-BOOK/`.

Write `{artifact_dir}/shard.md` with:
- `# FINAL REVIEW SHARD {shard_index}`
- Assigned groups covered.
- GO/NO-GO recommendation for this shard only.
- Highest-risk blockers with exact report paths and diff paths.
- Evidence checklist gaps relevant to these groups.
- Deletion/refactor proof gaps relevant to these groups.
- Validation you inspected or ran, if any.
- Optional follow-ups that should not block.

Be concise but evidence-backed. Do not merge. Do not edit source code.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        base = manifest.base_commit,
        branch = manifest.audit_branch,
        shard_index = shard_index,
        artifact_dir = artifact_dir.display(),
        group_list = group_list,
        skill_policy = skill_policy,
    )
}

fn build_final_review_repair_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    attempt: usize,
    archived_review_path: &Path,
) -> String {
    format!(
        r#"You are repairing actionable blockers from a professional `auto audit --everything` final review.

Repository root: `{repo}`
Report root: `{report_root}`
Base commit: `{base}`
Audit branch: `{branch}`
NO-GO review to repair: `{review}`
Repair attempt: {attempt}

Read the archived NO-GO review and identify only concrete, actionable blockers under `Required blockers before merge`. Apply the smallest grounded repair that clears those blockers.

Rules:
- Do not broaden the audit or invent new remediation scope.
- Do not hide or delete evidence. If a blocker is invalid, document why in the active group report or `FINAL-REVIEW-REPAIR-{attempt}.md`.
- Update source, tests, docs, group reports, `REMEDIATION-PLAN.md`, and `CODEBASE-BOOK/` only when the blocker requires it.
- Run or inspect the narrowest meaningful verification for the repaired surface.
- Write `{report_root}/FINAL-REVIEW-REPAIR-{attempt}.md` with blockers addressed, changed files, validation, and any blockers that remain.
- Do not write `Verdict: GO`; the host will rerun final review after this repair pass.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        base = manifest.base_commit,
        branch = manifest.audit_branch,
        review = archived_review_path.display(),
        attempt = attempt,
    )
}

fn build_file_quality_rerate_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    file: &FileState,
    pass_index: usize,
) -> String {
    let first_pass = Path::new(&file.artifact_dir).join("analysis.json");
    let first_pass_markdown = Path::new(&file.artifact_dir).join("analysis.md");
    let group_report = manifest
        .groups
        .iter()
        .find(|group| group.name == file.group)
        .map(|group| group.report_path.as_str())
        .unwrap_or("");
    let artifact_dir = file_quality_file_path(paths, pass_index, file);
    format!(
        r#"You are the file-quality rerating reviewer for `auto audit --everything`.

Repository root: `{repo}`
Report root: `{report_root}`
Pass: {pass_index}
File under rerating: `{path}`
First-pass JSON: `{first_pass}`
First-pass markdown: `{first_pass_markdown}`
Group report: `{group_report}`
Final review: `{final_review}`
Output directory: `{artifact_dir}`

Regrade the first-pass rating for exactly this file against the current repository state. Read the file itself, the first-pass artifacts, the group report, and the final review. Do not edit source code in this rerating step.

Apply strict professional standards. The target is {target:.0}/10. A score below {accept:.0}/10 means this file still needs another deliverable pass before merge. Regrade independently; do not rubber-stamp the original first-pass score.

Penalize the file if it still contains unnecessary code, orphaned/deprecated surfaces, duplicated responsibility, AI-slop, shallow ownership, vague comments, fake extensibility, or missed consolidation opportunities. A file should not reach {accept:.0}/10 if it obviously needs deletion, retirement, or architectural relocation and the audit failed to handle or document that.

Write:
1. `{artifact_dir}/rating.md`
2. `{artifact_dir}/rating.json`

`rating.md` must include:
- Current score out of 10.
- Whether the first-pass score was too high, too low, or accurate.
- Concrete deliverables needed to make the file a {target:.0}/10 work product.
- Any remaining deletion, consolidation, simplification, AI-slop, or architecture-deepening deliverables.
- What would be acceptable evidence that the file is at least {accept:.0}/10.

`rating.json` must be valid JSON with:
`path`, `pass_index`, `score_out_of_10`, `previous_score_out_of_10`, `first_pass_grade_was`, `debt_or_architecture_findings`, `deliverables_to_reach_10`, `minimum_evidence_for_9`, `confidence`.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        pass_index = pass_index,
        path = file.path,
        first_pass = first_pass.display(),
        first_pass_markdown = first_pass_markdown.display(),
        group_report = group_report,
        final_review = final_review_markdown_path(paths).display(),
        artifact_dir = artifact_dir.display(),
        target = FILE_QUALITY_TARGET_SCORE,
        accept = FILE_QUALITY_ACCEPT_SCORE,
    )
}

fn build_file_quality_deliverables_prompt(
    paths: &RunPaths,
    manifest: &EverythingManifest,
    file: &FileState,
    rating: &FileQualityRatingState,
    pass_index: usize,
) -> String {
    let group_report = manifest
        .groups
        .iter()
        .find(|group| group.name == file.group)
        .map(|group| group.report_path.as_str())
        .unwrap_or("");
    let artifact_dir = PathBuf::from(&rating.artifact_dir);
    format!(
        r#"You are running a per-file quality deliverable pass for `auto audit --everything`.

Repository root: `{repo}`
Report root: `{report_root}`
Pass: {pass_index}
Owned file: `{path}`
Current rerating score: {score}
Rating artifact: `{rating_json}`
Rating notes: `{rating_md}`
Group report: `{group_report}`
Final review: `{final_review}`

Raise this file toward a {target:.0}/10 work product. The immediate acceptance floor is {accept:.0}/10 on the next rerating pass, but the deliverables should aim at {target:.0}/10.

Rules:
- Keep the primary edit scope to `{path}`.
- You may update the nearest tests or focused docs only when that is necessary to prove or explain this file's change; list those as scope exceptions in your output.
- You may delete, simplify, consolidate, or relocate code when the rating artifact and group report provide evidence that this is the right way to raise quality.
- Before deleting or retiring code, prove the removal with references/imports/exports, entrypoints, config/docs/generated bindings, and narrow validation or behavior characterization where practical.
- Do not broaden into unrelated cleanup.
- Preserve evidence. Do not delete first-pass, group, final-review, or previous file-quality artifacts.
- Run or inspect the narrowest meaningful validation for this file when practical.
- Commit nothing manually; the host runner owns commits.

Write `{artifact_dir}/deliverables.md` with:
- Deliverables applied.
- Changed files.
- Deletion/refactor proof, or `not applicable`.
- Validation command/result or the reason validation was not practical.
- Remaining work, if any, to reach {target:.0}/10.
"#,
        repo = paths.worktree_root.display(),
        report_root = paths.report_root.display(),
        pass_index = pass_index,
        path = file.path,
        score = rating
            .score_out_of_10
            .map(|score| format!("{score:.1}/10"))
            .unwrap_or_else(|| "unknown".to_string()),
        rating_json = artifact_dir.join("rating.json").display(),
        rating_md = artifact_dir.join("rating.md").display(),
        group_report = group_report,
        final_review = final_review_markdown_path(paths).display(),
        target = FILE_QUALITY_TARGET_SCORE,
        accept = FILE_QUALITY_ACCEPT_SCORE,
        artifact_dir = artifact_dir.display(),
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
    append_named_file(
        &mut body,
        "CODEBASE-IMPROVEMENT-POLICY.md",
        &codebase_improvement_policy_path(paths),
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
        let absolute_path = worktree_root.join(&path);
        if !absolute_path.is_file() {
            continue;
        }
        let content = fs::read(&absolute_path)
            .with_context(|| format!("failed to read {}", absolute_path.display()))?;
        let hash = sha256_hex(&content);
        let artifact_path = file_artifact_dir(report_root, &path, &hash);
        let legacy_artifact_path = legacy_file_artifact_dir(report_root, &hash);
        migrate_legacy_file_artifact_if_matching(&legacy_artifact_path, &artifact_path, &path)?;
        let artifact_dir = artifact_path.display().to_string();
        let status = if artifact_complete(&artifact_path) {
            StageStatus::Complete
        } else {
            existing_status
                .get(&path)
                .copied()
                .filter(|status| !matches!(status, StageStatus::Complete))
                .unwrap_or(StageStatus::Pending)
        };
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
    let path = path.trim_start_matches("./");
    if DEFAULT_EXCLUDE_FILENAMES
        .iter()
        .any(|filename| path == *filename || path.ends_with(&format!("/{filename}")))
    {
        return true;
    }
    if DEFAULT_EXCLUDE_SUFFIXES
        .iter()
        .any(|suffix| path.to_ascii_lowercase().ends_with(suffix))
    {
        return true;
    }
    if DEFAULT_EXCLUDE_PATH_SEGMENTS
        .iter()
        .any(|segment| path.contains(segment))
    {
        return true;
    }
    DEFAULT_EXCLUDE_PREFIXES.iter().any(|prefix| {
        if prefix.ends_with('/') {
            path.starts_with(prefix)
        } else {
            path == *prefix || path.starts_with(prefix)
        }
    })
}

fn write_change_summary_markdown(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    fs::create_dir_all(&paths.report_root)
        .with_context(|| format!("failed to create {}", paths.report_root.display()))?;

    let head = git_stdout(&paths.worktree_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let range = format!("{}..{head}", manifest.base_commit);
    let changed_files = git_stdout(
        &paths.worktree_root,
        ["diff", "--name-status", "--find-renames", &range],
    )?;
    let diff_stat = git_stdout(
        &paths.worktree_root,
        ["diff", "--stat", "--find-renames", &range],
    )?;
    let commit_log = git_stdout(
        &paths.worktree_root,
        [
            "log",
            "--reverse",
            "--no-merges",
            "--stat",
            "--find-renames",
            "--date=short",
            "--pretty=format:## Commit %h%n%n- Subject: %s%n- Author: %an <%ae>%n- Date: %ad%n",
            &range,
        ],
    )?;

    let mut body = String::new();
    body.push_str("# Auto Audit Change Summary\n\n");
    body.push_str("This artifact summarizes repository changes made by the `auto audit --everything` run. It is intended to let an engineer understand what changed without reconstructing the run from scattered commits, lane logs, and review notes.\n\n");
    body.push_str("## Run\n\n");
    body.push_str(&format!("- Run id: `{}`\n", manifest.run_id));
    body.push_str(&format!("- Primary branch: `{}`\n", manifest.branch));
    body.push_str(&format!("- Audit branch: `{}`\n", manifest.audit_branch));
    body.push_str(&format!("- Base commit: `{}`\n", manifest.base_commit));
    body.push_str(&format!("- Audit head: `{head}`\n"));
    body.push_str(&format!(
        "- Final review: {:?}\n",
        manifest.final_review.status
    ));
    body.push_str(&format!(
        "- File quality: {:?}\n",
        manifest.file_quality.status
    ));
    body.push_str(&format!("- Merge: {:?}\n\n", manifest.merge.status));

    body.push_str("## High-Level Diff Stat\n\n");
    if diff_stat.trim().is_empty() {
        body.push_str("No source or report changes are present on the audit branch beyond the base commit.\n\n");
    } else {
        body.push_str("```text\n");
        body.push_str(diff_stat.trim_end());
        body.push_str("\n```\n\n");
    }

    body.push_str("## Changed Files\n\n");
    if changed_files.trim().is_empty() {
        body.push_str("- none\n\n");
    } else {
        for line in changed_files.lines() {
            body.push_str(&format!("- `{}`\n", line.trim()));
        }
        body.push('\n');
    }

    body.push_str("## Remediation Task Summary\n\n");
    if manifest.remediation_tasks.is_empty() {
        body.push_str("- No remediation tasks were generated for this run.\n\n");
    } else {
        for task in &manifest.remediation_tasks {
            body.push_str(&format!(
                "- `{}` ({:?}) group `{}`: {}\n",
                task.id,
                task.status,
                task.group,
                task.note
                    .as_deref()
                    .map(one_line)
                    .unwrap_or_else(|| "no note recorded".to_string())
            ));
        }
        body.push('\n');
    }

    body.push_str("## Commit-by-Commit Detail\n\n");
    if commit_log.trim().is_empty() {
        body.push_str("No audit commits were created after the base commit.\n\n");
    } else {
        body.push_str(commit_log.trim_end());
        body.push_str("\n\n");
    }

    body.push_str("## Related Artifacts\n\n");
    body.push_str(&format!(
        "- Run status: `{}`\n",
        run_status_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Remediation plan: `{}`\n",
        remediation_plan_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Final review: `{}`\n",
        final_review_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- File quality reratings: `{}`\n",
        file_quality_root_path(paths).display()
    ));

    atomic_write(&change_summary_markdown_path(paths), body.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            change_summary_markdown_path(paths).display()
        )
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
        if report_path.exists() && matches!(group.synthesis_status, StageStatus::Complete) {
            continue;
        }
        let mut body = String::new();
        body.push_str(&format!("# Audit Report: {}\n\n", group.name));
        body.push_str("## Scope\n\n");
        body.push_str("This report is assembled from first-pass one-file analyses. The synthesis pass may revise it based on cross-file relationships.\n\n");
        body.push_str("The authoritative first-pass inputs are the artifact paths listed under each file below. Ignore unreferenced artifact directories; interrupted or upgraded runs may leave stale artifacts in `audit/everything/*/files`.\n\n");
        body.push_str("## Debt Register\n\n");
        body.push_str("Synthesis must classify debt candidates with `safe_delete`, `deprecated_remove`, `consolidate`, `simplify`, `deepen_module`, or `leave_with_reason`. Each item needs path(s), action, proof found, proof still missing, behavior-preservation needs, and risk. If no candidates exist, write `No actionable debt candidates found.`\n\n");
        for file_path in &group.files {
            if let Some(file) = manifest.files.iter().find(|file| &file.path == file_path) {
                body.push_str(&format!("## `{}`\n\n", file.path));
                let analysis = Path::new(&file.artifact_dir).join("analysis.md");
                body.push_str(&format!("First-pass artifact: `{}`\n\n", file.artifact_dir));
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
    let byte_len = bytes.len();
    match String::from_utf8(bytes) {
        Ok(text) if byte_len > MAX_FILE_PROMPT_BYTES => {
            let line_count = text.lines().count();
            Ok(format!(
                "[large UTF-8 file omitted from inline prompt because it is {byte_len} bytes and {line_count} lines. Mandatory full-file review: inspect `{}` directly inside the worktree before writing artifacts. Read the entire file in ordered chunks no larger than 250 lines, from line 1 through line {line_count}, using `sed -n '<start>,<end>p'`, `nl -ba`, or an equivalent command. Do not sample. Do not rely on metadata only. In `analysis.md`, include a Coverage note that states this was a large-file chunked review and names the line count. In `analysis.json`, set `coverage` to a concise statement confirming full-file chunked inspection. If you cannot inspect every line, fail instead of writing artifacts.]",
                path.display()
            ))
        }
        Ok(text) => Ok(text),
        Err(err) => Ok(format!(
            "[binary or non-UTF8 file omitted from prompt: {} valid bytes before error]",
            err.utf8_error().valid_up_to()
        )),
    }
}

fn artifact_complete(artifact_dir: &Path) -> bool {
    artifact_dir.join("analysis.md").exists()
        && artifact_dir.join("analysis.json").exists()
        && !artifact_has_legacy_large_file_prompt(artifact_dir)
}

fn artifact_has_legacy_large_file_prompt(artifact_dir: &Path) -> bool {
    fs::read_to_string(artifact_dir.join("first-pass-prompt.md"))
        .is_ok_and(|prompt| prompt.contains(LEGACY_LARGE_FILE_OMISSION_MARKER))
}

fn file_artifact_dir(report_root: &Path, path: &str, content_hash: &str) -> PathBuf {
    report_root
        .join("files")
        .join(file_artifact_slug(path, content_hash))
}

fn file_artifact_slug(path: &str, content_hash: &str) -> String {
    let path_hash = sha256_hex(path.as_bytes());
    format!("{}-{}", short_hash(&path_hash), short_hash(content_hash))
}

fn legacy_file_artifact_dir(report_root: &Path, content_hash: &str) -> PathBuf {
    report_root.join("files").join(short_hash(content_hash))
}

fn migrate_legacy_file_artifact_if_matching(
    legacy_artifact_dir: &Path,
    artifact_dir: &Path,
    path: &str,
) -> Result<()> {
    if artifact_complete(artifact_dir)
        || !artifact_complete(legacy_artifact_dir)
        || !artifact_matches_path(legacy_artifact_dir, path)
    {
        return Ok(());
    }
    fs::create_dir_all(artifact_dir)
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;
    for file_name in ["analysis.md", "analysis.json"] {
        fs::copy(
            legacy_artifact_dir.join(file_name),
            artifact_dir.join(file_name),
        )
        .with_context(|| {
            format!(
                "failed to migrate {} from {} to {}",
                file_name,
                legacy_artifact_dir.display(),
                artifact_dir.display()
            )
        })?;
    }
    Ok(())
}

fn artifact_matches_path(artifact_dir: &Path, path: &str) -> bool {
    let json = fs::read_to_string(artifact_dir.join("analysis.json")).unwrap_or_default();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) {
        if ["path", "file"]
            .iter()
            .filter_map(|key| value.get(*key).and_then(|value| value.as_str()))
            .any(|value| value == path)
        {
            return true;
        }
    }
    fs::read_to_string(artifact_dir.join("analysis.md"))
        .map(|markdown| {
            markdown
                .lines()
                .next()
                .is_some_and(|line| line.trim() == format!("# {path}"))
        })
        .unwrap_or(false)
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
    manifest.remediation_plan.status = StageStatus::Skipped;
    manifest.remediation_plan.note = Some(reason.to_string());
    for task in &mut manifest.remediation_tasks {
        if !matches!(task.status, StageStatus::Complete) {
            task.status = StageStatus::Skipped;
            task.note = Some(reason.to_string());
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
    let path = final_review_markdown_path(paths);
    fs::read_to_string(path)
        .map(|text| text.lines().any(|line| line.trim() == "Verdict: GO"))
        .unwrap_or(false)
}

fn final_review_has_actionable_blockers(paths: &RunPaths) -> bool {
    let path = final_review_markdown_path(paths);
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    if !text.lines().any(|line| line.trim() == "Verdict: NO-GO") {
        return false;
    }
    let Some(section) = markdown_section(&text, "Required blockers before merge") else {
        return false;
    };
    section.lines().any(actionable_blocker_line)
}

fn markdown_section<'a>(text: &'a str, heading: &str) -> Option<&'a str> {
    let mut start = None;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let line_start = offset + line.len();
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let normalized = trimmed.trim_start_matches('#').trim();
            if let Some(start) = start {
                return Some(&text[start..offset]);
            }
            if normalized.eq_ignore_ascii_case(heading) {
                start = Some(line_start);
            }
        }
        offset += line.len();
    }
    start.map(|start| &text[start..])
}

fn actionable_blocker_line(line: &str) -> bool {
    let trimmed = line.trim();
    let Some(item) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .map(str::trim)
    else {
        return false;
    };
    if item.is_empty() {
        return false;
    }
    let lowered = item.to_ascii_lowercase();
    !matches!(
        lowered.as_str(),
        "none" | "n/a" | "na" | "no blockers" | "no required blockers"
    )
}

fn first_verdict_line(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.trim().starts_with("Verdict:"))
        .map(|line| line.trim().to_string())
}

fn print_status(paths: &RunPaths, manifest: &EverythingManifest) {
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
    let remediation_tasks_done = manifest
        .remediation_tasks
        .iter()
        .filter(|task| matches!(task.status, StageStatus::Complete))
        .count();
    println!("status");
    println!("context:     {:?}", manifest.context.status);
    println!("files:       {files_done}/{}", manifest.files.len());
    println!("synthesis:   {synthesis_done}/{}", manifest.groups.len());
    println!("remediation: {remediation_done}/{}", manifest.groups.len());
    println!("remed plan:  {:?}", manifest.remediation_plan.status);
    println!(
        "remed tasks: {remediation_tasks_done}/{}",
        manifest.remediation_tasks.len()
    );
    let branch_summary = audit_branch_summary(manifest);
    println!(
        "primary:     {} {}",
        manifest.branch,
        branch_summary
            .primary_head
            .as_deref()
            .unwrap_or("(unknown)")
    );
    println!(
        "audit branch: {} {}",
        manifest.audit_branch,
        branch_summary.audit_head.as_deref().unwrap_or("(unknown)")
    );
    println!("branch state: {}", branch_summary.state);
    println!(
        "running remed: {}",
        format_task_ids_by_status(manifest, StageStatus::Running, 10)
    );
    println!(
        "failed remed: {}",
        format_task_ids_by_status(manifest, StageStatus::Failed, 10)
    );
    if let Some((task, unmet)) = first_dependency_blocked_remediation_task(manifest) {
        println!("blocked next: {} waiting on {}", task.id, unmet.join(", "));
    }
    println!(
        "paused:      {}",
        if pause_requested(paths) { "yes" } else { "no" }
    );
    println!("pause file:  {}", paths.pause_path.display());
    println!("status doc:  {}", run_status_markdown_path(paths).display());
    println!(
        "remed plan:  {}",
        remediation_plan_markdown_path(paths).display()
    );
    println!(
        "codebase book: {}",
        codebase_book_root_path(paths).display()
    );
    println!("final review:{:?}", manifest.final_review.status);
    println!("file quality:{:?}", manifest.file_quality.status);
    println!("change summary:{:?}", manifest.change_summary.status);
    println!("merge:       {:?}", manifest.merge.status);
}

fn write_manifest(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    atomic_write(&paths.manifest_path, &serde_json::to_vec_pretty(manifest)?)
        .with_context(|| format!("failed to write {}", paths.manifest_path.display()))?;
    write_run_status_if_possible(paths, manifest)
}

fn write_run_status_if_possible(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
    if !paths.worktree_root.join(".git").exists() && !paths.report_root.exists() {
        return Ok(());
    }
    write_run_status_markdown(paths, manifest)
}

fn run_status_markdown_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("RUN-STATUS.md")
}

fn final_review_markdown_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("FINAL-REVIEW.md")
}

fn final_review_shards_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("FINAL-REVIEW-SHARDS")
}

fn change_summary_markdown_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("CHANGE-SUMMARY.md")
}

fn codebase_book_root_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("CODEBASE-BOOK")
}

fn codebase_book_index_path(paths: &RunPaths) -> PathBuf {
    codebase_book_root_path(paths).join("README.md")
}

fn file_quality_root_path(paths: &RunPaths) -> PathBuf {
    paths.report_root.join("FILE-QUALITY")
}

fn file_quality_pass_path(paths: &RunPaths, pass_index: usize) -> PathBuf {
    file_quality_root_path(paths).join(format!("pass-{pass_index:02}"))
}

fn file_quality_file_path(paths: &RunPaths, pass_index: usize, file: &FileState) -> PathBuf {
    file_quality_pass_path(paths, pass_index)
        .join(file_artifact_slug(&file.path, &file.content_hash))
}

fn next_file_quality_pass_index(manifest: &EverythingManifest) -> usize {
    manifest
        .file_quality_passes
        .iter()
        .map(|pass| pass.pass_index)
        .max()
        .unwrap_or(0)
        + 1
}

fn require_file_quality_acceptance(manifest: &EverythingManifest) -> Result<()> {
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
        .collect::<BTreeMap<_, _>>();
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

fn write_run_status_markdown(paths: &RunPaths, manifest: &EverythingManifest) -> Result<()> {
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
    let task_count = |status| {
        manifest
            .remediation_tasks
            .iter()
            .filter(|task| task.status == status)
            .count()
    };
    let mut body = String::new();
    body.push_str("# Auto Audit Run Status\n\n");
    body.push_str(&format!("Run: `{}`  \n", manifest.run_id));
    body.push_str(&format!("Repository: `{}`  \n", manifest.repo_root));
    body.push_str(&format!("Audit branch: `{}`  \n", manifest.audit_branch));
    body.push_str(&format!("Primary branch: `{}`  \n", manifest.branch));
    body.push_str(&format!("Status captured: `{}`\n\n", timestamp_slug()));
    let branch_summary = audit_branch_summary(manifest);
    body.push_str("## Current State\n\n");
    body.push_str(&format!(
        "- Context engineering: {:?}\n",
        manifest.context.status
    ));
    body.push_str(&format!(
        "- File pass: {files_done} / {} complete\n",
        manifest.files.len()
    ));
    body.push_str(&format!(
        "- Synthesis: {synthesis_done} / {} complete\n",
        manifest.groups.len()
    ));
    body.push_str(&format!(
        "- Remediation groups: {remediation_done} / {} complete\n",
        manifest.groups.len()
    ));
    body.push_str(&format!(
        "- Remediation plan: {:?}\n",
        manifest.remediation_plan.status
    ));
    if let Some(note) = manifest.remediation_plan.note.as_deref() {
        body.push_str(&format!("- Remediation plan note: {}\n", one_line(note)));
    }
    body.push_str(&format!(
        "- Remediation tasks: {} complete, {} running, {} pending, {} failed, {} skipped\n",
        task_count(StageStatus::Complete),
        task_count(StageStatus::Running),
        task_count(StageStatus::Pending),
        task_count(StageStatus::Failed),
        task_count(StageStatus::Skipped)
    ));
    body.push_str(&format!("- Branch position: {}\n", branch_summary.state));
    body.push_str(&format!(
        "- Running remediation tasks: {}\n",
        format_task_ids_by_status(manifest, StageStatus::Running, 10)
    ));
    body.push_str(&format!(
        "- Failed remediation tasks: {}\n",
        format_task_ids_by_status(manifest, StageStatus::Failed, 10)
    ));
    if let Some((task, unmet)) = first_dependency_blocked_remediation_task(manifest) {
        body.push_str(&format!(
            "- First dependency-blocked remediation task: `{}` waiting on {}\n",
            task.id,
            unmet
                .iter()
                .map(|dependency| format!("`{dependency}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    body.push_str(&format!(
        "- Pause requested: {}\n",
        if pause_requested(paths) { "yes" } else { "no" }
    ));
    body.push_str(&format!(
        "- Final review: {:?}\n",
        manifest.final_review.status
    ));
    body.push_str(&format!(
        "- File quality: {:?}\n",
        manifest.file_quality.status
    ));
    if let Some(note) = manifest.file_quality.note.as_deref() {
        body.push_str(&format!("- File quality note: {}\n", one_line(note)));
    }
    body.push_str(&format!(
        "- Change summary: {:?}\n",
        manifest.change_summary.status
    ));
    if let Some(note) = manifest.change_summary.note.as_deref() {
        body.push_str(&format!("- Change summary note: {}\n", one_line(note)));
    }
    body.push_str(&format!("- Merge: {:?}\n", manifest.merge.status));
    body.push_str(&format!(
        "- Final status refresh: {:?}\n",
        manifest.final_status.status
    ));
    if let Some(note) = manifest.final_status.note.as_deref() {
        body.push_str(&format!("- Final status note: {}\n", one_line(note)));
    }
    body.push('\n');
    body.push_str("## Branch State\n\n");
    body.push_str(&format!("- Primary branch: `{}`", manifest.branch));
    if let Some(head) = branch_summary.primary_head.as_deref() {
        body.push_str(&format!(" at `{head}`"));
    }
    body.push('\n');
    body.push_str(&format!("- Audit branch: `{}`", manifest.audit_branch));
    if let Some(head) = branch_summary.audit_head.as_deref() {
        body.push_str(&format!(" at `{head}`"));
    }
    body.push('\n');
    if let (Some(ahead), Some(behind)) = (branch_summary.ahead, branch_summary.behind) {
        body.push_str(&format!(
            "- Audit branch delta: {ahead} ahead, {behind} behind\n"
        ));
    }
    body.push_str(&format!("- Interpretation: {}\n\n", branch_summary.state));
    body.push_str("## Evidence Class Checklist\n\n");
    body.push_str(
        "Final review must classify each evidence class as `pass`, `not run`, `blocked`, or `not applicable`, with exact artifact or command evidence. Treat local, fixture, regtest, and synthetic proof as non-production evidence unless live production/mainnet artifacts are cited.\n\n",
    );
    body.push_str("- Local static/build/unit validation\n");
    body.push_str("- Generated contract/binding validation\n");
    body.push_str("- Browser QA or visual/product workflow validation\n");
    body.push_str("- Deployment/canary/health validation\n");
    body.push_str("- Live production or mainnet/on-chain validation\n");
    body.push_str("- External-owner or cross-repo validation\n");
    body.push_str("- Documentation/status artifact integrity\n\n");
    body.push_str("## Important Paths\n\n");
    body.push_str(&format!(
        "- Manifest: `{}`\n",
        paths.manifest_path.display()
    ));
    body.push_str(&format!("- Pause file: `{}`\n", paths.pause_path.display()));
    body.push_str(&format!(
        "- Audit worktree: `{}`\n",
        paths.worktree_root.display()
    ));
    body.push_str(&format!(
        "- Audit reports: `{}`\n",
        paths.report_root.display()
    ));
    body.push_str(&format!(
        "- Remediation plan: `{}`\n",
        remediation_plan_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Remediation plan JSON: `{}`\n",
        remediation_plan_json_path(paths).display()
    ));
    body.push_str(&format!(
        "- Final review: `{}`\n",
        final_review_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Change summary: `{}`\n",
        change_summary_markdown_path(paths).display()
    ));
    body.push_str(&format!(
        "- Codebase book: `{}`\n\n",
        codebase_book_root_path(paths).display()
    ));
    body.push_str(&format!(
        "- File quality reratings: `{}`\n\n",
        file_quality_root_path(paths).display()
    ));
    body.push_str("## Remediation Tasks\n\n");
    append_status_tasks(&mut body, "Running", manifest, StageStatus::Running);
    append_status_tasks(&mut body, "Failed", manifest, StageStatus::Failed);
    append_status_tasks(&mut body, "Pending", manifest, StageStatus::Pending);
    append_status_tasks(&mut body, "Complete", manifest, StageStatus::Complete);
    append_status_tasks(&mut body, "Skipped", manifest, StageStatus::Skipped);

    atomic_write(&run_status_markdown_path(paths), body.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            run_status_markdown_path(paths).display()
        )
    })
}

fn append_status_tasks(
    body: &mut String,
    heading: &str,
    manifest: &EverythingManifest,
    status: StageStatus,
) {
    let tasks = manifest
        .remediation_tasks
        .iter()
        .filter(|task| task.status == status)
        .collect::<Vec<_>>();
    if tasks.is_empty() {
        return;
    }
    body.push_str(&format!("### {heading}\n\n"));
    for task in tasks {
        body.push_str(&format!("- `{}` `{}`", task.id, task.group));
        if let Some(note) = task.note.as_deref().filter(|note| !note.trim().is_empty()) {
            body.push_str(&format!(" - {}", one_line(note)));
        }
        body.push('\n');
    }
    body.push('\n');
}

#[derive(Debug)]
struct AuditBranchSummary {
    primary_head: Option<String>,
    audit_head: Option<String>,
    ahead: Option<usize>,
    behind: Option<usize>,
    state: String,
}

fn audit_branch_summary(manifest: &EverythingManifest) -> AuditBranchSummary {
    let repo_root = Path::new(&manifest.repo_root);
    let primary_head = git_rev_parse_short(repo_root, &manifest.branch);
    let audit_head = git_rev_parse_short(repo_root, &manifest.audit_branch);
    let (ahead, behind) = git_ahead_behind(repo_root, &manifest.branch, &manifest.audit_branch)
        .map(|(ahead, behind)| (Some(ahead), Some(behind)))
        .unwrap_or((None, None));
    let state = match (
        primary_head.as_deref(),
        audit_head.as_deref(),
        ahead,
        behind,
    ) {
        (None, _, _, _) => format!("unable to resolve primary branch `{}`", manifest.branch),
        (_, None, _, _) => format!("unable to resolve audit branch `{}`", manifest.audit_branch),
        (Some(primary), Some(audit), _, _) if primary == audit => {
            "audit branch matches primary branch".to_string()
        }
        (Some(_), Some(_), Some(ahead), Some(0)) if ahead > 0 => format!(
            "audit branch is {ahead} {} ahead of primary; merge back is still pending",
            commit_word(ahead)
        ),
        (Some(_), Some(_), Some(0), Some(behind)) if behind > 0 => format!(
            "audit branch is {behind} {} behind primary; refresh before continuing",
            commit_word(behind)
        ),
        (Some(_), Some(_), Some(ahead), Some(behind)) if ahead > 0 && behind > 0 => {
            format!("audit branch has diverged from primary ({ahead} ahead, {behind} behind)")
        }
        (Some(_), Some(_), _, _) => "audit branch differs from primary".to_string(),
    };

    AuditBranchSummary {
        primary_head,
        audit_head,
        ahead,
        behind,
        state,
    }
}

fn git_rev_parse_short(repo_root: &Path, reference: &str) -> Option<String> {
    git_stdout(repo_root, ["rev-parse", "--short=9", reference])
        .ok()
        .map(|head| head.trim().to_string())
        .filter(|head| !head.is_empty())
}

fn git_ahead_behind(
    repo_root: &Path,
    primary_ref: &str,
    audit_ref: &str,
) -> Option<(usize, usize)> {
    let range = format!("{primary_ref}...{audit_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-list", "--left-right", "--count", &range])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut parts = stdout.split_whitespace();
    let primary_only = parts.next()?.parse::<usize>().ok()?;
    let audit_only = parts.next()?.parse::<usize>().ok()?;
    Some((audit_only, primary_only))
}

fn format_task_ids_by_status(
    manifest: &EverythingManifest,
    status: StageStatus,
    limit: usize,
) -> String {
    let matching = manifest
        .remediation_tasks
        .iter()
        .filter(|task| task.status == status)
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return "none".to_string();
    }
    let mut formatted = matching
        .iter()
        .take(limit)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if matching.len() > limit {
        formatted.push_str(&format!(" (+{} more)", matching.len() - limit));
    }
    formatted
}

fn first_dependency_blocked_remediation_task(
    manifest: &EverythingManifest,
) -> Option<(&RemediationTaskState, Vec<String>)> {
    let complete = complete_remediation_task_ids(manifest);
    manifest
        .remediation_tasks
        .iter()
        .filter(|task| matches!(task.status, StageStatus::Pending))
        .find_map(|task| {
            let unmet = unmet_remediation_dependencies(task, &complete);
            (!unmet.is_empty()).then_some((task, unmet))
        })
}

fn commit_word(count: usize) -> &'static str {
    if count == 1 {
        "commit"
    } else {
        "commits"
    }
}

fn one_line(text: &str) -> String {
    text.trim().replace('\n', " ")
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

fn audit_lane_changed_files(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
) -> Result<Vec<String>> {
    if base_commit == head_ref {
        return Ok(Vec::new());
    }
    let range = format!("{base_commit}..{head_ref}");
    let output = git_stdout(repo_root, ["diff", "--name-only", &range])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn fetch_lane_commit(repo_root: &Path, lane_repo_root: &Path, lane_head: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("fetch")
        .arg(lane_repo_root)
        .arg(lane_head)
        .output()
        .with_context(|| {
            format!(
                "failed to fetch lane commit {} from {}",
                lane_head,
                lane_repo_root.display()
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git fetch failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn git_ref_is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .with_context(|| {
            format!(
                "failed checking whether {ancestor} is an ancestor of {descendant} in {}",
                repo_root.display()
            )
        })?;
    Ok(output.status.success())
}

fn cherry_pick_lane_range(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
    abort_on_failure: bool,
) -> Result<()> {
    if audit_lane_changed_files(repo_root, base_commit, head_ref)?.is_empty() {
        return Ok(());
    }
    let range = format!("{base_commit}..{head_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cherry-pick")
        .arg("--empty=drop")
        .arg(&range)
        .output()
        .with_context(|| format!("failed to cherry-pick {range} in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }
    if abort_on_failure {
        let _ = run_git(repo_root, ["cherry-pick", "--abort"]);
    }
    bail!(
        "git cherry-pick failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
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
        assert!(excluded_path(".claude/worktrees/agent-a123"));
        assert!(excluded_path(".claude/worktrees/agent-a123/README.md"));
        assert!(excluded_path(
            "audit/everything/20260424-115535/reports/src.md"
        ));
        assert!(excluded_path("audit/old-run/FINAL-REVIEW.md"));
        assert!(excluded_path("docs/ops/operator-evidence/canary.md"));
        assert!(excluded_path("web/client/dist/bitino-client.js"));
        assert!(excluded_path("web/play/dist/rplay.js"));
        assert!(excluded_path(".github/ISSUE_TEMPLATE/bug.md"));
        assert!(excluded_path("Cargo.lock"));
        assert!(excluded_path("web/play/dist/rplay.js.map"));
        assert!(excluded_path("docs/ops/operator-evidence/smoke.png"));
        assert!(excluded_path(
            "audit/everything/20260424-115535/files/hash/analysis.md"
        ));
        assert!(excluded_path("target/debug/app"));
        assert!(excluded_path("gen-20260424/spec.md"));
        assert!(!excluded_path("crates/bitino-house/src/lib.rs"));
    }

    #[test]
    fn file_artifact_slug_is_per_file_even_for_identical_content() {
        let content_hash = sha256_hex(b"same generated content");
        assert_ne!(
            file_artifact_slug("crates/a/generated.d.ts", &content_hash),
            file_artifact_slug("crates/b/generated.d.ts", &content_hash)
        );
    }

    #[test]
    fn large_utf8_file_prompt_requires_full_chunked_review() {
        let dir =
            std::env::temp_dir().join(format!("auto-audit-large-file-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let path = dir.join("large.rs");
        let mut body = String::new();
        for index in 0..20_000 {
            body.push_str(&format!("fn line_{index}() {{}}\n"));
        }
        fs::write(&path, body).expect("failed to write large file");

        let prompt_body = prompt_file_body(&path).expect("failed to build prompt file body");
        assert!(prompt_body.contains("large UTF-8 file omitted from inline prompt"));
        assert!(prompt_body.contains("Mandatory full-file review"));
        assert!(prompt_body.contains("Read the entire file in ordered chunks"));
        assert!(!prompt_body.contains("metadata and path only"));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn legacy_large_file_prompt_invalidates_artifact_completion() {
        let dir =
            std::env::temp_dir().join(format!("auto-audit-legacy-artifact-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        fs::write(dir.join("analysis.md"), "# src/lib.rs\n").expect("failed to write analysis.md");
        fs::write(dir.join("analysis.json"), "{}\n").expect("failed to write analysis.json");
        fs::write(
            dir.join("first-pass-prompt.md"),
            "[file omitted from prompt because it is 300000 bytes; inspect metadata and path only]",
        )
        .expect("failed to write first-pass prompt");

        assert!(!artifact_complete(&dir));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn codebase_improvement_policy_defines_deletion_contract() {
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("proved-dead code"));
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("AI slop"));
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("Required Proof Before Removal"));
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("safe_delete"));
        assert!(CODEBASE_IMPROVEMENT_POLICY.contains("leave_with_reason"));
    }

    #[test]
    fn first_pass_prompt_collects_debt_and_architecture_fields() {
        let file = FileState {
            path: "src/lib.rs".to_string(),
            group: "src".to_string(),
            content_hash: "hash".to_string(),
            artifact_dir: "/tmp/run/worktree/audit/everything/test-run/files/src-lib".to_string(),
            status: StageStatus::Pending,
        };
        let prompt = build_file_prompt(&file, "# Context\n", "pub fn live() {}\n");

        assert!(prompt.contains("orphaned, deprecated, duplicated"));
        assert!(prompt.contains("AI-slop signals"));
        assert!(prompt.contains("deletion_candidates"));
        assert!(prompt.contains("architecture_smells"));
        assert!(prompt.contains("behavior_preservation_needs"));
    }

    #[test]
    fn pending_group_report_rebuilds_with_authoritative_artifact_refs() {
        let dir = std::env::temp_dir().join(format!(
            "auto-audit-group-report-rebuild-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let report_root = dir.join("audit/everything/test-run");
        let artifact_dir = report_root.join("files/path-hash-content-hash");
        fs::create_dir_all(&artifact_dir).expect("failed to create artifact dir");
        fs::write(
            artifact_dir.join("analysis.md"),
            "# src/lib.rs\n\nA focused first-pass analysis.\n",
        )
        .expect("failed to write analysis");

        let report_path = report_root.join("reports/src.md");
        fs::create_dir_all(report_path.parent().unwrap()).expect("failed to create reports dir");
        fs::write(&report_path, "stale partial synthesis\n").expect("failed to write stale report");

        let mut manifest = manifest_with_groups(vec![GroupState {
            name: "src".to_string(),
            slug: "src".to_string(),
            files: vec!["src/lib.rs".to_string()],
            report_path: report_path.display().to_string(),
            synthesis_status: StageStatus::Pending,
            remediation_status: StageStatus::Pending,
        }]);
        manifest.files = vec![FileState {
            path: "src/lib.rs".to_string(),
            group: "src".to_string(),
            content_hash: "content-hash".to_string(),
            artifact_dir: artifact_dir.display().to_string(),
            status: StageStatus::Complete,
        }];

        let paths = RunPaths {
            host_root: dir.clone(),
            manifest_path: dir.join("manifest.json"),
            latest_path: dir.join("latest"),
            worktree_root: dir.clone(),
            report_root,
            pause_path: dir.join(PAUSE_REQUEST_FILE),
            in_place: false,
        };

        build_initial_group_reports(&paths, &manifest).expect("failed to build group reports");
        let report = fs::read_to_string(&report_path).expect("failed to read report");

        assert!(!report.contains("stale partial synthesis"));
        assert!(report.contains("First-pass artifact:"));
        assert!(report.contains("path-hash-content-hash"));
        assert!(report.contains("Ignore unreferenced artifact directories"));
        assert!(report.contains("## Debt Register"));
        assert!(report.contains("safe_delete"));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn synthesis_prompt_warns_against_unreferenced_artifact_globs() {
        let paths = RunPaths {
            host_root: PathBuf::from("/tmp/run"),
            manifest_path: PathBuf::from("/tmp/run/manifest.json"),
            latest_path: PathBuf::from("/tmp/run/latest"),
            worktree_root: PathBuf::from("/tmp/run/worktree"),
            report_root: PathBuf::from("/tmp/run/worktree/audit/everything/test-run"),
            pause_path: PathBuf::from("/tmp/run/PAUSE"),
            in_place: false,
        };
        let group = GroupState {
            name: "src".to_string(),
            slug: "src".to_string(),
            files: vec!["src/lib.rs".to_string()],
            report_path: "/tmp/run/worktree/audit/everything/test-run/reports/src.md".to_string(),
            synthesis_status: StageStatus::Pending,
            remediation_status: StageStatus::Pending,
        };

        let prompt = build_synthesis_prompt(&paths, &group);

        assert!(prompt.contains("exact first-pass artifact paths referenced inside it"));
        assert!(prompt.contains("Do not glob or enumerate"));
        assert!(prompt.contains("/tmp/run/worktree/audit/everything/test-run/files"));
        assert!(prompt.contains("Build or update a debt register"));
        assert!(prompt.contains("AI-slop"));
        assert!(prompt.contains("## Debt Register"));
    }

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

    #[test]
    fn final_review_prompt_requires_codebase_book() {
        let manifest = manifest_with_groups(Vec::new());
        let paths = RunPaths {
            host_root: PathBuf::from("/tmp/run"),
            manifest_path: PathBuf::from("/tmp/run/MANIFEST.json"),
            latest_path: PathBuf::from("/tmp/run/latest-run"),
            worktree_root: PathBuf::from("/tmp/run/worktree"),
            report_root: PathBuf::from("/tmp/run/worktree/audit/everything/test-run"),
            pause_path: PathBuf::from("/tmp/run/PAUSE"),
            in_place: false,
        };
        let prompt = build_final_review_prompt(&paths, &manifest);
        assert!(prompt.contains("CODEBASE-BOOK/"));
        assert!(prompt.contains("must not be a single giant markdown file"));
        assert!(prompt.contains("Numbered chapter files"));
        assert!(prompt.contains("File-catalog chapters split by subsystem/group"));
        assert!(prompt.contains("cover every tracked file"));
        assert!(prompt.contains("changed:"));
        assert!(prompt.contains("Evidence class checklist"));
        assert!(prompt.contains("Deletion and refactor proof checklist"));
        assert!(prompt.contains("live production or mainnet/on-chain validation"));
        assert!(prompt.contains("Do not merge bulky first-pass mirrors"));
    }

    #[test]
    fn final_review_blocker_detection_requires_no_go_and_real_bullet() {
        let dir = std::env::temp_dir().join(format!(
            "auto-audit-final-review-blockers-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let report_root = dir.join("audit/everything/test-run");
        fs::create_dir_all(&report_root).expect("failed to create report root");
        let paths = RunPaths {
            host_root: dir.clone(),
            manifest_path: dir.join("MANIFEST.json"),
            latest_path: dir.join("latest-run"),
            worktree_root: dir.clone(),
            report_root,
            pause_path: dir.join(PAUSE_REQUEST_FILE),
            in_place: true,
        };
        fs::write(
            final_review_markdown_path(&paths),
            "# FINAL REVIEW\n\nVerdict: NO-GO\n\n## Required blockers before merge\n\n- Fix missing validation proof\n\n## Optional follow-ups\n\n- Later\n",
        )
        .expect("failed to write final review");
        assert!(final_review_has_actionable_blockers(&paths));

        fs::write(
            final_review_markdown_path(&paths),
            "# FINAL REVIEW\n\nVerdict: NO-GO\n\n## Required blockers before merge\n\n- none\n",
        )
        .expect("failed to rewrite final review");
        assert!(!final_review_has_actionable_blockers(&paths));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn final_review_repair_prompt_is_bounded_to_actionable_blockers() {
        let manifest = manifest_with_groups(Vec::new());
        let paths = RunPaths {
            host_root: PathBuf::from("/tmp/run"),
            manifest_path: PathBuf::from("/tmp/run/MANIFEST.json"),
            latest_path: PathBuf::from("/tmp/run/latest-run"),
            worktree_root: PathBuf::from("/tmp/run/worktree"),
            report_root: PathBuf::from("/tmp/run/worktree/audit/everything/test-run"),
            pause_path: PathBuf::from("/tmp/run/PAUSE"),
            in_place: false,
        };
        let prompt = build_final_review_repair_prompt(
            &paths,
            &manifest,
            1,
            Path::new(
                "/tmp/run/worktree/audit/everything/test-run/FINAL-REVIEW.no-go-attempt-1.md",
            ),
        );
        assert!(prompt.contains("only concrete, actionable blockers"));
        assert!(prompt.contains("Do not broaden the audit"));
        assert!(prompt.contains("host will rerun final review"));
    }

    #[test]
    fn file_quality_prompts_target_ten_and_accept_nine() {
        let file = FileState {
            path: "src/lib.rs".to_string(),
            group: "src".to_string(),
            content_hash: "hash".to_string(),
            artifact_dir: "/tmp/run/worktree/audit/everything/test-run/files/src-lib".to_string(),
            status: StageStatus::Complete,
        };
        let mut manifest = manifest_with_groups(vec![group_for_test("src", &["src/lib.rs"])]);
        manifest.files = vec![file.clone()];
        let paths = RunPaths {
            host_root: PathBuf::from("/tmp/run"),
            manifest_path: PathBuf::from("/tmp/run/MANIFEST.json"),
            latest_path: PathBuf::from("/tmp/run/latest-run"),
            worktree_root: PathBuf::from("/tmp/run/worktree"),
            report_root: PathBuf::from("/tmp/run/worktree/audit/everything/test-run"),
            pause_path: PathBuf::from("/tmp/run/PAUSE"),
            in_place: false,
        };

        let rerate = build_file_quality_rerate_prompt(&paths, &manifest, &file, 3);
        assert!(rerate.contains("Regrade the first-pass rating"));
        assert!(rerate.contains("target is 10/10"));
        assert!(rerate.contains("below 9/10"));
        assert!(rerate.contains("rating.json"));
        assert!(rerate.contains("AI-slop"));
        assert!(rerate.contains("debt_or_architecture_findings"));

        let rating = FileQualityRatingState {
            path: file.path.clone(),
            score_out_of_10: Some(8.0),
            status: StageStatus::Complete,
            artifact_dir:
                "/tmp/run/worktree/audit/everything/test-run/FILE-QUALITY/pass-03/src-lib"
                    .to_string(),
            note: None,
        };
        let deliverables =
            build_file_quality_deliverables_prompt(&paths, &manifest, &file, &rating, 3);
        assert!(deliverables.contains("Owned file: `src/lib.rs`"));
        assert!(deliverables.contains("acceptance floor is 9/10"));
        assert!(deliverables.contains("aim at 10/10"));
        assert!(deliverables.contains("deliverables.md"));
        assert!(deliverables.contains("delete, simplify, consolidate, or relocate code"));
        assert!(deliverables.contains("Deletion/refactor proof"));
    }

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

    #[test]
    fn run_status_markdown_records_pause_paths_and_task_counts() {
        let dir =
            std::env::temp_dir().join(format!("auto-audit-run-status-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let report_root = dir.join("worktree/audit/everything/test-run");
        fs::create_dir_all(&report_root).expect("failed to create report root");
        let paths = RunPaths {
            host_root: dir.clone(),
            manifest_path: dir.join("MANIFEST.json"),
            latest_path: dir.join("latest-run"),
            worktree_root: dir.join("worktree"),
            report_root,
            pause_path: dir.join(PAUSE_REQUEST_FILE),
            in_place: false,
        };
        fs::write(&paths.pause_path, "pause requested\n").expect("failed to write pause file");
        let mut manifest = manifest_with_groups(vec![
            group_for_test("crates/core", &["crates/core/src/lib.rs"]),
            group_for_test("docs", &["docs/architecture.md"]),
        ]);
        manifest.files = vec![FileState {
            path: "crates/core/src/lib.rs".to_string(),
            group: "crates/core".to_string(),
            content_hash: "hash".to_string(),
            artifact_dir: "artifact".to_string(),
            status: StageStatus::Complete,
        }];
        manifest.remediation_tasks = vec![
            task_for_test("AUD-REM-001", "crates/core", &[]),
            task_for_test("AUD-REM-002", "docs", &["AUD-REM-001"]),
        ];
        manifest.remediation_tasks[0].status = StageStatus::Complete;
        manifest.remediation_tasks[0].note = Some("landed 2 changed file(s)".to_string());
        manifest.remediation_tasks[1].status = StageStatus::Running;

        write_run_status_markdown(&paths, &manifest).expect("failed to write run status");
        let status = fs::read_to_string(run_status_markdown_path(&paths))
            .expect("failed to read run status");

        assert!(status.contains("Pause requested: yes"));
        assert!(status.contains("1 complete, 1 running, 0 pending, 0 failed, 0 skipped"));
        assert!(status.contains("`AUD-REM-002` `docs`"));
        assert!(status.contains("REMEDIATION-PLAN.md"));
        assert!(status.contains("CODEBASE-BOOK"));
        assert!(status.contains("Final status refresh:"));
        assert!(status.contains("Evidence Class Checklist"));
        assert!(status.contains("Live production or mainnet/on-chain validation"));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    fn manifest_with_groups(groups: Vec<GroupState>) -> EverythingManifest {
        EverythingManifest {
            run_id: "test-run".to_string(),
            repo_root: ".".to_string(),
            worktree_root: ".".to_string(),
            report_root: "audit/everything/test-run".to_string(),
            in_place: false,
            branch: "main".to_string(),
            audit_branch: "auto-audit/test".to_string(),
            base_commit: "base".to_string(),
            created_at: "now".to_string(),
            context: ContextState::default(),
            files: Vec::new(),
            groups,
            remediation_plan: StageState::default(),
            remediation_tasks: Vec::new(),
            final_review_repairs: Vec::new(),
            file_quality: StageState::default(),
            file_quality_passes: Vec::new(),
            change_summary: StageState::default(),
            final_review: StageState::default(),
            merge: StageState::default(),
            final_status: StageState::default(),
        }
    }

    fn group_for_test(name: &str, files: &[&str]) -> GroupState {
        GroupState {
            name: name.to_string(),
            slug: slugify(name),
            files: files.iter().map(|file| file.to_string()).collect(),
            report_path: format!("audit/everything/test-run/reports/{}.md", slugify(name)),
            synthesis_status: StageStatus::Complete,
            remediation_status: StageStatus::Pending,
        }
    }

    fn task_for_test(id: &str, group: &str, dependencies: &[&str]) -> RemediationTaskState {
        RemediationTaskState {
            id: id.to_string(),
            group: group.to_string(),
            slug: slugify(group),
            report_path: format!("audit/everything/test-run/reports/{}.md", slugify(group)),
            owned_paths: Vec::new(),
            dependencies: dependencies
                .iter()
                .map(|dependency| dependency.to_string())
                .collect(),
            lane_index: 1,
            lane_root: ".auto/audit-everything/test/remediation-lanes/lane-1".to_string(),
            lane_repo_root: ".auto/audit-everything/test/remediation-lanes/lane-1/repo".to_string(),
            base_commit: None,
            status: StageStatus::Pending,
            note: None,
        }
    }
}
