use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::task_parser::{parse_tasks, TaskStatus};
use crate::util::{active_plan_path, active_plan_relative, repo_name};

/// Primary branch names auto treats as the repo's integration branch when no
/// explicit branch is requested. Shared by `auto ship` base-branch resolution
/// and `auto loop` branch selection.
pub(crate) const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];

const GIT_INDEX_LOCK_RETRY_DELAYS: [Duration; 6] = [
    Duration::from_millis(10),
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
];

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

const CHECKPOINT_EXCLUDE_RULES: [CheckpointExcludeRule; 12] = [
    CheckpointExcludeRule::Root(".auto"),
    CheckpointExcludeRule::Exact(".auto-review-input-quarantine.json"),
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
        // Read-only Git commands such as `status` may otherwise refresh the
        // index and briefly create `.git/index.lock`. Status polling is allowed
        // to run beside the canonical landing host, so those optional writes
        // must never contend with host-owned queue reconciliation.
        .env("GIT_OPTIONAL_LOCKS", "0")
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
    let args = args.into_iter().collect::<Vec<_>>();
    for attempt in 0..=GIT_INDEX_LOCK_RETRY_DELAYS.len() {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(&args)
            .output()
            .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
        if output.status.success() {
            return Ok(());
        }
        let detail = git_failure_message(&output);
        if transient_git_index_lock_failure(&detail) {
            if let Some(delay) = GIT_INDEX_LOCK_RETRY_DELAYS.get(attempt) {
                thread::sleep(*delay);
                continue;
            }
        }
        bail!("git command failed in {}: {}", repo_root.display(), detail);
    }
    unreachable!("bounded Git retry loop always returns or reports its final error")
}

fn transient_git_index_lock_failure(detail: &str) -> bool {
    detail.contains("index.lock")
        && (detail.contains("File exists") || detail.contains("Unable to create"))
}

pub(crate) fn git_cherry_pick_empty_arg() -> &'static str {
    static ARG: OnceLock<&'static str> = OnceLock::new();
    ARG.get_or_init(|| {
        let output = Command::new("git").args(["cherry-pick", "-h"]).output();
        let Ok(output) = output else {
            return "--keep-redundant-commits";
        };
        let help = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        git_cherry_pick_empty_arg_from_help(&help)
    })
}

fn git_cherry_pick_empty_arg_from_help(help: &str) -> &'static str {
    if help.contains("--empty=") || help.contains("--empty <") || help.contains("--empty (") {
        "--empty=drop"
    } else {
        "--keep-redundant-commits"
    }
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
    refuse_unsealed_task_completion_checkpoint(repo_root)?;

    stage_checkpoint_changes(repo_root)?;
    refuse_checkpoint_excluded_staged_paths(repo_root, "generic checkpoint")?;
    if !has_staged_changes(repo_root)? {
        eprintln!(
            "warning: pre-existing worktree changes did not produce stageable checkpoint changes; \
             continuing without checkpoint"
        );
        return Ok(None);
    }

    let message = format!("{}: {message_suffix}", repo_name(repo_root));
    let commit = commit_staged_checkpoint_cas(repo_root, branch, &message)?;
    if let Err(err) = push_branch_with_remote_sync(repo_root, branch) {
        bail!(
            "created checkpoint commit {} but failed to sync/push: {err}",
            commit
        );
    }
    Ok(Some(commit))
}

/// Create a generic checkpoint from the exact index tree without running any
/// repository hooks, then publish it to the checked-out branch with a
/// compare-and-swap.
///
/// `git commit` is intentionally not used for host authority commits. Hooks
/// can mutate and stage additional bytes after the host's final scope checks.
/// `commit-tree` consumes the already-written tree object directly, and
/// `update-ref <ref> <new> <old>` refuses publication when another writer moved
/// the branch after the parent was captured.
pub(crate) fn commit_staged_checkpoint_cas(
    repo_root: &Path,
    branch: &str,
    message: &str,
) -> Result<String> {
    isolated_commit_from_index_cas(
        repo_root,
        Some(branch),
        message,
        false,
        |parent, candidate| {
            validate_generic_checkpoint_commit_transition(repo_root, parent, candidate)
        },
    )
}

/// Branch-derived generic host commit used by queue closeouts that are not
/// minting a task-completion receipt.
pub(crate) fn commit_staged_queue_checkpoint_cas(
    repo_root: &Path,
    message: &str,
    allow_empty: bool,
) -> Result<String> {
    isolated_commit_from_index_cas(
        repo_root,
        None,
        message,
        allow_empty,
        |parent, candidate| {
            validate_generic_checkpoint_commit_transition(repo_root, parent, candidate)
        },
    )
}

#[derive(Debug)]
struct IndexTreeSnapshot {
    branch_ref: String,
    parent: String,
    tree: String,
}

/// Opaque closeout candidate captured from one immutable index tree and
/// validated against the saved branch parent before any receipt footer is
/// minted. Later index writes cannot alter the tree object this token names.
#[derive(Debug)]
pub(crate) struct ValidatedTaskCloseoutTree {
    snapshot: IndexTreeSnapshot,
    task_id: String,
}

