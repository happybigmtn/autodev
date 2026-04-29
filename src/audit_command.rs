//! `auto audit` — file-by-file audit against an operator-authored doctrine.
//!
//! # Model
//!
//! Each tracked file in the repo is audited independently. For each file the
//! auditor receives three inputs:
//!
//! 1. **Bundled rubric** — ships with this command. Defines the five verdicts
//!    (`CLEAN` / `DRIFT-SMALL` / `DRIFT-LARGE` / `SLOP` / `RETIRE` /
//!    `REFACTOR`), the output contract (auditor writes verdict.json +
//!    optional patch.diff / worklist-entry.md / retire-reason.md into
//!    `audit/files/<hash>/`), and the tool policy.
//!
//! 2. **Operator doctrine** — 100% operator-controlled markdown at
//!    `audit/DOCTRINE.md` (or `--doctrine-prompt <path>`). This is the
//!    judgment framework: what counts as drift, slop, retire-worthy;
//!    path-scoped rules; do-not-flag lists; canonical doctrine docs to
//!    reference.
//!
//! 3. **The file itself**, verbatim, with its path.
//!
//! # Resumability
//!
//! A master `audit/MANIFEST.json` tracks every tracked file plus three
//! hashes:
//!
//! - `content_hash`: sha256 of the file at audit time
//! - `doctrine_hash`: sha256 of the doctrine prompt at audit time
//! - `rubric_hash`: sha256 of the rubric at audit time
//!
//! A file is considered fresh-enough to skip only when its current content
//! hash matches the manifest entry AND the doctrine/rubric hashes at audit
//! time match current doctrine/rubric hashes. Any drift triggers a re-audit.
//! Kill mid-run: partial `audit/files/<hash>/` directories are dropped and
//! re-audited on next run; the manifest only flips to `audited` after a
//! clean write.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::process::Command as TokioCommand;
use tokio::task::JoinSet;
use tokio::time;

use crate::codex_exec::MAX_CODEX_MODEL_CONTEXT_WINDOW;
use crate::codex_stream::{capture_codex_output_prefixed, capture_pi_output};
use crate::kimi_backend::{
    extract_final_text as kimi_extract_final_text, kimi_exec_args, parse_kimi_error,
    preflight_kimi_cli, resolve_kimi_bin, resolve_kimi_cli_model,
};
use crate::prompt_ethos::with_autodev_prompt_ethos;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, run_git,
};
use crate::{AuditArgs, AuditResumeMode};

