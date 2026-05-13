//! `auto gc` — disk steward for `.auto/` working directories.
//!
//! The bulk of `.auto/` space (hundreds of GB in practice) is consumed by
//! per-lane / per-worktree clones of the source repository created by
//! `auto audit`, `auto parallel`, and `auto super` for isolation. Those clones
//! are regenerable from git. The findings they produced (per-file analyses,
//! syntheses, harvest summaries, design reports) are tiny and irreplaceable.
//!
//! Before this command existed, two out-of-tree shell scripts at
//! `~/.local/bin/auto-archive-audit-findings` and `~/.local/bin/auto-prune-scratch`
//! handled archive + prune. They lived outside the framework, which meant disk
//! lifecycle was an operator concern rather than a framework one. Folding them
//! in-tree lets `auto super` auto-trigger archive after each run and stops
//! evidence from getting wiped by a clean checkout.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};

#[derive(Args, Clone, Debug)]
pub(crate) struct GcArgs {
    /// Repository to operate on (defaults to current working directory).
    #[arg(long)]
    pub repo: Option<PathBuf>,

    /// Run on every repo under /home/r/Coding/* with a `.auto/` directory.
    #[arg(long, conflicts_with = "repo")]
    pub all: bool,

    /// Archive durable evidence into the repo's canonical archive dir
    /// before pruning. Idempotent: re-runs only refresh in-progress harvest.
    #[arg(long)]
    pub archive: bool,

    /// Delete per-lane / per-worktree repo clones under `.auto/`.
    /// Without this flag the command is read-only (show + maybe archive).
    #[arg(long)]
    pub prune: bool,

    /// Show planned actions without writing or deleting.
    #[arg(long)]
    pub dry_run: bool,

    /// Preserve the most recently modified `.auto/super/<run-id>/` dir on the
    /// assumption it belongs to an in-flight run. Default behavior.
    #[arg(long, default_value_t = true)]
    pub keep_running: bool,

    /// Disable the keep-running guard.
    #[arg(long)]
    pub no_keep_running: bool,