pub(crate) fn capture_validated_task_closeout_tree(
    repo_root: &Path,
    task_id: &str,
    allowed_paths: &[&str],
    allow_empty: bool,
) -> Result<ValidatedTaskCloseoutTree> {
    let snapshot = capture_index_tree_snapshot(repo_root, None, allow_empty)?;
    validate_task_closeout_tree_transition(repo_root, &snapshot, task_id, allowed_paths)?;
    Ok(ValidatedTaskCloseoutTree {
        snapshot,
        task_id: task_id.to_string(),
    })
}

/// Publish a previously validated closeout tree. The caller may generate the
/// exact receipt-bearing message after capture without reopening the mutable
/// repository index as candidate input.
pub(crate) fn commit_validated_task_closeout_tree_cas(
    repo_root: &Path,
    validated: ValidatedTaskCloseoutTree,
    message: &str,
) -> Result<String> {
    let task_id = validated.task_id;
    isolated_commit_from_snapshot_cas(
        repo_root,
        validated.snapshot,
        message,
        |parent, candidate| {
            validate_task_closeout_commit_transition(repo_root, parent, candidate, &task_id)
        },
    )
}

/// Global checkpoint interlock for the parallel completion transaction.
///
/// Every command-level checkpoint funnels through this function, including the
/// pre-tmux startup checkpoint. A newly-Done row relative to `HEAD` may only be
/// committed by the receipt-bearing closeout path, never a generic checkpoint.
/// This comparison does not depend on process-local hold files, so it also
/// covers a crash before a failing gate had time to record one.
pub(crate) fn refuse_unsealed_task_completion_checkpoint(repo_root: &Path) -> Result<()> {
    let views = completion_plan_views(repo_root)?;

    let mut newly_done = Vec::new();
    for (view, plan) in [
        ("worktree", views.worktree.as_str()),
        ("index", views.indexed.as_str()),
    ] {
        for (contract, excess) in excess_completed_task_contracts(&views.head, plan) {
            let head_has_task_id = parse_tasks(&views.head)
                .into_iter()
                .any(|task| task.status == TaskStatus::Done && task.id == contract.id);
            let transition = if head_has_task_id {
                "Done contract changed or multiplicity increased"
            } else {
                "newly completed relative to HEAD"
            };
            newly_done.push(format!(
                "`{}` ({view} {transition}; excess contract count {excess})",
                contract.id
            ));
        }
    }
    newly_done.sort();
    newly_done.dedup();
    if !newly_done.is_empty() {
        bail!(
            "refusing generic checkpoint of an unsealed task completion transition; use the host closeout path: {}",
            newly_done.join(", ")
        );
    }

    Ok(())
}

pub(crate) fn unsealed_task_completion_ids(repo_root: &Path) -> Result<Vec<String>> {
    let views = completion_plan_views(repo_root)?;
    let mut ids = excess_completed_task_contracts(&views.head, &views.worktree)
        .into_iter()
        .chain(excess_completed_task_contracts(&views.head, &views.indexed))
        .map(|(contract, _)| contract.id)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Closeout-only companion to the generic checkpoint interlock.
///
/// A receipt-bearing closeout may seal either no new Done row (legacy empty
/// footer backfill) or one exact Done contract for `allowed_task_id`, provided
/// the worktree and index carry the same transition. Any other task,
/// multiplicity increase, or view mismatch is refused before `git commit`.
pub(crate) fn refuse_unsealed_task_completion_transitions_except(
    repo_root: &Path,
    allowed_task_id: &str,
) -> Result<()> {
    let views = completion_plan_views(repo_root)?;
    validate_task_closeout_plan_transition(
        &views.head,
        &views.worktree,
        &views.indexed,
        allowed_task_id,
    )
}

fn validate_task_closeout_plan_transition(
    head: &str,
    worktree: &str,
    indexed: &str,
    allowed_task_id: &str,
) -> Result<()> {
    let head_target = unique_task_contract(head, allowed_task_id, "HEAD")?;
    let worktree_target = unique_task_contract(worktree, allowed_task_id, "worktree")?;
    let indexed_target = unique_task_contract(indexed, allowed_task_id, "index")?;
    if worktree_target.status != TaskStatus::Done || indexed_target.status != TaskStatus::Done {
        bail!(
            "refusing closeout for `{allowed_task_id}` because both worktree and index must carry \
             its Done row"
        );
    }
    if head_target.status == TaskStatus::Done {
        if worktree_target.markdown != head_target.markdown
            || indexed_target.markdown != head_target.markdown
        {
            bail!(
                "refusing closeout for already-Done `{allowed_task_id}` because its completed \
                 task contract changed relative to HEAD"
            );
        }
    } else {
        let head_contract = status_neutral_task_markdown(&head_target.markdown)?;
        if status_neutral_task_markdown(&worktree_target.markdown)? != head_contract
            || status_neutral_task_markdown(&indexed_target.markdown)? != head_contract
        {
            bail!(
                "refusing closeout for `{allowed_task_id}` because its task contract changed \
                 while transitioning to Done"
            );
        }
    }

    let head_other_contracts = task_contract_counts_without(head, allowed_task_id);
    for (view, plan) in [("worktree", worktree), ("index", indexed)] {
        if task_contract_counts_without(plan, allowed_task_id) != head_other_contracts {
            let changed_ids = changed_task_contract_ids(head, plan, allowed_task_id).join(", ");
            bail!(
                "refusing closeout for `{allowed_task_id}` because {view} changes another task's \
                 status, contract, presence, or multiplicity relative to HEAD: {changed_ids}"
            );
        }
    }

    let worktree_excess = excess_completed_task_contracts(head, worktree);
    let indexed_excess = excess_completed_task_contracts(head, indexed);
    let unexpected = worktree_excess
        .iter()
        .chain(indexed_excess.iter())
        .filter(|(contract, _)| contract.id != allowed_task_id)
        .map(|(contract, excess)| format!("`{}` (excess contract count {excess})", contract.id))
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        bail!(
            "refusing closeout for `{allowed_task_id}` with another task's unsealed Done \
             transition: {}",
            unexpected.join(", ")
        );
    }
    if worktree_excess != indexed_excess {
        bail!(
            "refusing closeout for `{allowed_task_id}` because worktree and index carry \
             different unsealed Done contracts"
        );
    }
    let allowed_excess = worktree_excess
        .iter()
        .map(|(_, count)| *count)
        .sum::<usize>();
    if allowed_excess > 1 {
        bail!(
            "refusing closeout for `{allowed_task_id}` because its Done contract multiplicity \
             increased by {allowed_excess}; exactly one task transition is allowed"
        );
    }
    Ok(())
}