const AUDITOR_TIMEOUT_SECS: u64 = 30 * 60; // 30 minutes per file — generous
const FINDING_RESOLUTION_TIMEOUT_SECS: u64 = 4 * 60 * 60; // remediation lanes can pay fresh dependency-build cost
const BUNDLED_RUBRIC: &str = include_str!("audit_rubric.md");
const DEFAULT_INCLUDE_GLOBS: &[&str] = &[
    "**/*.rs",
    "**/*.ts",
    "**/*.tsx",
    "**/*.py",
    "specs/**/*.md",
    "AUTONOMY-GDD.md",
    "RSOCIETY-GDD.md",
    "SECURITY_PLAN.md",
    "IMPLEMENTATION_PLAN.md",
    "DESIGN.md",
    "AGENTS.md",
    "CLAUDE.md",
    "INVARIANTS.md",
    "OS.md",
    "REVIEW.md",
    "WORKLIST.md",
    "LEARNINGS.md",
];
const DEFAULT_EXCLUDE_GLOBS: &[&str] = &[
    "**/target/**",
    "**/node_modules/**",
    "**/dist/**",
    "**/build/**",
    "**/.auto/**",
    "**/.cache/**",
    "**/.claude/worktrees/**",
    "**/.config/**",
    "**/.next/**",
    "**/.pytest_cache/**",
    "**/.turbo/**",
    "**/.venv/**",
    "**/__pycache__/**",
    "**/bug/**",
    "**/coverage/**",
    "**/nemesis/**",
    "**/playwright-report/**",
    "**/reports/**",
    "**/steward/**",
    "**/temp/**",
    "**/test-results/**",
    "**/tmp/**",
    "**/venv/**",
    "**/audit/**",
    "**/fixtures/**",
    "**/vendor/**",
    "**/*.min.js",
    "**/*.lock",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    started_at: String,
    repo_head: String,
    doctrine_hash: String,
    rubric_hash: String,
    files: Vec<ManifestEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ManifestEntry {
    path: String,
    status: EntryStatus,
    content_hash: Option<String>,
    audited_doctrine_hash: Option<String>,
    audited_rubric_hash: Option<String>,
    verdict: Option<String>,
    audited_at: Option<String>,
    commit: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum EntryStatus {
    Pending,
    Audited,
    ApplyFailed,
    Escalated,
    Skipped,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
struct FileVerdict {
    verdict: String,
    rationale: String,
    #[serde(default)]
    touched_paths: Vec<String>,
    #[serde(default)]
    escalate: bool,
}

#[derive(Clone, Debug, Serialize)]
struct FindingVerificationReport {
    generated_at: String,
    manifest_path: String,
    total_flagged: usize,
    resolved_removed: usize,
    still_open: usize,
    needs_reaudit: usize,
    findings: Vec<FindingVerificationEntry>,
}

#[derive(Clone, Debug, Serialize)]
struct FindingVerificationEntry {
    path: String,
    verdict: String,
    status: EntryStatus,
    result: FindingVerificationResult,
    manifest_content_hash: Option<String>,
    current_content_hash: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum FindingVerificationResult {
    ResolvedRemoved,
    NeedsReaudit,
    StillOpen,
}

struct AuditWorkerResult {
    idx: usize,
    entry: ManifestEntry,
    content_hash: String,
    file_dir: PathBuf,
    response: String,
}

#[derive(Clone, Debug)]
struct FindingResolutionLane {
    id: usize,
    name: String,
    entries: Vec<ManifestEntry>,
}

#[derive(Clone, Debug, Serialize)]
struct FindingResolutionRunStatus {
    generated_at: String,
    run_id: String,
    phase: String,
    run_dir: String,
    worktree_root: String,
    target_root: String,
    lanes: Vec<FindingResolutionLaneStatus>,
}

#[derive(Clone, Debug, Serialize)]
struct FindingResolutionLaneStatus {
    id: usize,
    name: String,
    finding_count: usize,
    state: String,
    repo_dir: String,
    target_dir: String,
    prompt_path: String,
    response_path: String,
    landed_commit: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct FindingResolutionLaneOutcome {
    lane_id: usize,
    lane_repo_root: PathBuf,
    base_commit: String,
}

#[derive(Clone, Debug)]
struct FindingResolutionLaneAssignment {
    lane: FindingResolutionLane,
    lane_root: PathBuf,
    lane_repo_root: PathBuf,
    lane_target_dir: PathBuf,
    base_commit: String,
}

pub(crate) async fn run_audit(args: AuditArgs) -> Result<()> {
    if args.everything {
        return crate::audit_everything::run_audit_everything(args).await;
    }

    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string();
    if !args.dry_run && !args.report_only && current_branch.is_empty() {
        bail!("auto audit requires a checked-out branch");
    }
    if let Some(required) = args.branch.as_deref() {
        if current_branch != required {
            bail!(
                "auto audit must run on branch `{}` (current: `{}`)",
                required,
                current_branch
            );
        }
    }

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join("audit"));
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    if args.verify_findings {
        return verify_audit_findings(&repo_root, &output_dir);
    }
    if args.resolve_findings {
        return resolve_audit_findings(&repo_root, &output_dir, args).await;
    }
    fs::create_dir_all(output_dir.join("files"))
        .with_context(|| format!("failed to create {}", output_dir.join("files").display()))?;

    let doctrine_path = if args.doctrine_prompt.is_absolute() {
        args.doctrine_prompt.clone()
    } else {
        repo_root.join(&args.doctrine_prompt)
    };
    if !doctrine_path.exists() {
        bail!(
            "auto audit doctrine prompt not found at {}. Author it before running; \
             the command intentionally does not auto-generate the doctrine — that's \
             your repo's judgment framework.\n\n\
             See `docs/audit-doctrine-template.md` in the autodev repo for a \
             starter shape, or copy one from a sibling repo.",
            doctrine_path.display()
        );
    }
    let doctrine = fs::read_to_string(&doctrine_path)
        .with_context(|| format!("failed to read {}", doctrine_path.display()))?;
    let doctrine_hash = sha256_hex(doctrine.as_bytes());

    let rubric = if let Some(path) = args.rubric_prompt.as_deref() {
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            repo_root.join(path)
        };
        fs::read_to_string(&resolved)
            .with_context(|| format!("failed to read {}", resolved.display()))?
    } else {
        BUNDLED_RUBRIC.to_string()
    };
    let rubric_hash = sha256_hex(rubric.as_bytes());

    if args.use_kimi_cli && is_kimi_model(&args.model) && !args.dry_run {
        let kimi_bin = resolve_kimi_bin(&args.kimi_bin);
        preflight_kimi_cli(&kimi_bin, &args.model)
            .with_context(|| "kimi-cli preflight failed; aborting auto audit".to_string())?;
    }

    let include_globs = if args.include_paths.is_empty() {
        DEFAULT_INCLUDE_GLOBS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        args.include_paths.clone()
    };
    let exclude_globs: Vec<String> = DEFAULT_EXCLUDE_GLOBS
        .iter()
        .map(|s| (*s).to_string())
        .chain(args.exclude_paths.iter().cloned())
        .collect();

    let tracked_files = enumerate_tracked_files(&repo_root, &include_globs, &exclude_globs)?;
    let manifest_path = output_dir.join("MANIFEST.json");

    let mut manifest = match args.resume_mode {
        AuditResumeMode::Fresh => {
            if manifest_path.exists() {
                let stamp = crate::util::timestamp_slug();
                let archive = output_dir.join(format!("MANIFEST-{stamp}.archive.json"));
                fs::rename(&manifest_path, &archive).with_context(|| {
                    format!(
                        "failed to archive existing manifest {} -> {}",
                        manifest_path.display(),
                        archive.display()
                    )
                })?;
                println!("archived old manifest to {}", archive.display());
            }
            initial_manifest(&repo_root, &tracked_files, &doctrine_hash, &rubric_hash)?
        }
        _ => {
            if manifest_path.exists() {
                let raw = fs::read_to_string(&manifest_path)
                    .with_context(|| format!("failed to read {}", manifest_path.display()))?;
                let mut existing: Manifest = serde_json::from_str(&raw)
                    .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
                reconcile_manifest_with_tree(&mut existing, &tracked_files, &repo_root)?;
                existing.doctrine_hash = doctrine_hash.clone();
                existing.rubric_hash = rubric_hash.clone();
                existing
            } else {
                initial_manifest(&repo_root, &tracked_files, &doctrine_hash, &rubric_hash)?
            }
        }
    };

    let plan = plan_audit_queue(
        &mut manifest,
        args.resume_mode,
        &repo_root,
        &doctrine_hash,
        &rubric_hash,
    )?;
    let total = plan.len();
    let cap = if args.max_files == 0 {
        total
    } else {
        args.max_files.min(total)
    };

    println!("auto audit");
    println!("repo root:    {}", repo_root.display());
    println!("output dir:   {}", output_dir.display());
    println!(
        "doctrine:     {} ({})",
        doctrine_path.display(),
        &doctrine_hash[..12]
    );
    println!(
        "rubric:       {} ({})",
        args.rubric_prompt
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "bundled".to_string()),
        &rubric_hash[..12]
    );
    println!("branch:       {}", current_branch);
    println!("auditor:      {} ({})", args.model, args.reasoning_effort);
    println!(
        "tracked:      {} files ({} included after filters)",
        tracked_files.len(),
        manifest.files.len()
    );
    println!(
        "queue:        {} file(s) to audit this run (cap {})",
        total, cap
    );
    if args.report_only {
        println!("mode:         report-only");
    }
    if args.dry_run && total > 0 {
        let first = &plan[0];
        let preview_prompt = build_file_prompt(
            &repo_root,
            &repo_root.join(&first.path),
            &doctrine,
            &rubric,
            &output_dir,
            &first.path,
        )?;
        println!();
        println!("--- first pending prompt ---");
        println!("{preview_prompt}");
        return Ok(());
    }
    if args.dry_run {
        println!("--dry-run: nothing pending");
        return Ok(());
    }

    if !args.report_only && !current_branch.is_empty() {
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, current_branch.as_str(), "audit checkpoint")?
        {
            println!("checkpoint:  committed pre-existing changes at {commit}");
        }
    }
    write_manifest(&manifest_path, &manifest)?;

    let mut audited = 0usize;
    let mut applied = 0usize;
    let mut clean = 0usize;
    let mut worklisted = 0usize;
    let mut retired = 0usize;
    let mut apply_failed = 0usize;
    let workers = args.audit_threads.clamp(1, 15).min(cap.max(1));
    println!("workers:      {workers}");
    let selected_plan = plan.into_iter().take(cap).collect::<Vec<_>>();
    let repo_root_arc = Arc::new(repo_root.clone());
    let output_dir_arc = Arc::new(output_dir.clone());
    let doctrine_arc = Arc::new(doctrine);
    let rubric_arc = Arc::new(rubric);
    let mut join_set = JoinSet::new();
    let mut plan_iter = selected_plan.into_iter().enumerate();
    let mut active = 0usize;
    for _ in 0..workers {
        if let Some((idx, entry)) = plan_iter.next() {
            spawn_audit_worker(
                &mut join_set,
                repo_root_arc.clone(),
                output_dir_arc.clone(),
                doctrine_arc.clone(),
                rubric_arc.clone(),
                args.clone(),
                idx,
                cap,
                entry,
            );
            active += 1;
        }
    }

    while active > 0 {
        let Some(joined) = join_set.join_next().await else {
            break;
        };
        active -= 1;
        let worker = match joined {
            Ok(Ok(worker)) => worker,
            Ok(Err(err)) => {
                eprintln!("audit worker failed: {err:#}");
                if let Some((idx, entry)) = plan_iter.next() {
                    spawn_audit_worker(
                        &mut join_set,
                        repo_root_arc.clone(),
                        output_dir_arc.clone(),
                        doctrine_arc.clone(),
                        rubric_arc.clone(),
                        args.clone(),
                        idx,
                        cap,
                        entry,
                    );
                    active += 1;
                }
                continue;
            }
            Err(err) => {
                eprintln!("audit worker task panicked: {err}");
                if let Some((idx, entry)) = plan_iter.next() {
                    spawn_audit_worker(
                        &mut join_set,
                        repo_root_arc.clone(),
                        output_dir_arc.clone(),
                        doctrine_arc.clone(),
                        rubric_arc.clone(),
                        args.clone(),
                        idx,
                        cap,
                        entry,
                    );
                    active += 1;
                }
                continue;
            }
        };

        atomic_write(
            &worker.file_dir.join("response.log"),
            worker.response.as_bytes(),
        )
        .with_context(|| {
            format!(
                "failed to write {}",
                worker.file_dir.join("response.log").display()
            )
        })?;

        let verdict_path = worker.file_dir.join("verdict.json");
        let verdict = match fs::read_to_string(&verdict_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<FileVerdict>(&raw).ok())
        {
            Some(v) => v,
            None => {
                eprintln!(
                    "audit finished but verdict.json missing / invalid for {}; keeping pending",
                    worker.entry.path
                );
                mark_entry(
                    &mut manifest,
                    &worker.entry.path,
                    EntryStatus::Pending,
                    Some(worker.content_hash),
                    Some(doctrine_hash.clone()),
                    Some(rubric_hash.clone()),
                    None,
                    None,
                );
                write_manifest(&manifest_path, &manifest)?;
                if let Some((idx, entry)) = plan_iter.next() {
                    spawn_audit_worker(
                        &mut join_set,
                        repo_root_arc.clone(),
                        output_dir_arc.clone(),
                        doctrine_arc.clone(),
                        rubric_arc.clone(),
                        args.clone(),
                        idx,
                        cap,
                        entry,
                    );
                    active += 1;
                }
                continue;
            }
        };
        println!(
            "verdict [{idx}/{cap}] {path}: {verdict} — {rationale}",
            idx = worker.idx + 1,
            cap = cap,
            path = worker.entry.path,
            verdict = verdict.verdict,
            rationale = first_line(&verdict.rationale)
        );

        let (new_status, commit_sha) = apply_verdict(
            &repo_root,
            &output_dir,
            &current_branch,
            &worker.entry.path,
            &worker.file_dir,
            &verdict,
            args.report_only,
        )?;
        match new_status {
            EntryStatus::Audited => match verdict.verdict.as_str() {
                "CLEAN" => clean += 1,
                "DRIFT-SMALL" | "SLOP" => applied += 1,
                "DRIFT-LARGE" | "REFACTOR" => worklisted += 1,
                "RETIRE" => retired += 1,
                _ => {}
            },
            EntryStatus::ApplyFailed => apply_failed += 1,
            _ => {}
        }
        audited += 1;
        mark_entry(
            &mut manifest,
            &worker.entry.path,
            new_status,
            Some(worker.content_hash),
            Some(doctrine_hash.clone()),
            Some(rubric_hash.clone()),
            Some(verdict.verdict.clone()),
            commit_sha,
        );
        write_manifest(&manifest_path, &manifest)?;
        if !args.report_only && audited.is_multiple_of(25) {
            write_progress_snapshot(
                &output_dir,
                &manifest,
                audited,
                clean,
                applied,
                worklisted,
                retired,
                apply_failed,
            )?;
        }
        if let Some((idx, entry)) = plan_iter.next() {
            spawn_audit_worker(
                &mut join_set,
                repo_root_arc.clone(),
                output_dir_arc.clone(),
                doctrine_arc.clone(),
                rubric_arc.clone(),
                args.clone(),
                idx,
                cap,
                entry,
            );
            active += 1;
        }
    }

    println!();
    println!("auto audit run complete");
    println!(
        "audited {audited} file(s): {clean} CLEAN, {applied} applied, {worklisted} worklisted, \
         {retired} retire candidates, {apply_failed} apply failures"
    );
    write_progress_snapshot(
        &output_dir,
        &manifest,
        audited,
        clean,
        applied,
        worklisted,
        retired,
        apply_failed,
    )?;
    if !args.report_only
        && !current_branch.is_empty()
        && push_branch_with_remote_sync(&repo_root, current_branch.as_str())?
    {
        println!("remote sync: rebased onto origin/{}", current_branch);
    }
    Ok(())
}

fn spawn_audit_worker(
    join_set: &mut JoinSet<Result<AuditWorkerResult>>,
    repo_root: Arc<PathBuf>,
    output_dir: Arc<PathBuf>,
    doctrine: Arc<String>,
    rubric: Arc<String>,
    args: AuditArgs,
    idx: usize,
    cap: usize,
    entry: ManifestEntry,
) {
    join_set.spawn(async move {
        run_audit_worker(
            repo_root, output_dir, doctrine, rubric, args, idx, cap, entry,
        )
        .await
    });
}

async fn run_audit_worker(
    repo_root: Arc<PathBuf>,
    output_dir: Arc<PathBuf>,
    doctrine: Arc<String>,
    rubric: Arc<String>,
    args: AuditArgs,
    idx: usize,
    cap: usize,
    entry: ManifestEntry,
) -> Result<AuditWorkerResult> {
    let abs_path = repo_root.join(&entry.path);
    if !abs_path.exists() {
        bail!(
            "tracked audit path disappeared before worker start: {}",
            entry.path
        );
    }
    let content =
        fs::read(&abs_path).with_context(|| format!("failed to read {}", abs_path.display()))?;
    let content_hash = sha256_hex(&content);
    let file_dir = file_artifact_dir(&output_dir, &entry.path);
    if file_dir.exists() {
        fs::remove_dir_all(&file_dir).ok();
    }
    fs::create_dir_all(&file_dir)
        .with_context(|| format!("failed to create {}", file_dir.display()))?;
    let prompt = build_file_prompt(
        &repo_root,
        &abs_path,
        &doctrine,
        &rubric,
        &output_dir,
        &entry.path,
    )?;
    let prompt_path = file_dir.join("prompt.md");
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!(
        "[{idx}/{cap}] audit {path}",
        idx = idx + 1,
        cap = cap,
        path = entry.path
    );
    let label = format!("audit:{}/{}", idx + 1, entry.path);
    let response = run_auditor_labeled(&repo_root, &prompt, &args, Some(&label)).await?;
    Ok(AuditWorkerResult {
        idx,
        entry,
        content_hash,
        file_dir,
        response,
    })
}

fn initial_manifest(
    repo_root: &Path,
    tracked: &[String],
    doctrine_hash: &str,
    rubric_hash: &str,
) -> Result<Manifest> {
    let head = git_stdout(repo_root, ["rev-parse", "HEAD"]).unwrap_or_default();
    Ok(Manifest {
        started_at: now_iso8601(),
        repo_head: head.trim().to_string(),
        doctrine_hash: doctrine_hash.to_string(),
        rubric_hash: rubric_hash.to_string(),
        files: tracked
            .iter()
            .map(|path| ManifestEntry {
                path: path.clone(),
                status: EntryStatus::Pending,
                content_hash: None,
                audited_doctrine_hash: None,
                audited_rubric_hash: None,
                verdict: None,
                audited_at: None,
                commit: None,
            })
            .collect(),
    })
}

/// Reconcile an existing manifest with the current tree: add new files as
/// `Pending`, drop entries whose path no longer exists.
fn reconcile_manifest_with_tree(
    manifest: &mut Manifest,
    tracked: &[String],
    _repo_root: &Path,
) -> Result<()> {
    let tracked_set: std::collections::HashSet<&str> = tracked.iter().map(String::as_str).collect();
    manifest
        .files
        .retain(|entry| tracked_set.contains(entry.path.as_str()));
    let existing: std::collections::HashSet<String> = manifest
        .files
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    for path in tracked {
        if !existing.contains(path) {
            manifest.files.push(ManifestEntry {
                path: path.clone(),
                status: EntryStatus::Pending,
                content_hash: None,
                audited_doctrine_hash: None,
                audited_rubric_hash: None,
                verdict: None,
                audited_at: None,
                commit: None,
            });
        }
    }
    Ok(())
}

fn plan_audit_queue(
    manifest: &mut Manifest,
    mode: AuditResumeMode,
    repo_root: &Path,
    doctrine_hash: &str,
    rubric_hash: &str,
) -> Result<Vec<ManifestEntry>> {
    let mut queue = Vec::new();
    for entry in &manifest.files {
        let current_content = match fs::read(repo_root.join(&entry.path)) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to read {}", repo_root.join(&entry.path).display())
                });
            }
        };
        let current_content_hash = sha256_hex(&current_content);
        let content_matches_last_audit =
            entry.content_hash.as_deref() == Some(current_content_hash.as_str());
        let doctrine_matches = entry
            .audited_doctrine_hash
            .as_deref()
            .map(|h| h == doctrine_hash)
            .unwrap_or(false);
        let rubric_matches = entry
            .audited_rubric_hash
            .as_deref()
            .map(|h| h == rubric_hash)
            .unwrap_or(false);
        let is_audited = matches!(entry.status, EntryStatus::Audited);
        let is_applied_failed = matches!(entry.status, EntryStatus::ApplyFailed);
        let needs_reaudit =
            is_audited && (!content_matches_last_audit || !doctrine_matches || !rubric_matches);
        match mode {
            AuditResumeMode::Fresh => queue.push(entry.clone()),
            AuditResumeMode::Resume => {
                if !is_audited || needs_reaudit || is_applied_failed {
                    queue.push(entry.clone());
                }
            }
            AuditResumeMode::OnlyDrifted => {
                if (is_audited && needs_reaudit) || is_applied_failed {
                    queue.push(entry.clone());
                }
            }
        }
    }
    Ok(queue)
}

