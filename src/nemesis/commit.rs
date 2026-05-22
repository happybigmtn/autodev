//! Commit the nemesis output artifacts (output dir, root spec, root plan) with
//! index isolation so unrelated staged changes survive a failed commit.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::util::{git_stdout, push_branch_with_remote_sync, repo_name, run_git, timestamp_slug};

pub(crate) fn commit_nemesis_outputs_if_needed(
    repo_root: &Path,
    branch: &str,
    output_dir: &Path,
    root_spec: &Path,
    root_plan: &Path,
) -> Result<Option<String>> {
    let pathspecs = nemesis_commit_pathspecs(repo_root, output_dir, root_spec, root_plan);
    if pathspecs.is_empty() {
        return Ok(None);
    }

    let mut status_args = vec!["status", "--short", "--"];
    status_args.extend(pathspecs.iter().map(String::as_str));
    let status = git_stdout(repo_root, status_args)?;
    if status.trim().is_empty() {
        return Ok(None);
    }

    let mut snapshot_args = vec!["diff", "--cached", "--binary", "--"];
    snapshot_args.extend(pathspecs.iter().map(String::as_str));
    let staged_snapshot = git_stdout(repo_root, snapshot_args)?;
    let message = format!("{}: record nemesis outputs", repo_name(repo_root));
    let commit_result = (|| -> Result<()> {
        let mut add_args = vec!["add", "--all", "--"];
        add_args.extend(pathspecs.iter().map(String::as_str));
        run_git(repo_root, add_args)?;

        let mut commit_args = vec!["commit", "-m", &message, "--"];
        commit_args.extend(pathspecs.iter().map(String::as_str));
        run_git(repo_root, commit_args)?;
        Ok(())
    })();
    if let Err(error) = commit_result {
        restore_nemesis_commit_index(repo_root, &pathspecs, &staged_snapshot)
            .context("failed to restore index after Nemesis output commit error")?;
        return Err(error);
    }

    push_branch_with_remote_sync(repo_root, branch)?;
    let commit = git_stdout(repo_root, ["rev-parse", "HEAD"])?;
    Ok(Some(commit.trim().to_string()))
}

fn nemesis_commit_pathspecs(
    repo_root: &Path,
    output_dir: &Path,
    root_spec: &Path,
    root_plan: &Path,
) -> Vec<String> {
    let mut pathspecs = Vec::<String>::new();
    push_unique_pathspec(&mut pathspecs, repo_relative_path(repo_root, output_dir));
    if let Some(relative_output_dir) = repo_relative_path(repo_root, output_dir) {
        pathspecs.push(format!(":(exclude){relative_output_dir}/codex.stderr.log"));
    }
    push_unique_pathspec(&mut pathspecs, repo_relative_path(repo_root, root_spec));
    push_unique_pathspec(&mut pathspecs, repo_relative_path(repo_root, root_plan));
    pathspecs
}

fn push_unique_pathspec(pathspecs: &mut Vec<String>, candidate: Option<String>) {
    let Some(candidate) = candidate else {
        return;
    };
    if !pathspecs.iter().any(|existing| existing == &candidate) {
        pathspecs.push(candidate);
    }
}

fn repo_relative_path(repo_root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(repo_root).ok()?;
    let display = relative.to_string_lossy().replace('\\', "/");
    if display.is_empty() {
        return None;
    }
    Some(display)
}

