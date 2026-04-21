use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::claude_exec::{describe_claude_harness, run_claude_with_futility};
use crate::codex_exec::run_codex_exec;
use crate::codex_stream::CLAUDE_FUTILITY_THRESHOLD_REVIEW;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, sync_branch_with_remote, timestamp_slug,
};
use crate::ReviewArgs;

pub(crate) const DEFAULT_REVIEW_PROMPT: &str = r#"You are running one iteration of `auto review` against a BATCH of items pulled from `REVIEW.md`. The runner will give you another iteration if you make real progress.

## Setup (one-time reading, cheap)
- `AGENTS.md` — build, validation, staging rules for this repo.
- `specs/*`, `IMPLEMENTATION_PLAN.md`, `ARCHIVED.md`, `WORKLIST.md`, `LEARNINGS.md` — only read if the current batch references them.
- Installed `/ce:review` / `/review` / `/ce:work` helpers may be used if present, but you must still satisfy the contract below without them.
- Additional repos (if listed) are editable only when a reviewed item's owned surfaces live there; read that repo's `AGENTS.md` first.

## Contract for each batch item
1. **Treat the claim as suspect.** Queue prose is frozen at write time; the live tree is ground truth. Verify cited file paths, cited test names, and cited behaviors against the current code.
2. **Blast-radius reconstruct.** Find the changed files from git history for the item, scan adjacent tests / integration surfaces, compare against the base branch if discoverable.
3. **Review along five axes.** Correctness; readability + simplicity; architecture + boundaries; security + trust boundaries; performance + scalability. Pay special attention to SQL/query safety, trust-boundary violations, unintended conditional side effects, stale config or migration coupling, and blast-radius-wider-than-touched-files.
4. **Verify the verification story.** Run the cited cargo / pnpm / bash commands. If a command fails or names a non-existent test, that's a finding.
5. **Bounded simplification only** — inside the reviewed surface, no drive-by cleanup.
6. **Severity-tag findings** as `Critical`, `Required`, `Optional`, or `FYI`.

## If you find problems
- Fix the finding directly when the root cause is clear and bounded.
- Append severity-tagged follow-ups to `WORKLIST.md` (create if missing).
- Record durable engineering lessons in `LEARNINGS.md`.
- Leave unfinished items in `REVIEW.md`.

## If a batch item passes review
- Move the entry from `REVIEW.md` into `ARCHIVED.md` (append-only).
- Do not archive a claim whose cited paths show `EXISTS=false` in the live-tree verification block below without first reconciling the surface.

## Commits and branches
- Stay on the currently checked-out branch. Do not create or switch branches.
- Stage only files relevant to the review: the reviewed sources + `REVIEW.md` / `ARCHIVED.md` / `WORKLIST.md` / `LEARNINGS.md` / `AGENTS.md` when changed.
- One repo per commit if multiple repos are touched. Commit message: `repo-name: review <batch ids>`.
- Push the queue repo's branch back to origin after each commit-producing pass.

## Hard rules
- Prefer fixing over explaining.
- Do not archive an item the code + tests do not support.
- This is a bug-finding and hardening pass, not a feature pass.
- If the tests do not prove the claim, the implementation does not get a free pass.
- Do not invent work if the batch is empty — stop."#;

const EMPTY_COMPLETED_DOC: &str = "# COMPLETED\n\n";
const REVIEW_HEADER: &str = "# REVIEW";
const ARCHIVED_HEADER: &str = "# ARCHIVED";
const DIRECT_REVIEW_QUEUE_REVIEW_CLAUSE: &str = r#"

Repo-specific direct `REVIEW.md` mode:
- This repo forbids root `COMPLETED.md`, `WORKLIST.md`, and `ARCHIVED.md`.
  These bullets override any generic tracker instructions above.
- Review the items already in `REVIEW.md`; do not create or hand off from
  `COMPLETED.md`.
- If a review item passes, remove it from `REVIEW.md`. Git history is the
  archive.
