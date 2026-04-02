use std::fs;
use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use console::Style;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::util::{
    atomic_write, clip_line_for_display, ensure_repo_layout, git_repo_root, git_stdout, run_git,
    timestamp_slug,
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

#[derive(Default)]
struct LoopRenderState {
    tool_count: usize,
}

pub(crate) async fn run_loop(args: LoopArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;

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
        if commit_before.trim() == commit_after.trim() {
            println!("no new commit detected; stopping.");
            break;
        }

        run_git(&repo_root, ["push", "origin", args.branch.as_str()])?;
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

    let stdout_task = tokio::spawn(async move { stream_codex_output(stdout).await });
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

async fn stream_codex_output<R>(stream: R) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream).lines();
    let mut state = LoopRenderState::default();
    while let Some(line) = reader
        .next_line()
        .await
        .context("failed reading Codex JSON stream")?
    {
        render_codex_stream_line(&line, &mut state);
    }
    Ok(())
}

fn render_codex_stream_line(line: &str, state: &mut LoopRenderState) {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        if !line.trim().is_empty() {
            eprintln!("{line}");
        }
        return;
    };

    let green = Style::new().green();
    let yellow = Style::new().yellow();
    let red = Style::new().red();
    let cyan = Style::new().cyan();
    let dim = Style::new().dim();

    let event = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event {
        "item.started" | "task_started" => println!("{}", cyan.apply_to("* task_started")),
        "item.completed" => println!("{}", cyan.apply_to("* task_completed")),
        "agent_reasoning" | "reasoning" => {
            print_block("thinking: ", json_string(&value, "text"), &dim, 3);
        }
        "tool.call" | "tool_use" => {
            state.tool_count += 1;
            let name = value.get("name").and_then(Value::as_str).unwrap_or("tool");
            println!("{}", yellow.apply_to(format!("  > {name}")));
            print_block("   ", json_string(&value, "command"), &dim, 2);
        }
        "message" | "agent_message" => {
            let text = json_string(&value, "text").or_else(|| {
                value
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
            print_block("", text, &Style::new(), 6);
        }
        "completed" | "turn.completed" => {
            let usage = value.get("usage").cloned().unwrap_or(Value::Null);
            let input = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            println!();
            println!("========================================");
            println!(
                "{} | Tokens: in {} out {} | Tools: {}",
                green.apply_to("done"),
                input,
                output,
                state.tool_count
            );
        }
        "error" => {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            println!("{}", red.apply_to(format!("error: {message}")));
        }
        _ => {}
    }
}

fn print_block(prefix: &str, text: Option<String>, style: &Style, limit: usize) {
    let Some(text) = text else {
        return;
    };
    let mut shown = 0usize;
    let lines = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    for line in &lines {
        if shown >= limit {
            break;
        }
        let clipped = if line.chars().count() > 140 {
            format!("{}...", clip_line_for_display(line, 137))
        } else {
            (*line).to_string()
        };
        println!("{}", style.apply_to(format!("{prefix}{clipped}")));
        shown += 1;
    }
    if lines.len() > limit {
        println!(
            "{}",
            Style::new()
                .dim()
                .apply_to(format!("{prefix}... +{} more lines", lines.len() - limit))
        );
    }
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}