fn verify_audit_findings(repo_root: &Path, output_dir: &Path) -> Result<()> {
    let manifest_path = output_dir.join("MANIFEST.json");
    if !manifest_path.exists() {
        bail!(
            "audit finding verification requires an existing manifest at {}",
            manifest_path.display()
        );
    }

    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;

    let mut findings = Vec::new();
    for entry in manifest.files.iter().filter(audit_entry_requires_closure) {
        let path = repo_root.join(&entry.path);
        let current_content_hash = match fs::read(&path) {
            Ok(content) => Some(sha256_hex(&content)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let result = match current_content_hash.as_deref() {
            None => FindingVerificationResult::ResolvedRemoved,
            Some(current_hash) if entry.content_hash.as_deref() != Some(current_hash) => {
                FindingVerificationResult::NeedsReaudit
            }
            Some(_) => FindingVerificationResult::StillOpen,
        };
        findings.push(FindingVerificationEntry {
            path: entry.path.clone(),
            verdict: entry
                .verdict
                .clone()
                .unwrap_or_else(|| "UNKNOWN".to_string()),
            status: entry.status,
            result,
            manifest_content_hash: entry.content_hash.clone(),
            current_content_hash,
        });
    }

    findings.sort_by(|left, right| left.path.cmp(&right.path));
    let report = FindingVerificationReport {
        generated_at: now_iso8601(),
        manifest_path: manifest_path.display().to_string(),
        total_flagged: findings.len(),
        resolved_removed: findings
            .iter()
            .filter(|finding| finding.result == FindingVerificationResult::ResolvedRemoved)
            .count(),
        needs_reaudit: findings
            .iter()
            .filter(|finding| finding.result == FindingVerificationResult::NeedsReaudit)
            .count(),
        still_open: findings
            .iter()
            .filter(|finding| finding.result == FindingVerificationResult::StillOpen)
            .count(),
        findings,
    };
    write_finding_verification_report(output_dir, &report)?;

    println!("auto audit finding verification");
    println!("manifest:         {}", manifest_path.display());
    println!("flagged findings: {}", report.total_flagged);
    println!("resolved removed: {}", report.resolved_removed);
    println!("needs re-audit:   {}", report.needs_reaudit);
    println!("still open:       {}", report.still_open);
    println!(
        "report:           {}",
        output_dir.join("FINDING-VERIFY.md").display()
    );

    if report.needs_reaudit > 0 || report.still_open > 0 {
        bail!(
            "audit findings are not fully closed: {} need re-audit, {} are still open",
            report.needs_reaudit,
            report.still_open
        );
    }

    Ok(())
}

fn audit_entry_requires_closure(entry: &&ManifestEntry) -> bool {
    matches!(
        entry.verdict.as_deref(),
        Some("DRIFT-LARGE" | "DRIFT-SMALL" | "REFACTOR" | "RETIRE")
    ) || matches!(
        entry.status,
        EntryStatus::ApplyFailed | EntryStatus::Escalated
    )
}

fn write_finding_verification_report(
    output_dir: &Path,
    report: &FindingVerificationReport,
) -> Result<()> {
    let json = serde_json::to_vec_pretty(report)?;
    atomic_write(&output_dir.join("FINDING-VERIFY.json"), &json)?;

    let mut markdown = String::new();
    markdown.push_str("# Audit Finding Verification\n\n");
    markdown.push_str(&format!("- Generated: `{}`\n", report.generated_at));
    markdown.push_str(&format!("- Manifest: `{}`\n", report.manifest_path));
    markdown.push_str(&format!("- Flagged findings: `{}`\n", report.total_flagged));
    markdown.push_str(&format!(
        "- Resolved by removal: `{}`\n",
        report.resolved_removed
    ));
    markdown.push_str(&format!("- Needs re-audit: `{}`\n", report.needs_reaudit));
    markdown.push_str(&format!("- Still open: `{}`\n\n", report.still_open));

    if report.needs_reaudit == 0 && report.still_open == 0 {
        markdown.push_str("Verdict: GO. Every flagged finding has independent closure evidence.\n");
    } else {
        markdown.push_str(
            "Verdict: NO-GO. Re-run `auto audit --resume-mode only-drifted` after remediation, \
             then run `auto audit --verify-findings` again.\n\n",
        );
        markdown.push_str("| Result | Verdict | Status | Path |\n");
        markdown.push_str("|---|---|---|---|\n");
        for finding in &report.findings {
            if finding.result == FindingVerificationResult::ResolvedRemoved {
                continue;
            }
            markdown.push_str(&format!(
                "| `{:?}` | `{}` | `{:?}` | `{}` |\n",
                finding.result, finding.verdict, finding.status, finding.path
            ));
        }
    }
    atomic_write(&output_dir.join("FINDING-VERIFY.md"), markdown.as_bytes())
}

async fn resolve_audit_findings(
    repo_root: &Path,
    output_dir: &Path,
    args: AuditArgs,
) -> Result<()> {
    preflight_finding_resolution_roots(repo_root, &args)?;
    let target_branch = args
        .branch
        .clone()
        .unwrap_or_else(|| {
            git_stdout(repo_root, ["branch", "--show-current"])
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .trim()
        .to_string();
    if target_branch.is_empty() {
        bail!("auto audit --resolve-findings requires a checked-out branch");
    }
    if let Some(checkpoint) = auto_checkpoint_if_needed(
        repo_root,
        &target_branch,
        "audit finding resolution checkpoint",
    )? {
        println!("checkpoint: {checkpoint}");
    }

    let max_passes = args.resolve_passes.max(1);
    for resolve_pass in 1..=max_passes {
        println!("auto audit resolve findings pass {resolve_pass}/{max_passes}");
        match resolve_audit_findings_pass(
            repo_root,
            output_dir,
            args.clone(),
            &target_branch,
            resolve_pass,
            max_passes,
        )
        .await
        {
            Ok(ResolvePassOutcome::Verified) => return Ok(()),
            Ok(ResolvePassOutcome::RetryNeeded { reason }) => {
                if resolve_pass == max_passes {
                    bail!(
                        "audit findings are still not fully closed after {max_passes} resolve pass(es): {reason}"
                    );
                }
                eprintln!(
                    "auto audit resolve findings pass {resolve_pass}/{max_passes} did not close all findings: {reason}"
                );
            }
            Err(err) => return Err(err),
        }
    }

    bail!("audit findings are still not fully closed after {max_passes} resolve pass(es)")
}

enum ResolvePassOutcome {
    Verified,
    RetryNeeded { reason: String },
}

async fn resolve_audit_findings_pass(
    repo_root: &Path,
    output_dir: &Path,
    args: AuditArgs,
    target_branch: &str,
    resolve_pass: usize,
    max_passes: usize,
) -> Result<ResolvePassOutcome> {
    let manifest_path = output_dir.join("MANIFEST.json");
    if !manifest_path.exists() {
        bail!(
            "audit finding resolution requires an existing manifest at {}",
            manifest_path.display()
        );
    }
    fs::create_dir_all(output_dir.join("files"))
        .with_context(|| format!("failed to create {}", output_dir.join("files").display()))?;
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let findings = manifest
        .files
        .iter()
        .filter(audit_entry_requires_closure)
        .filter(|entry| repo_root.join(&entry.path).exists())
        .cloned()
        .collect::<Vec<_>>();
    let finding_paths = findings
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();

    if findings.is_empty() {
        println!("auto audit resolve findings: no existing flagged files to remediate");
        verify_audit_findings(repo_root, output_dir)?;
        return Ok(ResolvePassOutcome::Verified);
    }

    let max_lanes = args.audit_threads.clamp(1, 8).min(findings.len());
    let lanes = build_finding_resolution_lanes(findings, max_lanes);
    let run_id = format!("{}-pass-{resolve_pass:02}", crate::util::timestamp_slug());
    let run_dir = output_dir.join("finding-resolution").join(&run_id);
    let target_root = finding_resolution_target_root(repo_root, &run_id);
    let worktree_root = finding_resolution_worktree_root(repo_root, &run_id);
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("failed to create {}", run_dir.display()))?;
    fs::create_dir_all(&target_root)
        .with_context(|| format!("failed to create {}", target_root.display()))?;
    fs::create_dir_all(&worktree_root)
        .with_context(|| format!("failed to create {}", worktree_root.display()))?;

    println!("auto audit resolve findings");
    println!("manifest: {}", manifest_path.display());
    println!("pass:     {resolve_pass}/{max_passes}");
    println!("lanes:    {} (max {})", lanes.len(), max_lanes);
    println!("run dir:  {}", run_dir.display());
    println!("targets:  {}", target_root.display());
    println!("worktrees: {}", worktree_root.display());

    prune_finding_resolution_artifacts(
        repo_root,
        output_dir,
        &run_id,
        args.resolve_keep_runs,
        !args.no_resolve_target_prune,
        false,
    )?;

    let lane_assignments = lanes
        .into_iter()
        .map(|lane| {
            let lane_root = finding_resolution_lane_worktree_dir(&worktree_root, &lane);
            let lane_repo_root = lane_root.join("repo");
            let lane_target_dir = lane_root.join("cargo-target");
            reset_finding_resolution_lane_root(&lane_root)?;
            clone_finding_resolution_lane_repo(repo_root, &target_branch, &lane_repo_root)?;
            let base_commit = git_stdout(&lane_repo_root, ["rev-parse", "HEAD"])?
                .trim()
                .to_string();
            Ok(FindingResolutionLaneAssignment {
                lane,
                lane_root,
                lane_repo_root,
                lane_target_dir,
                base_commit,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut lane_statuses = lane_assignments
        .iter()
        .map(|assignment| {
            let lane = &assignment.lane;
            let lane_dir = finding_resolution_lane_dir(&run_dir, lane);
            FindingResolutionLaneStatus {
                id: lane.id,
                name: lane.name.clone(),
                finding_count: lane.entries.len(),
                state: "running".to_string(),
                repo_dir: assignment.lane_repo_root.display().to_string(),
                target_dir: assignment.lane_target_dir.display().to_string(),
                prompt_path: lane_dir.join("prompt.md").display().to_string(),
                response_path: lane_dir.join("response.log").display().to_string(),
                landed_commit: None,
                error: None,
            }
        })
        .collect::<Vec<_>>();
    write_finding_resolution_status(
        output_dir,
        &run_id,
        "running",
        &run_dir,
        &worktree_root,
        &target_root,
        &lane_statuses,
    )?;

    let mut join_set = JoinSet::new();
    for assignment in lane_assignments {
        let output_dir = output_dir.to_path_buf();
        let run_dir = run_dir.clone();
        let args = args.clone();
        let lane_id = assignment.lane.id;
        join_set.spawn(async move {
            run_finding_resolution_lane(assignment, output_dir, run_dir, args)
                .await
                .map_err(|err| (lane_id, err.to_string()))
        });
    }

    let mut failures = Vec::new();
    while let Some(result) = join_set.join_next().await {
        let outcome = result.context("finding resolution lane panicked")?;
        match outcome {
            Ok(outcome) => {
                if let Some(status) = lane_statuses
                    .iter_mut()
                    .find(|status| status.id == outcome.lane_id)
                {
                    status.state = "landing".to_string();
                    status.error = None;
                }
                write_finding_resolution_status(
                    output_dir,
                    &run_id,
                    if failures.is_empty() {
                        "running"
                    } else {
                        "failed"
                    },
                    &run_dir,
                    &worktree_root,
                    &target_root,
                    &lane_statuses,
                )?;
                let landed_commit =
                    land_finding_resolution_lane_result(repo_root, &target_branch, &outcome)
                        .with_context(|| format!("failed landing lane {}", outcome.lane_id + 1))?;
                if let Some(status) = lane_statuses
                    .iter_mut()
                    .find(|status| status.id == outcome.lane_id)
                {
                    status.state = "landed".to_string();
                    status.landed_commit = Some(landed_commit);
                    status.error = None;
                }
                prune_completed_finding_resolution_lane(&outcome.lane_repo_root)?;
            }
            Err((lane_id, error)) => {
                if let Some(status) = lane_statuses.iter_mut().find(|status| status.id == lane_id) {
                    status.state = "failed".to_string();
                    status.error = Some(error.clone());
                }
                failures.push(format!("lane {} failed: {error}", lane_id + 1));
            }
        }
        write_finding_resolution_status(
            output_dir,
            &run_id,
            if failures.is_empty() {
                "running"
            } else {
                "failed"
            },
            &run_dir,
            &worktree_root,
            &target_root,
            &lane_statuses,
        )?;
    }

    if !failures.is_empty() {
        bail!("{}", failures.join("\n"));
    }

    write_finding_resolution_status(
        output_dir,
        &run_id,
        "re-auditing-drifted",
        &run_dir,
        &worktree_root,
        &target_root,
        &lane_statuses,
    )?;
    let reaudit_status = rerun_only_drifted_audit(repo_root, output_dir, &args, &finding_paths)
        .await
        .context("failed to launch drifted finding re-audit")?;
    if let ReauditOutcome::NoGo { status } = &reaudit_status {
        eprintln!(
            "auto audit resolve findings: only-drifted re-audit exited with {status}; verifying findings and continuing the resolve loop if needed"
        );
    }
    match verify_audit_findings(repo_root, output_dir) {
        Ok(()) => {
            write_finding_resolution_status(
                output_dir,
                &run_id,
                "verified",
                &run_dir,
                &worktree_root,
                &target_root,
                &lane_statuses,
            )?;
            prune_finding_resolution_artifacts(
                repo_root,
                output_dir,
                &run_id,
                args.resolve_keep_runs,
                !args.no_resolve_target_prune,
                true,
            )?;
            Ok(ResolvePassOutcome::Verified)
        }
        Err(err) => {
            write_finding_resolution_status(
                output_dir,
                &run_id,
                "verification-no-go",
                &run_dir,
                &worktree_root,
                &target_root,
                &lane_statuses,
            )?;
            Ok(ResolvePassOutcome::RetryNeeded {
                reason: err.to_string(),
            })
        }
    }
}

enum ReauditOutcome {
    Success,
    NoGo { status: String },
}

fn build_finding_resolution_lanes(
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

fn finding_resolution_lane_dir(run_dir: &Path, lane: &FindingResolutionLane) -> PathBuf {
    run_dir.join(format!("{:02}-{}", lane.id + 1, slugify(&lane.name)))
}

fn finding_resolution_target_root(repo_root: &Path, run_id: &str) -> PathBuf {
    repo_root
        .join(".auto")
        .join("audit-resolve-targets")
        .join(run_id)
}

fn finding_resolution_worktree_root(repo_root: &Path, run_id: &str) -> PathBuf {
    repo_root
        .join(".auto")
        .join("audit-resolve-worktrees")
        .join(run_id)
}

fn finding_resolution_lane_worktree_dir(
    worktree_root: &Path,
    lane: &FindingResolutionLane,
) -> PathBuf {
    worktree_root.join(format!("{:02}-{}", lane.id + 1, slugify(&lane.name)))
}

fn prepare_finding_resolution_lane_env(
    repo_root: &Path,
    lane_target_dir: &Path,
    validation_threads: usize,
) -> Result<Vec<(String, String)>> {
    fs::create_dir_all(lane_target_dir)
        .with_context(|| format!("failed to create {}", lane_target_dir.display()))?;
    let bin_dir = lane_target_dir.join("autodev-bin");
    fs::create_dir_all(&bin_dir)
        .with_context(|| format!("failed to create {}", bin_dir.display()))?;
    let real_cargo = resolve_real_cargo()?;
    let wrapper = bin_dir.join("cargo");
    atomic_write(&wrapper, cargo_guard_wrapper_script().as_bytes())
        .with_context(|| format!("failed to write {}", wrapper.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&wrapper)
            .with_context(|| format!("failed to stat {}", wrapper.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions)
            .with_context(|| format!("failed to chmod {}", wrapper.display()))?;
    }
    let current_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    let path = format!("{}:{current_path}", bin_dir.display());
    Ok(vec![
        (
            "CARGO_TARGET_DIR".to_string(),
            lane_target_dir.display().to_string(),
        ),
        (
            "CARGO_BUILD_JOBS".to_string(),
            validation_threads.max(1).to_string(),
        ),
        ("AUTODEV_REAL_CARGO".to_string(), real_cargo),
        ("PATH".to_string(), path),
        (
            "AUTO_AUDIT_RESOLVE_VALIDATION_THREADS".to_string(),
            validation_threads.max(1).to_string(),
        ),
        (
            "AUTO_AUDIT_REPO_ROOT".to_string(),
            repo_root.display().to_string(),
        ),
    ])
}

fn resolve_real_cargo() -> Result<String> {
    if let Ok(path) = std::env::var("AUTODEV_REAL_CARGO") {
        if !path.trim().is_empty() {
            return Ok(path);
        }
    }
    let output = Command::new("sh")
        .arg("-lc")
        .arg("command -v cargo")
        .output()
        .context("failed to resolve cargo executable")?;
    if !output.status.success() {
        bail!(
            "failed to resolve cargo executable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        bail!("failed to resolve cargo executable: command -v cargo returned empty output");
    }
    Ok(path)
}

fn resolve_auto_executable() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("AUTODEV_AUTO_BIN") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
    }
    if let Ok(current) = std::env::current_exe() {
        if current.exists() {
            return Ok(current);
        }
        let current_text = current.to_string_lossy();
        if let Some(stripped) = current_text.strip_suffix(" (deleted)") {
            let stripped = PathBuf::from(stripped);
            if stripped.exists() {
                return Ok(stripped);
            }
        }
    }
    let output = Command::new("sh")
        .arg("-lc")
        .arg("command -v auto")
        .output()
        .context("failed to resolve auto executable from PATH")?;
    if output.status.success() {
        let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
        if path.exists() {
            return Ok(path);
        }
    }
    bail!(
        "failed to resolve auto executable; set AUTODEV_AUTO_BIN to the installed auto binary path"
    )
}

fn cargo_guard_wrapper_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail
real="${AUTODEV_REAL_CARGO:?AUTODEV_REAL_CARGO is required}"

if [[ "${1:-}" == "test" ]]; then
  filters=0
  skip_next=0
  after_dashdash=0
  for arg in "${@:2}"; do
    if [[ "$after_dashdash" == "1" ]]; then
      continue
    fi
    if [[ "$skip_next" == "1" ]]; then
      skip_next=0
      continue
    fi
    case "$arg" in
      --)
        after_dashdash=1
        continue
        ;;
      -p|--package|--manifest-path|--target|--target-dir|--bin|--test|--bench|--example|--features|--color|--message-format|--jobs|--profile)
        skip_next=1
        continue
        ;;
      --package=*|--manifest-path=*|--target=*|--target-dir=*|--bin=*|--test=*|--bench=*|--example=*|--features=*|--color=*|--message-format=*|--jobs=*|--profile=*)
        continue
        ;;
      --lib|--bins|--tests|--benches|--examples|--all-targets|--all-features|--no-default-features|--workspace|--all|--locked|--offline|--frozen|--release|--no-fail-fast|--doc|--quiet|--verbose|-q|-v)
        continue
        ;;
      -*)
        continue
        ;;
      *)
        filters=$((filters + 1))
        ;;
    esac
  done
  if (( filters > 1 )); then
    echo "AUTO_AUDIT_CARGO_FILTER_ERROR: cargo test accepts only one test filter. Split exact tests into separate commands or use one common module-level filter." >&2
    exit 64
  fi
fi

exec "$real" "$@"
"#
}

fn preflight_finding_resolution_roots(repo_root: &Path, args: &AuditArgs) -> Result<()> {
    if args.allow_missing_resolve_roots {
        return Ok(());
    }
    let tracked_candidates = [
        "AGENTS.md",
        "IMPLEMENTATION_PLAN.md",
        "REVIEW.md",
        "audit/DOCTRINE.md",
        "AUTONOMY-GDD.md",
    ];
    let agents_text = fs::read_to_string(repo_root.join("AGENTS.md")).unwrap_or_default();
    let mut missing = Vec::new();
    for path in tracked_candidates {
        if !tracked_path_exists(repo_root, path)? {
            continue;
        }
        if repo_root.join(path).exists() {
            continue;
        }
        if path == "AUTONOMY-GDD.md" && agents_text.contains("RSOCIETY-GDD.md") {
            eprintln!(
                "auto audit resolve findings: tracked AUTONOMY-GDD.md is missing, but AGENTS.md names RSOCIETY-GDD.md as canonical successor"
            );
            continue;
        }
        missing.push(path.to_string());
    }
    if !missing.is_empty() {
        bail!(
            "auto audit --resolve-findings refuses to run with missing source-of-truth file(s): {}. Restore them, update AGENTS.md to name the successor doctrine, or pass --allow-missing-resolve-roots.",
            missing.join(", ")
        );
    }
    Ok(())
}

fn tracked_path_exists(repo_root: &Path, path: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(path)
        .output()
        .with_context(|| format!("failed to check whether {path} is tracked"))?;
    Ok(output.status.success())
}

fn write_finding_resolution_status(
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
    atomic_write(&output_dir.join("FINDING-RESOLVE-STATUS.json"), &json)?;

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
    atomic_write(
        &output_dir.join("FINDING-RESOLVE-STATUS.md"),
        markdown.as_bytes(),
    )
}

fn chrono_like_now() -> String {
    crate::util::timestamp_slug()
}

fn prune_finding_resolution_artifacts(
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
        if keep_by_count || (is_current && !include_current) {
            continue;
        }
        fs::remove_dir_all(&path).with_context(|| format!("failed to prune {}", path.display()))?;
    }
    Ok(())
}