- If a review item fails and cannot be fixed in this pass, leave it in
  `REVIEW.md` or add an explicit unchecked `IMPLEMENTATION_PLAN.md` follow-up.
  Do not write `WORKLIST.md`.
- Stage only files relevant to review fixes plus `REVIEW.md`,
  `IMPLEMENTATION_PLAN.md`, `LEARNINGS.md`, and `AGENTS.md` when they changed.
  Do not create or stage `COMPLETED.md`, `WORKLIST.md`, or `ARCHIVED.md`."#;

pub(crate) async fn run_review(args: ReviewArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let reference_repos =
        resolve_reference_repos(&repo_root, &args.reference_repos, args.include_siblings)?;

    let completed_path = repo_root.join("COMPLETED.md");
    let review_path = repo_root.join("REVIEW.md");
    let archived_path = repo_root.join("ARCHIVED.md");
    let direct_review_queue = repo_forbids_legacy_review_trackers(&repo_root);
    let moved_items = if direct_review_queue {
        ensure_review_doc(&review_path)?;
        0
    } else {
        ensure_review_docs(&review_path, &archived_path)?;
        handoff_completed_items_to_review_queue(&completed_path, &review_path)?
    };
    if !review_path.exists() || !has_reviewable_items(&review_path)? {
        println!("auto review");
        println!("repo root:   {}", repo_root.display());
        println!("status:      no reviewable items in REVIEW.md");
        return Ok(());
    }

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    let push_branch = args
        .branch
        .clone()
        .unwrap_or_else(|| current_branch.clone());
    if let Some(required_branch) = args.branch.as_deref() {
        if current_branch != required_branch {
            bail!(
                "auto review must run on branch `{}` (current: `{}`)",
                required_branch,
                current_branch
            );
        }
    }

    let prompt_template = match &args.prompt_file {
        Some(path) => {
            let prompt = fs::read_to_string(path)
                .with_context(|| format!("failed to read prompt file {}", path.display()))?;
            append_reference_repo_clause(prompt, &reference_repos)
        }
        None => {
            let mut prompt = DEFAULT_REVIEW_PROMPT.to_string();
            if direct_review_queue {
                prompt.push_str(DIRECT_REVIEW_QUEUE_REVIEW_CLAUSE);
            }
            append_reference_repo_clause(prompt, &reference_repos)
        }
    };

    let run_root = args
        .run_root
        .unwrap_or_else(|| repo_root.join(".auto").join("review"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let stderr_log_path = run_root.join("codex.stderr.log");

    let harness = if args.claude { "Claude" } else { "Codex" };

    println!("auto review");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", push_branch);
    if args.claude {
        println!(
            "harness:     {}",
            describe_claude_harness(&args.model, &args.reasoning_effort)
        );
        println!(
            "max turns:   {}",
            args.max_turns
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unlimited".to_string())
        );
    } else {
        println!("model:       {}", args.model);
        println!("reasoning:   {}", args.reasoning_effort);
    }
    println!("review doc:  {}", review_path.display());
    println!(
        "batch size:  {}",
        if args.batch_size == 0 {
            "unlimited (legacy)".to_string()
        } else {
            args.batch_size.to_string()
        }
    );
    if !reference_repos.is_empty() {
        println!("references:  {}", reference_repos.len());
        for path in &reference_repos {
            println!("  - {}", path.display());
        }
    } else if !args.include_siblings {
        println!("references:  none (pass --include-siblings or --reference-repo to enroll)");
    }
    if moved_items > 0 {
        println!(
            "handoff:     moved {} item(s) from COMPLETED.md",
            moved_items
        );
    } else if direct_review_queue {
        println!("handoff:     direct REVIEW.md mode");
    }
    println!("run root:    {}", run_root.display());

    if let Some(commit) =
        auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "review checkpoint")?
    {
        println!("checkpoint:  committed pre-existing review changes at {commit}");
    } else if sync_branch_with_remote(&repo_root, push_branch.as_str())? {
        println!("remote sync: rebased onto origin/{}", push_branch);
    }

    let mut iteration = 0usize;
    let mut previous_batch_identity: Option<Vec<String>> = None;
    let mut stale_batch_counts: HashMap<Vec<String>, usize> = HashMap::new();
    while args.max_iterations == 0 || iteration < args.max_iterations {
        if !has_reviewable_items(&review_path)? {
            println!();
            println!("REVIEW.md is empty; stopping.");
            break;
        }
        let (batch, total) = select_review_batch(&review_path, args.batch_size)?;
        if batch.is_empty() {
            println!();
            println!("no reviewable items selected; stopping.");
            break;
        }

        let batch_identity = batch_identity_set(&batch);
        if previous_batch_identity.as_ref() == Some(&batch_identity) {
            let counter = stale_batch_counts.entry(batch_identity.clone()).or_insert(0);
            *counter += 1;
            if *counter >= 1 {
                eprintln!();
                eprintln!(
                    "stale batch: iteration {} would process the identical item set as \
                     iteration {}. Reviewer did not archive or convert any of: {}.",
                    iteration + 1,
                    iteration,
                    batch_identity.join(", ")
                );
                eprintln!(
                    "stopping to avoid an infinite loop. Convert these items into \
                     IMPLEMENTATION_PLAN.md follow-ups or remove them from REVIEW.md \
                     manually, then re-run `auto review`."
                );
                break;
            }
        }

        let live_tree_annotation = build_live_tree_annotation(&repo_root, &batch);
        let batch_block = format_batch_block(
            &batch,
            total,
            iteration + 1,
            args.max_iterations,
            args.batch_size,
        );
        let full_prompt = format!(
            "{prompt_template}{live_tree_annotation}{batch_block}\nExecute the instructions \
             above against the batch items listed. Remaining queue items stay in REVIEW.md \
             for the next iteration — do not try to drain the whole queue in one pass."
        );

        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("review-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        println!("prompt log:  {}", prompt_path.display());
        println!(
            "batch:       {} of {} queued item(s)",
            batch.len(),
            total
        );
        println!("batch ids:   {}", batch_identity.join(", "));

        if args.dry_run {
            println!();
            println!("--dry-run: not invoking {harness}. Prompt written above.");
            println!("--- live-tree annotation ---");
            print!("{}", live_tree_annotation);
            println!("--- batch block ---");
            print!("{}", batch_block);
            break;
        }

        let iteration_before =
            IterationSnapshot::capture(&repo_root, &review_path).with_context(|| {
                format!("failed to snapshot review state in {}", repo_root.display())
            })?;
        let state_before = collect_tracked_repo_states(&repo_root, &reference_repos)?;
        println!();
        println!("running {harness} review iteration {}", iteration + 1);

        let exit_status = if args.claude {
            run_claude_with_futility(
                &repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                args.max_turns,
                &stderr_log_path,
                None,
                "auto review",
                Some(CLAUDE_FUTILITY_THRESHOLD_REVIEW),
            )
            .await?
        } else {
            run_codex_exec(
                &repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                &args.codex_bin,
                &stderr_log_path,
                None,
                "auto review",
            )
            .await?
        };
        if !exit_status.success() {
            bail!(
                "{harness} exited with status {}; see {}",
                exit_status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                stderr_log_path.display()
            );
        }

        println!();
        println!("{harness} review iteration complete");

        let iteration_after =
            IterationSnapshot::capture(&repo_root, &review_path).with_context(|| {
                format!("failed to snapshot review state in {}", repo_root.display())
            })?;
        print!(
            "{}",
            format_iteration_summary(
                iteration + 1,
                &iteration_before,
                &iteration_after,
                &repo_root,
            )
        );
        previous_batch_identity = Some(batch_identity);

        let state_after = collect_tracked_repo_states(&repo_root, &reference_repos)?;
        match summarize_repo_progress(&state_before, &state_after) {
            RepoProgress::NewCommits => {}
            RepoProgress::DirtyChanges(repos) => {
                bail!(
                    "tracked repo changes were left uncommitted in: {}; commit or revert them before continuing",
                    repos.join(", ")
                );
            }
            RepoProgress::None => {
                if let Some(commit) = auto_checkpoint_if_needed(
                    &repo_root,
                    push_branch.as_str(),
                    "review checkpoint",
                )? {
                    iteration += 1;
                    println!("checkpoint:  committed iteration changes at {commit}");
                    println!();
                    println!("================ REVIEW {} ================", iteration);
                    continue;
                }
                println!("no new commit detected; stopping.");
                break;
            }
        }

        if push_branch_with_remote_sync(&repo_root, push_branch.as_str())? {
            println!("remote sync: rebased onto origin/{}", push_branch);
        }
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "review checkpoint")?
        {
            println!("checkpoint:  committed trailing changes at {commit}");
        }
        iteration += 1;
        println!();
        println!("================ REVIEW {} ================", iteration);
    }

    Ok(())
}

pub(crate) fn has_reviewable_items(path: &Path) -> Result<bool> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(!extract_review_items(&content).is_empty())
}