fn task_contract_counts_without(plan: &str, excluded_task_id: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for task in parse_tasks(plan)
        .into_iter()
        .filter(|task| task.id != excluded_task_id)
    {
        *counts.entry(task.markdown).or_insert(0) += 1;
    }
    counts
}

fn changed_task_contract_ids(head: &str, candidate: &str, excluded_task_id: &str) -> Vec<String> {
    let counts = |plan: &str| {
        let mut by_id = BTreeMap::<String, BTreeMap<String, usize>>::new();
        for task in parse_tasks(plan)
            .into_iter()
            .filter(|task| task.id != excluded_task_id)
        {
            *by_id
                .entry(task.id)
                .or_default()
                .entry(task.markdown)
                .or_insert(0) += 1;
        }
        by_id
    };
    let head = counts(head);
    let candidate = counts(candidate);
    let mut ids = head
        .keys()
        .chain(candidate.keys())
        .filter(|id| head.get(*id) != candidate.get(*id))
        .map(|id| format!("`{id}`"))
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn unique_task_contract(
    plan: &str,
    task_id: &str,
    view: &str,
) -> Result<crate::task_parser::PlanTask> {
    let matching = parse_tasks(plan)
        .into_iter()
        .filter(|task| task.id == task_id)
        .collect::<Vec<_>>();
    let [task] = matching.as_slice() else {
        bail!(
            "refusing closeout for `{task_id}` because {view} contains {} matching task rows; \
             expected exactly one",
            matching.len()
        );
    };
    Ok(task.clone())
}

fn status_neutral_task_markdown(markdown: &str) -> Result<String> {
    let mut lines = markdown.lines();
    let header = lines.next().context("task contract has no header")?;
    let (_, header_body) = header
        .split_once("] ")
        .context("task contract header has no status marker")?;
    let mut normalized = format!("- [?] {header_body}");
    for line in lines {
        normalized.push('\n');
        normalized.push_str(line);
    }
    Ok(normalized)
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CompletedTaskContract {
    id: String,
    markdown: String,
}

struct CompletionPlanViews {
    head: String,
    worktree: String,
    indexed: String,
}

fn completion_plan_views(repo_root: &Path) -> Result<CompletionPlanViews> {
    let plan_relative = active_plan_relative(repo_root);
    let head_object = format!("HEAD:{plan_relative}");
    let index_object = format!(":{plan_relative}");
    let head = git_plan_blob_or_empty(repo_root, &head_object, true)?;
    let worktree_path = active_plan_path(repo_root);
    let worktree = match std::fs::read_to_string(&worktree_path) {
        Ok(plan) => plan,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", worktree_path.display()))
        }
    };
    let indexed = git_plan_blob_or_empty(repo_root, &index_object, false)?;
    Ok(CompletionPlanViews {
        head,
        worktree,
        indexed,
    })
}

fn git_plan_blob_or_empty(repo_root: &Path, object: &str, committed: bool) -> Result<String> {
    let plan_relative = object
        .rsplit_once(':')
        .map(|(_, path)| path)
        .unwrap_or(object);
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show", object])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if output.status.success() {
        return String::from_utf8(output.stdout).context("git plan blob was not valid UTF-8");
    }

    let absence = if committed {
        Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["cat-file", "-e", object])
            .output()
    } else {
        Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["ls-files", "--error-unmatch", "--", plan_relative])
            .output()
    }
    .with_context(|| format!("failed to inspect plan presence in {}", repo_root.display()))?;
    if !absence.status.success() {
        return Ok(String::new());
    }
    bail!(
        "failed to read `{object}` in {}: {}",
        repo_root.display(),
        git_failure_message(&output)
    )
}

fn completed_task_contract_counts(plan: &str) -> BTreeMap<CompletedTaskContract, usize> {
    let mut counts = BTreeMap::new();
    for task in parse_tasks(plan)
        .into_iter()
        .filter(|task| task.status == TaskStatus::Done)
    {
        *counts
            .entry(CompletedTaskContract {
                id: task.id,
                markdown: task.markdown,
            })
            .or_insert(0) += 1;
    }
    counts
}