    /// Skip the "is auto super running for this repo" pgrep check.
    /// Without `--force`, the command refuses while a super run is active.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArchivedRun {
    pub(crate) run_id: String,
    pub(crate) source: PathBuf,
    pub(crate) destination: PathBuf,
    pub(crate) file_count: usize,
    pub(crate) already_archived: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GcReport {
    pub(crate) repo: PathBuf,
    pub(crate) before_bytes: Option<u64>,
    pub(crate) after_bytes: Option<u64>,
    pub(crate) archived: Vec<ArchivedRun>,
    pub(crate) pruned_paths: Vec<PathBuf>,
    pub(crate) dry_run: bool,
    pub(crate) skipped_reason: Option<String>,
}

pub(crate) fn run_gc(args: GcArgs) -> Result<()> {
    let mut keep_running = args.keep_running;
    if args.no_keep_running {
        keep_running = false;
    }

    let repos = if args.all {
        discover_repos(Path::new("/home/r/Coding"))?
    } else {
        let repo = args.repo.clone().unwrap_or_else(|| {
            std::env::current_dir().expect("current dir resolvable")
        });
        vec![canonicalize(&repo)?]
    };

    let mut all_reports = Vec::with_capacity(repos.len());
    for repo in &repos {
        let report = run_one_repo(repo, &args, keep_running)?;
        print_report(&report);
        all_reports.push(report);
    }

    if all_reports.iter().all(|r| r.skipped_reason.is_some()) && !all_reports.is_empty() {
        return Err(anyhow!(
            "auto gc: every target repo skipped — see reasons above (pass --force to override)"
        ));
    }

    Ok(())
}

fn run_one_repo(repo: &Path, args: &GcArgs, keep_running: bool) -> Result<GcReport> {
    let auto_root = repo.join(".auto");
    if !auto_root.is_dir() {
        return Ok(GcReport {
            repo: repo.to_owned(),
            before_bytes: None,
            after_bytes: None,
            archived: Vec::new(),
            pruned_paths: Vec::new(),
            dry_run: args.dry_run,
            skipped_reason: Some(format!("no .auto/ at {}", repo.display())),
        });
    }

    if !args.force && super_is_active_for(repo) {
        return Ok(GcReport {
            repo: repo.to_owned(),
            before_bytes: dir_bytes(&auto_root).ok(),
            after_bytes: None,
            archived: Vec::new(),
            pruned_paths: Vec::new(),
            dry_run: args.dry_run,
            skipped_reason: Some(format!(
                "`auto super` is running for {} (pass --force to override)",
                repo.display()
            )),
        });
    }

    let before_bytes = dir_bytes(&auto_root).ok();

    let active_super = if keep_running {
        most_recent_super_run(&auto_root)
    } else {
        None
    };

    let archived = if args.archive {
        archive_audit_runs(repo, args.dry_run)?
    } else {
        Vec::new()
    };

    let prune_paths = if args.prune {
        let candidates = enumerate_prune_paths(&auto_root)?;
        filter_active_super(candidates, active_super.as_deref())
    } else {
        Vec::new()
    };

    if !args.dry_run {
        for path in &prune_paths {
            if path.is_dir() {
                delete_dir_safe(path).with_context(|| format!("delete {}", path.display()))?;
            }
        }
    }

    let after_bytes = dir_bytes(&auto_root).ok();

    Ok(GcReport {
        repo: repo.to_owned(),
        before_bytes,
        after_bytes,
        archived,
        pruned_paths: prune_paths,
        dry_run: args.dry_run,
        skipped_reason: None,
    })
}

/// Locate the archive root for a repo. autonomy-style layouts use
/// `ops/evidence/`; bitino-style layouts use `docs/ops/operator-evidence/`.
/// Skips repos that have neither.
pub(crate) fn archive_root_for(repo: &Path) -> Option<PathBuf> {
    if repo.join("ops/evidence").is_dir() {
        return Some(repo.join("ops/evidence/audit-archive"));
    }
    if repo.join("docs/ops/operator-evidence").is_dir() {
        return Some(repo.join("docs/ops/operator-evidence/audit-archive"));
    }
    None
}

/// Walk `.auto/audit-everything/<run-id>/` and copy per-file analyses,
/// synthesis reports, and harvest into the archive root. Idempotent.
pub(crate) fn archive_audit_runs(repo: &Path, dry_run: bool) -> Result<Vec<ArchivedRun>> {
    let runs_root = repo.join(".auto/audit-everything");
    if !runs_root.is_dir() {
        return Ok(Vec::new());
    }
    let Some(archive_root) = archive_root_for(repo) else {
        return Ok(Vec::new());
    };
    if !dry_run {
        fs::create_dir_all(&archive_root).with_context(|| {
            format!("create archive root {}", archive_root.display())
        })?;
    }

    let mut archived = Vec::new();
    for entry in fs::read_dir(&runs_root)
        .with_context(|| format!("read {}", runs_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let run_dir = entry.path();
        let run_id = entry.file_name().to_string_lossy().into_owned();
        let dest = archive_root.join(&run_id);
        let already_archived = dest.join(".archived").is_file();

        let mut file_count = 0_usize;
        if already_archived {
            // Refresh harvest for in-progress runs without re-touching the
            // immutable per-file analyses.
            if !dry_run {
                refresh_harvest(&run_dir, &dest, &mut file_count)?;
            }
        } else if !dry_run {
            file_count = copy_findings(&run_dir, &dest)?;
            fs::File::create(dest.join(".archived"))
                .with_context(|| format!("touch {}", dest.join(".archived").display()))?;
        }

        archived.push(ArchivedRun {
            run_id,
            source: run_dir,
            destination: dest,
            file_count,
            already_archived,
        });
    }

    Ok(archived)
}

/// First-time copy of a run's findings into the archive. Returns the file count.
fn copy_findings(run_dir: &Path, dest: &Path) -> Result<usize> {
    fs::create_dir_all(dest).with_context(|| format!("mkdir {}", dest.display()))?;
    let mut count = 0_usize;

    // Per-file analyses + synthesis reports live under worktree/audit/everything/<id>/.
    let everything_root = run_dir.join("worktree/audit/everything");
    if let Some(inner) = newest_subdir_starting_with(&everything_root, "20") {
        fs::create_dir_all(dest.join("files"))?;
        fs::create_dir_all(dest.join("reports"))?;
        count += copy_filtered(&inner.join("files"), &dest.join("files"), &["analysis.md"])?;
        count += copy_dir_files(&inner.join("reports"), &dest.join("reports"), &["md", "json"])?;
    }

    // Harvest summary if present.
    let harvest = run_dir.join("harvest");
    if harvest.is_dir() {
        let harvest_dest = dest.join("harvest");
        fs::create_dir_all(&harvest_dest)?;
        count += copy_dir_files(&harvest, &harvest_dest, &[])?;
    }

    Ok(count)
}

/// Re-sync only the `harvest/` for an in-progress run (idempotent for completed
/// runs since harvest is small and cheap to re-copy).
fn refresh_harvest(run_dir: &Path, dest: &Path, count: &mut usize) -> Result<()> {
    let harvest = run_dir.join("harvest");
    if !harvest.is_dir() {
        return Ok(());
    }
    let dest_harvest = dest.join("harvest");
    fs::create_dir_all(&dest_harvest)?;
    *count = copy_dir_files(&harvest, &dest_harvest, &[])?;
    Ok(())
}

/// Copy a directory tree, keeping only files whose basename appears in
/// `allowed_filenames`. Returns the count of copied files. Empty allow-list
/// means "copy everything".
fn copy_filtered(src: &Path, dst: &Path, allowed_filenames: &[&str]) -> Result<usize> {
    if !src.is_dir() {
        return Ok(0);
    }
    let mut count = 0_usize;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let entry_path = entry.path();
        let entry_name = entry.file_name();
        let dst_child = dst.join(&entry_name);
        if file_type.is_dir() {
            fs::create_dir_all(&dst_child)?;
            count += copy_filtered(&entry_path, &dst_child, allowed_filenames)?;
        } else if file_type.is_file() {
            let basename = entry_name.to_string_lossy();
            if !allowed_filenames.is_empty()
                && !allowed_filenames.iter().any(|allowed| basename == *allowed)
            {
                continue;
            }
            fs::copy(&entry_path, &dst_child)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Copy a flat directory of files, keeping only those whose extension appears in
/// `allowed_exts`. Empty allow-list means "copy everything".
fn copy_dir_files(src: &Path, dst: &Path, allowed_exts: &[&str]) -> Result<usize> {
    if !src.is_dir() {
        return Ok(0);
    }
    let mut count = 0_usize;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let entry_path = entry.path();
        if !allowed_exts.is_empty() {
            let ext = entry_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if !allowed_exts.iter().any(|allowed| *allowed == ext) {
                continue;
            }
        }
        let dst_child = dst.join(entry.file_name());
        fs::copy(&entry_path, &dst_child)?;
        count += 1;
    }
    Ok(count)
}

fn newest_subdir_starting_with(root: &Path, prefix: &str) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(prefix) {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        match best {
            Some((prior, _)) if prior >= mtime => {}
            _ => best = Some((mtime, entry.path())),
        }
    }
    best.map(|(_, path)| path)
}

/// Identify candidate prune paths -- per-lane `repo/` clones under
/// `.auto/audit-everything/*/lanes/*/repo`,
/// `.auto/parallel-host/*/lanes/*/repo`,
/// `.auto/super/*/<phase>/.../repo`, and `.auto/audit-everything/*/worktree/repo`.
fn enumerate_prune_paths(auto_root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for stage in ["audit-everything", "parallel-host", "super"] {
        let root = auto_root.join(stage);
        if !root.is_dir() {
            continue;
        }
        find_repo_dirs(&root, 0, 8, &mut out);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn find_repo_dirs(root: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // Don't recurse into the discovered `repo/` dirs themselves -- they are
        // full git clones with thousands of files.
        if name == "repo" {
            out.push(path);
            continue;
        }
        find_repo_dirs(&path, depth + 1, max_depth, out);
    }
}

fn filter_active_super(paths: Vec<PathBuf>, active: Option<&Path>) -> Vec<PathBuf> {
    let Some(active) = active else { return paths };
    paths
        .into_iter()
        .filter(|p| !p.starts_with(active))
        .collect()
}

fn most_recent_super_run(auto_root: &Path) -> Option<PathBuf> {
    let super_root = auto_root.join("super");
    if !super_root.is_dir() {
        return None;
    }
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(&super_root).ok()?.flatten() {
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        match best {
            Some((prior, _)) if prior >= mtime => {}
            _ => best = Some((mtime, entry.path())),
        }
    }
    best.map(|(_, path)| path)
}

/// Use `find -depth -delete` semantics: depth-first removal so directories are
/// emptied before themselves. We do this in pure Rust rather than shelling out
/// so the deletion is auditable.
fn delete_dir_safe(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    // std::fs::remove_dir_all is depth-first and tolerant of missing entries.
    fs::remove_dir_all(path).with_context(|| format!("remove_dir_all {}", path.display()))
}

fn discover_repos(scan_root: &Path) -> Result<Vec<PathBuf>> {
    let mut repos = Vec::new();
    if !scan_root.is_dir() {
        return Ok(repos);
    }
    for entry in fs::read_dir(scan_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if path.join(".auto").is_dir() {
            repos.push(path);
        }
    }
    repos.sort();
    Ok(repos)
}

fn canonicalize(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))
}

fn dir_bytes(path: &Path) -> Result<u64> {
    let mut total: u64 = 0;
    let Ok(entries) = fs::read_dir(path) else {
        return Ok(0);
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if metadata.is_file() {
            total += metadata.len();
        } else if metadata.is_dir() {
            total += dir_bytes(&entry.path()).unwrap_or(0);
        }
    }
    Ok(total)
}

fn super_is_active_for(repo: &Path) -> bool {
    // Best-effort pgrep. Failure (no pgrep, no permission) means "assume safe"
    // -- this matches the bash script's behavior and avoids false-positive
    // skipping on systems without pgrep.
    let needle = format!("auto super.*{}", repo.display());
    let output = Command::new("pgrep")
        .args(["-af", &needle])
        .output()
        .ok();
    match output {
        Some(out) => !out.stdout.is_empty(),
        None => false,
    }
}

fn human_bytes(bytes: Option<u64>) -> String {
    let Some(b) = bytes else {
        return "?".to_string();
    };
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KiB", b as f64 / 1024.0)
    } else if b < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn print_report(report: &GcReport) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "== auto gc: {} ==", report.repo.display());
    if let Some(reason) = &report.skipped_reason {
        let _ = writeln!(stderr, "skipped: {reason}");
        return;
    }
    let _ = writeln!(stderr, "before: {}", human_bytes(report.before_bytes));
    if !report.archived.is_empty() {
        let new_archives: BTreeMap<&str, &ArchivedRun> = report
            .archived
            .iter()
            .filter(|r| !r.already_archived)
            .map(|r| (r.run_id.as_str(), r))
            .collect();
        let _ = writeln!(
            stderr,
            "archived: {} new ({} already existed)",
            new_archives.len(),
            report.archived.len() - new_archives.len()
        );
        for (run_id, run) in new_archives.iter().take(5) {
            let _ = writeln!(stderr, "  + {run_id}: {} file(s)", run.file_count);
        }
    }
    if !report.pruned_paths.is_empty() {
        let _ = writeln!(
            stderr,
            "{}{} prune target(s)",
            if report.dry_run { "[dry-run] " } else { "" },
            report.pruned_paths.len()
        );
        for path in report.pruned_paths.iter().take(10) {
            let _ = writeln!(stderr, "  - {}", path.display());
        }
        if report.pruned_paths.len() > 10 {
            let _ = writeln!(
                stderr,
                "  ... ({} more)",
                report.pruned_paths.len() - 10
            );
        }
    }
    if !report.dry_run && (report.archived.iter().any(|a| !a.already_archived) || !report.pruned_paths.is_empty()) {
        let _ = writeln!(stderr, "after:  {}", human_bytes(report.after_bytes));
    }
}

