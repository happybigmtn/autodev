//! `auto review`: iterate over the REVIEW.md queue, dispatching a review harness per batch.

mod harvest;
mod progress;
mod queue;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::claude_exec::{describe_claude_harness, run_claude_with_futility};
use crate::codex_exec::run_codex_exec_max_context;
use crate::codex_stream::CLAUDE_FUTILITY_THRESHOLD_REVIEW;
use crate::qa_only_command::print_final_status_block;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, sync_branch_with_remote, timestamp_slug,
};
use crate::ReviewArgs;

use crate::review_command::harvest::harvest_completed_plan_items_for_review;
use crate::review_command::progress::{
    append_reference_repo_clause, build_live_tree_annotation, collect_tracked_repo_states,
    format_batch_block, format_iteration_summary, summarize_repo_progress, IterationSnapshot,
    RepoProgress,
};
use crate::review_command::queue::{
    batch_identity_set, ensure_review_doc, ensure_review_docs,
    handoff_completed_items_to_review_queue, has_reviewable_items,
    mechanically_triage_stale_review_items, select_review_batch_excluding,
};

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
4. **Source-of-truth review.** For every behavior claim, identify the runtime/API/spec owner. UI must consume canonical runtime output and must not duplicate engine-owned catalogs, constants, settlement math, eligibility rules, risk classifications, balances, or status derivations.
5. **Contract and fixture review.** Verify generated bindings/schemas/docs were regenerated when runtime/API shapes changed. Verify production code does not import fixture/demo/sample data as fallback truth.
6. **Cross-surface review.** When UI/presentation changed, require at least one runtime-output-to-UI/readback proof or explain why no runtime/UI boundary exists. Component-only tests are insufficient when the original risk is runtime/UI drift.
7. **Retirement review.** If specs, modules, routes, tests, or generated artifacts were marked retired/superseded, verify they were deleted, archived, tombstoned, or explicitly gated so future agents cannot keep implementing from them.
8. **Verify the verification story.** Run the cited cargo / pnpm / bash commands. If a command fails, reports `0 tests`, names a non-existent test, or cannot prove the original claim, that's a finding.
9. **Bounded simplification only** — inside the reviewed surface, no drive-by cleanup.
10. **Severity-tag findings** as `Critical`, `Required`, `Optional`, or `FYI`.

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
- A simple compile/check is not enough when the claim was about drift removal, retired surface deletion, generated contract synchronization, or runtime/UI consistency. Use grep/assertion proof, generated diff proof, fixture-boundary proof, or runtime-to-UI readback proof.
- Do not invent work if the batch is empty — stop."#;

const DIRECT_REVIEW_QUEUE_REVIEW_CLAUSE: &str = r#"

Repo-specific direct `REVIEW.md` mode:
- This repo forbids root `COMPLETED.md`, `WORKLIST.md`, and `ARCHIVED.md`.
  These bullets override any generic tracker instructions above.
- Review the items already in `REVIEW.md`. Startup harvest moves completed
  `IMPLEMENTATION_PLAN.md` rows directly into `REVIEW.md`; do not create or
  hand off from `COMPLETED.md`.
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
    let plan_harvest = if direct_review_queue {
        ensure_review_doc(&review_path)?;
        harvest_completed_plan_items_for_review(&repo_root, true)?
    } else {
        ensure_review_docs(&review_path, &archived_path)?;
        harvest_completed_plan_items_for_review(&repo_root, false)?
    };
    let moved_items = if direct_review_queue {
        0
    } else {
        handoff_completed_items_to_review_queue(&completed_path, &review_path)?
    };
    if !review_path.exists() || !has_reviewable_items(&review_path)? {
        println!("auto review");
        println!("repo root:   {}", repo_root.display());
        println!("status:      no reviewable items in REVIEW.md");
        print_final_status_block(
            "no reviewable items",
            &[review_path.display().to_string()],
            "none",
            "continue with implementation or run auto review after new REVIEW.md items appear",
        );
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
    }
    if plan_harvest.removed_count > 0 {
        let destination = if direct_review_queue {
            "REVIEW.md"
        } else {
            "COMPLETED.md -> REVIEW.md"
        };
        println!(
            "handoff:     moved {} completed IMPLEMENTATION_PLAN.md item(s) to {} ({} already reviewed/queued)",
            plan_harvest.removed_count, destination, plan_harvest.skipped_count
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
    let mut skipped_stale_identities: HashSet<String> = HashSet::new();
    while args.max_iterations == 0 || iteration < args.max_iterations {
        if !has_reviewable_items(&review_path)? {
            println!();
            println!("REVIEW.md is empty; stopping.");
            break;
        }
        let (batch, total, skipped_total) = select_review_batch_excluding(
            &review_path,
            args.batch_size,
            &skipped_stale_identities,
        )?;
        if batch.is_empty() {
            println!();
            if skipped_total > 0 {
                println!(
                    "no non-stale reviewable items selected; {} stale item(s) were skipped in this run.",
                    skipped_total
                );
            } else {
                println!("no reviewable items selected; stopping.");
            }
            break;
        }

        let batch_identity = batch_identity_set(&batch);
        if previous_batch_identity.as_ref() == Some(&batch_identity) {
            let counter = stale_batch_counts
                .entry(batch_identity.clone())
                .or_insert(0);
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
                let triage =
                    mechanically_triage_stale_review_items(&repo_root, &review_path, &batch)?;
                eprintln!(
                    "mechanically triaged stale batch: removed {} item(s) from REVIEW.md \
                     and appended {} follow-up(s) to {}.",
                    triage.removed_count,
                    triage.appended_count,
                    triage.followup_path.display()
                );
                if let Some(commit) = auto_checkpoint_if_needed(
                    &repo_root,
                    push_branch.as_str(),
                    "review stale batch triage",
                )? {
                    println!("checkpoint:  committed stale-batch triage at {commit}");
                    if push_branch_with_remote_sync(&repo_root, push_branch.as_str())? {
                        println!("remote sync: rebased onto origin/{}", push_branch);
                    }
                } else {
                    for identity in &batch_identity {
                        skipped_stale_identities.insert(identity.clone());
                    }
                }
                previous_batch_identity = None;
                continue;
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
        println!("batch:       {} of {} queued item(s)", batch.len(), total);
        if skipped_total > 0 {
            println!(
                "stale skip:  {} item(s) skipped for this run",
                skipped_total
            );
        }
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
            run_codex_exec_max_context(
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

    let still_reviewable = has_reviewable_items(&review_path)?;
    print_final_status_block(
        "review loop stopped",
        &[
            review_path.display().to_string(),
            run_root.display().to_string(),
        ],
        if still_reviewable {
            "remaining reviewable items in REVIEW.md"
        } else {
            "none"
        },
        if still_reviewable {
            "rerun auto review after addressing blockers or increasing iteration budget"
        } else {
            "continue with the next implementation, QA, or ship workflow"
        },
    );
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        discover_sibling_git_repos, repo_forbids_legacy_review_trackers, resolve_reference_repos,
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

        let resolved = resolve_reference_repos(&repo_root, &[PathBuf::from("../sharedlib")], false)
            .expect("resolve repos");

        assert_eq!(
            resolved,
            vec![explicit_repo.canonicalize().expect("canonical explicit")],
            "sibling repo should not be enrolled without --include-siblings"
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }
}
