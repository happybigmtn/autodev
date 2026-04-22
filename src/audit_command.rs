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
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncRead;
use tokio::process::Command as TokioCommand;
use tokio::time;

use crate::codex_stream::capture_pi_output;
use crate::kimi_backend::{
    extract_final_text as kimi_extract_final_text, kimi_exec_args, parse_kimi_error,
    preflight_kimi_cli, resolve_kimi_bin, resolve_kimi_cli_model,
};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, run_git,
};
use crate::{AuditArgs, AuditResumeMode};

const AUDITOR_TIMEOUT_SECS: u64 = 30 * 60; // 30 minutes per file — generous
const BUNDLED_RUBRIC: &str = include_str!("audit_rubric.md");
const DEFAULT_INCLUDE_GLOBS: &[&str] = &[
    "**/*.rs",
    "**/*.ts",
    "**/*.tsx",
    "**/*.py",
    "specs/**/*.md",
    "AUTONOMY-GDD.md",
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
    "**/bug/**",
    "**/nemesis/**",
    "**/steward/**",
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

pub(crate) async fn run_audit(args: AuditArgs) -> Result<()> {
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

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join("audit"));
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    fs::create_dir_all(output_dir.join("files"))
        .with_context(|| format!("failed to create {}", output_dir.join("files").display()))?;

    if args.use_kimi_cli && !args.dry_run {
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
    for (idx, entry) in plan.iter().enumerate() {
        if idx >= cap {
            break;
        }
        let abs_path = repo_root.join(&entry.path);
        if !abs_path.exists() {
            mark_entry(
                &mut manifest,
                &entry.path,
                EntryStatus::Skipped,
                None,
                None,
                None,
                None,
                None,
            );
            write_manifest(&manifest_path, &manifest)?;
            continue;
        }
        let content = fs::read(&abs_path)
            .with_context(|| format!("failed to read {}", abs_path.display()))?;
        let content_hash = sha256_hex(&content);
        let file_dir = file_artifact_dir(&output_dir, &entry.path);
        // Start each file's artifact dir fresh so we never mix a partial prior
        // run's verdict.json with this run's outputs.
        if file_dir.exists() {
            fs::remove_dir_all(&file_dir).ok();
        }
        fs::create_dir_all(&file_dir)
            .with_context(|| format!("failed to create {}", file_dir.display()))?;
        let prompt = build_file_prompt(&abs_path, &doctrine, &rubric, &output_dir, &entry.path)?;
        let prompt_path = file_dir.join("prompt.md");
        atomic_write(&prompt_path, prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        println!();
        println!(
            "[{idx}/{cap}] audit {path}",
            idx = idx + 1,
            cap = cap,
            path = entry.path
        );

        let run_result = run_auditor(&repo_root, &prompt, &args).await;
        let response = match run_result {
            Ok(r) => r,
            Err(err) => {
                eprintln!("audit failed for {}: {err:#}", entry.path);
                mark_entry(
                    &mut manifest,
                    &entry.path,
                    EntryStatus::Pending,
                    Some(content_hash.clone()),
                    Some(doctrine_hash.clone()),
                    Some(rubric_hash.clone()),
                    None,
                    None,
                );
                write_manifest(&manifest_path, &manifest)?;
                continue;
            }
        };
        atomic_write(&file_dir.join("response.log"), response.as_bytes()).with_context(|| {
            format!(
                "failed to write {}",
                file_dir.join("response.log").display()
            )
        })?;

        let verdict_path = file_dir.join("verdict.json");
        let verdict = match fs::read_to_string(&verdict_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<FileVerdict>(&raw).ok())
        {
            Some(v) => v,
            None => {
                eprintln!(
                    "audit finished but verdict.json missing / invalid for {}; keeping pending",
                    entry.path
                );
                mark_entry(
                    &mut manifest,
                    &entry.path,
                    EntryStatus::Pending,
                    Some(content_hash.clone()),
                    Some(doctrine_hash.clone()),
                    Some(rubric_hash.clone()),
                    None,
                    None,
                );
                write_manifest(&manifest_path, &manifest)?;
                continue;
            }
        };
        println!(
            "verdict:      {} — {}",
            verdict.verdict,
            first_line(&verdict.rationale)
        );

        let (new_status, commit_sha) = apply_verdict(
            &repo_root,
            &output_dir,
            &current_branch,
            &entry.path,
            &file_dir,
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
            &entry.path,
            new_status,
            Some(content_hash),
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
        let current_content = fs::read(repo_root.join(&entry.path))
            .with_context(|| format!("failed to read {}", repo_root.join(&entry.path).display()))?;
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
    files.retain(|path| matches_any(path, include) && !matches_any(path, exclude));
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
        .strip_prefix(
            abs_path
                .parent()
                .and_then(Path::parent)
                .unwrap_or(Path::new(".")),
        )
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
                    let commit = commit_all(repo_root, branch, &message)?;
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

fn commit_all(repo_root: &Path, branch: &str, message: &str) -> Result<Option<String>> {
    let _ = branch;
    run_git(repo_root, ["add", "-A"])?;
    let status = git_stdout(repo_root, ["status", "--porcelain"])?;
    if status.trim().is_empty() {
        return Ok(None);
    }
    run_git(repo_root, ["commit", "-m", message])?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    Ok(Some(commit))
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
    let commit = commit_all(repo_root, branch, &message)?;
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
    let commit = commit_all(repo_root, branch, &message)?;
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

async fn run_auditor(repo_root: &Path, prompt: &str, args: &AuditArgs) -> Result<String> {
    if args.use_kimi_cli {
        run_auditor_kimi(repo_root, prompt, args).await
    } else {
        bail!("auto audit currently requires --use-kimi-cli");
    }
}

async fn run_auditor_kimi(repo_root: &Path, prompt: &str, args: &AuditArgs) -> Result<String> {
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
    let wait_result = time::timeout(Duration::from_secs(AUDITOR_TIMEOUT_SECS), child.wait()).await;
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
        bail!(
            "kimi-cli audit pass timed out after {}s",
            AUDITOR_TIMEOUT_SECS
        );
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
        apply_verdict, glob_match, plan_audit_queue, run_auditor, sha256_hex, EntryStatus,
        FileVerdict, Manifest, ManifestEntry,
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

    #[tokio::test]
    async fn run_audit_requires_use_kimi_cli() {
        let repo_root = TestTempDir::new("run-audit-requires-kimi");
        let args = AuditArgs {
            doctrine_prompt: PathBuf::from("audit/DOCTRINE.md"),
            rubric_prompt: None,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            max_files: 0,
            output_dir: None,
            resume_mode: AuditResumeMode::Resume,
            report_only: false,
            dry_run: false,
            branch: None,
            model: "k2.6".to_string(),
            reasoning_effort: "high".to_string(),
            escalation_model: "gpt-5.4".to_string(),
            escalation_effort: "high".to_string(),
            codex_bin: PathBuf::from("codex"),
            kimi_bin: PathBuf::from("kimi-cli"),
            pi_bin: PathBuf::from("pi"),
            use_kimi_cli: false,
        };

        let err = run_auditor(repo_root.path(), "prompt", &args)
            .await
            .expect_err("run_auditor should reject --no-use-kimi-cli");

        assert_eq!(
            err.to_string(),
            "auto audit currently requires --use-kimi-cli"
        );
    }
}
