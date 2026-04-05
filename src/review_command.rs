use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::codex_exec::run_codex_exec;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    run_git, timestamp_slug,
};
use crate::ReviewArgs;

pub(crate) const DEFAULT_REVIEW_PROMPT: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, and staging rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, and `LEARNINGS.md` if they exist.
0c. You may use installed helper workflows like `/ce:review`, `/review`, `/ce:work`, or `/ce:compound` if they are available, but you must still satisfy the full review contract below even if those helpers are missing.

1. Your task is to review the items currently listed in `REVIEW.md`.
   - Treat each review item as a claim that must be verified against the codebase, the specs, and the implementation plan.
   - Re-read the owned surfaces, integration touchpoints, and validation evidence for those items before trusting the claim.
   - Run a broad engineering review, not a status recap: look for regressions, weak assumptions, missing edge cases, security issues, integration gaps, and test blind spots.

2. Use this review workflow for every item:
   - Understand the intended behavior and expected change first.
   - Review the tests and verification evidence before reviewing the implementation details.
   - Reconstruct the changed-file set and blast radius for the reviewed item from commits, diffs, touched tests, and adjacent integration surfaces before you decide the item is safe.
   - Review the implementation across these five axes:
     - correctness
     - readability and simplicity
     - architecture and boundaries
     - security and trust boundaries
     - performance and scalability
   - If a base branch is discoverable, compare the current branch diff against that base instead of reviewing files in isolation.
   - Pay special attention to structural issues that tests often miss: SQL/query safety, trust-boundary violations, unintended conditional side effects, stale config or migration coupling, and changes whose blast radius is wider than the touched files imply.
   - For browser-facing or runtime-sensitive items, use browser/runtime verification when available instead of static review alone.
   - Verify the verification story itself: commands actually run, outputs believable, screenshots or runtime evidence consistent with the code.
   - Categorize any findings as `Critical`, required, `Optional`, or `FYI`.

3. Respect the queue split:
   - `REVIEW.md` is the in-flight review queue.
   - `COMPLETED.md` is free to keep receiving new implementation completions while review is running.
   - Do not move items back into `IMPLEMENTATION_PLAN.md`.

4. If you find problems:
   - Append concrete, severity-tagged follow-up items to `WORKLIST.md`. Create it if missing.
   - Fix review findings directly when the root cause is clear and the work is bounded.
   - Record durable learnings in `LEARNINGS.md`.
   - Leave any not-yet-cleared entries in `REVIEW.md` until the fixes are actually landed and supported by the codebase.
   - Keep `AGENTS.md` operational only.

5. If a review item passes review:
   - Move its entry from `REVIEW.md` into `ARCHIVED.md`.
   - `ARCHIVED.md` should be append-only history.
   - Only archive items that are genuinely complete after review and any follow-up fixes.

6. Commit and push only truthful review increments:
   - Stay on the branch that is already checked out when `auto review` starts.
   - Do not create or switch branches during the review pass.
   - Stage only the files relevant to the review fixes plus `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, `LEARNINGS.md`, and `AGENTS.md` when they changed.
   - Commit with a message like `repo-name: review completed items`.
   - Push back to that same branch after each successful commit-producing pass.

7. If `REVIEW.md` is empty or has no reviewable items:
   - Do not invent work.
   - Say so briefly and stop without making changes.

99999. Important: prefer fixing findings over explaining them.
999999. Important: do not archive an item until the code and review evidence support it.
9999999. Important: this is a bug-finding and hardening pass, not a feature pass.
99999999. Important: if the tests do not prove the claim, the implementation does not get a free pass."#;

const EMPTY_COMPLETED_DOC: &str = "# COMPLETED\n\n";
const REVIEW_HEADER: &str = "# REVIEW";

pub(crate) async fn run_review(args: ReviewArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;

    let completed_path = repo_root.join("COMPLETED.md");
    let review_path = repo_root.join("REVIEW.md");
    let moved_items = handoff_completed_items_to_review_queue(&completed_path, &review_path)?;
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
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display()))?,
        None => DEFAULT_REVIEW_PROMPT.to_string(),
    };
    let full_prompt = format!("{prompt_template}\n\nExecute the instructions above.");

    let run_root = args
        .run_root
        .unwrap_or_else(|| repo_root.join(".auto").join("review"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let stderr_log_path = run_root.join("codex.stderr.log");

    println!("auto review");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", push_branch);
    println!("model:       {}", args.model);
    println!("reasoning:   {}", args.reasoning_effort);
    println!("review doc:  {}", review_path.display());
    if moved_items > 0 {
        println!(
            "handoff:     moved {} item(s) from COMPLETED.md",
            moved_items
        );
    }
    println!("run root:    {}", run_root.display());

    if let Some(commit) =
        auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "review checkpoint")?
    {
        println!("checkpoint:  committed pre-existing review changes at {commit}");
    }

    let mut iteration = 0usize;
    while iteration < args.max_iterations {
        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("review-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;

        let commit_before = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        println!();
        println!("running review iteration {}", iteration + 1);

        let exit_status = run_codex_exec(
            &repo_root,
            &full_prompt,
            &args.model,
            &args.reasoning_effort,
            &args.codex_bin,
            &stderr_log_path,
            "auto review",
        )
        .await?;
        if !exit_status.success() {
            bail!(
                "Codex exited with status {}; see {}",
                exit_status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                stderr_log_path.display()
            );
        }

        println!();
        println!("review iteration complete");

        let commit_after = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        if commit_before.trim() == commit_after.trim() {
            if let Some(commit) =
                auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "review checkpoint")?
            {
                iteration += 1;
                println!("checkpoint:  committed iteration changes at {commit}");
                println!();
                println!("================ REVIEW {} ================", iteration);
                continue;
            }
            println!("no new commit detected; stopping.");
            break;
        }

        run_git(&repo_root, ["push", "origin", push_branch.as_str()])?;
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
    content
        .lines()
        .map(str::trim_end)
        .filter(|line| line.starts_with("- "))
        .map(ToOwned::to_owned)
        .collect()
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

#[cfg(test)]
mod tests {
    use super::extract_review_items;

    #[test]
    fn extracts_bullet_review_items() {
        let content = "# COMPLETED\n\n- `VAL-001` Added validation\n- `SEC-001` Hardened auth\n";
        let items = extract_review_items(content);
        assert_eq!(
            items,
            vec![
                "- `VAL-001` Added validation".to_string(),
                "- `SEC-001` Hardened auth".to_string()
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
}
