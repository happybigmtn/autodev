use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

    stage_checkpoint_changes(repo_root)?;
    let message = format!("{}: {message_suffix}", repo_name(repo_root));
    run_git(repo_root, ["commit", "-m", &message])?;
    run_git(repo_root, ["push", "origin", branch])?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    Ok(Some(commit.trim().to_string()))
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
    fs::rename(&temp, path)
        .with_context(|| format!("failed to atomically replace {}", path.display()))?;
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{checkpoint_status, clip_line_for_display, stage_checkpoint_changes};

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
}
