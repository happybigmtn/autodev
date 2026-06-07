//! Iteration progress: cited-path extraction, snapshots, batch rendering, repo-state tracking.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::review_command::queue::{extract_review_items, item_identity};
use crate::util::{git_status_short_filtered, git_stdout};

/// Extract `path/file.ext`-shaped tokens from a REVIEW.md item body. Only the
/// characters between matching backticks count; this avoids treating prose
/// phrases as paths. A path must contain at least one `/` and at least one
/// `.` (to screen out constants / env vars named in bullets).
pub(crate) fn extract_cited_paths(item_body: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut iter = item_body.char_indices().peekable();
    while let Some((idx, ch)) = iter.next() {
        if ch != '`' {
            continue;
        }
        let start = idx + 1;
        let mut end = None;
        for (j, c) in item_body[start..].char_indices() {
            if c == '`' {
                end = Some(start + j);
                break;
            }
        }
        let Some(end_idx) = end else { break };
        let token = &item_body[start..end_idx];
        while let Some((next_idx, _)) = iter.peek() {
            if *next_idx <= end_idx {
                iter.next();
            } else {
                break;
            }
        }
        if token.is_empty() || token.len() > 200 {
            continue;
        }
        if !token.contains('/') || !token.contains('.') {
            continue;
        }
        if token.chars().any(|c| c.is_whitespace()) {
            continue;
        }
        // Drop anchor / query / colon suffixes (e.g. `foo/bar.rs:123`).
        let cleaned = token
            .split([':', '#', '?'])
            .next()
            .unwrap_or(token)
            .trim_start_matches("./")
            .to_string();
        if cleaned.is_empty() {
            continue;
        }
        paths.push(cleaned);
    }
    paths.sort();
    paths.dedup();
    paths
}

/// Snapshot of observable review-pass state captured before and after each
/// iteration so we can report a structured summary instead of a generic
/// "iteration complete".
#[derive(Clone, Debug)]
pub(crate) struct IterationSnapshot {
    pub review_count: usize,
    pub worklist_bytes: u64,
    pub archived_count: Option<usize>,
    pub learnings_bytes: u64,
    pub head_commit: String,
}

impl IterationSnapshot {
    pub(crate) fn capture(repo_root: &Path, review_path: &Path) -> Result<Self> {
        let review_count = if review_path.exists() {
            let content = fs::read_to_string(review_path)
                .with_context(|| format!("failed to read {}", review_path.display()))?;
            extract_review_items(&content).len()
        } else {
            0
        };
        let worklist_bytes = path_size(repo_root.join("WORKLIST.md"));
        let learnings_bytes = path_size(repo_root.join("LEARNINGS.md"));
        let archived_path = repo_root.join("ARCHIVED.md");
        let archived_count = if archived_path.exists() {
            let content = fs::read_to_string(&archived_path).ok();
            content.map(|text| extract_review_items(&text).len())
        } else {
            None
        };
        let head_commit = git_stdout(repo_root, ["rev-parse", "HEAD"])
            .unwrap_or_default()
            .trim()
            .to_string();
        Ok(Self {
            review_count,
            worklist_bytes,
            archived_count,
            learnings_bytes,
            head_commit,
        })
    }
}