/// Read REVIEW.md and return the first `batch_size` items. A `batch_size` of 0
/// means "pick every item" (legacy behavior — brittle on large queues).
pub(crate) fn select_review_batch(
    review_path: &Path,
    batch_size: usize,
) -> Result<(Vec<String>, usize)> {
    let content = fs::read_to_string(review_path)
        .with_context(|| format!("failed to read {}", review_path.display()))?;
    let items = extract_review_items(&content);
    let total = items.len();
    if batch_size == 0 || items.len() <= batch_size {
        return Ok((items, total));
    }
    let batch = items.into_iter().take(batch_size).collect();
    Ok((batch, total))
}

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

/// Compute a stable identity string for a REVIEW.md item from its first
/// non-empty line, after stripping leading `## `/`- `/backtick decoration.
/// Used to compare batches across iterations for stale-queue detection.
pub(crate) fn item_identity(item: &str) -> String {
    let first_line = item
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .trim_start_matches("## ")
        .trim_start_matches("- ")
        .trim_start_matches("* ")
        .trim()
        .to_string();
    first_line
}

/// Sorted identity set for a batch. Two batches with the same identity set
/// are considered "the same batch" even if the body prose drifted slightly.
pub(crate) fn batch_identity_set(batch: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = batch.iter().map(|item| item_identity(item)).collect();
    ids.sort();
    ids.dedup();
    ids
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
            let content = fs::read_to_string(review_path).with_context(|| {
                format!("failed to read {}", review_path.display())
            })?;
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

fn path_size(path: PathBuf) -> u64 {
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
        let commit_log = git_stdout(repo_root, ["log", "--oneline", range.as_str()])
            .unwrap_or_default();
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

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

fn signed_delta(before: i64, after: i64) -> String {
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
         (a) convert them into fresh IMPLEMENTATION_PLAN.md tasks that re-land the surface, \
         or (b) rewrite the queue entry truthfully.\n\n",
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

fn append_reference_repo_clause(prompt: String, reference_repos: &[PathBuf]) -> String {
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

fn repo_forbids_legacy_review_trackers(repo_root: &Path) -> bool {
    ["AGENTS.md", "WORKFLOW.md"].iter().any(|relative| {
        fs::read_to_string(repo_root.join(relative)).is_ok_and(|content| {
            content.contains("Do not restore")
                && content.contains("COMPLETED.md")
                && content.contains("WORKLIST.md")
                && content.contains("ARCHIVED.md")
                && content.contains("REVIEW.md")
        })
    })
}

fn resolve_reference_repos(
    repo_root: &Path,
    paths: &[PathBuf],
    include_siblings: bool,
) -> Result<Vec<PathBuf>> {
    let mut resolved = if include_siblings {
        discover_sibling_git_repos(repo_root)?
    } else {
        Vec::new()
    };
    for path in paths {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            repo_root.join(path)
        };
        let canonical = absolute
            .canonicalize()
            .with_context(|| format!("failed to resolve reference repo {}", absolute.display()))?;
        if !canonical.is_dir() {
            bail!("reference repo {} is not a directory", canonical.display());
        }

        let git_root =
            git_stdout(&canonical, ["rev-parse", "--show-toplevel"]).with_context(|| {
                format!(
                    "reference repo {} is not a git repository",
                    canonical.display()
                )
            })?;
        let git_root = PathBuf::from(git_root.trim())
            .canonicalize()
            .with_context(|| {
                format!(
                    "failed to canonicalize git root for {}",
                    canonical.display()
                )
            })?;
        if git_root != repo_root {
            resolved.push(git_root);
        }
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

fn discover_sibling_git_repos(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let Some(parent) = repo_root.parent() else {
        return Ok(Vec::new());
    };

    let mut siblings = Vec::new();
    for entry in fs::read_dir(parent).with_context(|| {
        format!(
            "failed to read sibling directories under {}",
            parent.display()
        )
    })? {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", parent.display()))?;
        let candidate = entry.path();
        if !candidate.is_dir() {
            continue;
        }

        let canonical = candidate.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize sibling directory {}",
                candidate.display()
            )
        })?;
        if canonical == repo_root {
            continue;
        }

        let Ok(git_root) = git_stdout(&canonical, ["rev-parse", "--show-toplevel"]) else {
            continue;
        };
        let git_root = PathBuf::from(git_root.trim())
            .canonicalize()
            .with_context(|| {
                format!(
                    "failed to canonicalize git root for {}",
                    canonical.display()
                )
            })?;
        if git_root == canonical {
            siblings.push(git_root);
        }
    }

    siblings.sort();
    siblings.dedup();
    Ok(siblings)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrackedRepoState {
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
enum RepoProgress {
    None,
    NewCommits,
    DirtyChanges(Vec<String>),
}

fn collect_tracked_repo_states(
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
        let status = git_stdout(&path, ["status", "--short"]).unwrap_or_default();
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
fn summarize_repo_progress(
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

pub(crate) fn handoff_completed_items_to_review_queue(
    completed_path: &Path,
    review_path: &Path,
) -> Result<usize> {
    let completed_items = if completed_path.exists() {
        extract_review_items(
            &fs::read_to_string(completed_path)
                .with_context(|| format!("failed to read {}", completed_path.display()))?,
        )
    } else {
        Vec::new()
    };
    if completed_items.is_empty() {
        return Ok(0);
    }

    let mut review_items = if review_path.exists() {
        extract_review_items(
            &fs::read_to_string(review_path)
                .with_context(|| format!("failed to read {}", review_path.display()))?,
        )
    } else {
        Vec::new()
    };
    let moved_count = completed_items.len();
    review_items.extend(completed_items);

    write_queue(review_path, REVIEW_HEADER, &review_items)?;
    atomic_write(completed_path, EMPTY_COMPLETED_DOC.as_bytes())
        .with_context(|| format!("failed to reset {}", completed_path.display()))?;
    Ok(moved_count)
}

fn extract_review_items(content: &str) -> Vec<String> {
    if content.lines().any(|line| line.starts_with("## ")) {
        return extract_section_review_items(content);
    }
    extract_bullet_review_items(content)
}

fn extract_section_review_items(content: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = Vec::new();
    for line in content.lines() {
        if line.starts_with("## ") {
            if !current.is_empty() {
                items.push(current.join("\n").trim_end().to_string());
                current.clear();
            }
            current.push(line.to_string());
        } else if !current.is_empty() {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        items.push(current.join("\n").trim_end().to_string());
    }
    items
}

fn write_queue(path: &Path, title: &str, items: &[String]) -> Result<()> {
    let mut content = String::from(title);
    content.push_str("\n\n");
    if !items.is_empty() {
        content.push_str(&items.join("\n\n"));
        content.push('\n');
    }
    atomic_write(path, content.as_bytes())
}

fn ensure_review_doc(review_path: &Path) -> Result<()> {
    if !review_path.exists() {
        atomic_write(review_path, format!("{REVIEW_HEADER}\n\n").as_bytes())
            .with_context(|| format!("failed to initialize {}", review_path.display()))?;
    }
    Ok(())
}

fn ensure_review_docs(review_path: &Path, archived_path: &Path) -> Result<()> {
    ensure_review_doc(review_path)?;
    if !archived_path.exists() {
        atomic_write(archived_path, format!("{ARCHIVED_HEADER}\n\n").as_bytes())
            .with_context(|| format!("failed to initialize {}", archived_path.display()))?;
    }
    Ok(())
}

fn extract_bullet_review_items(content: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim_end();
        if line.starts_with("- ") {
            if !current.is_empty() {
                items.push(current.join("\n").trim_end().to_string());
                current.clear();
            }
            current.push(line.to_string());
            continue;
        }

        if current.is_empty() {
            continue;
        }

        if line.trim().is_empty() {
            current.push(String::new());
            continue;
        }

        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            current.push(line.to_string());
            continue;
        }

        items.push(current.join("\n").trim_end().to_string());
        current.clear();
    }

    if !current.is_empty() {
        items.push(current.join("\n").trim_end().to_string());
    }

    items
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        append_reference_repo_clause, batch_identity_set, build_live_tree_annotation,
        collect_tracked_repo_states, discover_sibling_git_repos, ensure_review_docs,
        extract_cited_paths, extract_review_items, format_batch_block, format_iteration_summary,
        item_identity, repo_forbids_legacy_review_trackers, resolve_reference_repos,
        select_review_batch, summarize_repo_progress, IterationSnapshot, RepoProgress,
        TrackedRepoState, ARCHIVED_HEADER, REVIEW_HEADER,
    };

    #[test]
    fn extracts_bullet_review_items() {
        let content = "# COMPLETED\n\n- `VAL-001` Added validation\n  Validation:\n  `cargo test`\n\n- `SEC-001` Hardened auth\n  Note: tightened auth boundary\n";
        let items = extract_review_items(content);
        assert_eq!(
            items,
            vec![
                "- `VAL-001` Added validation\n  Validation:\n  `cargo test`".to_string(),
                "- `SEC-001` Hardened auth\n  Note: tightened auth boundary".to_string()
            ]
        );
    }

    #[test]
    fn extracts_section_review_items() {
        let content = "# COMPLETED\n\n## `VAL-001` Added validation\nValidation: pytest\n\n## `SEC-001` Hardened auth\nValidation: ruff check";
        let items = extract_review_items(content);
        assert_eq!(items.len(), 2);
        assert!(items[0].starts_with("## `VAL-001`"));
        assert!(items[1].starts_with("## `SEC-001`"));
    }

    #[test]
    fn initializes_review_and_archived_docs() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        let review_path = temp.join("REVIEW.md");
        let archived_path = temp.join("ARCHIVED.md");

        ensure_review_docs(&review_path, &archived_path).expect("init docs");

        assert_eq!(
            fs::read_to_string(review_path).expect("read review"),
            format!("{REVIEW_HEADER}\n\n")
        );
        assert_eq!(
            fs::read_to_string(archived_path).expect("read archived"),
            format!("{ARCHIVED_HEADER}\n\n")
        );

        fs::remove_dir_all(temp).expect("cleanup temp dir");
    }

    #[test]
    fn appends_reference_repo_clause_when_repos_present() {
        let prompt = append_reference_repo_clause(
            "review prompt".to_string(),
            &[PathBuf::from("/home/r/coding/robopokermulti")],
        );

        assert!(prompt.contains("Additional repositories you may inspect or edit"));
        assert!(prompt.contains("/home/r/coding/robopokermulti"));
        assert!(prompt.contains("owned or changed surfaces live in one of these repos"));
    }

    #[test]
    fn detects_direct_review_queue_policy() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        fs::write(
            temp.join("AGENTS.md"),
            "Do not restore `COMPLETED.md`, `WORKLIST.md`, or `ARCHIVED.md`; use `REVIEW.md`.",
        )
        .expect("write policy");

        assert!(repo_forbids_legacy_review_trackers(&temp));

        fs::remove_dir_all(temp).expect("cleanup temp dir");
    }

    #[test]
    fn discovers_sibling_git_repos_by_default() {
        let workspace = unique_temp_dir();
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let non_repo = workspace.join("notes");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        fs::create_dir_all(&non_repo).expect("failed to create non-repo dir");

        let discovered = discover_sibling_git_repos(&repo_root).expect("discover siblings");
        assert_eq!(
            discovered,
            vec![sibling_repo.canonicalize().expect("canonical sibling")]
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn resolve_reference_repos_merges_siblings_and_explicit_paths_when_opted_in() {
        let workspace = unique_temp_dir();
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let explicit_repo = workspace.join("sharedlib");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        init_git_repo(&explicit_repo);

        let resolved = resolve_reference_repos(
            &repo_root,
            &[PathBuf::from("../sharedlib"), sibling_repo.clone()],
            true,
        )
        .expect("resolve repos");

        assert_eq!(
            resolved,
            vec![
                sibling_repo.canonicalize().expect("canonical sibling"),
                explicit_repo.canonicalize().expect("canonical explicit"),
            ]
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn resolve_reference_repos_skips_siblings_by_default() {
        let workspace = unique_temp_dir();
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let explicit_repo = workspace.join("sharedlib");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        init_git_repo(&explicit_repo);

        let resolved = resolve_reference_repos(
            &repo_root,
            &[PathBuf::from("../sharedlib")],
            false,
        )
        .expect("resolve repos");

        assert_eq!(
            resolved,
            vec![explicit_repo.canonicalize().expect("canonical explicit")],
            "sibling repo should not be enrolled without --include-siblings"
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
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
    fn select_review_batch_respects_batch_size() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        let review_path = temp.join("REVIEW.md");
        fs::write(
            &review_path,
            "# REVIEW\n\n- `A` one\n- `B` two\n- `C` three\n- `D` four\n",
        )
        .expect("write review");

        let (batch, total) = select_review_batch(&review_path, 2).expect("select");
        assert_eq!(total, 4);
        assert_eq!(batch.len(), 2);
        assert!(batch[0].starts_with("- `A`"));
        assert!(batch[1].starts_with("- `B`"));

        let (all_batch, _) = select_review_batch(&review_path, 0).expect("select all");
        assert_eq!(all_batch.len(), 4, "batch_size 0 must fall back to all items");

        fs::remove_dir_all(temp).expect("cleanup");
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
    fn item_identity_strips_leading_markers_and_dedups() {
        assert_eq!(item_identity("- `A-1` thing"), "`A-1` thing");
        assert_eq!(item_identity("## `A-1` thing"), "`A-1` thing");
        assert_eq!(item_identity("   \n  - `A-1` thing"), "`A-1` thing");
        let ids = batch_identity_set(&[
            "- `A-1` one".to_string(),
            "## `B-2` two".to_string(),
            "- `A-1` one".to_string(),
        ]);
        assert_eq!(ids, vec!["`A-1` one".to_string(), "`B-2` two".to_string()]);
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
            TrackedRepoState::new(
                "bitpoker",
                "/tmp/bitpoker",
                "aaa111",
                " M src/main.rs",
            ),
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

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-review-test-{nanos}"))
    }
}
