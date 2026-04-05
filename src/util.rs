use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use chrono::Utc;

pub(crate) const CLI_LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\ncommit: ",
    env!("AUTODEV_GIT_SHA"),
    "\ndirty: ",
    env!("AUTODEV_GIT_DIRTY"),
    "\nprofile: ",
    env!("AUTODEV_BUILD_PROFILE"),
);

const CHECKPOINT_EXCLUDES: [&str; 4] = [
    ":(exclude).auto",
    ":(exclude)bug",
    ":(exclude)nemesis",
    ":(exclude)gen-*",
];
const AUTO_LOG_KEEP_FILES: usize = 64;
const AUTO_FRESH_INPUT_KEEP_ENTRIES: usize = 12;
const AUTO_QUEUE_RUN_KEEP_ENTRIES: usize = 12;
const PI_RUNTIME_LOG_KEEP_FILES: usize = 5;
const PI_RUNTIME_LOG_MAX_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn git_repo_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to run `git rev-parse --show-toplevel`")?;
    if !output.status.success() {
        bail!(
            "not inside a git repository: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let root = String::from_utf8(output.stdout).context("git repo root was not UTF-8")?;
    Ok(PathBuf::from(root.trim()))
}

pub(crate) fn git_stdout<'a>(
    repo_root: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if !output.status.success() {
        bail!(
            "git command failed in {}: {}",
            repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("git stdout was not valid UTF-8")
}

pub(crate) fn run_git<'a>(repo_root: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git command failed in {}: {}",
        repo_root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn checkpoint_status(repo_root: &Path) -> Result<String> {
    let mut args = vec!["status", "--short", "--", "."];
    args.extend(CHECKPOINT_EXCLUDES);
    git_stdout(repo_root, args)
}

pub(crate) fn repo_name(repo_root: &Path) -> String {
    repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo")
        .to_string()
}

pub(crate) fn auto_checkpoint_if_needed(
    repo_root: &Path,
    branch: &str,
    message_suffix: &str,
) -> Result<Option<String>> {
    let status = checkpoint_status(repo_root)?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    sync_branch_with_remote(repo_root, branch)?;
    stage_checkpoint_changes(repo_root)?;
    let message = format!("{}: {message_suffix}", repo_name(repo_root));
    run_git(repo_root, ["commit", "-m", &message])?;
    push_branch_with_remote_sync(repo_root, branch)?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    Ok(Some(commit.trim().to_string()))
}

pub(crate) fn sync_branch_with_remote(repo_root: &Path, branch: &str) -> Result<bool> {
    if !remote_branch_exists(repo_root, branch)? {
        return Ok(false);
    }

    run_git(
        repo_root,
        ["pull", "--rebase", "--autostash", "origin", branch],
    )?;
    Ok(true)
}

pub(crate) fn push_branch_with_remote_sync(repo_root: &Path, branch: &str) -> Result<bool> {
    let synced = sync_branch_with_remote(repo_root, branch)?;
    run_git(repo_root, ["push", "origin", branch])?;
    Ok(synced)
}

fn remote_branch_exists(repo_root: &Path, branch: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-remote", "--heads", "origin", branch])
        .output()
        .with_context(|| format!("failed to query origin in {}", repo_root.display()))?;
    if !output.status.success() {
        bail!(
            "git command failed in {}: {}",
            repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(!output.stdout.is_empty())
}

fn stage_checkpoint_changes(repo_root: &Path) -> Result<()> {
    let mut tracked_args = vec!["add", "-u", "--", "."];
    tracked_args.extend(CHECKPOINT_EXCLUDES);
    run_git(repo_root, tracked_args)?;

    let untracked = git_stdout(
        repo_root,
        ["ls-files", "-z", "--others", "--exclude-standard"],
    )?;
    let stageable = untracked
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| !is_checkpoint_excluded_path(path))
        .map(|path| path.to_string())
        .collect::<Vec<_>>();

    for chunk in stageable.chunks(100) {
        let mut add_args = vec!["add".to_string(), "--".to_string()];
        add_args.extend(chunk.iter().cloned());
        run_git(repo_root, add_args.iter().map(|arg| arg.as_str()))?;
    }

    Ok(())
}

fn is_checkpoint_excluded_path(path: &str) -> bool {
    path == ".auto"
        || path == "bug"
        || path == "nemesis"
        || path.starts_with(".auto/")
        || path.starts_with("bug/")
        || path.starts_with("nemesis/")
        || path
            .split('/')
            .next()
            .map(|segment| segment.starts_with("gen-"))
            .unwrap_or(false)
}

pub(crate) fn ensure_repo_layout(repo_root: &Path) -> Result<()> {
    for rel in [
        ".auto",
        ".auto/fresh-input",
        ".auto/logs",
        ".auto/queue-runs",
    ] {
        let path = repo_root.join(rel);
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    prune_old_entries(&repo_root.join(".auto").join("logs"), AUTO_LOG_KEEP_FILES)?;
    prune_old_entries(
        &repo_root.join(".auto").join("fresh-input"),
        AUTO_FRESH_INPUT_KEEP_ENTRIES,
    )?;
    prune_old_entries(
        &repo_root.join(".auto").join("queue-runs"),
        AUTO_QUEUE_RUN_KEEP_ENTRIES,
    )?;
    prune_pi_runtime_state(repo_root)?;
    Ok(())
}

pub(crate) fn timestamp_slug() -> String {
    Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

pub(crate) fn current_binary_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(crate) fn binary_provenance_line() -> String {
    format!(
        "{} @ {} ({}, {})",
        env!("CARGO_PKG_VERSION"),
        current_binary_path(),
        env!("AUTODEV_GIT_SHA"),
        env!("AUTODEV_GIT_DIRTY")
    )
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let temp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name().and_then(|v| v.to_str()).unwrap_or("write"),
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::write(&temp, bytes).with_context(|| format!("failed to write {}", temp.display()))?;
    if let Err(rename_error) = fs::rename(&temp, path) {
        let cleanup_error = match fs::remove_file(&temp) {
            Ok(()) => None,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => Some(err),
        };
        let mut context = format!("failed to atomically replace {}", path.display());
        if let Some(err) = cleanup_error {
            context.push_str(&format!(
                "; also failed to remove temp {}: {}",
                temp.display(),
                err
            ));
        }
        return Err(rename_error).with_context(|| context);
    }
    Ok(())
}

pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if src.is_file() {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(src, dst)
            .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))?;
        return Ok(());
    }

    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let child_src = entry.path();
        let child_dst = dst.join(entry.file_name());
        if child_src.is_dir() {
            copy_tree(&child_src, &child_dst)?;
        } else {
            fs::copy(&child_src, &child_dst).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    child_src.display(),
                    child_dst.display()
                )
            })?;
        }
    }
    Ok(())
}

