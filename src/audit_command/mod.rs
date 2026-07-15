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
//!
//! Submodules own the cohesive concerns: `manifest` is the resume model,
//! `verify` is `--verify-findings`, `resolve` is the `--resolve-findings`
//! engine, `files` holds enumeration/glob/prompt helpers, and `auditor` is the
//! process layer.

mod auditor;
mod files;
mod manifest;
mod resolve;
mod verify;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::task::JoinSet;

use crate::audit_command::auditor::{is_kimi_model, run_auditor_labeled};
use crate::audit_command::files::{
    build_file_prompt, enumerate_tracked_files, file_artifact_dir, first_line,
    literal_git_pathspec, repo_relative_pathspec, resolve_rubric, sha256_hex,
};
use crate::audit_command::manifest::{
    initial_manifest, mark_entry, plan_audit_queue, reconcile_manifest_with_tree, write_manifest,
    EntryStatus, Manifest, ManifestEntry,
};
use crate::audit_command::resolve::resolve_audit_findings;
use crate::audit_command::verify::verify_audit_findings;
use crate::kimi_backend::{preflight_kimi_cli, resolve_kimi_bin};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, run_git,
};
use crate::{AuditArgs, AuditResumeMode};

const AUDITOR_TIMEOUT_SECS: u64 = 30 * 60; // 30 minutes per file — generous
const FINDING_RESOLUTION_TIMEOUT_SECS: u64 = 4 * 60 * 60; // remediation lanes can pay fresh dependency-build cost
const BUNDLED_RUBRIC: &str = include_str!("../audit_rubric.md");
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

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct FileVerdict {
    pub(crate) verdict: String,
    pub(crate) rationale: String,
    #[serde(default)]
    touched_paths: Vec<String>,
    #[serde(default)]
    escalate: bool,
}

struct AuditWorkerResult {
    idx: usize,
    entry: ManifestEntry,
    content_hash: String,
    file_dir: PathBuf,
    response: String,
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

    let rubric = resolve_rubric(&repo_root, args.rubric_prompt.as_deref())?;
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
    let worker_context = AuditWorkerContext {
        repo_root: repo_root_arc,
        output_dir: output_dir_arc,
        doctrine: doctrine_arc,
        rubric: rubric_arc,
        args: args.clone(),
        cap,
    };
    let mut join_set = JoinSet::new();
    let mut plan_iter = selected_plan.into_iter().enumerate();
    let mut active = 0usize;
    for _ in 0..workers {
        if let Some((idx, entry)) = plan_iter.next() {
            spawn_audit_worker(&mut join_set, worker_context.clone(), idx, entry);
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
                    spawn_audit_worker(&mut join_set, worker_context.clone(), idx, entry);
                    active += 1;
                }
                continue;
            }
            Err(err) => {
                eprintln!("audit worker task panicked: {err}");
                if let Some((idx, entry)) = plan_iter.next() {
                    spawn_audit_worker(&mut join_set, worker_context.clone(), idx, entry);
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
                    spawn_audit_worker(&mut join_set, worker_context.clone(), idx, entry);
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
            spawn_audit_worker(&mut join_set, worker_context.clone(), idx, entry);
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

#[derive(Clone)]
struct AuditWorkerContext {
    repo_root: Arc<PathBuf>,
    output_dir: Arc<PathBuf>,
    doctrine: Arc<String>,
    rubric: Arc<String>,
    args: AuditArgs,
    cap: usize,
}

fn spawn_audit_worker(
    join_set: &mut JoinSet<Result<AuditWorkerResult>>,
    context: AuditWorkerContext,
    idx: usize,
    entry: ManifestEntry,
) {
    join_set.spawn(async move { run_audit_worker(context, idx, entry).await });
}

async fn run_audit_worker(
    context: AuditWorkerContext,
    idx: usize,
    entry: ManifestEntry,
) -> Result<AuditWorkerResult> {
    let AuditWorkerContext {
        repo_root,
        output_dir,
        doctrine,
        rubric,
        args,
        cap,
    } = context;
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

/// Apply the auditor's verdict: patch the file, append WORKLIST / retire
/// entries, commit per-file. Returns the new manifest status + the commit
/// SHA (if any).
#[allow(clippy::too_many_arguments)]
fn apply_verdict(
    repo_root: &std::path::Path,
    output_dir: &std::path::Path,
    branch: &str,
    rel_path: &str,
    file_dir: &std::path::Path,
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

fn apply_patch(repo_root: &std::path::Path, patch: &std::path::Path) -> Result<()> {
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
    repo_root: &std::path::Path,
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

fn record_worklist_entry(
    repo_root: &std::path::Path,
    _output_dir: &std::path::Path,
    branch: &str,
    rel_path: &str,
    file_dir: &std::path::Path,
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
    repo_root: &std::path::Path,
    output_dir: &std::path::Path,
    branch: &str,
    rel_path: &str,
    file_dir: &std::path::Path,
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
    output_dir: &std::path::Path,
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
    let mut by_verdict: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{AuditArgs, AuditResumeMode};

    use super::auditor::run_auditor;
    use super::files::{build_file_prompt, enumerate_tracked_files, sha256_hex};
    use super::{apply_verdict, commit_scoped, EntryStatus, FileVerdict, DEFAULT_EXCLUDE_GLOBS};
    use crate::audit_command::files::matches_any;

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
            first_pass_model: "gpt-5.6-sol".to_string(),
            first_pass_effort: "low".to_string(),
            first_pass_retries: 3,
            synthesis_model: "gpt-5.6-sol".to_string(),
            synthesis_effort: "high".to_string(),
            remediation_model: "gpt-5.6-sol".to_string(),
            remediation_effort: "high".to_string(),
            final_review_model: "gpt-5.6-sol".to_string(),
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
            escalation_model: "gpt-5.6-sol".to_string(),
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