fn reset_finding_resolution_lane_root(lane_root: &Path) -> Result<()> {
    if lane_root.exists() {
        fs::remove_dir_all(lane_root)
            .with_context(|| format!("failed to reset {}", lane_root.display()))?;
    }
    fs::create_dir_all(lane_root)
        .with_context(|| format!("failed to create {}", lane_root.display()))
}

fn clone_finding_resolution_lane_repo(
    repo_root: &Path,
    target_branch: &str,
    lane_repo_root: &Path,
) -> Result<()> {
    let parent = lane_repo_root
        .parent()
        .with_context(|| format!("{} has no parent", lane_repo_root.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let output = Command::new("git")
        .args(["clone", "--quiet", "--local"])
        .arg("--branch")
        .arg(target_branch)
        .arg("--single-branch")
        .arg(repo_root)
        .arg(lane_repo_root)
        .output()
        .with_context(|| {
            format!(
                "failed to clone lane repo into {}",
                lane_repo_root.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "git clone failed for finding resolution lane {}: {}",
            lane_repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let remotes = git_stdout(lane_repo_root, ["remote"]).unwrap_or_default();
    if remotes.lines().any(|remote| remote.trim() == "origin") {
        run_git(lane_repo_root, ["remote", "rename", "origin", "canonical"])?;
    }
    run_git(lane_repo_root, ["checkout", "--quiet", "--detach", "HEAD"])
}

fn commit_finding_resolution_lane_changes(
    lane_repo_root: &Path,
    lane: &FindingResolutionLane,
    _base_commit: &str,
) -> Result<()> {
    let status = git_stdout(lane_repo_root, ["status", "--short"])?;
    if status.trim().is_empty() {
        return Ok(());
    }
    stage_finding_resolution_lane_changes(lane_repo_root)?;
    if !finding_resolution_lane_has_staged_changes(lane_repo_root)? {
        return Ok(());
    }
    let message = format!(
        "audit: resolve findings lane {:02} {}",
        lane.id + 1,
        lane.name
    );
    run_git(lane_repo_root, ["commit", "-m", &message])
}

fn stage_finding_resolution_lane_changes(lane_repo_root: &Path) -> Result<()> {
    let excludes = finding_resolution_commit_exclude_pathspecs();
    let mut args = vec!["add", "-A", "--", "."];
    args.extend(excludes.iter().map(String::as_str));
    run_git(lane_repo_root, args)
}

fn finding_resolution_commit_exclude_pathspecs() -> Vec<String> {
    [
        ":(exclude).auto",
        ":(exclude).auto/**",
        ":(exclude)audit/AUDIT-PROGRESS.md",
        ":(exclude)audit/FINDING-RESOLVE-STATUS.json",
        ":(exclude)audit/FINDING-RESOLVE-STATUS.md",
        ":(exclude)audit/FINDING-VERIFY.json",
        ":(exclude)audit/FINDING-VERIFY.md",
        ":(exclude)audit/MANIFEST.json",
        ":(exclude)audit/live.log",
        ":(exclude)audit/files/**",
        ":(exclude)audit/finding-resolution/**",
        ":(exclude)audit/logs/**",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn finding_resolution_lane_has_staged_changes(lane_repo_root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(lane_repo_root)
        .args(["diff", "--cached", "--quiet", "--exit-code"])
        .output()
        .with_context(|| format!("failed to inspect {}", lane_repo_root.display()))?;
    if output.status.success() {
        return Ok(false);
    }
    if output.status.code() == Some(1) {
        return Ok(true);
    }
    bail!(
        "git diff --cached failed in {}: {}",
        lane_repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn land_finding_resolution_lane_result(
    repo_root: &Path,
    target_branch: &str,
    outcome: &FindingResolutionLaneOutcome,
) -> Result<String> {
    let lane_head = git_stdout(&outcome.lane_repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    fetch_finding_resolution_lane_commit(repo_root, &outcome.lane_repo_root, &lane_head)?;
    if lane_head != outcome.base_commit && !git_ref_is_ancestor(repo_root, "FETCH_HEAD", "HEAD")? {
        let range_base = git_stdout(repo_root, ["merge-base", "HEAD", "FETCH_HEAD"])
            .map(|value| value.trim().to_string())
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| outcome.base_commit.clone());
        cherry_pick_finding_resolution_lane_range(repo_root, &range_base, "FETCH_HEAD")?;
    }
    if push_branch_with_remote_sync(repo_root, target_branch)? {
        println!("remote sync: rebased onto origin/{target_branch}");
    }
    Ok(git_stdout(repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string())
}

fn fetch_finding_resolution_lane_commit(
    repo_root: &Path,
    lane_repo_root: &Path,
    lane_head: &str,
) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("fetch")
        .arg(lane_repo_root)
        .arg(lane_head)
        .output()
        .with_context(|| {
            format!(
                "failed to fetch finding resolution lane commit {lane_head} from {}",
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
        .with_context(|| format!("failed to inspect git ancestry in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    bail!(
        "git merge-base failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn cherry_pick_finding_resolution_lane_range(
    repo_root: &Path,
    range_base: &str,
    head_ref: &str,
) -> Result<()> {
    let range = format!("{range_base}..{head_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cherry-pick", "--empty=drop"])
        .arg(&range)
        .output()
        .with_context(|| format!("failed to cherry-pick {range} into {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cherry-pick", "--abort"])
        .output();
    bail!(
        "git cherry-pick failed in {} for {range}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn prune_completed_finding_resolution_lane(lane_repo_root: &Path) -> Result<()> {
    let lane_root = lane_repo_root.parent().unwrap_or(lane_repo_root);
    if lane_root.exists() {
        fs::remove_dir_all(lane_root)
            .with_context(|| format!("failed to prune {}", lane_root.display()))?;
    }
    Ok(())
}

async fn run_finding_resolution_lane(
    assignment: FindingResolutionLaneAssignment,
    output_dir: PathBuf,
    run_dir: PathBuf,
    args: AuditArgs,
) -> Result<FindingResolutionLaneOutcome> {
    let FindingResolutionLaneAssignment {
        lane,
        lane_root: _lane_root,
        lane_repo_root,
        lane_target_dir,
        base_commit,
    } = assignment;
    let lane_env = prepare_finding_resolution_lane_env(
        &lane_repo_root,
        &lane_target_dir,
        args.resolve_validation_threads,
    )?;
    let prompt = build_finding_resolution_prompt(
        &lane_repo_root,
        &output_dir,
        &lane,
        &lane_target_dir,
        &args,
    )?;
    let lane_dir = finding_resolution_lane_dir(&run_dir, &lane);
    fs::create_dir_all(&lane_dir)
        .with_context(|| format!("failed to create {}", lane_dir.display()))?;
    atomic_write(&lane_dir.join("prompt.md"), prompt.as_bytes())?;
    println!(
        "[resolve:{}/{}] {} finding(s) across {}",
        lane.id + 1,
        lane.entries.len(),
        lane.entries.len(),
        lane.name
    );
    let label = format!("audit-resolve:{}/{}", lane.id + 1, lane.name);
    let response = run_auditor_labeled_with_env_and_timeout(
        &lane_repo_root,
        &prompt,
        &args,
        Some(&label),
        &lane_env,
        FINDING_RESOLUTION_TIMEOUT_SECS,
    )
    .await?;
    atomic_write(&lane_dir.join("response.log"), response.as_bytes())?;
    commit_finding_resolution_lane_changes(&lane_repo_root, &lane, &base_commit)?;
    Ok(FindingResolutionLaneOutcome {
        lane_id: lane.id,
        lane_repo_root,
        base_commit,
    })
}

fn build_finding_resolution_prompt(
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
         - Keep edits scoped to this lane's paths and direct dependencies.\n\
         - Do not mark implementation-plan rows complete and do not edit audit/MANIFEST.json.\n\
         - Run targeted validation when practical and summarize what changed.\n\
         - Use the provided lane-local CARGO_TARGET_DIR. Do not override it.\n\
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
        "- The lane PATH contains an `auto audit` Cargo guard wrapper that rejects multi-filter `cargo test` invocations before they waste compile time.\n\n",
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

async fn rerun_only_drifted_audit(
    repo_root: &Path,
    output_dir: &Path,
    args: &AuditArgs,
    focus_paths: &[String],
) -> Result<ReauditOutcome> {
    let exe = resolve_auto_executable()?;
    let mut command = TokioCommand::new(exe);
    command
        .arg("audit")
        .arg("--resume-mode")
        .arg("only-drifted")
        .arg("--audit-threads")
        .arg(args.audit_threads.clamp(1, 8).to_string())
        .arg("--output-dir")
        .arg(output_dir)
        .arg("--model")
        .arg(&args.model)
        .arg("--reasoning-effort")
        .arg(&args.reasoning_effort)
        .arg("--escalation-model")
        .arg(&args.escalation_model)
        .arg("--escalation-effort")
        .arg(&args.escalation_effort)
        .arg("--codex-bin")
        .arg(&args.codex_bin)
        .arg("--kimi-bin")
        .arg(&args.kimi_bin)
        .arg("--pi-bin")
        .arg(&args.pi_bin)
        .arg("--use-kimi-cli")
        .arg(if args.use_kimi_cli { "true" } else { "false" })
        .current_dir(repo_root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(path) = args.rubric_prompt.as_deref() {
        command.arg("--rubric-prompt").arg(path);
    }
    command.arg("--doctrine-prompt").arg(&args.doctrine_prompt);
    if args.report_only {
        command.arg("--report-only");
    }
    if let Some(branch) = args.branch.as_deref() {
        command.arg("--branch").arg(branch);
    }
    let include_paths = if focus_paths.is_empty() {
        &args.include_paths
    } else {
        focus_paths
    };
    for path in include_paths {
        command.arg("--paths").arg(path);
    }
    for path in &args.exclude_paths {
        command.arg("--exclude").arg(path);
    }

    let status = command
        .status()
        .await
        .context("failed to launch only-drifted audit subprocess")?;
    if !status.success() {
        return Ok(ReauditOutcome::NoGo {
            status: status.to_string(),
        });
    }
    Ok(ReauditOutcome::Success)
}

fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn enumerate_tracked_files(
    repo_root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<Vec<String>> {
    let listing = git_stdout(repo_root, ["ls-files", "-z"])?;
    let mut files: Vec<String> = listing
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    files.retain(|path| {
        matches_any(path, include) && !matches_any(path, exclude) && repo_root.join(path).exists()
    });
    files.sort();
    files.dedup();
    Ok(files)
}

fn matches_any(path: &str, globs: &[String]) -> bool {
    globs.iter().any(|pat| glob_match(pat, path))
}

/// Minimal glob matcher supporting `*`, `**`, and literal components. Good
/// enough for path filters without pulling in the `glob` crate.
fn glob_match(pattern: &str, path: &str) -> bool {
    glob_match_recursive(pattern.as_bytes(), path.as_bytes())
}

fn glob_match_recursive(pattern: &[u8], path: &[u8]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    // `**` matches any (possibly empty) sequence of characters including `/`.
    if pattern.starts_with(b"**/") {
        let rest = &pattern[3..];
        for i in 0..=path.len() {
            if glob_match_recursive(rest, &path[i..]) {
                return true;
            }
            if path.get(i) == Some(&b'/') {
                // continue scanning; `**/` happy to skip across `/`
            }
        }
        return false;
    }
    if pattern == b"**" {
        return true;
    }
    match pattern[0] {
        b'*' => {
            let rest = &pattern[1..];
            for i in 0..=path.len() {
                // `*` does not match `/` by POSIX glob convention
                if i > 0 && path[i - 1] == b'/' {
                    return glob_match_recursive(rest, &path[i - 1..]);
                }
                if glob_match_recursive(rest, &path[i..]) {
                    return true;
                }
            }
            false
        }
        _ => {
            if path.is_empty() {
                return false;
            }
            if pattern[0] != path[0] {
                return false;
            }
            glob_match_recursive(&pattern[1..], &path[1..])
        }
    }
}

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

fn write_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let body = serde_json::to_string_pretty(manifest)?;
    atomic_write(path, body.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

#[allow(clippy::too_many_arguments)]
fn mark_entry(
    manifest: &mut Manifest,
    path: &str,
    status: EntryStatus,
    content_hash: Option<String>,
    audited_doctrine_hash: Option<String>,
    audited_rubric_hash: Option<String>,
    verdict: Option<String>,
    commit: Option<String>,
) {
    if let Some(entry) = manifest.files.iter_mut().find(|e| e.path == path) {
        entry.status = status;
        if content_hash.is_some() {
            entry.content_hash = content_hash;
        }
        if audited_doctrine_hash.is_some() {
            entry.audited_doctrine_hash = audited_doctrine_hash;
        }
        if audited_rubric_hash.is_some() {
            entry.audited_rubric_hash = audited_rubric_hash;
        }
        if verdict.is_some() {
            entry.verdict = verdict;
        }
        if commit.is_some() {
            entry.commit = commit;
        }
        if matches!(status, EntryStatus::Audited | EntryStatus::ApplyFailed) {
            entry.audited_at = Some(now_iso8601());
        }
    }
}

fn file_artifact_dir(output_dir: &Path, rel_path: &str) -> PathBuf {
    let hash = sha256_hex(rel_path.as_bytes());
    output_dir.join("files").join(&hash[..16])
}

fn build_file_prompt(
    repo_root: &Path,
    abs_path: &Path,
    doctrine: &str,
    rubric: &str,
    output_dir: &Path,
    rel_path: &str,
) -> Result<String> {
    let content = fs::read_to_string(abs_path).with_context(|| {
        format!(
            "failed to read {} (binary file? pass --exclude to skip)",
            abs_path.display()
        )
    })?;
    let file_dir = file_artifact_dir(output_dir, rel_path);
    let file_dir_rel = file_dir
        .strip_prefix(repo_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| file_dir.display().to_string());
    Ok(format!(
        r#"{rubric}

---

# Doctrine (operator-authored)

{doctrine}

---

# File under audit

Path: `{rel_path}`

Artifact directory for your outputs: `{file_dir_rel}`

```
{content}
```
"#,
        rubric = rubric,
        doctrine = doctrine,
        rel_path = rel_path,
        file_dir_rel = file_dir_rel,
        content = content,
    ))
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

/// Apply the auditor's verdict: patch the file, append WORKLIST / retire
/// entries, commit per-file. Returns the new manifest status + the commit
/// SHA (if any).
#[allow(clippy::too_many_arguments)]
fn apply_verdict(
    repo_root: &Path,
    output_dir: &Path,
    branch: &str,
    rel_path: &str,
    file_dir: &Path,
    verdict: &FileVerdict,
    report_only: bool,
) -> Result<(EntryStatus, Option<String>)> {
    match verdict.verdict.as_str() {
        "CLEAN" => Ok((EntryStatus::Audited, None)),
        "DRIFT-SMALL" | "SLOP" => {
            if report_only {
                return Ok((EntryStatus::Audited, None));
            }
            let patch = file_dir.join("patch.diff");
            if !patch.exists() {
                eprintln!(
                    "verdict {} for {} but no patch.diff; downgrading to DRIFT-LARGE + worklist",
                    verdict.verdict, rel_path
                );
                return record_worklist_entry(
                    repo_root,
                    output_dir,
                    branch,
                    rel_path,
                    file_dir,
                    "DRIFT-LARGE",
                    "auditor emitted DRIFT-SMALL / SLOP without a patch.diff; promoted to worklist",
                );
            }
            match apply_patch(repo_root, &patch) {
                Ok(_) => {
                    let message = format!("audit: {} {}", verdict.verdict, rel_path);
                    let commit =
                        commit_scoped(repo_root, branch, &message, &[rel_path.to_string()])?;
                    Ok((EntryStatus::Audited, commit))
                }
                Err(err) => {
                    eprintln!("apply failed for {}: {err}", rel_path);
                    record_worklist_entry(
                        repo_root,
                        output_dir,
                        branch,
                        rel_path,
                        file_dir,
                        "DRIFT-LARGE",
                        &format!("patch apply failed, promoted to worklist: {err}"),
                    )
                    .map(|(status, commit)| {
                        if matches!(status, EntryStatus::Audited) {
                            (EntryStatus::ApplyFailed, commit)
                        } else {
                            (status, commit)
                        }
                    })
                }
            }
        }
        "DRIFT-LARGE" | "REFACTOR" => {
            if report_only {
                return Ok((EntryStatus::Audited, None));
            }
            record_worklist_entry(
                repo_root,
                output_dir,
                branch,
                rel_path,
                file_dir,
                &verdict.verdict,
                &verdict.rationale,
            )
        }
        "RETIRE" => {
            if report_only {
                return Ok((EntryStatus::Audited, None));
            }
            append_retire_candidate(
                repo_root,
                output_dir,
                branch,
                rel_path,
                file_dir,
                &verdict.rationale,
            )
        }
        other => {
            eprintln!(
                "unknown verdict `{other}` for {rel_path}; leaving status pending for operator review"
            );
            Ok((EntryStatus::Pending, None))
        }
    }
}

fn apply_patch(repo_root: &Path, patch: &Path) -> Result<()> {
    let check = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["apply", "--check"])
        .arg(patch)
        .output()
        .with_context(|| format!("failed to `git apply --check` on {}", patch.display()))?;
    if !check.status.success() {
        bail!(
            "git apply --check failed: {}",
            String::from_utf8_lossy(&check.stderr).trim()
        );
    }
    let apply = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("apply")
        .arg(patch)
        .output()
        .with_context(|| format!("failed to `git apply` {}", patch.display()))?;
    if !apply.status.success() {
        bail!(
            "git apply failed: {}",
            String::from_utf8_lossy(&apply.stderr).trim()
        );
    }
    Ok(())
}

fn commit_scoped(
    repo_root: &Path,
    branch: &str,
    message: &str,
    pathspecs: &[String],
) -> Result<Option<String>> {
    let _ = branch;
    if pathspecs.is_empty() {
        bail!("audit commit requires at least one scoped pathspec");
    }

    let literal_pathspecs = pathspecs
        .iter()
        .map(|pathspec| literal_git_pathspec(pathspec))
        .collect::<Vec<_>>();

    let mut add_args = vec!["add", "--"];
    add_args.extend(literal_pathspecs.iter().map(String::as_str));
    run_git(repo_root, add_args)?;

    let mut status_args = vec!["status", "--porcelain", "--"];
    status_args.extend(literal_pathspecs.iter().map(String::as_str));
    let status = git_stdout(repo_root, status_args)?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    let mut commit_args = vec!["commit", "-m", message, "--"];
    commit_args.extend(literal_pathspecs.iter().map(String::as_str));
    run_git(repo_root, commit_args)?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    Ok(Some(commit))
}

fn literal_git_pathspec(pathspec: &str) -> String {
    format!(":(literal){pathspec}")
}

fn repo_relative_pathspec(repo_root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(repo_root).with_context(|| {
        format!(
            "audit commit path {} is outside repo {}",
            path.display(),
            repo_root.display()
        )
    })?;
    let pathspec = relative.to_string_lossy().replace('\\', "/");
    if pathspec.is_empty() {
        bail!("audit commit path {} resolved to repo root", path.display());
    }
    Ok(pathspec)
}

fn record_worklist_entry(
    repo_root: &Path,
    _output_dir: &Path,
    branch: &str,
    rel_path: &str,
    file_dir: &Path,
    verdict_tag: &str,
    rationale: &str,
) -> Result<(EntryStatus, Option<String>)> {
    let worklist_path = repo_root.join("WORKLIST.md");
    let stage_path = file_dir.join("worklist-entry.md");
    let staged = fs::read_to_string(&stage_path).ok();
    let entry =
        staged.unwrap_or_else(|| format!("- `{}` audit {verdict_tag}: {}", rel_path, rationale));
    let mut current = if worklist_path.exists() {
        fs::read_to_string(&worklist_path)
            .with_context(|| format!("failed to read {}", worklist_path.display()))?
    } else {
        "# WORKLIST\n\n".to_string()
    };
    if !current.ends_with('\n') {
        current.push('\n');
    }
    current.push('\n');
    current.push_str(entry.trim_end());
    current.push('\n');
    atomic_write(&worklist_path, current.as_bytes())
        .with_context(|| format!("failed to write {}", worklist_path.display()))?;
    let message = format!("audit: {} {} (worklist)", verdict_tag, rel_path);
    let pathspecs = vec![repo_relative_pathspec(repo_root, &worklist_path)?];
    let commit = commit_scoped(repo_root, branch, &message, &pathspecs)?;
    Ok((EntryStatus::Audited, commit))
}

fn append_retire_candidate(
    repo_root: &Path,
    output_dir: &Path,
    branch: &str,
    rel_path: &str,
    file_dir: &Path,
    rationale: &str,
) -> Result<(EntryStatus, Option<String>)> {
    let retire_path = output_dir.join("RETIRE-BATCH.md");
    let mut current = if retire_path.exists() {
        fs::read_to_string(&retire_path)
            .with_context(|| format!("failed to read {}", retire_path.display()))?
    } else {
        "# RETIRE-BATCH\n\nCandidates for retirement, produced by `auto audit`. Review and run a \
         separate delete pass when ready.\n\n"
            .to_string()
    };
    let staged = fs::read_to_string(file_dir.join("retire-reason.md")).ok();
    let reason = staged.unwrap_or_else(|| rationale.to_string());
    if !current.ends_with('\n') {
        current.push('\n');
    }
    current.push('\n');
    current.push_str(&format!(
        "- [ ] `{}` — {}\n",
        rel_path,
        reason.lines().next().unwrap_or("(no reason given)")
    ));
    atomic_write(&retire_path, current.as_bytes())
        .with_context(|| format!("failed to write {}", retire_path.display()))?;
    let message = format!("audit: RETIRE candidate {}", rel_path);
    let pathspecs = vec![repo_relative_pathspec(repo_root, &retire_path)?];
    let commit = commit_scoped(repo_root, branch, &message, &pathspecs)?;
    Ok((EntryStatus::Audited, commit))
}

#[allow(clippy::too_many_arguments)]
fn write_progress_snapshot(
    output_dir: &Path,
    manifest: &Manifest,
    audited: usize,
    clean: usize,
    applied: usize,
    worklisted: usize,
    retired: usize,
    apply_failed: usize,
) -> Result<()> {
    let pending = manifest
        .files
        .iter()
        .filter(|e| matches!(e.status, EntryStatus::Pending | EntryStatus::ApplyFailed))
        .count();
    let mut by_verdict: HashMap<String, usize> = HashMap::new();
    for entry in &manifest.files {
        if let Some(v) = entry.verdict.as_deref() {
            *by_verdict.entry(v.to_string()).or_default() += 1;
        }
    }
    let mut body = String::new();
    body.push_str("# AUDIT-PROGRESS\n\n");
    body.push_str(&format!(
        "- total files tracked: {}\n",
        manifest.files.len()
    ));
    body.push_str(&format!("- pending: {pending}\n"));
    body.push_str(&format!(
        "- audited this run: {audited} ({clean} CLEAN, {applied} applied patches, \
         {worklisted} worklisted, {retired} retire candidates, {apply_failed} apply failures)\n"
    ));
    body.push_str("\n## Verdict distribution (all time)\n\n");
    let mut verdicts: Vec<_> = by_verdict.iter().collect();
    verdicts.sort();
    for (v, n) in verdicts {
        body.push_str(&format!("- {v}: {n}\n"));
    }
    atomic_write(&output_dir.join("AUDIT-PROGRESS.md"), body.as_bytes())
        .with_context(|| "failed to write AUDIT-PROGRESS.md".to_string())
}

#[allow(dead_code)]
async fn run_auditor(repo_root: &Path, prompt: &str, args: &AuditArgs) -> Result<String> {
    run_auditor_labeled(repo_root, prompt, args, None).await
}

async fn run_auditor_labeled(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    label: Option<&str>,
) -> Result<String> {
    run_auditor_labeled_with_env(repo_root, prompt, args, label, &[]).await
}

async fn run_auditor_labeled_with_env(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    label: Option<&str>,
    extra_env: &[(String, String)],
) -> Result<String> {
    run_auditor_labeled_with_env_and_timeout(
        repo_root,
        prompt,
        args,
        label,
        extra_env,
        AUDITOR_TIMEOUT_SECS,
    )
    .await
}

async fn run_auditor_labeled_with_env_and_timeout(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    label: Option<&str>,
    extra_env: &[(String, String)],
    timeout_secs: u64,
) -> Result<String> {
    let prompt = with_autodev_prompt_ethos(prompt);
    if args.use_kimi_cli && is_kimi_model(&args.model) {
        run_auditor_kimi(repo_root, &prompt, args, extra_env, timeout_secs).await
    } else if is_kimi_model(&args.model) {
        bail!("auto audit Kimi models currently require --use-kimi-cli");
    } else {
        run_auditor_codex(repo_root, &prompt, args, label, extra_env, timeout_secs).await
    }
}

fn is_kimi_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower.contains("kimi") || lower.starts_with("k2.") || lower.starts_with("k2p")
}

async fn run_auditor_codex(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    label: Option<&str>,
    extra_env: &[(String, String)],
    timeout_secs: u64,
) -> Result<String> {
    let mut command = TokioCommand::new(&args.codex_bin);
    command
        .arg("exec")
        .arg("--json")
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .arg("--skip-git-repo-check")
        .arg("--cd")
        .arg(repo_root)
        .arg("-m")
        .arg(&args.model)
        .arg("-c")
        .arg(format!(
            "model_reasoning_effort=\"{}\"",
            args.reasoning_effort
        ))
        .arg("-c")
        .arg(format!(
            "model_context_window={MAX_CODEX_MODEL_CONTEXT_WINDOW}"
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);
    for (key, value) in extra_env {
        command.env(key, value);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch Codex at {} from {}",
            args.codex_bin.display(),
            repo_root.display()
        )
    })?;
    let mut stdin = child
        .stdin
        .take()
        .context("Codex stdin should be piped for auto audit")?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .context("failed to write auto audit prompt to Codex")?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .context("Codex stdout should be piped for auto audit")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex stderr should be piped for auto audit")?;
    let label = label.map(str::to_string);
    let stdout_task =
        tokio::spawn(
            async move { capture_codex_output_prefixed(stdout, label.as_deref(), None).await },
        );
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });
    let wait_result = time::timeout(Duration::from_secs(timeout_secs), child.wait()).await;
    let timed_out = wait_result.is_err();
    if timed_out {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let stdout = stdout_task
        .await
        .context("Codex stdout capture task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Codex stderr capture task panicked")??;
    if timed_out {
        bail!("Codex audit pass timed out after {}s", timeout_secs);
    }
    let status = wait_result
        .expect("timeout already handled")
        .context("failed waiting for Codex")?;
    if !status.success() {
        bail!(
            "Codex audit failed: {}",
            if !stderr_text.trim().is_empty() {
                stderr_text.trim().to_string()
            } else {
                stdout.trim().to_string()
            }
        );
    }
    Ok(stdout)
}

async fn run_auditor_kimi(
    repo_root: &Path,
    prompt: &str,
    args: &AuditArgs,
    extra_env: &[(String, String)],
    timeout_secs: u64,
) -> Result<String> {
    let kimi_bin = resolve_kimi_bin(&args.kimi_bin);
    let model = resolve_kimi_cli_model(&args.model);
    let exec_args = kimi_exec_args(&model, &args.reasoning_effort, prompt);
    let mut command = TokioCommand::new(&kimi_bin);
    command
        .args(&exec_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch kimi-cli at {} from {}",
            kimi_bin.display(),
            repo_root.display()
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .context("kimi-cli stdout should be piped for auto audit")?;
    let stderr = child
        .stderr
        .take()
        .context("kimi-cli stderr should be piped for auto audit")?;
    let stdout_task =
        tokio::spawn(async move { capture_pi_output(stdout, "auto audit kimi-cli", 30).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });
    let wait_result = time::timeout(Duration::from_secs(timeout_secs), child.wait()).await;
    let timed_out = wait_result.is_err();
    if timed_out {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let stdout = stdout_task
        .await
        .context("kimi-cli stdout capture task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("kimi-cli stderr capture task panicked")??;
    if timed_out {
        bail!("kimi-cli audit pass timed out after {}s", timeout_secs);
    }
    let status = wait_result
        .expect("timeout already handled")
        .context("failed waiting for kimi-cli")?;
    if !status.success() {
        bail!(
            "kimi-cli audit failed: {}",
            if !stderr_text.trim().is_empty() {
                stderr_text.trim().to_string()
            } else {
                parse_kimi_error(&stdout).unwrap_or_else(|| stdout.trim().to_string())
            }
        );
    }
    if let Some(detail) = parse_kimi_error(&stdout) {
        bail!("kimi-cli audit failed: {detail}");
    }
    let mut final_text = String::new();
    for line in stdout.lines() {
        if let Some(chunk) = kimi_extract_final_text(line) {
            if !final_text.is_empty() {
                final_text.push('\n');
            }
            final_text.push_str(&chunk);
        }
    }
    if final_text.trim().is_empty() {
        Ok(stdout)
    } else {
        Ok(final_text)
    }
}

async fn read_stream<R>(stream: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = tokio::io::BufReader::new(stream);
    let mut text = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut reader, &mut text)
        .await
        .context("failed to read child stream")?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{AuditArgs, AuditResumeMode};

    use super::{
        apply_verdict, build_file_prompt, commit_scoped, enumerate_tracked_files, glob_match,
        matches_any, plan_audit_queue, run_auditor, sha256_hex, verify_audit_findings, EntryStatus,
        FileVerdict, Manifest, ManifestEntry, DEFAULT_EXCLUDE_GLOBS,
    };

    fn temp_repo_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "autodev-audit-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new(name: &str) -> Self {
            let path = temp_repo_path(name);
            fs::create_dir_all(&path).expect("failed to create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn run_git_in<'a>(repo: &Path, args: impl IntoIterator<Item = &'a str>) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to launch git");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git stdout should be utf-8")
    }

    fn last_commit_paths(repo: &Path) -> Vec<String> {
        run_git_in(repo, ["show", "--format=", "--name-only", "HEAD"])
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect()
    }

    fn init_repo(name: &str) -> TestTempDir {
        let repo = TestTempDir::new(name);
        run_git_in(repo.path(), ["init"]);
        run_git_in(repo.path(), ["config", "user.name", "autodev tests"]);
        run_git_in(repo.path(), ["config", "user.email", "autodev@example.com"]);
        fs::write(repo.path().join("README.md"), "# temp\n").expect("failed to write README");
        run_git_in(repo.path(), ["add", "README.md"]);
        run_git_in(repo.path(), ["commit", "-m", "init"]);
        run_git_in(repo.path(), ["branch", "-M", "main"]);
        repo
    }

    fn verdict(verdict: &str, rationale: &str) -> FileVerdict {
        FileVerdict {
            verdict: verdict.to_string(),
            rationale: rationale.to_string(),
            touched_paths: Vec::new(),
            escalate: false,
        }
    }

    fn audited_manifest(path: &str, content: &[u8]) -> Manifest {
        Manifest {
            started_at: "unix:0".to_string(),
            repo_head: "head".to_string(),
            doctrine_hash: "doctrine-new".to_string(),
            rubric_hash: "rubric-new".to_string(),
            files: vec![ManifestEntry {
                path: path.to_string(),
                status: EntryStatus::Audited,
                content_hash: Some(sha256_hex(content)),
                audited_doctrine_hash: Some("doctrine-new".to_string()),
                audited_rubric_hash: Some("rubric-new".to_string()),
                verdict: Some("CLEAN".to_string()),
                audited_at: Some("unix:0".to_string()),
                commit: None,
            }],
        }
    }

    #[test]
    fn glob_match_handles_double_star_prefix() {
        assert!(glob_match("**/target/**", "foo/target/bar"));
        assert!(glob_match("**/target/**", "target/bar"));
        assert!(!glob_match("**/target/**", "foo/barfoo/bar"));
    }

    #[test]
    fn glob_match_handles_extension_wildcard() {
        assert!(glob_match("**/*.rs", "src/lib.rs"));
        assert!(glob_match("**/*.rs", "foo/bar/baz.rs"));
        assert!(!glob_match("**/*.rs", "src/lib.py"));
    }

    #[test]
    fn glob_match_handles_literal_path() {
        assert!(glob_match("AGENTS.md", "AGENTS.md"));
        assert!(!glob_match("AGENTS.md", "foo/AGENTS.md"));
    }

    #[test]
    fn default_excludes_generated_audit_and_build_artifacts() {
        let exclude_globs = DEFAULT_EXCLUDE_GLOBS
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>();
        for path in [
            ".auto/audit-everything/MANIFEST.json",
            ".cache/tool/index.json",
            ".claude/worktrees/lane/README.md",
            ".config/quota-router/profiles/default.json",
            "audit/files/deadbeef/verdict.json",
            "apps/web/.next/static/chunk.js",
            "apps/web/.turbo/cache.bin",
            "apps/web/coverage/lcov.info",
            "apps/web/playwright-report/index.html",
            "apps/web/test-results/results.json",
            "reports/final-review.md",
            "tmp/generated.md",
            "venv/lib/python/site.py",
            "src/__pycache__/module.pyc",
        ] {
            assert!(matches_any(path, &exclude_globs), "{path}");
        }
    }

    #[test]
    fn sha256_hex_is_deterministic() {
        let a = sha256_hex(b"hello");
        let b = sha256_hex(b"hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn resume_reaudits_when_file_content_hash_changes() {
        let repo = TestTempDir::new("resume-content-drift");
        fs::write(repo.path().join("README.md"), "# changed\n").expect("failed to write README");
        let mut manifest = audited_manifest("README.md", b"# old\n");

        let queue = plan_audit_queue(
            &mut manifest,
            AuditResumeMode::Resume,
            repo.path(),
            "doctrine-new",
            "rubric-new",
        )
        .expect("plan should succeed");

        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].path, "README.md");
    }

    #[test]
    fn resume_skips_when_file_content_and_prompts_match() {
        let repo = TestTempDir::new("resume-no-drift");
        fs::write(repo.path().join("README.md"), "# same\n").expect("failed to write README");
        let mut manifest = audited_manifest("README.md", b"# same\n");

        let queue = plan_audit_queue(
            &mut manifest,
            AuditResumeMode::Resume,
            repo.path(),
            "doctrine-new",
            "rubric-new",
        )
        .expect("plan should succeed");

        assert!(queue.is_empty());
    }

    #[test]
    fn resume_skips_manifest_entries_for_deleted_files() {
        let repo = TestTempDir::new("resume-deleted-file");
        let mut manifest = audited_manifest("deleted.md", b"# old\n");

        let queue = plan_audit_queue(
            &mut manifest,
            AuditResumeMode::Resume,
            repo.path(),
            "doctrine-new",
            "rubric-new",
        )
        .expect("plan should tolerate deleted manifest entries");

        assert!(queue.is_empty());
    }

    #[test]
    fn verify_audit_findings_requires_reaudit_for_changed_flagged_files() {
        let repo = TestTempDir::new("verify-findings-reaudit");
        let audit_dir = repo.path().join("audit");
        fs::create_dir_all(&audit_dir).unwrap();
        fs::write(repo.path().join("a.rs"), "fn old() {}\n").unwrap();
        let manifest = Manifest {
            started_at: "2026-04-28T00:00:00Z".to_string(),
            repo_head: "HEAD".to_string(),
            doctrine_hash: "doctrine".to_string(),
            rubric_hash: "rubric".to_string(),
            files: vec![ManifestEntry {
                path: "a.rs".to_string(),
                status: EntryStatus::Audited,
                content_hash: Some(sha256_hex(b"fn old() {}\n")),
                audited_doctrine_hash: Some("doctrine".to_string()),
                audited_rubric_hash: Some("rubric".to_string()),
                verdict: Some("DRIFT-LARGE".to_string()),
                audited_at: Some("2026-04-28T00:00:00Z".to_string()),
                commit: None,
            }],
        };
        fs::write(
            audit_dir.join("MANIFEST.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(repo.path().join("a.rs"), "fn fixed() {}\n").unwrap();

        let err = verify_audit_findings(repo.path(), &audit_dir)
            .expect_err("changed flagged files must be re-audited before closure");
        assert!(err.to_string().contains("need re-audit"), "{err}");
        let report = fs::read_to_string(audit_dir.join("FINDING-VERIFY.md")).unwrap();
        assert!(report.contains("NeedsReaudit"), "{report}");
    }

    #[test]
    fn verify_audit_findings_accepts_removed_flagged_files() {
        let repo = TestTempDir::new("verify-findings-removed");
        let audit_dir = repo.path().join("audit");
        fs::create_dir_all(&audit_dir).unwrap();
        let manifest = Manifest {
            started_at: "2026-04-28T00:00:00Z".to_string(),
            repo_head: "HEAD".to_string(),
            doctrine_hash: "doctrine".to_string(),
            rubric_hash: "rubric".to_string(),
            files: vec![ManifestEntry {
                path: "retire-me.rs".to_string(),
                status: EntryStatus::Audited,
                content_hash: Some(sha256_hex(b"obsolete\n")),
                audited_doctrine_hash: Some("doctrine".to_string()),
                audited_rubric_hash: Some("rubric".to_string()),
                verdict: Some("RETIRE".to_string()),
                audited_at: Some("2026-04-28T00:00:00Z".to_string()),
                commit: None,
            }],
        };
        fs::write(
            audit_dir.join("MANIFEST.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        verify_audit_findings(repo.path(), &audit_dir).unwrap();
        let report = fs::read_to_string(audit_dir.join("FINDING-VERIFY.md")).unwrap();
        assert!(report.contains("Verdict: GO"), "{report}");
    }

    #[test]
    fn enumerate_tracked_files_skips_deleted_worktree_paths() {
        let repo = init_repo("enumerate-deleted-tracked");
        fs::write(repo.path().join("gone.md"), "# gone\n").expect("failed to write tracked file");
        run_git_in(repo.path(), ["add", "gone.md"]);
        run_git_in(repo.path(), ["commit", "-m", "track gone"]);
        fs::remove_file(repo.path().join("gone.md")).expect("failed to delete tracked file");

        let files = enumerate_tracked_files(repo.path(), &["**".to_string()], &[])
            .expect("enumeration should tolerate dirty deletes");

        assert!(files.contains(&"README.md".to_string()));
        assert!(!files.contains(&"gone.md".to_string()));
    }

    #[test]
    fn file_prompt_uses_repo_relative_artifact_dir_for_root_files() {
        let repo = TestTempDir::new("prompt-root-artifact-dir");
        fs::write(repo.path().join("AGENTS.md"), "# agents\n").expect("failed to write AGENTS");
        let prompt = build_file_prompt(
            repo.path(),
            &repo.path().join("AGENTS.md"),
            "doctrine",
            "rubric",
            &repo.path().join("audit"),
            "AGENTS.md",
        )
        .expect("prompt should build");

        assert!(
            prompt.contains("Artifact directory for your outputs: `audit/files/a54ff182c7e8acf5`")
        );
        assert!(!prompt.contains("`prompt-root-artifact-dir/audit/files/"));
    }

    #[test]
    fn file_prompt_uses_repo_relative_artifact_dir_for_nested_files() {
        let repo = TestTempDir::new("prompt-nested-artifact-dir");
        fs::create_dir_all(repo.path().join("docs")).expect("failed to create docs");
        fs::write(repo.path().join("docs/README.md"), "# docs\n")
            .expect("failed to write docs README");
        let prompt = build_file_prompt(
            repo.path(),
            &repo.path().join("docs/README.md"),
            "doctrine",
            "rubric",
            &repo.path().join("audit"),
            "docs/README.md",
        )
        .expect("prompt should build");

        assert!(
            prompt.contains("Artifact directory for your outputs: `audit/files/0b5ca119d2be595a`")
        );
        assert!(!prompt.contains("`README.md/audit/files/"));
    }

    #[test]
    fn file_prompt_keeps_external_output_dir_absolute() {
        let repo = TestTempDir::new("prompt-external-artifact-dir");
        fs::write(repo.path().join("AGENTS.md"), "# agents\n").expect("failed to write AGENTS");
        let output_dir = std::env::temp_dir().join("autodev-audit-external-output");
        let prompt = build_file_prompt(
            repo.path(),
            &repo.path().join("AGENTS.md"),
            "doctrine",
            "rubric",
            &output_dir,
            "AGENTS.md",
        )
        .expect("prompt should build");

        assert!(prompt.contains(&format!(
            "Artifact directory for your outputs: `{}`",
            output_dir.join("files/a54ff182c7e8acf5").display()
        )));
    }

    #[test]
    fn apply_verdict_clean_returns_audited() {
        let repo = init_repo("apply-verdict-clean");
        let output_dir = repo.path().join("audit");
        let file_dir = output_dir.join("files").join("clean");
        fs::create_dir_all(&file_dir).expect("failed to create file dir");
        let head_before = run_git_in(repo.path(), ["rev-parse", "HEAD"]);

        let (status, commit) = apply_verdict(
            repo.path(),
            &output_dir,
            "main",
            "README.md",
            &file_dir,
            &verdict("CLEAN", "already matches doctrine"),
            false,
        )
        .expect("clean verdict should succeed");

        assert_eq!(status, EntryStatus::Audited);
        assert_eq!(commit, None);
        assert_eq!(run_git_in(repo.path(), ["rev-parse", "HEAD"]), head_before);
        assert_eq!(run_git_in(repo.path(), ["status", "--short"]), "");
        assert!(!repo.path().join("WORKLIST.md").exists());
    }

    #[test]
    fn apply_verdict_unknown_leaves_pending() {
        let repo = init_repo("apply-verdict-unknown");
        let output_dir = repo.path().join("audit");
        let file_dir = output_dir.join("files").join("unknown");
        fs::create_dir_all(&file_dir).expect("failed to create file dir");
        let head_before = run_git_in(repo.path(), ["rev-parse", "HEAD"]);

        let (status, commit) = apply_verdict(
            repo.path(),
            &output_dir,
            "main",
            "README.md",
            &file_dir,
            &verdict("MYSTERY", "operator should review this verdict manually"),
            false,
        )
        .expect("unknown verdict branch should return pending");

        assert_eq!(status, EntryStatus::Pending);
        assert_eq!(commit, None);
        assert_eq!(run_git_in(repo.path(), ["rev-parse", "HEAD"]), head_before);
        assert_eq!(run_git_in(repo.path(), ["status", "--short"]), "");
        assert!(!repo.path().join("WORKLIST.md").exists());
    }

    #[test]
    fn apply_verdict_drift_small_without_patch_promotes_to_worklist() {
        let repo = init_repo("apply-verdict-worklist");
        let output_dir = repo.path().join("audit");
        let file_dir = output_dir.join("files").join("drift-small");
        fs::create_dir_all(&file_dir).expect("failed to create file dir");
        let head_before = run_git_in(repo.path(), ["rev-parse", "HEAD"]);

        let (status, commit) = apply_verdict(
            repo.path(),
            &output_dir,
            "main",
            "README.md",
            &file_dir,
            &verdict("DRIFT-SMALL", "small patch should have been emitted"),
            false,
        )
        .expect("missing patch should downgrade to worklist");

        let head_after = run_git_in(repo.path(), ["rev-parse", "HEAD"]);
        let worklist =
            fs::read_to_string(repo.path().join("WORKLIST.md")).expect("failed to read WORKLIST");

        assert_eq!(status, EntryStatus::Audited);
        assert_eq!(commit.as_deref(), Some(head_after.trim()));
        assert_ne!(head_before, head_after);
        assert!(worklist.contains("README.md"));
        assert!(worklist.contains("audit DRIFT-LARGE"));
        assert!(worklist.contains(
            "auditor emitted DRIFT-SMALL / SLOP without a patch.diff; promoted to worklist"
        ));
        assert_eq!(
            run_git_in(repo.path(), ["log", "--format=%s", "-1"]).trim(),
            "audit: DRIFT-LARGE README.md (worklist)"
        );
        assert_eq!(run_git_in(repo.path(), ["status", "--short"]), "");
    }

    #[test]
    fn commit_audit_outputs_uses_scoped_pathspecs() {
        let repo = init_repo("audit-scoped-patch");
        let output_dir = repo.path().join("audit");
        let file_dir = output_dir.join("files").join("drift-small");
        fs::create_dir_all(&file_dir).expect("failed to create file dir");
        fs::create_dir_all(repo.path().join(".auto").join("logs"))
            .expect("failed to create auto dir");
        fs::create_dir_all(repo.path().join("bug")).expect("failed to create bug dir");
        fs::create_dir_all(repo.path().join("nemesis")).expect("failed to create nemesis dir");
        fs::create_dir_all(repo.path().join("gen-001")).expect("failed to create gen dir");
        fs::write(
            repo.path().join(".auto").join("logs").join("run.log"),
            "runtime\n",
        )
        .expect("failed to write auto log");
        fs::write(repo.path().join("bug").join("BUG.md"), "# bug\n")
            .expect("failed to write bug artifact");
        fs::write(repo.path().join("nemesis").join("REPORT.md"), "# nemesis\n")
            .expect("failed to write nemesis artifact");
        fs::write(repo.path().join("gen-001").join("SPEC.md"), "# generated\n")
            .expect("failed to write generated artifact");
        fs::write(
            file_dir.join("patch.diff"),
            "\
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-# temp
+# audited
",
        )
        .expect("failed to write patch");

        let (status, commit) = apply_verdict(
            repo.path(),
            &output_dir,
            "main",
            "README.md",
            &file_dir,
            &verdict("DRIFT-SMALL", "tighten README"),
            false,
        )
        .expect("patch verdict should commit only the audited file");

        assert_eq!(status, EntryStatus::Audited);
        assert!(commit.is_some());
        assert_eq!(last_commit_paths(repo.path()), vec!["README.md"]);
        let status = run_git_in(repo.path(), ["status", "--short"]);
        assert!(status.contains("?? .auto/"), "{status}");
        assert!(status.contains("?? audit/"), "{status}");
        assert!(status.contains("?? bug/"), "{status}");
        assert!(status.contains("?? gen-001/"), "{status}");
        assert!(status.contains("?? nemesis/"), "{status}");
    }

    #[test]
    fn commit_scoped_treats_repo_paths_as_literals() {
        let repo = init_repo("audit-literal-pathspec");
        let magic_path = ":(glob)*";
        fs::write(repo.path().join(magic_path), "before\n").expect("failed to write magic file");
        fs::write(repo.path().join("other.md"), "before\n").expect("failed to write other file");
        run_git_in(repo.path(), ["add", "."]);
        run_git_in(repo.path(), ["commit", "-m", "add magic path"]);

        fs::write(repo.path().join(magic_path), "after\n").expect("failed to edit magic file");
        fs::write(repo.path().join("other.md"), "after\n").expect("failed to edit other file");

        let commit = commit_scoped(
            repo.path(),
            "main",
            "audit: literal pathspec",
            &[magic_path.to_string()],
        )
        .expect("literal pathspec commit should succeed");

        assert!(commit.is_some());
        assert_eq!(last_commit_paths(repo.path()), vec![magic_path]);
        assert_eq!(
            run_git_in(repo.path(), ["status", "--short", "--", "other.md"]).trim(),
            "M other.md"
        );
    }

    #[test]
    fn audit_commit_excludes_generated_and_runtime_artifacts() {
        let repo = init_repo("audit-scoped-worklist");
        let output_dir = repo.path().join("audit");
        let file_dir = output_dir.join("files").join("drift-large");
        fs::create_dir_all(&file_dir).expect("failed to create file dir");
        fs::write(
            file_dir.join("worklist-entry.md"),
            "- `README.md` audit DRIFT-LARGE: capture follow-up\n",
        )
        .expect("failed to write transient audit output");
        fs::create_dir_all(repo.path().join(".auto").join("audit"))
            .expect("failed to create auto dir");
        fs::create_dir_all(repo.path().join("bug")).expect("failed to create bug dir");
        fs::create_dir_all(repo.path().join("nemesis")).expect("failed to create nemesis dir");
        fs::create_dir_all(repo.path().join("gen-001")).expect("failed to create gen dir");
        fs::create_dir_all(
            repo.path()
                .join(".config")
                .join("quota-router")
                .join("profiles"),
        )
        .expect("failed to create quota profile dir");
        fs::write(
            repo.path().join(".auto").join("audit").join("receipt.json"),
            "{}\n",
        )
        .expect("failed to write auto receipt");
        fs::write(repo.path().join("bug").join("BUG.md"), "# bug\n")
            .expect("failed to write bug artifact");
        fs::write(repo.path().join("nemesis").join("REPORT.md"), "# nemesis\n")
            .expect("failed to write nemesis artifact");
        fs::write(repo.path().join("gen-001").join("SPEC.md"), "# generated\n")
            .expect("failed to write generated artifact");
        fs::write(
            repo.path()
                .join(".config")
                .join("quota-router")
                .join("profiles")
                .join("default.json"),
            "{}\n",
        )
        .expect("failed to write quota profile");

        let (status, commit) = apply_verdict(
            repo.path(),
            &output_dir,
            "main",
            "README.md",
            &file_dir,
            &verdict("DRIFT-LARGE", "capture follow-up"),
            false,
        )
        .expect("worklist verdict should commit only durable queue output");

        assert_eq!(status, EntryStatus::Audited);
        assert!(commit.is_some());
        assert_eq!(last_commit_paths(repo.path()), vec!["WORKLIST.md"]);
        let committed = run_git_in(repo.path(), ["show", "--format=", "--name-only", "HEAD"]);
        for excluded in [
            ".auto",
            ".config",
            "audit/files",
            "bug",
            "gen-001",
            "nemesis",
        ] {
            assert!(
                !committed.contains(excluded),
                "{excluded} should not be committed:\n{committed}"
            );
        }
        let status = run_git_in(repo.path(), ["status", "--short"]);
        assert!(status.contains("?? .auto/"), "{status}");
        assert!(status.contains("?? .config/"), "{status}");
        assert!(status.contains("?? audit/"), "{status}");
        assert!(status.contains("?? bug/"), "{status}");
        assert!(status.contains("?? gen-001/"), "{status}");
        assert!(status.contains("?? nemesis/"), "{status}");
    }

    #[tokio::test]
    async fn run_audit_kimi_models_require_use_kimi_cli() {
        let repo_root = TestTempDir::new("run-audit-requires-kimi");
        let args = AuditArgs {
            everything: false,
            everything_phase: crate::AuditEverythingPhase::All,
            everything_run_id: None,
            everything_run_root: None,
            everything_in_place: false,
            everything_threads: 15,
            remediation_threads: 1,
            first_pass_model: "gpt-5.5".to_string(),
            first_pass_effort: "low".to_string(),
            synthesis_model: "gpt-5.5".to_string(),
            synthesis_effort: "high".to_string(),
            remediation_model: "gpt-5.5".to_string(),
            remediation_effort: "high".to_string(),
            final_review_model: "gpt-5.5".to_string(),
            final_review_effort: "xhigh".to_string(),
            final_review_retries: 1,
            file_quality_passes: 10,
            no_everything_merge: false,
            doctrine_prompt: PathBuf::from("audit/DOCTRINE.md"),
            rubric_prompt: None,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            max_files: 0,
            audit_threads: 15,
            output_dir: None,
            verify_findings: false,
            resolve_findings: false,
            resolve_validation_threads: 2,
            resolve_keep_runs: 2,
            resolve_passes: 10,
            no_resolve_target_prune: false,
            allow_missing_resolve_roots: false,
            resume_mode: AuditResumeMode::Resume,
            report_only: false,
            dry_run: false,
            branch: None,
            model: "k2.6".to_string(),
            reasoning_effort: "high".to_string(),
            escalation_model: "gpt-5.5".to_string(),
            escalation_effort: "high".to_string(),
            codex_bin: PathBuf::from("codex"),
            kimi_bin: PathBuf::from("kimi-cli"),
            pi_bin: PathBuf::from("pi"),
            use_kimi_cli: false,
        };

        let err = run_auditor(repo_root.path(), "prompt", &args)
            .await
            .expect_err("run_auditor should reject Kimi without --use-kimi-cli");

        assert_eq!(
            err.to_string(),
            "auto audit Kimi models currently require --use-kimi-cli"
        );
    }
}