pub(crate) fn list_markdown_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    collect_markdown_files(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_markdown_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            files.push(path);
        }
    }
    Ok(())
}

pub(crate) fn clip_line_for_display(line: &str, max_chars: usize) -> String {
    line.chars().take(max_chars).collect()
}

pub(crate) fn prune_old_entries(dir: &Path, keep: usize) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if keep == 0 {
        clear_dir_contents(dir)?;
        return Ok(());
    }

    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", dir.display()))?
        .into_iter()
        .map(|entry| {
            let path = entry.path();
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(UNIX_EPOCH);
            (modified, path)
        })
        .collect::<Vec<_>>();
    if entries.len() <= keep {
        return Ok(());
    }

    entries.sort_by_key(|(modified, path)| (*modified, path.clone()));
    let remove_count = entries.len().saturating_sub(keep);
    for (_, path) in entries.into_iter().take(remove_count) {
        remove_path(&path)?;
    }
    Ok(())
}

pub(crate) fn truncate_file_to_max_bytes(path: &Path, max_bytes: usize) -> Result<()> {
    if !path.exists() || max_bytes == 0 {
        return Ok(());
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() <= max_bytes {
        return Ok(());
    }
    let keep_from = bytes.len().saturating_sub(max_bytes);
    atomic_write(path, &bytes[keep_from..])?;
    Ok(())
}

pub(crate) fn opencode_agent_dir(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".auto")
        .join("opencode-data")
        .join("opencode")
}

pub(crate) fn prune_pi_runtime_state(repo_root: &Path) -> Result<()> {
    let agent_dir = opencode_agent_dir(repo_root);
    if !agent_dir.exists() {
        return Ok(());
    }

    let log_dir = agent_dir.join("log");
    if log_dir.exists() {
        prune_old_entries(&log_dir, PI_RUNTIME_LOG_KEEP_FILES)?;
        for entry in fs::read_dir(&log_dir)
            .with_context(|| format!("failed to read {}", log_dir.display()))?
        {
            let path = entry?.path();
            if path.is_file() {
                truncate_file_to_max_bytes(&path, PI_RUNTIME_LOG_MAX_BYTES)?;
            }
        }
    }

    clear_and_recreate_dir(&agent_dir.join("snapshot"))?;
    clear_and_recreate_dir(&agent_dir.join("storage").join("session_diff"))?;
    Ok(())
}

pub(crate) fn clear_and_recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to clear {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    Ok(())
}