fn excess_completed_task_contracts(
    head_plan: &str,
    candidate_plan: &str,
) -> Vec<(CompletedTaskContract, usize)> {
    let head = completed_task_contract_counts(head_plan);
    completed_task_contract_counts(candidate_plan)
        .into_iter()
        .filter_map(|(contract, candidate_count)| {
            let head_count = head.get(&contract).copied().unwrap_or(0);
            candidate_count
                .checked_sub(head_count)
                .filter(|excess| *excess > 0)
                .map(|excess| (contract, excess))
        })
        .collect()
}

fn isolated_commit_from_index_cas<F>(
    repo_root: &Path,
    expected_branch: Option<&str>,
    message: &str,
    allow_empty: bool,
    validate_transition: F,
) -> Result<String>
where
    F: FnOnce(&str, &str) -> Result<()>,
{
    let snapshot = capture_index_tree_snapshot(repo_root, expected_branch, allow_empty)?;
    isolated_commit_from_snapshot_cas(repo_root, snapshot, message, validate_transition)
}

fn capture_index_tree_snapshot(
    repo_root: &Path,
    expected_branch: Option<&str>,
    allow_empty: bool,
) -> Result<IndexTreeSnapshot> {
    if let Some(branch) = expected_branch {
        ensure_checked_out_branch(repo_root, branch, "create isolated commit on")?;
    }

    let branch_ref = git_stdout(repo_root, ["symbolic-ref", "--quiet", "HEAD"])
        .context("refusing isolated commit from detached HEAD")?;
    let branch_ref = branch_ref.trim().to_string();
    if !branch_ref.starts_with("refs/heads/") {
        bail!("refusing isolated commit because HEAD resolves to non-branch ref `{branch_ref}`");
    }
    if let Some(branch) = expected_branch {
        let expected_ref = format!("refs/heads/{}", branch.trim());
        if branch_ref != expected_ref {
            bail!(
                "refusing isolated commit for branch `{branch}` while HEAD resolves to \
                 `{branch_ref}`"
            );
        }
    }

    let parent = git_stdout(repo_root, ["rev-parse", "--verify", "HEAD"])?;
    let parent = parent.trim().to_string();
    let branch_tip = git_stdout(repo_root, ["rev-parse", "--verify", &branch_ref])?;
    if branch_tip.trim() != parent {
        bail!(
            "refusing isolated commit because HEAD `{parent}` and `{branch_ref}` `{}` disagree",
            branch_tip.trim()
        );
    }
    let tree = git_stdout(repo_root, ["write-tree"])?;
    let tree = tree.trim().to_string();
    if !allow_empty {
        let parent_tree = git_stdout(
            repo_root,
            ["rev-parse", "--verify", &format!("{parent}^{{tree}}")],
        )?;
        if tree == parent_tree.trim() {
            bail!("refusing isolated commit because the index has no staged changes");
        }
    }
    Ok(IndexTreeSnapshot {
        branch_ref,
        parent,
        tree,
    })
}

fn isolated_commit_from_snapshot_cas<F>(
    repo_root: &Path,
    snapshot: IndexTreeSnapshot,
    message: &str,
    validate_transition: F,
) -> Result<String>
where
    F: FnOnce(&str, &str) -> Result<()>,
{
    let IndexTreeSnapshot {
        branch_ref,
        parent,
        tree,
    } = snapshot;
    let canonical_message = message.trim_end_matches('\n');
    if canonical_message.trim().is_empty() {
        bail!("refusing isolated commit with an empty commit message");
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["commit-tree", &tree, "-p", &parent, "-F", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to launch git commit-tree in {}",
                repo_root.display()
            )
        })?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .context("git commit-tree stdin was unavailable")?;
        stdin
            .write_all(canonical_message.as_bytes())
            .context("failed to write isolated commit message")?;
        stdin
            .write_all(b"\n")
            .context("failed to terminate isolated commit message")?;
    }
    let output = child
        .wait_with_output()
        .context("failed waiting for git commit-tree")?;
    if !output.status.success() {
        bail!(
            "git commit-tree failed in {}: {}",
            repo_root.display(),
            git_failure_message(&output)
        );
    }
    let candidate =
        String::from_utf8(output.stdout).context("git commit-tree output was not UTF-8")?;
    let candidate = candidate.trim().to_string();
    validate_isolated_commit_object(repo_root, &candidate, &parent, &tree, canonical_message)?;
    validate_transition(&parent, &candidate)?;

    let update = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "update-ref",
            "-m",
            "autodev isolated authority commit",
            &branch_ref,
            &candidate,
            &parent,
        ])
        .output()
        .with_context(|| format!("failed to launch git update-ref in {}", repo_root.display()))?;
    if !update.status.success() {
        bail!(
            "refusing to publish isolated commit {candidate}: `{branch_ref}` moved away from \
             saved parent {parent}: {}",
            git_failure_message(&update)
        );
    }

    let published = git_stdout(repo_root, ["rev-parse", "--verify", &branch_ref])?;
    if published.trim() != candidate {
        bail!(
            "isolated commit {candidate} was published, but `{branch_ref}` moved concurrently to \
             {}; refusing remote push",
            published.trim()
        );
    }
    Ok(candidate)
}

