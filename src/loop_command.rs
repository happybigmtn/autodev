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
use crate::LoopArgs;

pub(crate) const DEFAULT_LOOP_PROMPT: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, and staging rules.
0b. Study `specs/*` with full repo context to understand the application specifications.
0c. Study `IMPLEMENTATION_PLAN.md`.

1. Your task is to implement functionality per the specifications using the full repository context. Follow `IMPLEMENTATION_PLAN.md` in order and take the next unchecked task from top to bottom. Do not reprioritize the queue yourself. Before making changes, search the codebase and existing planning artifacts. Do not assume a surface is missing until you verify it.

2. Implement the task completely:
   - Stay within the task contract's owned surfaces plus the minimum adjacent integration edits needed to make the code work.
   - Run the relevant proof commands for the task and debug until they pass.
   - If the repo is still greenfield, perform the bootstrap work the plan requires instead of pretending later tasks are ready.
   - Do not leave placeholders, TODOs, or half-wired scaffolding.

3. Keep the planning artifacts current:
   - When you discover important implementation facts or blockers, update `IMPLEMENTATION_PLAN.md`.
   - When you finish a task, remove its entry from `IMPLEMENTATION_PLAN.md` so the plan remains an active queue of unfinished work only.
   - Append a concise record to `COMPLETED.md` with task id, validation command, and commit sha.
   - Update `AGENTS.md` only when you learn something operational that will help future loops run or validate the repo correctly.

4. When validation passes, commit the increment:
   - Stage only the files relevant to the completed task plus `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, and `AGENTS.md`.
   - Do not sweep unrelated pre-existing churn into the commit.
   - Commit with a message like `repo-name: TASK-ID short description` using this repository's actual name.
   - After committing, run `git status` to verify no implementation files were left unstaged. If any were, amend the commit.
   - Push directly to `origin/main` after the commit.

5. If you hit a real blocker after genuine debugging:
   - Record the blocker under the task in `IMPLEMENTATION_PLAN.md`.
   - Commit the planning update if it materially changes the execution record.
   - Move to the next ready task instead of repeating the same failed attempt.

6. Task-order rule:
   - Treat the order in `IMPLEMENTATION_PLAN.md` as authoritative.
   - Work on the first unchecked task unless its explicit dependencies are still unchecked.
   - If the current task is already satisfied, remove it from `IMPLEMENTATION_PLAN.md`, append a truthful note to `COMPLETED.md`, and continue downward.

7. Branch rule:
   - Work only on branch `main`.
   - Do not create or push feature branches, lane branches, or topic branches.

99999. Important: keep `AGENTS.md` operational only.
999999. Important: prefer complete working increments over placeholders.
9999999. Important: if unrelated tests fail and they prevent a truthful green result, fix them as part of the increment.
99999999. CRITICAL: Do not assume functionality is missing — search the codebase to confirm before implementing anything new.
999999999. Every new module must be importable and wired into the package. Dead code that isn't reachable from any entry point is an island — wire it before committing.
9999999999. When you learn something new about how to build, run, or validate the repo, update `AGENTS.md` — but keep it brief and operational only.
99999999999. As soon as there are no build or test errors, create a git tag. If no git tags exist start at 0.0.0 and increment patch by 1 (e.g. 0.0.1)."#;

pub(crate) async fn run_loop(args: LoopArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    ensure_tracked_worktree_clean(&repo_root, "auto loop")?;

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    if current_branch.trim() != args.branch {
        bail!(
            "auto loop must run on branch `{}` (current: `{}`)",
            args.branch,
            current_branch.trim()
        );
    }

    let prompt_template = match &args.prompt_file {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display()))?,
        None => DEFAULT_LOOP_PROMPT.to_string(),
    };
    let full_prompt = format!("{prompt_template}\n\nExecute the instructions above.");

    let run_root = args
        .run_root
        .unwrap_or_else(|| repo_root.join(".auto").join("loop"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let stderr_log_path = run_root.join("codex.stderr.log");

    println!("auto loop");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", args.branch);
    println!("model:       {}", args.model);
    println!("reasoning:   {}", args.reasoning_effort);
    println!("run root:    {}", run_root.display());
    println!(
        "prompt:      {}",
        args.prompt_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "built-in Ralph worker".to_string())
    );

    let mut iteration = 0usize;
    loop {
        if args.max_iterations.is_some_and(|limit| iteration >= limit) {
            println!(
                "reached max iterations: {}",
                args.max_iterations.unwrap_or_default()
            );
            break;
        }

        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("loop-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;

        let commit_before = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        let tracked_status_before = git_tracked_status(&repo_root)?;
        println!();
        println!("running Codex iteration {}", iteration + 1);

        let exit_status = run_codex_iteration(
            &repo_root,
            &prompt_path,
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
        println!("Codex iteration complete");

        let commit_after = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        let tracked_status_after = git_tracked_status(&repo_root)?;
        if commit_before.trim() == commit_after.trim() {
            if tracked_status_before.trim() != tracked_status_after.trim() {
                bail!(
                    "Codex changed tracked files without creating a commit during auto loop:\n{}",
                    tracked_status_after.trim_end()
                );
            }
            println!("no new commit detected; stopping.");
            break;
        }

        run_git(&repo_root, ["push", "origin", args.branch.as_str()])?;
        if tracked_status_before.trim() != tracked_status_after.trim() {
            bail!(
                "auto loop iteration created commit {} but left tracked changes behind:\n{}",
                commit_after.trim(),
                tracked_status_after.trim_end()
            );
        }
        iteration += 1;
        println!();
        println!("================ LOOP {} ================", iteration);
    }

    Ok(())
}

async fn run_codex_iteration(
    repo_root: &Path,
    prompt_path: &Path,
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
        .context("Codex stdin should be piped for auto loop")?;
    stdin
        .write_all(full_prompt.as_bytes())
        .await
        .context("failed to write Codex loop prompt")?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .context("Codex stdout should be piped for auto loop")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex stderr should be piped for auto loop")?;

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

    let _ = prompt_path;
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