fn clear_dir_contents(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let path = entry?.path();
        remove_path(&path)?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread::sleep;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        atomic_write, checkpoint_status, clip_line_for_display, prune_old_entries,
        push_branch_with_remote_sync, stage_checkpoint_changes, sync_branch_with_remote,
        truncate_file_to_max_bytes,
    };

    fn temp_repo_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{name}-{}-{nonce}", std::process::id()))
    }

    fn init_repo(name: &str) -> PathBuf {
        let repo = temp_repo_path(name);
        fs::create_dir_all(&repo).expect("failed to create temp repo");
        run_git_in(&repo, ["init"]);
        run_git_in(&repo, ["config", "user.name", "autodev tests"]);
        run_git_in(&repo, ["config", "user.email", "autodev@example.com"]);
        fs::write(repo.join("README.md"), "# temp\n").expect("failed to write README");
        run_git_in(&repo, ["add", "README.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);
        repo
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

    fn init_remote_and_clones(name: &str, branch: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = temp_repo_path(name);
        let remote = root.join("remote.git");
        let upstream = root.join("upstream");
        let worker = root.join("worker");

        fs::create_dir_all(&root).expect("failed to create temp root");
        run_git_in(
            &root,
            [
                "init",
                "--bare",
                remote.to_str().expect("remote path utf-8"),
            ],
        );
        run_git_in(
            &root,
            [
                "clone",
                remote.to_str().expect("remote path utf-8"),
                upstream.to_str().expect("upstream path utf-8"),
            ],
        );
        run_git_in(&upstream, ["config", "user.name", "autodev tests"]);
        run_git_in(&upstream, ["config", "user.email", "autodev@example.com"]);
        fs::write(upstream.join("README.md"), "# init\n").expect("failed to write README");
        run_git_in(&upstream, ["add", "README.md"]);
        run_git_in(&upstream, ["commit", "-m", "init"]);
        run_git_in(&upstream, ["branch", "-M", branch]);
        run_git_in(&upstream, ["push", "-u", "origin", branch]);

        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                branch,
                remote.to_str().expect("remote path utf-8"),
                worker.to_str().expect("worker path utf-8"),
            ],
        );
        run_git_in(&worker, ["config", "user.name", "autodev tests"]);
        run_git_in(&worker, ["config", "user.email", "autodev@example.com"]);

        (root, remote, upstream, worker)
    }

    #[test]
    fn clips_on_char_boundaries() {
        let line = "╔══════════════════╗";
        let clipped = clip_line_for_display(line, 6);
        assert_eq!(clipped, "╔═════");
    }

    #[test]
    fn checkpoint_status_ignores_autodev_generated_dirs() {
        let repo = init_repo("checkpoint-status");
        fs::create_dir_all(repo.join(".auto").join("review")).expect("failed to create .auto");
        fs::create_dir_all(repo.join("bug")).expect("failed to create bug dir");
        fs::write(repo.join(".auto").join("review").join("log.txt"), "log\n")
            .expect("failed to write .auto file");
        fs::write(repo.join("bug").join("BUG_REPORT.md"), "# bug\n")
            .expect("failed to write bug report");

        let raw_status = run_git_in(&repo, ["status", "--short"]);
        assert!(
            !raw_status.trim().is_empty(),
            "raw git status should include generated output"
        );
        assert_eq!(
            checkpoint_status(&repo).expect("checkpoint status failed"),
            ""
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn checkpoint_stage_skips_autodev_generated_dirs() {
        let repo = init_repo("checkpoint-stage");
        fs::write(repo.join(".gitignore"), ".auto/\n").expect("failed to write .gitignore");
        run_git_in(&repo, ["add", ".gitignore"]);
        run_git_in(&repo, ["commit", "-m", "ignore auto"]);
        fs::create_dir_all(repo.join(".auto").join("review")).expect("failed to create .auto");
        fs::create_dir_all(repo.join("bug")).expect("failed to create bug dir");
        fs::create_dir_all(repo.join("src")).expect("failed to create src dir");
        fs::write(repo.join(".auto").join("review").join("log.txt"), "log\n")
            .expect("failed to write .auto file");
        fs::write(repo.join("bug").join("BUG_REPORT.md"), "# bug\n")
            .expect("failed to write bug report");
        fs::write(repo.join("README.md"), "# changed\n").expect("failed to update README");
        fs::write(repo.join("src").join("new.txt"), "new\n").expect("failed to write new file");

        stage_checkpoint_changes(&repo).expect("checkpoint add should succeed");

        let staged = run_git_in(&repo, ["diff", "--cached", "--name-only"]);
        assert_eq!(staged, "README.md\nsrc/new.txt\n");

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn truncate_file_to_max_bytes_keeps_tail() {
        let dir = temp_repo_path("truncate-file");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let path = dir.join("log.txt");
        fs::write(&path, b"abcdefghij").expect("failed to write log");

        truncate_file_to_max_bytes(&path, 4).expect("failed to truncate file");

        let text = fs::read_to_string(&path).expect("failed to read log");
        assert_eq!(text, "ghij");
        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn prune_old_entries_keeps_latest_paths() {
        let dir = temp_repo_path("prune-old-entries");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let first = dir.join("one.txt");
        let second = dir.join("two.txt");
        let third = dir.join("three.txt");

        fs::write(&first, "one").expect("failed to write first");
        sleep(Duration::from_millis(5));
        fs::write(&second, "two").expect("failed to write second");
        sleep(Duration::from_millis(5));
        fs::write(&third, "three").expect("failed to write third");

        prune_old_entries(&dir, 2).expect("failed to prune entries");

        assert!(!first.exists());
        assert!(second.exists());
        assert!(third.exists());
        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn sync_branch_with_remote_rebases_local_commits() {
        let (root, _remote, upstream, worker) =
            init_remote_and_clones("sync-remote-rebase", "trunk");

        fs::write(upstream.join("UPSTREAM.md"), "upstream\n").expect("failed to write upstream");
        run_git_in(&upstream, ["add", "UPSTREAM.md"]);
        run_git_in(&upstream, ["commit", "-m", "upstream change"]);
        run_git_in(&upstream, ["push", "origin", "trunk"]);

        fs::write(worker.join("WORKER.md"), "worker\n").expect("failed to write worker");
        run_git_in(&worker, ["add", "WORKER.md"]);
        run_git_in(&worker, ["commit", "-m", "worker change"]);

        let synced = sync_branch_with_remote(&worker, "trunk").expect("failed to sync branch");

        assert!(synced);
        assert!(worker.join("UPSTREAM.md").exists());
        assert!(worker.join("WORKER.md").exists());
        let log = run_git_in(&worker, ["log", "--format=%s", "-2"]);
        assert_eq!(log, "worker change\nupstream change\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn sync_branch_with_remote_preserves_dirty_worktree_with_autostash() {
        let (root, _remote, upstream, worker) =
            init_remote_and_clones("sync-remote-dirty", "trunk");

        fs::write(upstream.join("UPSTREAM.md"), "upstream\n").expect("failed to write upstream");
        run_git_in(&upstream, ["add", "UPSTREAM.md"]);
        run_git_in(&upstream, ["commit", "-m", "upstream change"]);
        run_git_in(&upstream, ["push", "origin", "trunk"]);

        fs::write(worker.join("README.md"), "# dirty\n").expect("failed to dirty README");

        let synced = sync_branch_with_remote(&worker, "trunk").expect("failed to sync branch");

        assert!(synced);
        assert!(worker.join("UPSTREAM.md").exists());
        let status = run_git_in(&worker, ["status", "--short"]);
        assert!(status.contains(" M README.md"));
        let readme = fs::read_to_string(worker.join("README.md")).expect("failed to read README");
        assert_eq!(readme, "# dirty\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn push_branch_with_remote_sync_rebases_then_pushes() {
        let (root, _remote, upstream, worker) = init_remote_and_clones("push-remote-sync", "trunk");

        fs::write(upstream.join("UPSTREAM.md"), "upstream\n").expect("failed to write upstream");
        run_git_in(&upstream, ["add", "UPSTREAM.md"]);
        run_git_in(&upstream, ["commit", "-m", "upstream change"]);
        run_git_in(&upstream, ["push", "origin", "trunk"]);

        fs::write(worker.join("WORKER.md"), "worker\n").expect("failed to write worker");
        run_git_in(&worker, ["add", "WORKER.md"]);
        run_git_in(&worker, ["commit", "-m", "worker change"]);

        let synced =
            push_branch_with_remote_sync(&worker, "trunk").expect("failed to push synced branch");

        assert!(synced);
        run_git_in(&upstream, ["fetch", "origin", "trunk"]);
        let log = run_git_in(&upstream, ["log", "--format=%s", "-2", "origin/trunk"]);
        assert_eq!(log, "worker change\nupstream change\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn atomic_write_removes_temp_file_after_rename_failure() {
        let dir = temp_repo_path("atomic-write-cleanup");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let target = dir.join("result.json");
        fs::create_dir_all(&target).expect("failed to create conflicting target directory");

        let err = atomic_write(&target, br#"{"ok":true}"#)
            .expect_err("renaming a file over a directory should fail");
        assert!(err.to_string().contains("failed to atomically replace"));

        let mut entries = fs::read_dir(&dir)
            .expect("failed to read temp dir")
            .map(|entry| {
                entry
                    .expect("failed to read temp dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, vec!["result.json".to_string()]);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }
}