pub(crate) fn path_size(path: PathBuf) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Render a human-readable summary of what changed between two iteration
/// snapshots so the surrounding run log is self-describing.
pub(crate) fn format_iteration_summary(
    iteration: usize,
    before: &IterationSnapshot,
    after: &IterationSnapshot,
    repo_root: &Path,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("iteration {} summary:\n", iteration));
    out.push_str(&format!(
        "  - REVIEW.md items:   {} -> {} ({})\n",
        before.review_count,
        after.review_count,
        signed_delta(before.review_count as i64, after.review_count as i64),
    ));
    if let (Some(before_arc), Some(after_arc)) = (before.archived_count, after.archived_count) {
        out.push_str(&format!(
            "  - ARCHIVED.md items: {} -> {} ({})\n",
            before_arc,
            after_arc,
            signed_delta(before_arc as i64, after_arc as i64),
        ));
    }
    if before.worklist_bytes != after.worklist_bytes {
        out.push_str(&format!(
            "  - WORKLIST.md size:  {} -> {} bytes ({})\n",
            before.worklist_bytes,
            after.worklist_bytes,
            signed_delta(before.worklist_bytes as i64, after.worklist_bytes as i64),
        ));
    }
    if before.learnings_bytes != after.learnings_bytes {
        out.push_str(&format!(
            "  - LEARNINGS.md size: {} -> {} bytes ({})\n",
            before.learnings_bytes,
            after.learnings_bytes,
            signed_delta(before.learnings_bytes as i64, after.learnings_bytes as i64),
        ));
    }
    if before.head_commit != after.head_commit && !before.head_commit.is_empty() {
        let range = format!("{}..{}", before.head_commit, after.head_commit);
        let commit_log =
            git_stdout(repo_root, ["log", "--oneline", range.as_str()]).unwrap_or_default();
        let commit_lines: Vec<&str> = commit_log.lines().filter(|l| !l.is_empty()).collect();
        out.push_str(&format!(
            "  - new commits:       {} ({}..{})\n",
            commit_lines.len(),
            short_sha(&before.head_commit),
            short_sha(&after.head_commit),
        ));
        for line in commit_lines.iter().take(5) {
            out.push_str(&format!("      {}\n", line));
        }
    } else {
        out.push_str("  - new commits:       0\n");
    }
    out
}

pub(crate) fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

pub(crate) fn signed_delta(before: i64, after: i64) -> String {
    let delta = after - before;
    if delta > 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    }
}

