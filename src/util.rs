use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Utc;

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

pub(crate) fn git_tracked_status(repo_root: &Path) -> Result<String> {
    git_stdout(
        repo_root,
        ["status", "--short", "--untracked-files=no", "--ignored=no"],
    )
}

pub(crate) fn ensure_tracked_worktree_clean(repo_root: &Path, command_name: &str) -> Result<()> {
    let status = git_tracked_status(repo_root)?;
    if status.trim().is_empty() {
        return Ok(());
    }
    bail!(
        "`{command_name}` requires a clean tracked worktree before it starts:\n{}",
        status.trim_end()
    );
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
    use super::clip_line_for_display;

    #[test]
    fn clips_on_char_boundaries() {
        let line = "╔══════════════════╗";
        let clipped = clip_line_for_display(line, 6);
        assert_eq!(clipped, "╔═════");
    }
}