fn validate_task_closeout_tree_transition(
    repo_root: &Path,
    snapshot: &IndexTreeSnapshot,
    task_id: &str,
    allowed_paths: &[&str],
) -> Result<()> {
    let changed = git_stdout(
        repo_root,
        [
            "diff",
            "--name-only",
            "-z",
            &snapshot.parent,
            &snapshot.tree,
            "--",
        ],
    )?;
    let outside = changed
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| !allowed_paths.contains(path))
        .collect::<Vec<_>>();
    if !outside.is_empty() {
        bail!(
            "refusing closeout for `{task_id}` because immutable candidate tree contains path(s) outside host queue authority: {}",
            outside.join(", ")
        );
    }

    let plan_relative = active_plan_relative(repo_root);
    let parent_plan = git_plan_blob_or_empty(
        repo_root,
        &format!("{}:{plan_relative}", snapshot.parent),
        true,
    )?;
    let candidate_plan = git_plan_blob_or_empty(
        repo_root,
        &format!("{}:{plan_relative}", snapshot.tree),
        true,
    )?;
    validate_task_closeout_plan_transition(&parent_plan, &candidate_plan, &candidate_plan, task_id)
}

fn validate_isolated_commit_object(
    repo_root: &Path,
    candidate: &str,
    expected_parent: &str,
    expected_tree: &str,
    expected_message: &str,
) -> Result<()> {
    let object = git_stdout(repo_root, ["cat-file", "-p", candidate])
        .with_context(|| format!("failed to inspect isolated commit candidate `{candidate}`"))?;
    let (headers, message) = object
        .split_once("\n\n")
        .context("isolated commit candidate had no message separator")?;
    let tree = headers
        .lines()
        .find_map(|line| line.strip_prefix("tree "))
        .context("isolated commit candidate had no tree header")?;
    if tree != expected_tree {
        bail!(
            "isolated commit candidate tree `{tree}` did not match intended index tree \
             `{expected_tree}`"
        );
    }
    let parents = headers
        .lines()
        .filter_map(|line| line.strip_prefix("parent "))
        .collect::<Vec<_>>();
    if parents != [expected_parent] {
        bail!(
            "isolated commit candidate parents {:?} did not match saved parent \
             `{expected_parent}`",
            parents
        );
    }
    let expected_message = format!("{expected_message}\n");
    if message != expected_message {
        bail!("isolated commit candidate message did not match the exact intended message");
    }
    Ok(())
}

fn validate_generic_checkpoint_commit_transition(
    repo_root: &Path,
    parent: &str,
    candidate: &str,
) -> Result<()> {
    let plan_relative = active_plan_relative(repo_root);
    let parent_plan =
        git_plan_blob_or_empty(repo_root, &format!("{parent}:{plan_relative}"), true)?;
    let candidate_plan =
        git_plan_blob_or_empty(repo_root, &format!("{candidate}:{plan_relative}"), true)?;
    let excess = excess_completed_task_contracts(&parent_plan, &candidate_plan);
    if !excess.is_empty() {
        let tasks = excess
            .into_iter()
            .map(|(contract, count)| format!("`{}` (excess contract count {count})", contract.id))
            .collect::<Vec<_>>();
        bail!(
            "refusing generic checkpoint candidate with an unsealed task completion transition: {}",
            tasks.join(", ")
        );
    }
    Ok(())
}

