use std::fs;
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;
use std::{env, ffi::OsStr};

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

const AUTO_LOG_KEEP_FILES: usize = 64;
const AUTO_FRESH_INPUT_KEEP_ENTRIES: usize = 12;
const AUTO_QUEUE_RUN_KEEP_ENTRIES: usize = 12;
const PI_RUNTIME_LOG_KEEP_FILES: usize = 5;
const PI_RUNTIME_LOG_MAX_BYTES: usize = 2 * 1024 * 1024;
static ATOMIC_WRITE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckpointExcludeRule {
    Root(&'static str),
    PathPrefix(&'static str),
    TopLevelPrefix(&'static str),
}

impl CheckpointExcludeRule {
    fn git_pathspec(self) -> String {
        match self {
            Self::Root(root) => format!(":(exclude){root}"),
            Self::PathPrefix(prefix) => format!(":(exclude){prefix}*"),
            Self::TopLevelPrefix(prefix) => format!(":(exclude){prefix}*"),
        }
    }

    fn matches(self, path: &str) -> bool {
        let first_segment = path.split('/').next().unwrap_or(path);
        match self {
            Self::Root(root) => first_segment == root,
            Self::PathPrefix(prefix) => {
                let prefix = prefix.trim_end_matches('/');
                path == prefix || path.starts_with(&format!("{prefix}/"))
            }
            Self::TopLevelPrefix(prefix) => first_segment.starts_with(prefix),
        }
    }
}

const CHECKPOINT_EXCLUDE_RULES: [CheckpointExcludeRule; 5] = [
    CheckpointExcludeRule::Root(".auto"),
    CheckpointExcludeRule::PathPrefix(".claude/worktrees"),
    CheckpointExcludeRule::Root("bug"),
    CheckpointExcludeRule::Root("nemesis"),
    CheckpointExcludeRule::TopLevelPrefix("gen-"),
];

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
            git_failure_message(&output)
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
        git_failure_message(&output)
    );
}

fn checkpoint_status(repo_root: &Path) -> Result<String> {
    git_status_short_filtered(repo_root)
}

pub(crate) fn git_status_short_filtered(repo_root: &Path) -> Result<String> {
    let mut args = vec!["status", "--short", "--", "."];
    let excludes = checkpoint_exclude_pathspecs();
    args.extend(excludes.iter().map(String::as_str));
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
    ensure_checked_out_branch(repo_root, branch, "checkpoint")?;

    let status = checkpoint_status(repo_root)?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    stage_checkpoint_changes(repo_root)?;
    if !has_staged_changes(repo_root)? {
        eprintln!(
            "warning: pre-existing worktree changes did not produce stageable checkpoint changes; \
             continuing without checkpoint"
        );
        return Ok(None);
    }

    let message = format!("{}: {message_suffix}", repo_name(repo_root));
    run_git(repo_root, ["commit", "-m", &message])?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    let commit = commit.trim().to_string();
    if let Err(err) = push_branch_with_remote_sync(repo_root, branch) {
        bail!(
            "created checkpoint commit {} but failed to sync/push: {err}",
            commit
        );
    }
    Ok(Some(commit))
}

fn git_failure_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("exit status {}", output.status)
}

fn has_staged_changes(repo_root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["diff", "--cached", "--quiet", "--exit-code"])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(false);
    }
    if output.status.code() == Some(1) {
        return Ok(true);
    }
    bail!(
        "git command failed in {}: {}",
        repo_root.display(),
        git_failure_message(&output)
    );
}