fn restore_nemesis_commit_index(
    repo_root: &Path,
    pathspecs: &[String],
    staged_snapshot: &str,
) -> Result<()> {
    let mut reset_args = vec!["reset", "--"];
    reset_args.extend(pathspecs.iter().map(String::as_str));
    run_git(repo_root, reset_args)?;
    if staged_snapshot.trim().is_empty() {
        return Ok(());
    }

    let patch_path = std::env::temp_dir().join(format!(
        "autodev-nemesis-index-{}-{}.patch",
        std::process::id(),
        timestamp_slug()
    ));
    fs::write(&patch_path, staged_snapshot)
        .with_context(|| format!("failed to write {}", patch_path.display()))?;
    let patch_path_text = patch_path.display().to_string();
    let apply_result = run_git(repo_root, ["apply", "--cached", &patch_path_text]);
    let cleanup_result = fs::remove_file(&patch_path);
    if let Err(error) = apply_result {
        cleanup_result.with_context(|| format!("failed to remove {}", patch_path.display()))?;
        return Err(error);
    }
    cleanup_result.with_context(|| format!("failed to remove {}", patch_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::commit_nemesis_outputs_if_needed;

    fn temp_repo_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "autodev-nemesis-{name}-{}-{nonce}",
            std::process::id()
        ))
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

    fn init_remote_and_worker(name: &str, branch: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
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
                remote.to_str().expect("remote path should be utf-8"),
            ],
        );
        run_git_in(
            &root,
            [
                "clone",
                remote.to_str().expect("remote path should be utf-8"),
                upstream.to_str().expect("upstream path should be utf-8"),
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
                remote.to_str().expect("remote path should be utf-8"),
                worker.to_str().expect("worker path should be utf-8"),
            ],
        );
        run_git_in(&worker, ["config", "user.name", "autodev tests"]);
        run_git_in(&worker, ["config", "user.email", "autodev@example.com"]);
        (root, remote, upstream, worker)
    }

    #[test]
    fn output_commit_ignores_preexisting_staged_changes() {
        let (root, _remote, _upstream, worker) = init_remote_and_worker("commit-isolation", "main");
        let output_dir = worker.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        fs::create_dir_all(worker.join("specs")).expect("failed to create specs dir");
        fs::create_dir_all(worker.join("src")).expect("failed to create src dir");
        fs::write(output_dir.join("nemesis-audit.md"), "# Specification:\n")
            .expect("failed to write audit");
        fs::write(
            output_dir.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n",
        )
        .expect("failed to write nemesis plan");
        fs::write(worker.join("specs").join("nemesis.md"), "# spec\n")
            .expect("failed to write root spec");
        fs::write(
            worker.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
        )
        .expect("failed to write root plan");
        fs::write(worker.join("src").join("lib.rs"), "pub fn untouched() {}\n")
            .expect("failed to write unrelated file");
        run_git_in(&worker, ["add", "src/lib.rs"]);

        let commit = commit_nemesis_outputs_if_needed(
            &worker,
            "main",
            &output_dir,
            &worker.join("specs").join("nemesis.md"),
            &worker.join("IMPLEMENTATION_PLAN.md"),
        )
        .expect("output commit should succeed")
        .expect("output commit should produce a commit");
        assert!(!commit.is_empty());

        let committed = run_git_in(&worker, ["show", "--name-only", "--format=", "HEAD"]);
        assert!(committed.contains("nemesis/nemesis-audit.md"));
        assert!(committed.contains("nemesis/IMPLEMENTATION_PLAN.md"));
        assert!(committed.contains("specs/nemesis.md"));
        assert!(committed.contains("IMPLEMENTATION_PLAN.md"));
        assert!(!committed.contains("src/lib.rs"));

        let staged = run_git_in(&worker, ["diff", "--cached", "--name-only"]);
        assert_eq!(staged, "src/lib.rs\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repos");
    }

    #[test]
    fn output_commit_restores_index_after_commit_failure() {
        let repo = init_repo("commit-rollback");
        let output_dir = repo.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        fs::create_dir_all(repo.join("specs")).expect("failed to create specs dir");
        fs::create_dir_all(repo.join("src")).expect("failed to create src dir");
        fs::write(output_dir.join("nemesis-audit.md"), "# Specification:\n")
            .expect("failed to write audit");
        fs::write(
            output_dir.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n",
        )
        .expect("failed to write nemesis plan");
        fs::write(repo.join("specs").join("nemesis.md"), "# spec\n")
            .expect("failed to write root spec");
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
        )
        .expect("failed to write root plan");
        fs::write(repo.join("src").join("lib.rs"), "pub fn staged() {}\n")
            .expect("failed to write unrelated file");
        run_git_in(&repo, ["add", "src/lib.rs"]);
        run_git_in(&repo, ["config", "user.useConfigOnly", "true"]);
        run_git_in(&repo, ["config", "--unset", "user.name"]);
        run_git_in(&repo, ["config", "--unset", "user.email"]);
        let branch = run_git_in(&repo, ["branch", "--show-current"]);

        let error = commit_nemesis_outputs_if_needed(
            &repo,
            branch.trim(),
            &output_dir,
            &repo.join("specs").join("nemesis.md"),
            &repo.join("IMPLEMENTATION_PLAN.md"),
        )
        .expect_err("commit should fail without user identity")
        .to_string();
        assert!(error.contains("git command failed"));

        let staged = run_git_in(&repo, ["diff", "--cached", "--name-only"]);
        assert_eq!(staged, "src/lib.rs\n");
        let status = run_git_in(&repo, ["status", "--short"]);
        assert!(status.contains("A  src/lib.rs"));
        assert!(output_dir.join("IMPLEMENTATION_PLAN.md").exists());
        assert!(output_dir.join("nemesis-audit.md").exists());
        assert!(repo.join("specs").join("nemesis.md").exists());
        assert!(repo.join("IMPLEMENTATION_PLAN.md").exists());

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }
}