/// Render the batch of review items into a markdown block the reviewer sees.
/// This is appended to the prompt so the reviewer works against a bounded
/// list rather than re-parsing the entire REVIEW.md file. Also injects an
/// iteration-budget note so the reviewer knows whether to be thorough or
/// efficient (iteration 1 of ~35 calls for discipline).
pub(crate) fn format_batch_block(
    batch: &[String],
    total: usize,
    iteration: usize,
    max_iterations: usize,
    batch_size: usize,
) -> String {
    let mut out = String::from("\n## Iteration context\n\n");
    let effective_batch = if batch_size == 0 {
        total.max(1)
    } else {
        batch_size.max(1)
    };
    let estimated_batches = total.div_ceil(effective_batch);
    out.push_str(&format!(
        "- Current iteration: {iteration}\n\
         - Estimated batches to drain queue at this size: {estimated_batches}\n\
         - Iteration cap: {iteration_cap}\n\
         - Posture: review only the batch below. Do NOT try to drain the whole \
         queue in one pass; the surrounding runner will give you another \
         iteration if progress is real.\n\n",
        iteration = iteration,
        estimated_batches = estimated_batches,
        iteration_cap = if max_iterations == 0 {
            "unlimited (runs until queue empties or progress stalls)".to_string()
        } else {
            max_iterations.to_string()
        },
    ));
    out.push_str("## Review batch for this iteration\n\n");
    out.push_str(&format!(
        "Queue has {total} total item(s); this iteration reviews {batch_len}. \
         Complete only these items; leave the rest of REVIEW.md alone.\n\n",
        total = total,
        batch_len = batch.len(),
    ));
    for (index, item) in batch.iter().enumerate() {
        out.push_str(&format!("### Batch item {}\n\n", index + 1));
        out.push_str(item);
        if !item.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Emit a `## Live-tree verification` prompt annotation enumerating each
/// batch item's cited paths and whether they still exist. The reviewer sees
/// `EXISTS=false` against deleted surfaces and refuses to archive a stale
/// claim rather than trusting the prose in REVIEW.md.
pub(crate) fn build_live_tree_annotation(repo_root: &Path, batch: &[String]) -> String {
    let mut out = String::from("\n## Live-tree verification\n\n");
    out.push_str(
        "The queue entries below name one or more file paths. Before archiving any item, \
         refuse items whose cited paths no longer exist in the current tree and either \
         (a) rewrite the queue entry truthfully to state the surface is gone, or (b) only if an \
         end user or operator observably lost a capability, create ONE fresh task naming that \
         exact lost capability. Do not re-create a task merely to rebuild a deleted path.\n\n",
    );
    for (index, item) in batch.iter().enumerate() {
        let label_source = item_identity(item);
        let label = if label_source.is_empty() {
            format!("item {}", index + 1)
        } else {
            label_source
        };
        out.push_str(&format!("- {label}\n"));
        let paths = extract_cited_paths(item);
        if paths.is_empty() {
            out.push_str("  - no `/`-containing paths cited in the body\n");
            continue;
        }
        for path in paths {
            let exists = repo_root.join(&path).exists();
            out.push_str(&format!("  - `{path}` EXISTS={exists}\n"));
        }
    }
    out.push('\n');
    out
}

pub(crate) fn append_reference_repo_clause(prompt: String, reference_repos: &[PathBuf]) -> String {
    if reference_repos.is_empty() {
        return prompt;
    }

    let listing = reference_repos
        .iter()
        .map(|path| format!("- `{}`", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{prompt}\n\nAdditional repositories you may inspect or edit when the review contract points there:\n{listing}\n\nRepository-crossing rules:\n- If a reviewed item's owned or changed surfaces live in one of these repos, review and fix that repo directly instead of pretending the queue repo owns it.\n- Keep `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, and `LEARNINGS.md` truthful in the queue repo even when code lands in another repo.\n- Read each touched repo's `AGENTS.md`, tests, and operational docs before editing it.\n- Commit and push each touched repo separately.\n"
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrackedRepoState {
    name: String,
    path: PathBuf,
    head: String,
    status: String,
}

impl TrackedRepoState {
    #[cfg(test)]
    fn new(name: &str, path: &str, head: &str, status: &str) -> Self {
        Self {
            name: name.to_string(),
            path: PathBuf::from(path),
            head: head.to_string(),
            status: status.to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RepoProgress {
    None,
    NewCommits,
    DirtyChanges(Vec<String>),
}

pub(crate) fn collect_tracked_repo_states(
    repo_root: &Path,
    reference_repos: &[PathBuf],
) -> Result<Vec<TrackedRepoState>> {
    let mut repos = Vec::with_capacity(reference_repos.len() + 1);
    repos.push(repo_root.to_path_buf());
    repos.extend(reference_repos.iter().cloned());

    let mut states = Vec::with_capacity(repos.len());
    for path in repos {
        let Ok(head) = git_stdout(&path, ["rev-parse", "HEAD"]) else {
            continue;
        };
        let status = git_status_short_filtered(&path).unwrap_or_default();
        states.push(TrackedRepoState {
            name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("repo")
                .to_string(),
            path,
            head: head.trim().to_string(),
            status: status.trim().to_string(),
        });
    }
    Ok(states)
}

/// Summarize repo progress. The first entry in `before`/`after` is the primary
/// (queue) repo; the rest are reference repos. Uncommitted changes in the
/// primary repo are a hard signal (`DirtyChanges`) so the reviewer is forced
/// to resolve them; dirty reference repos only emit a warning — one dirty
/// unrelated sibling must not abort an otherwise-healthy review pass.
pub(crate) fn summarize_repo_progress(
    before: &[TrackedRepoState],
    after: &[TrackedRepoState],
) -> RepoProgress {
    let mut dirty_primary = Vec::new();
    let mut dirty_references = Vec::new();
    let mut any_new_commits = false;
    for (index, after_state) in after.iter().enumerate() {
        let is_primary = index == 0;
        let Some(before_state) = before.iter().find(|state| state.path == after_state.path) else {
            any_new_commits = true;
            continue;
        };
        if before_state.head != after_state.head {
            any_new_commits = true;
            continue;
        }
        if before_state.status != after_state.status {
            if is_primary {
                dirty_primary.push(after_state.name.clone());
            } else {
                dirty_references.push(after_state.name.clone());
            }
        }
    }

    if !dirty_references.is_empty() {
        dirty_references.sort();
        dirty_references.dedup();
        eprintln!(
            "warning: reference repo(s) left uncommitted changes: {}; ignoring and continuing \
             (use --reference-repo only for repos you actually want the reviewer to touch)",
            dirty_references.join(", ")
        );
    }

    if any_new_commits {
        return RepoProgress::NewCommits;
    }
    if !dirty_primary.is_empty() {
        dirty_primary.sort();
        dirty_primary.dedup();
        return RepoProgress::DirtyChanges(dirty_primary);
    }
    RepoProgress::None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        append_reference_repo_clause, build_live_tree_annotation, collect_tracked_repo_states,
        extract_cited_paths, format_batch_block, format_iteration_summary, summarize_repo_progress,
        IterationSnapshot, RepoProgress, TrackedRepoState,
    };

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-review-test-{nanos}"))
    }

    fn init_git_repo(path: &PathBuf) {
        fs::create_dir_all(path).expect("failed to create repo dir");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(path)
            .status()
            .expect("failed to run git init");
        assert!(status.success(), "git init should succeed");
    }

    fn commit_empty_change(path: &PathBuf) {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Autodev Tests",
                "-c",
                "user.email=autodev-tests@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "initial commit",
            ])
            .current_dir(path)
            .status()
            .expect("failed to run git commit");
        assert!(status.success(), "git commit should succeed");
    }

    #[test]
    fn appends_reference_repo_clause_when_repos_present() {
        let prompt = append_reference_repo_clause(
            "review prompt".to_string(),
            &[PathBuf::from("/tmp/robopokermulti")],
        );

        assert!(prompt.contains("Additional repositories you may inspect or edit"));
        assert!(prompt.contains("/tmp/robopokermulti"));
        assert!(prompt.contains("owned or changed surfaces live in one of these repos"));
    }

    #[test]
    fn extract_cited_paths_finds_rs_and_md_paths_in_backticks() {
        let body = "- `P-020B` fix at `observatory-tui/src/nl/parser.rs:42`\n  - note `scripts/check-autoloop-affected-rust.sh`\n  - verbatim `not/a/path.plain text` should not match";
        let paths = extract_cited_paths(body);
        assert!(paths.contains(&"observatory-tui/src/nl/parser.rs".to_string()));
        assert!(paths.contains(&"scripts/check-autoloop-affected-rust.sh".to_string()));
        for path in &paths {
            assert!(!path.contains(' '), "paths must not contain whitespace");
            assert!(!path.contains(':'), "paths must strip trailing :N anchors");
        }
    }

    #[test]
    fn extract_cited_paths_skips_non_path_tokens() {
        let body = "- `W2-NS-39` references `BRIDGE_COSIGN_VALIDATOR_PUBKEYS` and `SomeType`";
        let paths = extract_cited_paths(body);
        assert!(
            paths.is_empty(),
            "bare identifiers without / or . should not be flagged as paths, got {paths:?}"
        );
    }

    #[test]
    fn format_batch_block_includes_each_item_and_total_count() {
        let batch = vec![
            "- `A` first item body".to_string(),
            "- `B` second item body".to_string(),
        ];
        let rendered = format_batch_block(&batch, 5, 1, 0, 2);
        assert!(rendered.contains("Iteration context"));
        assert!(rendered.contains("Current iteration: 1"));
        assert!(rendered.contains("Estimated batches to drain queue at this size: 3"));
        assert!(rendered.contains("Queue has 5 total"));
        assert!(rendered.contains("reviews 2"));
        assert!(rendered.contains("Batch item 1"));
        assert!(rendered.contains("- `A` first item body"));
        assert!(rendered.contains("Batch item 2"));
        assert!(rendered.contains("- `B` second item body"));
    }

    #[test]
    fn build_live_tree_annotation_flags_missing_paths() {
        let workspace = unique_temp_dir();
        fs::create_dir_all(workspace.join("src")).expect("create workspace");
        fs::write(workspace.join("src/present.rs"), "").expect("write present.rs");

        let batch = vec![format!(
            "- `FAKE-001` exists via `src/present.rs` and absent via `missing/elsewhere.rs`"
        )];
        let annotation = build_live_tree_annotation(&workspace, &batch);
        assert!(annotation.contains("Live-tree verification"));
        assert!(annotation.contains("`src/present.rs` EXISTS=true"));
        assert!(annotation.contains("`missing/elsewhere.rs` EXISTS=false"));

        fs::remove_dir_all(workspace).expect("cleanup");
    }

    #[test]
    fn format_batch_block_shows_iteration_context() {
        let batch = vec!["- `A` one".to_string()];
        let rendered = format_batch_block(&batch, 1, 4, 10, 5);
        assert!(rendered.contains("Current iteration: 4"));
        assert!(rendered.contains("Iteration cap: 10"));
        let rendered_unlimited = format_batch_block(&batch, 1, 1, 0, 5);
        assert!(rendered_unlimited.contains("unlimited"));
    }

    #[test]
    fn format_iteration_summary_reports_review_and_archived_deltas() {
        let temp = unique_temp_dir();
        init_git_repo(&temp);
        commit_empty_change(&temp);

        let before = IterationSnapshot {
            review_count: 5,
            worklist_bytes: 100,
            archived_count: Some(10),
            learnings_bytes: 200,
            head_commit: "aaaaaaaa".to_string(),
        };
        let after = IterationSnapshot {
            review_count: 3,
            worklist_bytes: 150,
            archived_count: Some(12),
            learnings_bytes: 200,
            head_commit: "aaaaaaaa".to_string(),
        };
        let summary = format_iteration_summary(2, &before, &after, &temp);
        assert!(summary.contains("iteration 2 summary"));
        assert!(summary.contains("5 -> 3 (-2)"));
        assert!(summary.contains("10 -> 12 (+2)"));
        assert!(summary.contains("100 -> 150 bytes (+50)"));
        assert!(summary.contains("new commits:       0"));

        fs::remove_dir_all(temp).expect("cleanup");
    }

    #[test]
    fn repo_progress_detects_reference_repo_commit() {
        let before = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb111", ""),
        ];
        let after = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb222", ""),
        ];

        let progress = summarize_repo_progress(&before, &after);
        assert_eq!(progress, RepoProgress::NewCommits);
    }

    #[test]
    fn repo_progress_warns_on_dirty_reference_repo_without_bailing() {
        let before = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb111", ""),
        ];
        let after = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new(
                "robopokermulti",
                "/tmp/robopokermulti",
                "bbb111",
                " M src/lib.rs",
            ),
        ];

        let progress = summarize_repo_progress(&before, &after);
        assert_eq!(
            progress,
            RepoProgress::None,
            "dirty reference repo should warn (via stderr), not force the caller to bail"
        );
    }

    #[test]
    fn repo_progress_bails_only_on_dirty_primary_repo() {
        let before = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb111", ""),
        ];
        let after = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", " M src/main.rs"),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb111", ""),
        ];

        let progress = summarize_repo_progress(&before, &after);
        assert_eq!(
            progress,
            RepoProgress::DirtyChanges(vec!["bitpoker".to_string()]),
            "dirty primary repo must still bail out"
        );
    }

    #[test]
    fn collect_tracked_repo_states_skips_unborn_reference_repo() {
        let workspace = unique_temp_dir();
        let repo_root = workspace.join("bitpoker");
        let unborn_reference = workspace.join("hermes-autodev-framework");

        init_git_repo(&repo_root);
        commit_empty_change(&repo_root);
        init_git_repo(&unborn_reference);

        let states =
            collect_tracked_repo_states(&repo_root, std::slice::from_ref(&unborn_reference))
                .expect("collect repo states");

        assert_eq!(states.len(), 1);
        assert_eq!(states[0].path, repo_root);

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }
}