pub(crate) fn sync_branch_with_remote(repo_root: &Path, branch: &str) -> Result<bool> {
    ensure_checked_out_branch(repo_root, branch, "sync")?;

    if skip_remote_sync() {
        eprintln!("warning: AUTO_SKIP_REMOTE_SYNC=1; skipping pull/rebase for branch `{branch}`");
        return Ok(false);
    }

    if !remote_branch_exists(repo_root, branch)? {
        return Ok(false);
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["pull", "--rebase", "--autostash", "origin", branch])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;

    if output.status.success() {
        return Ok(true);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no candidate for rebasing against")
        || stderr.contains("unrelated histories")
    {
        eprintln!(
            "warning: cannot rebase {branch} onto origin/{branch} \
             (unrelated histories); continuing without sync"
        );
        return Ok(false);
    }

    let aborted_conflicted_rebase = abort_rebase_if_in_progress(repo_root).unwrap_or(false);
    let conflict_note = if aborted_conflicted_rebase {
        " (aborted conflicted rebase and restored the local branch state)"
    } else {
        ""
    };

    bail!(
        "git command failed in {}: {}{}",
        repo_root.display(),
        git_failure_message(&output),
        conflict_note
    );
}

pub(crate) fn push_branch_with_remote_sync(repo_root: &Path, branch: &str) -> Result<bool> {
    ensure_checked_out_branch(repo_root, branch, "push")?;

    if skip_remote_sync() {
        eprintln!(
            "warning: AUTO_SKIP_REMOTE_SYNC=1; skipping remote sync/push for branch `{branch}`"
        );
        return Ok(false);
    }

    let mut synced = sync_branch_with_remote(repo_root, branch)?;
    let output = git_output(repo_root, ["push", "origin", branch])?;
    if output.status.success() {
        return Ok(synced);
    }
    if !is_non_fast_forward_push_failure(&output) {
        bail!(
            "git command failed in {}: {}",
            repo_root.display(),
            git_failure_message(&output)
        );
    }

    eprintln!(
        "warning: push of {branch} was rejected as non-fast-forward after sync; rebasing and retrying once"
    );
    synced |= sync_branch_with_remote(repo_root, branch)?;
    let retry = git_output(repo_root, ["push", "origin", branch])?;
    if !retry.status.success() {
        bail!(
            "git command failed in {}: {}",
            repo_root.display(),
            git_failure_message(&retry)
        );
    }
    Ok(synced)
}

fn skip_remote_sync() -> bool {
    env::var_os("AUTO_SKIP_REMOTE_SYNC").is_some_and(|value| value != OsStr::new(""))
}

fn git_output<'a>(repo_root: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))
}

fn ensure_checked_out_branch(repo_root: &Path, branch: &str, operation: &str) -> Result<()> {
    let branch = branch.trim();
    if branch.is_empty() {
        bail!("refusing to {operation} without a checked-out target branch");
    }

    let current = git_stdout(repo_root, ["branch", "--show-current"])?;
    let current = current.trim();
    if current.is_empty() {
        bail!("refusing to {operation} branch `{branch}` from detached HEAD");
    }
    if current != branch {
        bail!(
            "refusing to {operation} branch `{branch}` while checked out on `{current}`; \
             checkout `{branch}` or pass the current branch explicitly"
        );
    }
    Ok(())
}

fn is_non_fast_forward_push_failure(output: &Output) -> bool {
    let message = git_failure_message(output).to_ascii_lowercase();
    message.contains("non-fast-forward")
        || message.contains("fetch first")
        || message.contains("updates were rejected")
        || message.contains("incorrect old value provided")
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
            git_failure_message(&output)
        );
    }
    Ok(!output.stdout.is_empty())
}

fn abort_rebase_if_in_progress(repo_root: &Path) -> Result<bool> {
    let rebase_merge = git_stdout(repo_root, ["rev-parse", "--git-path", "rebase-merge"])?;
    let rebase_apply = git_stdout(repo_root, ["rev-parse", "--git-path", "rebase-apply"])?;
    let rebase_merge = resolve_git_path(repo_root, rebase_merge.trim());
    let rebase_apply = resolve_git_path(repo_root, rebase_apply.trim());
    if !rebase_merge.exists() && !rebase_apply.exists() {
        return Ok(false);
    }
    run_git(repo_root, ["rebase", "--abort"])?;
    Ok(true)
}

fn resolve_git_path(repo_root: &Path, git_path: &str) -> PathBuf {
    let path = PathBuf::from(git_path);
    if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    }
}

fn stage_checkpoint_changes(repo_root: &Path) -> Result<()> {
    let mut tracked_args = vec!["add", "-u", "--", "."];
    let excludes = checkpoint_exclude_pathspecs();
    tracked_args.extend(excludes.iter().map(String::as_str));
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
    CHECKPOINT_EXCLUDE_RULES
        .iter()
        .copied()
        .any(|rule| rule.matches(path))
}

fn checkpoint_exclude_pathspecs() -> Vec<String> {
    CHECKPOINT_EXCLUDE_RULES
        .iter()
        .copied()
        .map(CheckpointExcludeRule::git_pathspec)
        .collect()
}

