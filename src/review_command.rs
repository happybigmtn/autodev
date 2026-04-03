use std::fs;
use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::codex_stream;
use crate::util::{
    atomic_write, ensure_repo_layout, ensure_tracked_worktree_clean, git_repo_root,
    git_stdout, git_tracked_status, run_git, timestamp_slug,
};
use crate::ReviewArgs;

pub(crate) const DEFAULT_REVIEW_PROMPT: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, and staging rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, and `LEARNINGS.md` if they exist.
0c. Use the installed `/ce:review` workflow as your primary review process if it is available in this Codex environment. If `/ce:review` is unavailable, fall back to `/review`. Use `/ce:work` when you need to turn concrete review findings into follow-up implementation work. Use `/ce:compound` to capture durable learnings in `LEARNINGS.md`.

1. Your task is to review the items currently listed in `REVIEW.md`.
   - Treat each review item as a claim that must be verified against the codebase, the specs, and the implementation plan.
   - Re-read the owned surfaces, integration touchpoints, and validation evidence for those items before trusting the claim.
   - Run a broad engineering review, not a status recap: look for regressions, weak assumptions, missing edge cases, security issues, integration gaps, and test blind spots.

2. Respect the queue split:
   - `REVIEW.md` is the in-flight review queue.
   - `COMPLETED.md` is free to keep receiving new implementation completions while review is running.
   - Do not move items back into `IMPLEMENTATION_PLAN.md`.

3. If you find problems:
   - Append concrete follow-up items to `WORKLIST.md`. Create it if missing.
   - Use `/ce:work` to address the worklist items and review findings directly when the next best action is implementation.
   - Record durable learnings in `LEARNINGS.md` via `/ce:compound`.
   - Leave any not-yet-cleared entries in `REVIEW.md` until the fixes are actually landed and supported by the codebase.
   - Keep `AGENTS.md` operational only.

4. If a review item passes review:
   - Move its entry from `REVIEW.md` into `ARCHIVED.md`.
   - `ARCHIVED.md` should be append-only history.
   - Only archive items that are genuinely complete after review and any follow-up fixes.

5. Commit and push only truthful review increments:
   - Stay on the branch that is already checked out when `auto review` starts.
   - Do not create or switch branches during the review pass.
   - Stage only the files relevant to the review fixes plus `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, `LEARNINGS.md`, and `AGENTS.md` when they changed.
   - Commit with a message like `repo-name: review completed items`.
   - Push back to that same branch after each successful commit-producing pass.

6. If `REVIEW.md` is empty or has no reviewable items:
   - Do not invent work.
   - Say so briefly and stop without making changes.

99999. Important: prefer fixing findings over explaining them.
999999. Important: do not archive an item until the code and review evidence support it.
9999999. Important: use `/ce:review` aggressively, use `/ce:work` for concrete fixes, and use `/ce:compound` to make future work easier. This is a bug-finding and hardening pass, not a feature pass."#;

const EMPTY_COMPLETED_DOC: &str = "# COMPLETED\n\n";
const REVIEW_HEADER: &str = "# REVIEW";

pub(crate) async fn run_review(args: ReviewArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    ensure_tracked_worktree_clean(&repo_root, "auto review")?;

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

    let mut iteration = 0usize;
    while iteration < args.max_iterations {
        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("review-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;

        let commit_before = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        let tracked_status_before = git_tracked_status(&repo_root)?;
        println!();
        println!("running review iteration {}", iteration + 1);

        let exit_status = run_codex_iteration(
            &repo_root,
            &full_prompt,
            &args.model,
            &args.reasoning_effort,
            &args.codex_bin,
            &stderr_log_path,
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
        let tracked_status_after = git_tracked_status(&repo_root)?;
        if commit_before.trim() == commit_after.trim() {
            if tracked_status_before.trim() != tracked_status_after.trim() {
                bail!(
                    "Codex changed tracked files without creating a commit during auto review:\n{}",
                    tracked_status_after.trim_end()
                );
            }
            println!("no new commit detected; stopping.");
            break;
        }

        run_git(&repo_root, ["push", "origin", push_branch.as_str()])?;
        if tracked_status_before.trim() != tracked_status_after.trim() {
            bail!(
                "auto review iteration created commit {} but left tracked changes behind:\n{}",
                commit_after.trim(),
                tracked_status_after.trim_end()
            );
        }
        iteration += 1;
        println!();
        println!("================ REVIEW {} ================", iteration);
    }

    Ok(())
}

async fn run_codex_iteration(
    repo_root: &Path,
    full_prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    stderr_log_path: &Path,
) -> Result<std::process::ExitStatus> {
    let mut command = TokioCommand::new(codex_bin);
    command
        .arg("exec")
        .arg("--json")
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .arg("--skip-git-repo-check")
        .arg("--cd")
        .arg(repo_root)
        .arg("-m")
        .arg(model)
        .arg("-c")
        .arg(format!("model_reasoning_effort=\"{reasoning_effort}\""))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch Codex at {} from {}",
            codex_bin.display(),
            repo_root.display()
        )
    })?;

    let mut stdin = child
        .stdin
        .take()
        .context("Codex stdin should be piped for auto review")?;
    stdin
        .write_all(full_prompt.as_bytes())
        .await
        .context("failed to write Codex review prompt")?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .context("Codex stdout should be piped for auto review")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex stderr should be piped for auto review")?;

    let stdout_task = tokio::spawn(async move { codex_stream::stream_codex_output(stdout).await });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child.wait().await.context("failed waiting for Codex")?;
    stdout_task
        .await
        .context("Codex stdout streaming task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Codex stderr capture task panicked")??;
    if !stderr_text.trim().is_empty() {
        let entry = format!("\n===== {} =====\n{stderr_text}\n", timestamp_slug());
        let mut existing = if stderr_log_path.exists() {
            fs::read(stderr_log_path)
                .with_context(|| format!("failed to read {}", stderr_log_path.display()))?
        } else {
            Vec::new()
        };
        existing.extend_from_slice(entry.as_bytes());
        atomic_write(stderr_log_path, &existing)?;
    }

    Ok(status)
}

async fn read_stream<R>(stream: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .await
        .context("failed to read child stream")?;
    Ok(text)
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