fn validate_task_closeout_commit_transition(
    repo_root: &Path,
    parent: &str,
    candidate: &str,
    task_id: &str,
) -> Result<()> {
    let plan_relative = active_plan_relative(repo_root);
    let parent_plan =
        git_plan_blob_or_empty(repo_root, &format!("{parent}:{plan_relative}"), true)?;
    let candidate_plan =
        git_plan_blob_or_empty(repo_root, &format!("{candidate}:{plan_relative}"), true)?;
    validate_task_closeout_plan_transition(&parent_plan, &candidate_plan, &candidate_plan, task_id)
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

fn staged_paths(repo_root: &Path) -> Result<Vec<String>> {
    let staged = git_stdout(repo_root, ["diff", "--cached", "--name-only", "-z", "--"])?;
    Ok(staged
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

fn refuse_checkpoint_excluded_staged_paths(repo_root: &Path, operation: &str) -> Result<()> {
    let excluded = staged_paths(repo_root)?
        .into_iter()
        .filter(|path| is_checkpoint_excluded_path(path))
        .collect::<Vec<_>>();
    if !excluded.is_empty() {
        bail!(
            "refusing {operation} because excluded runtime path(s) were already staged: {}",
            excluded.join(", ")
        );
    }
    Ok(())
}

/// Refuse publication/authority commits while any non-allowlisted repository
/// state is staged, unstaged, or untracked. Ignored runtime files are omitted
/// by Git's `--exclude-standard`; everything else must be committed through its
/// owning seam before a queue-only closeout can claim the source was sealed.
pub(crate) fn refuse_worktree_paths_outside(
    repo_root: &Path,
    allowed_paths: &[&str],
    operation: &str,
) -> Result<()> {
    let mut dirty = staged_paths(repo_root)?;
    for args in [
        vec!["diff", "--name-only", "-z", "--"],
        vec!["ls-files", "--others", "--exclude-standard", "-z", "--"],
    ] {
        let paths = git_stdout(repo_root, args)?;
        dirty.extend(
            paths
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_string),
        );
    }
    dirty.sort();
    dirty.dedup();
    let outside = dirty
        .into_iter()
        .filter(|path| !allowed_paths.iter().any(|allowed| path == allowed))
        .collect::<Vec<_>>();
    if !outside.is_empty() {
        bail!(
            "refusing {operation} because dirty path(s) are outside its authority: {}",
            outside.join(", ")
        );
    }
    Ok(())
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
        // A concurrent writer can move the remote ref *during* our push, after
        // git has already computed its compare-and-swap expectation. The remote
        // then reports a stale ref lock ("cannot lock ref ...: is at X but
        // expected Y" / "failed to update ref") rather than a plain
        // non-fast-forward. This is the same retryable race: re-sync and retry.
        || message.contains("cannot lock ref")
        || message.contains("failed to update ref")
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
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        auto_checkpoint_if_needed, capture_validated_task_closeout_tree, checkpoint_status,
        commit_staged_checkpoint_cas, commit_validated_task_closeout_tree_cas,
        completion_plan_views, git_cherry_pick_empty_arg_from_help, is_checkpoint_excluded_path,
        parse_origin_head_branch, push_branch_with_remote_sync,
        refuse_checkpoint_excluded_staged_paths,
        refuse_unsealed_task_completion_transitions_except, run_git, stage_checkpoint_changes,
        sync_branch_with_remote, transient_git_index_lock_failure,
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
    fn cherry_pick_empty_arg_prefers_drop_when_supported() {
        let help = "usage: git cherry-pick [--empty <stop|drop|keep>] <commit>...";
        assert_eq!(git_cherry_pick_empty_arg_from_help(help), "--empty=drop");
    }

    #[test]
    fn run_git_retries_a_brief_index_lock_without_removing_it() {
        let repo = init_repo("brief-index-lock-retry");
        fs::write(repo.join("README.md"), "# changed\n").expect("write changed README");
        let lock = repo.join(".git/index.lock");
        fs::write(&lock, "held by another Git process\n").expect("create index lock");
        let release_lock = lock.clone();
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            fs::remove_file(release_lock).expect("release index lock");
        });

        run_git(&repo, ["add", "README.md"])
            .expect("brief optional-lock contention should be retried");
        releaser.join().expect("lock releaser should finish");
        assert_eq!(
            run_git_in(&repo, ["diff", "--cached", "--name-only"]),
            "README.md\n"
        );
        fs::remove_dir_all(repo).expect("cleanup repo");
    }

    #[test]
    fn index_lock_retry_classifier_rejects_unrelated_git_failures() {
        assert!(transient_git_index_lock_failure(
            "fatal: Unable to create '/repo/.git/index.lock': File exists."
        ));
        assert!(!transient_git_index_lock_failure(
            "fatal: pathspec 'missing' did not match any files"
        ));
        assert!(!transient_git_index_lock_failure(
            "fatal: index.lock contains protected recovery data"
        ));
    }

    #[test]
    fn cherry_pick_empty_arg_falls_back_for_git_243_help() {
        let help = "--[no-]keep-redundant-commits keep redundant, empty commits";
        assert_eq!(
            git_cherry_pick_empty_arg_from_help(help),
            "--keep-redundant-commits"
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
    fn completion_plan_views_prefer_focused_plan_md() {
        let repo = init_repo("focused-completion-plan-views");
        fs::write(repo.join("PLAN.md"), "- [~] `TASK-PLAN-1` active\n").expect("write active plan");
        run_git_in(&repo, ["add", "PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "add focused plan"]);

        let views = completion_plan_views(&repo).expect("read focused plan views");
        assert!(views.head.contains("TASK-PLAN-1"));
        assert!(views.worktree.contains("TASK-PLAN-1"));
        assert!(views.indexed.contains("TASK-PLAN-1"));

        fs::remove_dir_all(repo).expect("cleanup");
    }

    #[test]
    fn generic_checkpoint_refuses_unsealed_done_transition_without_a_hold() {
        let repo = init_repo("checkpoint-unsealed-done");
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-CLOSEOUT` pending durable closeout\n",
        )
        .expect("write partial plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "seed partial task"]);
        let head = run_git_in(&repo, ["rev-parse", "HEAD"]);

        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-CLOSEOUT` pending durable closeout\n",
        )
        .expect("write unsafe done plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        let error = auto_checkpoint_if_needed(&repo, "master", "auto parallel checkpoint")
            .expect_err("generic checkpoint must not seal a task completion");

        assert!(
            format!("{error:#}").contains("TASK-CLOSEOUT"),
            "error should identify the held task: {error:#}"
        );
        assert_eq!(head, run_git_in(&repo, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn generic_checkpoint_refuses_mutated_completed_task_contract() {
        let repo = init_repo("checkpoint-mutated-done-contract");
        let original_plan = "\
- [x] `TASK-CLOSED` Completed contract
  Verification: `cargo test original_contract`
  Dependencies: none
";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), original_plan)
            .expect("write original completed contract");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "seed completed contract"]);
        let head = run_git_in(&repo, ["rev-parse", "HEAD"]);

        let mutated_plan = original_plan.replace("original_contract", "mutated_contract");
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), mutated_plan)
            .expect("write mutated completed contract");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);

        let error = auto_checkpoint_if_needed(&repo, "master", "auto parallel checkpoint")
            .expect_err("generic checkpoint must not seal a mutated Done contract");
        let detail = format!("{error:#}");
        assert!(detail.contains("TASK-CLOSED"), "{detail}");
        assert!(
            detail.contains("contract") || detail.contains("unsealed"),
            "{detail}"
        );
        assert_eq!(head, run_git_in(&repo, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn generic_checkpoint_refuses_duplicate_completed_task_contract() {
        let repo = init_repo("checkpoint-duplicate-done-contract");
        let completed_task = "\
- [x] `TASK-CLOSED` Completed contract
  Verification: `cargo test completed_contract`
  Dependencies: none
";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), completed_task)
            .expect("write original completed contract");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "seed completed contract"]);
        let head = run_git_in(&repo, ["rev-parse", "HEAD"]);

        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            format!("{completed_task}\n{completed_task}"),
        )
        .expect("write duplicate completed contract");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);

        let error = auto_checkpoint_if_needed(&repo, "master", "auto parallel checkpoint")
            .expect_err("generic checkpoint must not seal a duplicate Done contract");
        let detail = format!("{error:#}");
        assert!(detail.contains("TASK-CLOSED"), "{detail}");
        assert!(
            detail.contains("duplicate") || detail.contains("multiplicity"),
            "{detail}"
        );
        assert_eq!(head, run_git_in(&repo, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn closeout_scope_refuses_contract_mutation_while_marking_task_done() {
        let repo = init_repo("closeout-mutated-transition-contract");
        let partial_plan = "\
- [~] `TASK-CLOSEOUT` Stable contract
  Verification: `cargo test original_contract`
  Dependencies: none
";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), partial_plan).expect("write partial plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "seed partial contract"]);

        let mutated_done = partial_plan
            .replace("- [~]", "- [x]")
            .replace("original_contract", "mutated_contract");
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), mutated_done)
            .expect("write mutated Done contract");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);

        let error = refuse_unsealed_task_completion_transitions_except(&repo, "TASK-CLOSEOUT")
            .expect_err("closeout must allow only a status-only transition");
        assert!(
            format!("{error:#}").contains("contract changed"),
            "{error:#}"
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn closeout_scope_refuses_mutation_of_already_completed_contract() {
        let repo = init_repo("closeout-mutated-legacy-done-contract");
        let done_plan = "\
- [x] `TASK-CLOSEOUT` Stable completed contract
  Verification: `cargo test original_contract`
  Dependencies: none
";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), done_plan).expect("write Done plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "seed completed contract"]);

        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            done_plan.replace("original_contract", "mutated_contract"),
        )
        .expect("write mutated completed contract");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);

        let error = refuse_unsealed_task_completion_transitions_except(&repo, "TASK-CLOSEOUT")
            .expect_err("legacy Done closeout must not alter its completed contract");
        assert!(
            format!("{error:#}").contains("contract changed"),
            "{error:#}"
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn closeout_scope_refuses_another_tasks_non_done_status_transition() {
        let repo = init_repo("closeout-other-partial-transition");
        let plan = "\
- [~] `TASK-CLOSEOUT` Target contract
  Verification: `cargo test target`
  Dependencies: none

- [ ] `TASK-OTHER` Other contract
  Verification: `cargo test other`
  Dependencies: none
";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("write seed plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "seed two task contracts"]);

        let changed = plan
            .replace("- [~] `TASK-CLOSEOUT`", "- [x] `TASK-CLOSEOUT`")
            .replace("- [ ] `TASK-OTHER`", "- [~] `TASK-OTHER`");
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), changed)
            .expect("write cross-task status change");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);

        let error = refuse_unsealed_task_completion_transitions_except(&repo, "TASK-CLOSEOUT")
            .expect_err("target closeout must not absorb another task's non-Done transition");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("another task"), "{rendered}");
        assert!(rendered.contains("status"), "{rendered}");
        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn closeout_scope_refuses_another_tasks_contract_edit() {
        let repo = init_repo("closeout-other-contract-edit");
        let plan = "\
- [~] `TASK-CLOSEOUT` Target contract
  Verification: `cargo test target`
  Dependencies: none

- [ ] `TASK-OTHER` Other contract
  Verification: `cargo test other`
  Dependencies: none
";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("write seed plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "seed two task contracts"]);

        let changed = plan
            .replace("- [~] `TASK-CLOSEOUT`", "- [x] `TASK-CLOSEOUT`")
            .replace("cargo test other", "cargo test injected_other");
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), changed)
            .expect("write cross-task contract edit");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);

        let error = refuse_unsealed_task_completion_transitions_except(&repo, "TASK-CLOSEOUT")
            .expect_err("target closeout must not absorb another task's contract edit");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("another task"), "{rendered}");
        assert!(rendered.contains("contract"), "{rendered}");
        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn generic_checkpoint_refuses_prestaged_excluded_runtime_path() {
        let repo = init_repo("checkpoint-prestaged-runtime");
        let head = run_git_in(&repo, ["rev-parse", "HEAD"]);
        fs::create_dir_all(repo.join(".auto")).expect("create runtime directory");
        fs::write(repo.join(".auto/secret"), "must not be checkpointed\n")
            .expect("write runtime secret");
        run_git_in(&repo, ["add", "-f", ".auto/secret"]);
        fs::write(repo.join("README.md"), "# legitimate checkpoint change\n")
            .expect("dirty README");

        let error = auto_checkpoint_if_needed(&repo, "master", "auto parallel checkpoint")
            .expect_err("generic checkpoint must refuse pre-staged excluded runtime state");
        let detail = format!("{error:#}");
        assert!(detail.contains(".auto/secret"), "{detail}");
        assert!(
            detail.contains("excluded") || detail.contains("staged"),
            "{detail}"
        );
        assert_eq!(head, run_git_in(&repo, ["rev-parse", "HEAD"]));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[cfg(unix)]
    #[test]
    fn generic_checkpoint_uses_exact_index_tree_without_running_commit_hooks() {
        let repo = init_repo("checkpoint-isolated-hooks");
        let parent = run_git_in(&repo, ["rev-parse", "HEAD"]).trim().to_string();
        let hooks = repo.join(".git/hooks");
        for (name, sentinel) in [
            ("pre-commit", "pre-commit-ran"),
            ("prepare-commit-msg", "prepare-commit-msg-ran"),
        ] {
            let hook = hooks.join(name);
            fs::write(
                &hook,
                format!(
                    "#!/bin/sh\n\
                     printf '%s\\n' ran > {sentinel}\n\
                     printf '%s\\n' '- [x] `TASK-HOOK` unsafe hook completion' > \
                     IMPLEMENTATION_PLAN.md\n\
                     git add IMPLEMENTATION_PLAN.md {sentinel}\n"
                ),
            )
            .expect("write hostile hook");
            let mut permissions = fs::metadata(&hook).expect("stat hook").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&hook, permissions).expect("make hook executable");
        }
        fs::write(repo.join("README.md"), "# safe checkpoint\n").expect("dirty README");

        stage_checkpoint_changes(&repo).expect("stage exact checkpoint tree");
        refuse_checkpoint_excluded_staged_paths(&repo, "generic checkpoint")
            .expect("staged tree has no excluded paths");
        let message = format!(
            "{}: auto parallel checkpoint",
            repo.file_name()
                .and_then(|name| name.to_str())
                .expect("repo basename is UTF-8")
        );
        let candidate = commit_staged_checkpoint_cas(&repo, "master", &message)
            .expect("isolated checkpoint should succeed");

        assert_eq!(
            run_git_in(&repo, ["rev-parse", "HEAD^"]).trim(),
            parent,
            "candidate parent must be the captured branch tip"
        );
        assert_eq!(run_git_in(&repo, ["rev-parse", "HEAD"]).trim(), candidate);
        assert_eq!(
            run_git_in(&repo, ["rev-parse", "HEAD^{tree}"]).trim(),
            run_git_in(&repo, ["write-tree"]).trim(),
            "candidate must contain the exact intended index tree"
        );
        assert_eq!(
            run_git_in(&repo, ["log", "-1", "--format=%B"]).trim_end(),
            message
        );
        assert!(!repo.join("pre-commit-ran").exists());
        assert!(!repo.join("prepare-commit-msg-ran").exists());
        assert!(!repo.join("IMPLEMENTATION_PLAN.md").exists());
        assert!(
            run_git_in(&repo, ["ls-tree", "-r", "--name-only", "HEAD"])
                .lines()
                .all(|path| !path.contains("HOOK") && !path.contains("commit-msg-ran")),
            "hostile hook bytes must not enter HEAD"
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn task_closeout_publishes_the_immutable_validated_index_tree() {
        let repo = init_repo("closeout-immutable-validated-tree");
        let partial_plan = "\
- [~] `TASK-CLOSEOUT` Stable closeout contract
  Verification: `cargo test closeout`
  Dependencies: none
";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), partial_plan).expect("write partial plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "seed partial closeout"]);

        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            partial_plan.replace("- [~]", "- [x]"),
        )
        .expect("write Done plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        let validated = capture_validated_task_closeout_tree(
            &repo,
            "TASK-CLOSEOUT",
            &["IMPLEMENTATION_PLAN.md"],
            false,
        )
        .expect("capture validated closeout tree");

        fs::create_dir_all(repo.join("src")).expect("create source directory");
        fs::write(repo.join("src/injected.rs"), "pub fn unverified() {}\n")
            .expect("write later source");
        run_git_in(&repo, ["add", "src/injected.rs"]);

        commit_validated_task_closeout_tree_cas(
            &repo,
            validated,
            "autodev: TASK-CLOSEOUT queue sync",
        )
        .expect("publish the previously validated closeout tree");

        let committed_paths =
            run_git_in(&repo, ["diff-tree", "--name-only", "-r", "HEAD^", "HEAD"]);
        assert_eq!(committed_paths, "IMPLEMENTATION_PLAN.md\n");
        assert!(
            run_git_in(&repo, ["ls-tree", "-r", "--name-only", "HEAD"])
                .lines()
                .all(|path| path != "src/injected.rs"),
            "source staged after validation must not enter the closeout candidate"
        );
        assert_eq!(
            run_git_in(&repo, ["diff", "--cached", "--name-only"]),
            "src/injected.rs\n",
            "later index changes remain staged for an owning transaction"
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn completed_closeout_with_stale_hold_does_not_block_clean_startup() {
        let repo = init_repo("checkpoint-post-closeout-stale-hold");
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-CLOSED` durable closeout already committed\n",
        )
        .expect("write done plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "durable task closeout"]);
        let hold_dir = repo.join(".auto/parallel/gate-holds");
        fs::create_dir_all(&hold_dir).expect("create hold dir");
        fs::write(
            hold_dir.join("TASK-CLOSED.hold"),
            "simulated crash before hold cleanup\n",
        )
        .expect("write stale hold");

        let checkpoint = auto_checkpoint_if_needed(&repo, "master", "auto parallel checkpoint")
            .expect("clean startup should tolerate a post-commit stale hold");

        assert_eq!(checkpoint, None);
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
            ".auto-review-input-quarantine.json",
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