pub(crate) fn ensure_repo_layout(repo_root: &Path) -> Result<()> {
    ensure_repo_layout_with(repo_root, prune_old_entries, prune_pi_runtime_state)
}

fn ensure_repo_layout_with<F, G>(
    repo_root: &Path,
    mut prune_entries: F,
    mut prune_pi_state: G,
) -> Result<()>
where
    F: FnMut(&Path, usize) -> Result<()>,
    G: FnMut(&Path) -> Result<()>,
{
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

    let mut failures = Vec::new();
    for (path, keep) in [
        (repo_root.join(".auto").join("logs"), AUTO_LOG_KEEP_FILES),
        (
            repo_root.join(".auto").join("fresh-input"),
            AUTO_FRESH_INPUT_KEEP_ENTRIES,
        ),
        (
            repo_root.join(".auto").join("queue-runs"),
            AUTO_QUEUE_RUN_KEEP_ENTRIES,
        ),
    ] {
        if let Err(err) = prune_entries(&path, keep) {
            eprintln!("warning: failed to prune {}: {err}", path.display());
            failures.push(format!("{}: {err}", path.display()));
        }
    }

    if let Err(err) = prune_pi_state(repo_root) {
        let agent_dir = opencode_agent_dir(repo_root);
        eprintln!(
            "warning: failed to prune PI runtime state in {}: {err}",
            agent_dir.display()
        );
        failures.push(format!("{}: {err}", agent_dir.display()));
    }
    if !failures.is_empty() {
        bail!(
            "failed to finish repo layout pruning:\n- {}",
            failures.join("\n- ")
        );
    }
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

#[cfg(unix)]
pub(crate) fn chmod_0o600_if_unix(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to set owner-only permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn chmod_0o600_if_unix(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn write_0o600_if_unix(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    if path.exists() {
        chmod_0o600_if_unix(path)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;
    chmod_0o600_if_unix(path)
}

#[cfg(not(unix))]
pub(crate) fn write_0o600_if_unix(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
pub(crate) fn test_process_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let temp = parent.join(format!(
        ".{}.tmp-{}-{}-{}",
        path.file_name().and_then(|v| v.to_str()).unwrap_or("write"),
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        ATOMIC_WRITE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&temp, bytes).map_err(|err| {
        atomic_write_failure(err, &temp, format!("failed to write {}", temp.display()))
    })?;
    fs::rename(&temp, path).map_err(|err| {
        atomic_write_failure(
            err,
            &temp,
            format!("failed to atomically replace {}", path.display()),
        )
    })?;
    Ok(())
}

fn atomic_write_failure(error: std::io::Error, temp: &Path, context: String) -> anyhow::Error {
    let cleanup_error = match fs::remove_file(temp) {
        Ok(()) => None,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => Some(err),
    };
    let mut message = context;
    if let Some(err) = cleanup_error {
        message.push_str(&format!(
            "; also failed to remove temp {}: {}",
            temp.display(),
            err
        ));
    }
    anyhow::Error::new(error).context(message)
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{mpsc, Arc, Barrier};
    use std::thread::sleep;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        atomic_write, auto_checkpoint_if_needed, checkpoint_status, chmod_0o600_if_unix,
        clip_line_for_display, ensure_repo_layout_with, is_checkpoint_excluded_path,
        prune_old_entries, push_branch_with_remote_sync, stage_checkpoint_changes,
        sync_branch_with_remote, truncate_file_to_max_bytes, write_0o600_if_unix, CLI_LONG_VERSION,
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

    #[test]
    fn cli_long_version_exposes_build_provenance_metadata() {
        let lines: Vec<_> = CLI_LONG_VERSION.lines().collect();

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], env!("CARGO_PKG_VERSION"));

        let commit = lines[1]
            .strip_prefix("commit: ")
            .expect("version should label the build commit");
        assert!(!commit.trim().is_empty());

        let dirty = lines[2]
            .strip_prefix("dirty: ")
            .expect("version should label the dirty-state flag");
        assert!(
            matches!(dirty, "clean" | "dirty" | "unknown"),
            "unexpected dirty-state flag: {dirty}"
        );

        let profile = lines[3]
            .strip_prefix("profile: ")
            .expect("version should label the cargo build profile");
        assert!(
            matches!(profile, "debug" | "release" | "unknown"),
            "unexpected build profile: {profile}"
        );
    }

    fn write_repo_file(repo: &Path, path: &str, contents: &str) {
        let path = repo.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent dir");
        }
        fs::write(path, contents).expect("failed to write repo file");
    }

    fn seed_tracked_checkpoint_excluded_files(repo: &Path) {
        for path in [
            ".auto/review/log.txt",
            ".claude/worktrees/agent-a123/README.md",
            "bug/BUG_REPORT.md",
            "nemesis/nemesis-audit.md",
            "gen-001/SPEC.md",
        ] {
            write_repo_file(repo, path, "initial\n");
            run_git_in(repo, ["add", "-f", path]);
        }
        run_git_in(repo, ["commit", "-m", "seed excluded files"]);

        for path in [
            ".auto/review/log.txt",
            ".claude/worktrees/agent-a123/README.md",
            "bug/BUG_REPORT.md",
            "nemesis/nemesis-audit.md",
            "gen-001/SPEC.md",
        ] {
            write_repo_file(repo, path, "changed\n");
        }
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
        seed_tracked_checkpoint_excluded_files(&repo);

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
        seed_tracked_checkpoint_excluded_files(&repo);
        fs::create_dir_all(repo.join("src")).expect("failed to create src dir");
        fs::write(repo.join("README.md"), "# changed\n").expect("failed to update README");
        fs::write(repo.join("src").join("new.txt"), "new\n").expect("failed to write new file");

        stage_checkpoint_changes(&repo).expect("checkpoint add should succeed");

        let staged = run_git_in(&repo, ["diff", "--cached", "--name-only"]);
        assert_eq!(staged, "README.md\nsrc/new.txt\n");

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn checkpoint_exclusion_rules_cover_all_generated_paths() {
        assert_checkpoint_excludes_generated_and_runtime_paths();
    }

    #[test]
    fn checkpoint_excludes_generated_and_runtime_paths() {
        assert_checkpoint_excludes_generated_and_runtime_paths();
    }

    fn assert_checkpoint_excludes_generated_and_runtime_paths() {
        for excluded in [
            ".auto",
            ".auto/logs/run.log",
            ".claude/worktrees",
            ".claude/worktrees/agent-a123",
            "bug",
            "bug/BUG_REPORT.md",
            "nemesis",
            "nemesis/nemesis-audit.md",
            "gen-123",
            "gen-123/spec.md",
        ] {
            assert!(
                is_checkpoint_excluded_path(excluded),
                "{excluded} should be excluded"
            );
        }
        for included in [
            "",
            "README.md",
            "src/main.rs",
            "generated/output.md",
            "notes/gen-plan.md",
        ] {
            assert!(
                !is_checkpoint_excluded_path(included),
                "{included} should stay stageable"
            );
        }
    }

    #[test]
    fn checkpoint_status_matches_stageable_changes() {
        let repo = init_repo("checkpoint-consistency");
        fs::write(repo.join(".gitignore"), ".auto/\n").expect("failed to write .gitignore");
        run_git_in(&repo, ["add", ".gitignore"]);
        run_git_in(&repo, ["commit", "-m", "ignore auto"]);
        fs::create_dir_all(repo.join(".auto").join("review")).expect("failed to create .auto");
        fs::create_dir_all(repo.join("bug")).expect("failed to create bug dir");
        fs::create_dir_all(repo.join("gen-001")).expect("failed to create gen dir");
        fs::write(repo.join(".auto").join("review").join("log.txt"), "log\n")
            .expect("failed to write .auto file");
        fs::write(repo.join("bug").join("BUG_REPORT.md"), "# bug\n")
            .expect("failed to write bug report");
        fs::write(repo.join("gen-001").join("SPEC.md"), "# generated\n")
            .expect("failed to write gen spec");
        fs::write(repo.join("README.md"), "# changed\n").expect("failed to update README");
        fs::write(repo.join("new.txt"), "new\n").expect("failed to write new file");

        let stageable = checkpoint_status(&repo).expect("checkpoint status failed");
        let expected = stageable
            .lines()
            .map(|line| line[3..].to_string())
            .collect::<Vec<_>>();
        stage_checkpoint_changes(&repo).expect("checkpoint add should succeed");
        let staged = run_git_in(&repo, ["diff", "--cached", "--name-only"]);
        let actual = staged.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(actual, expected);

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn auto_checkpoint_skips_dirty_submodule_when_nothing_is_stageable() {
        let source = init_repo("checkpoint-dirty-submodule-source");
        let repo = init_repo("checkpoint-dirty-submodule-super");
        fs::create_dir_all(repo.join(".claude").join("worktrees"))
            .expect("failed to create submodule parent");

        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                source.to_str().expect("source path utf-8"),
                "vendor/agent-a",
            ])
            .output()
            .expect("failed to launch git submodule add");
        assert!(
            output.status.success(),
            "submodule add failed: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        run_git_in(&repo, ["commit", "-m", "add submodule"]);

        fs::write(repo.join("vendor/agent-a/README.md"), "# dirty submodule\n")
            .expect("failed to dirty submodule");
        let status = checkpoint_status(&repo).expect("checkpoint status should see submodule dirt");
        assert!(status.contains("vendor/agent-a"));

        let checkpoint = auto_checkpoint_if_needed(&repo, "master", "auto parallel checkpoint")
            .expect("dirty submodule should not abort checkpointing");
        assert_eq!(checkpoint, None);
        assert_eq!(run_git_in(&repo, ["diff", "--cached", "--name-only"]), "");

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
        fs::remove_dir_all(&source).expect("failed to remove temp source repo");
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
    fn auto_checkpoint_if_needed_commits_untracked_changes_before_remote_sync() {
        let (root, _remote, upstream, worker) =
            init_remote_and_clones("checkpoint-untracked-sync", "trunk");

        fs::write(upstream.join("UPSTREAM.md"), "upstream\n").expect("failed to write upstream");
        run_git_in(&upstream, ["add", "UPSTREAM.md"]);
        run_git_in(&upstream, ["commit", "-m", "upstream change"]);
        run_git_in(&upstream, ["push", "origin", "trunk"]);

        fs::create_dir_all(worker.join("notes")).expect("failed to create notes dir");
        fs::write(worker.join("notes").join("draft.md"), "draft\n")
            .expect("failed to write local draft");

        let commit = auto_checkpoint_if_needed(&worker, "trunk", "auto loop checkpoint")
            .expect("checkpoint should succeed")
            .expect("checkpoint commit should be created");

        assert!(!commit.is_empty());
        assert!(worker.join("UPSTREAM.md").exists());
        assert!(worker.join("notes").join("draft.md").exists());
        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        let log = run_git_in(&worker, ["log", "--format=%s", "-2"]);
        assert_eq!(log, "worker: auto loop checkpoint\nupstream change\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn auto_checkpoint_if_needed_refuses_branch_mismatch_before_commit() {
        let repo = init_repo("checkpoint-branch-mismatch");
        let target_branch = run_git_in(&repo, ["branch", "--show-current"])
            .trim()
            .to_string();
        run_git_in(&repo, ["checkout", "-b", "feature"]);
        fs::write(repo.join("README.md"), "# dirty\n").expect("failed to dirty README");
        let head_before = run_git_in(&repo, ["rev-parse", "HEAD"]);

        let err = auto_checkpoint_if_needed(&repo, &target_branch, "auto bug checkpoint")
            .expect_err("checkpoint should refuse branch mismatch before committing");

        assert!(err.to_string().contains("refusing to checkpoint branch"));
        assert_eq!(run_git_in(&repo, ["rev-parse", "HEAD"]), head_before);
        assert!(run_git_in(&repo, ["status", "--short"]).contains(" M README.md"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn auto_checkpoint_if_needed_aborts_conflicted_rebase_and_reports_checkpoint_commit() {
        let (root, _remote, upstream, worker) =
            init_remote_and_clones("checkpoint-conflict-sync", "trunk");

        fs::write(upstream.join("README.md"), "upstream change\n")
            .expect("failed to write upstream change");
        run_git_in(&upstream, ["add", "README.md"]);
        run_git_in(&upstream, ["commit", "-m", "upstream readme change"]);
        run_git_in(&upstream, ["push", "origin", "trunk"]);

        fs::write(worker.join("README.md"), "worker change\n").expect("failed to write worker");

        let err = auto_checkpoint_if_needed(&worker, "trunk", "auto loop checkpoint")
            .expect_err("checkpoint sync should report the rebase conflict");

        assert!(err.to_string().contains("created checkpoint commit"));
        assert!(err.to_string().contains("aborted conflicted rebase"));
        assert_eq!(run_git_in(&worker, ["branch", "--show-current"]), "trunk\n");
        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        let readme = fs::read_to_string(worker.join("README.md")).expect("failed to read README");
        assert_eq!(readme, "worker change\n");
        let log = run_git_in(&worker, ["log", "--format=%s", "-1"]);
        assert_eq!(log, "worker: auto loop checkpoint\n");

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

    #[cfg(unix)]
    #[test]
    fn push_branch_with_remote_sync_retries_non_fast_forward_push_race() {
        let (root, _remote, upstream, worker) = init_remote_and_clones("push-race-retry", "trunk");

        fs::write(worker.join("WORKER.md"), "worker\n").expect("failed to write worker");
        run_git_in(&worker, ["add", "WORKER.md"]);
        run_git_in(&worker, ["commit", "-m", "worker change"]);

        let marker = root.join("push-race-fired");
        let hook = worker.join(".git").join("hooks").join("pre-push");
        let script = format!(
            r#"#!/bin/sh
set -eu
if [ ! -f "{marker}" ]; then
  touch "{marker}"
  printf 'race\n' > "{upstream}/RACE.md"
  git -C "{upstream}" add RACE.md
  git -C "{upstream}" commit -m "race change"
  git -C "{upstream}" push origin trunk
fi
"#,
            marker = marker.display(),
            upstream = upstream.display()
        );
        fs::write(&hook, script).expect("failed to write pre-push hook");
        let mut permissions = fs::metadata(&hook)
            .expect("failed to stat pre-push hook")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).expect("failed to chmod pre-push hook");

        let synced =
            push_branch_with_remote_sync(&worker, "trunk").expect("push race should be retried");

        assert!(synced);
        assert!(marker.exists());
        run_git_in(&upstream, ["fetch", "origin", "trunk"]);
        let log = run_git_in(&upstream, ["log", "--format=%s", "-3", "origin/trunk"]);
        assert_eq!(log, "worker change\nrace change\ninit\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn atomic_write_works_outside_git_repo() {
        let dir = temp_repo_path("atomic-write-non-git");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let target = dir.join("state.json");
        let payload = br#"{"outside":"git"}"#;

        assert!(
            !dir.join(".git").exists(),
            "fixture should stay outside a git repo"
        );

        atomic_write(&target, payload).expect("atomic write should succeed outside a git repo");

        let written = fs::read(&target).expect("failed to read atomic write output");
        assert_eq!(written, payload);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn atomic_write_creates_missing_parent_dir() {
        let dir = temp_repo_path("atomic-write-missing-parent");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let target = dir.join("nested").join("missing").join("result.json");
        let parent = target.parent().expect("target should have a parent");
        let payload = br#"{"created":"parent"}"#;

        assert!(!parent.exists(), "parent should start missing");

        atomic_write(&target, payload).expect("atomic write should create missing parents");

        assert!(
            parent.is_dir(),
            "atomic write should create the parent directory"
        );
        let written = fs::read(&target).expect("failed to read atomic write output");
        assert_eq!(written, payload);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn atomic_write_handles_rapid_succession_collisions() {
        let dir = temp_repo_path("atomic-write-collision");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let target = dir.join("state.json");
        let concurrent_writers = 3usize;
        let start = Arc::new(Barrier::new(concurrent_writers + 1));
        let (done_tx, done_rx) = mpsc::channel();
        let mut handles = Vec::new();

        for writer in 0..concurrent_writers {
            let start = Arc::clone(&start);
            let done_tx = done_tx.clone();
            let target = target.clone();
            handles.push(std::thread::spawn(move || -> anyhow::Result<()> {
                let mut payload = format!("writer-{writer}:").into_bytes();
                payload.resize(128 * 1024, b'a' + writer as u8);

                start.wait();
                let result = atomic_write(&target, &payload);
                done_tx
                    .send(())
                    .expect("failed to signal concurrent writer completion");
                result
            }));
        }
        drop(done_tx);

        let final_payload = {
            let mut payload = b"writer-final:".to_vec();
            payload.resize(128 * 1024, b'z');
            payload
        };
        let start_for_final = Arc::clone(&start);
        let target_for_final = target.clone();
        handles.push(std::thread::spawn(move || -> anyhow::Result<()> {
            start_for_final.wait();
            for _ in 0..concurrent_writers {
                done_rx
                    .recv()
                    .expect("failed to wait for concurrent writer completion");
            }
            atomic_write(&target_for_final, &final_payload)
        }));

        for handle in handles {
            handle
                .join()
                .expect("writer thread should not panic")
                .expect("all atomic writes should succeed");
        }

        let written = fs::read(&target).expect("failed to read atomic write output");
        let mut expected = b"writer-final:".to_vec();
        expected.resize(128 * 1024, b'z');
        assert_eq!(written, expected);

        let temp_files = fs::read_dir(&dir)
            .expect("failed to read temp dir")
            .map(|entry| {
                entry
                    .expect("failed to read temp dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.starts_with(".state.json.tmp-"))
            .collect::<Vec<_>>();
        assert!(
            temp_files.is_empty(),
            "unexpected temp files after concurrent writes: {temp_files:?}"
        );

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
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

    #[cfg(unix)]
    #[test]
    fn chmod_0o600_if_unix_sets_owner_only_mode() {
        let dir = temp_repo_path("chmod-0600");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let path = dir.join("credentials.json");
        fs::write(&path, br#"{"token":"secret"}"#).expect("failed to seed credential file");

        let mut permissions = fs::metadata(&path)
            .expect("failed to stat credential file")
            .permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&path, permissions).expect("failed to loosen credential permissions");

        chmod_0o600_if_unix(&path).expect("chmod helper should succeed");

        let mode = fs::metadata(&path)
            .expect("failed to stat credential file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn write_0o600_if_unix_tightens_existing_file_before_write() {
        let dir = temp_repo_path("write-0600");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let path = dir.join("credentials.json");
        fs::write(&path, br#"{"token":"old"}"#).expect("failed to seed credential file");

        let mut permissions = fs::metadata(&path)
            .expect("failed to stat credential file")
            .permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&path, permissions).expect("failed to loosen credential permissions");

        write_0o600_if_unix(&path, br#"{"token":"new"}"#).expect("owner-only write should succeed");

        assert_eq!(
            fs::read(&path).expect("failed to read credential file"),
            br#"{"token":"new"}"#
        );
        let mode = fs::metadata(&path)
            .expect("failed to stat credential file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_leaves_no_temp_file_after_write_failure() {
        let dir = temp_repo_path("atomic-write-write-failure");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let original_permissions = fs::metadata(&dir)
            .expect("failed to stat temp dir")
            .permissions();
        let readonly_permissions = PermissionsExt::from_mode(0o500);
        fs::set_permissions(&dir, readonly_permissions).expect("failed to lock temp dir");

        let target = dir.join("result.json");
        let err = atomic_write(&target, br#"{"ok":true}"#)
            .expect_err("writing inside a non-writable directory should fail");
        assert!(err.to_string().contains("failed to write"));

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
        assert!(entries.is_empty(), "unexpected leftovers: {entries:?}");

        fs::set_permissions(&dir, original_permissions).expect("failed to unlock temp dir");
        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn ensure_repo_layout_collects_all_prune_failures() {
        let repo = temp_repo_path("ensure-repo-layout");
        let mut prune_calls = Vec::new();
        let mut pi_calls = 0usize;

        let err = ensure_repo_layout_with(
            &repo,
            |path, keep| {
                prune_calls.push((path.to_path_buf(), keep));
                match keep {
                    64 => anyhow::bail!("logs failure"),
                    12 if path.ends_with("queue-runs") => anyhow::bail!("queue failure"),
                    _ => Ok(()),
                }
            },
            |_repo_root| {
                pi_calls += 1;
                anyhow::bail!("pi failure")
            },
        )
        .expect_err("prune failures should bubble up after all attempts");

        assert_eq!(pi_calls, 1);
        assert_eq!(prune_calls.len(), 3);
        assert!(
            prune_calls[0].0.ends_with(".auto/logs"),
            "first prune should target logs"
        );
        assert!(
            prune_calls[1].0.ends_with(".auto/fresh-input"),
            "second prune should target fresh-input"
        );
        assert!(
            prune_calls[2].0.ends_with(".auto/queue-runs"),
            "third prune should target queue-runs"
        );
        let message = err.to_string();
        assert!(message.contains(".auto/logs"));
        assert!(message.contains("logs failure"));
        assert!(message.contains(".auto/queue-runs"));
        assert!(message.contains("queue failure"));
        assert!(message.contains(".auto/opencode-data/opencode"));
        assert!(message.contains("pi failure"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }
}
