use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};

use crate::util::repo_name;

/// Primary branch names auto treats as the repo's integration branch when no
/// explicit branch is requested. Shared by `auto ship` base-branch resolution
/// and `auto loop` branch selection.
pub(crate) const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckpointExcludeRule {
    Exact(&'static str),
    Root(&'static str),
    PathPrefix(&'static str),
    TopLevelPrefix(&'static str),
}

impl CheckpointExcludeRule {
    fn git_pathspec(self) -> String {
        match self {
            Self::Exact(path) => format!(":(exclude){path}"),
            Self::Root(root) => format!(":(exclude){root}"),
            Self::PathPrefix(prefix) => format!(":(exclude){prefix}*"),
            Self::TopLevelPrefix(prefix) => format!(":(exclude){prefix}*"),
        }
    }

    fn matches(self, path: &str) -> bool {
        let first_segment = path.split('/').next().unwrap_or(path);
        match self {
            Self::Exact(exact) => path == exact,
            Self::Root(root) => first_segment == root,
            Self::PathPrefix(prefix) => {
                let prefix = prefix.trim_end_matches('/');
                path == prefix || path.starts_with(&format!("{prefix}/"))
            }
            Self::TopLevelPrefix(prefix) => first_segment.starts_with(prefix),
        }
    }
}

const CHECKPOINT_EXCLUDE_RULES: [CheckpointExcludeRule; 11] = [
    CheckpointExcludeRule::Root(".auto"),
    CheckpointExcludeRule::PathPrefix(".claude/worktrees"),
    CheckpointExcludeRule::Exact("audit/AUDIT-PROGRESS.md"),
    CheckpointExcludeRule::PathPrefix("audit/finding-resolution"),
    CheckpointExcludeRule::PathPrefix("audit/logs"),
    CheckpointExcludeRule::Exact("audit/MANIFEST.json"),
    CheckpointExcludeRule::Exact("audit/live.log"),
    CheckpointExcludeRule::PathPrefix("audit/files"),
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

/// Strip an `origin/` prefix from a symbolic-ref short name. Shared by ship and
/// loop branch resolution.
pub(crate) fn parse_origin_head_branch(origin_head: &str) -> Option<String> {
    let trimmed = origin_head.trim();
    let branch = trimmed.strip_prefix("origin/").unwrap_or(trimmed).trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

/// Return true when `branch` exists as a local head or an origin remote branch.
pub(crate) fn git_branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_ref_exists(repo_root, &format!("refs/heads/{branch}"))
        || git_ref_exists(repo_root, &format!("refs/remotes/origin/{branch}"))
}

/// Return true when the given fully-qualified git ref resolves.
pub(crate) fn git_ref_exists(repo_root: &Path, git_ref: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show-ref", "--verify", "--quiet", git_ref])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        auto_checkpoint_if_needed, checkpoint_status, is_checkpoint_excluded_path,
        parse_origin_head_branch, push_branch_with_remote_sync, stage_checkpoint_changes,
        sync_branch_with_remote,
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
            "audit/AUDIT-PROGRESS.md",
            "audit/MANIFEST.json",
            "audit/files/deadbeef/prompt.md",
            "audit/files/deadbeef/response.log",
            "audit/files/deadbeef/verdict.json",
            "audit/live.log",
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
            "audit/AUDIT-PROGRESS.md",
            "audit/MANIFEST.json",
            "audit/files/deadbeef/prompt.md",
            "audit/files/deadbeef/response.log",
            "audit/files/deadbeef/verdict.json",
            "audit/live.log",
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
    fn parses_origin_head_branch() {
        assert_eq!(
            parse_origin_head_branch("origin/trunk"),
            Some("trunk".to_string())
        );
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
            "audit/AUDIT-PROGRESS.md",
            "audit/MANIFEST.json",
            "audit/files",
            "audit/files/deadbeef/prompt.md",
            "audit/live.log",
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
            "audit/everything/20260424-115535/FINAL-REVIEW.md",
            "audit/everything/20260424-115535/RUN-STATUS.md",
            "audit/some-durable-report.md",
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
}