/// Auto-trigger archive-only at the end of a super run. Called from
/// `super_command::run_super` after the run finishes. Best-effort: failures log
/// but don't propagate.
pub(crate) fn archive_after_super_run(repo: &Path) {
    if let Err(err) = archive_audit_runs(repo, false) {
        eprintln!(
            "auto gc: archive after super run failed (best-effort): {err:#}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn temp_root() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "autodev-gc-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn archive_root_for_prefers_ops_evidence_layout() {
        let root = temp_root();
        fs::create_dir_all(root.join("ops/evidence")).unwrap();
        let archive = archive_root_for(&root).unwrap();
        assert_eq!(archive, root.join("ops/evidence/audit-archive"));
    }

    #[test]
    fn archive_root_for_falls_back_to_bitino_layout() {
        let root = temp_root();
        fs::create_dir_all(root.join("docs/ops/operator-evidence")).unwrap();
        let archive = archive_root_for(&root).unwrap();
        assert_eq!(
            archive,
            root.join("docs/ops/operator-evidence/audit-archive")
        );
    }

    #[test]
    fn archive_root_for_returns_none_when_neither_layout_present() {
        let root = temp_root();
        assert!(archive_root_for(&root).is_none());
    }

    #[test]
    fn enumerate_prune_paths_finds_lane_repo_clones() {
        let root = temp_root();
        let auto = root.join(".auto");
        fs::create_dir_all(auto.join("audit-everything/run-1/lanes/lane-1/repo/.git")).unwrap();
        fs::create_dir_all(auto.join("parallel-host/run-2/lanes/lane-3/repo/.git")).unwrap();
        fs::create_dir_all(auto.join("super/run-3/design/parallel/pass-01/lanes/lane-1/repo/src")).unwrap();
        File::create(auto.join("super/run-3/manifest.json")).unwrap();

        let mut found = enumerate_prune_paths(&auto).unwrap();
        found.sort();
        assert_eq!(found.len(), 3);
        assert!(found.iter().any(|p| p.ends_with("audit-everything/run-1/lanes/lane-1/repo")));
        assert!(found.iter().any(|p| p.ends_with("parallel-host/run-2/lanes/lane-3/repo")));
        assert!(found
            .iter()
            .any(|p| p.ends_with("super/run-3/design/parallel/pass-01/lanes/lane-1/repo")));
        // The manifest.json file is not in the prune list.
        assert!(!found.iter().any(|p| p.ends_with("manifest.json")));
    }

    #[test]
    fn filter_active_super_excludes_descendants_of_active_run() {
        let active = PathBuf::from("/x/.auto/super/run-3");
        let paths = vec![
            PathBuf::from("/x/.auto/super/run-3/design/parallel/pass-01/lanes/lane-1/repo"),
            PathBuf::from("/x/.auto/super/run-2/design/parallel/pass-01/lanes/lane-1/repo"),
            PathBuf::from("/x/.auto/audit-everything/run-1/lanes/lane-1/repo"),
        ];
        let filtered = filter_active_super(paths, Some(&active));
        assert_eq!(filtered.len(), 2);
        assert!(filtered
            .iter()
            .any(|p| p.ends_with("audit-everything/run-1/lanes/lane-1/repo")));
        assert!(filtered.iter().any(|p| p.ends_with("super/run-2/design/parallel/pass-01/lanes/lane-1/repo")));
    }

    #[test]
    fn archive_audit_runs_copies_per_file_analyses_and_reports() {
        let repo = temp_root();
        fs::create_dir_all(repo.join("ops/evidence")).unwrap();
        let run_id = "20260513-120000";
        let run_dir = repo
            .join(".auto/audit-everything")
            .join(run_id);
        let inner = run_dir
            .join("worktree/audit/everything/20260513-120000-inner");
        fs::create_dir_all(inner.join("files/aaa-bbb")).unwrap();
        File::create(inner.join("files/aaa-bbb/analysis.md"))
            .unwrap()
            .write_all(b"# analysis")
            .unwrap();
        // The large attachment that we must NOT copy.
        File::create(inner.join("files/aaa-bbb/source.snippet.txt"))
            .unwrap()
            .write_all(b"large blob")
            .unwrap();
        fs::create_dir_all(inner.join("reports")).unwrap();
        File::create(inner.join("reports/web.md")).unwrap();
        File::create(inner.join("reports/synthesis-prompt.md")).unwrap();
        File::create(inner.join("reports/leftover.txt")).unwrap();
        fs::create_dir_all(run_dir.join("harvest")).unwrap();
        File::create(run_dir.join("harvest/AUDIT-FINDINGS-SUMMARY.json")).unwrap();

        let archived = archive_audit_runs(&repo, false).unwrap();
        assert_eq!(archived.len(), 1);
        let entry = &archived[0];
        assert_eq!(entry.run_id, run_id);
        assert!(!entry.already_archived);

        let dest = repo.join("ops/evidence/audit-archive").join(run_id);
        assert!(dest.join(".archived").is_file());
        assert!(dest.join("files/aaa-bbb/analysis.md").is_file());
        // The large attachment must not have been copied.
        assert!(!dest.join("files/aaa-bbb/source.snippet.txt").exists());
        assert!(dest.join("reports/web.md").is_file());
        // The .txt report file must be filtered out (only .md/.json kept).
        assert!(!dest.join("reports/leftover.txt").exists());
        assert!(dest.join("harvest/AUDIT-FINDINGS-SUMMARY.json").is_file());

        // Re-run should be idempotent (already archived).
        let second = archive_audit_runs(&repo, false).unwrap();
        assert!(second[0].already_archived);
    }

    #[test]
    fn dry_run_does_not_create_archive_dirs() {
        let repo = temp_root();
        fs::create_dir_all(repo.join("ops/evidence")).unwrap();
        let run_id = "20260513-120000";
        let run_dir = repo.join(".auto/audit-everything").join(run_id);
        fs::create_dir_all(run_dir.join("worktree/audit/everything/20260513-120000-i")).unwrap();

        let archived = archive_audit_runs(&repo, true).unwrap();
        assert_eq!(archived.len(), 1);
        // dry_run path: nothing was actually copied
        assert!(!repo
            .join("ops/evidence/audit-archive")
            .join(run_id)
            .join(".archived")
            .exists());
    }
}
