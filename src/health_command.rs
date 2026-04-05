use std::fs;

use anyhow::{bail, Context, Result};

use crate::codex_exec::run_codex_exec;
use crate::util::{atomic_write, ensure_repo_layout, git_repo_root, git_stdout, timestamp_slug};
use crate::HealthArgs;

const DEFAULT_HEALTH_PROMPT: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, staging, and local-run rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `REVIEW.md`, `WORKLIST.md`, `LEARNINGS.md`, and `HEALTH.md` if they exist.
0c. Run a monolithic repo-health pass. You may use helper workflows if they are available, but you must satisfy the health contract below even if those helpers are missing.

1. Your task is to produce a truthful repo-wide quality report for the currently checked-out branch.
   - Detect the real validation surface from the repository itself: AGENTS instructions, package manifests, CI config, Makefiles, scripts, and existing docs.
   - Prefer running the repo's actual checks over describing what should probably be run.
   - Do not invent checkers that are not present.

2. Use this health workflow:
   - Identify the main verification lanes that actually exist in the repo: build, lint, typecheck, tests, dead-code checks, formatting, smoke checks, or equivalent.
   - Run the strongest available commands that are honest for this repo.
   - Capture direct evidence for each lane: command, pass/fail result, notable warnings, and whether the result is complete or partial.
   - Distinguish repo problems from toolchain or environment problems when possible.

3. Maintain `HEALTH.md` as the durable report for this branch:
   - Record the date, branch, and the commands you ran.
   - Score the repo from 0-10 overall.
   - Include sub-scores for build, correctness, static analysis, and test confidence when the repo exposes those lanes.
   - Record blockers, warnings, and blind spots.
   - Include a short trend note if an older `HEALTH.md` exists and gives you a real prior comparison.

4. This is report-first:
   - Do not change source code, tests, build config, or docs other than `HEALTH.md`.
   - Do not stage, commit, or push.
   - Do not fake a green score when key lanes were skipped or unavailable.

99999. Important: prefer direct command evidence over assumptions.
999999. Important: a partial health run must say it is partial.
9999999. Important: the score is only useful if it reflects what you actually ran."#;

pub(crate) async fn run_health(args: HealthArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    if let Some(required_branch) = args.branch.as_deref() {
        if current_branch != required_branch {
            bail!(
                "auto health must run on branch `{}` (current: `{}`)",
                required_branch,
                current_branch
            );
        }
    }

    let prompt_template = match &args.prompt_file {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display()))?,
        None => DEFAULT_HEALTH_PROMPT.to_string(),
    };
    let full_prompt = format!("{prompt_template}\n\nExecute the instructions above.");

    let run_root = args
        .run_root
        .unwrap_or_else(|| repo_root.join(".auto").join("health"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let stderr_log_path = run_root.join("codex.stderr.log");
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("health-{}-prompt.md", timestamp_slug()));
    atomic_write(&prompt_path, full_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("prompt log:  {}", prompt_path.display());

    println!("auto health");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", current_branch);
    println!("model:       {}", args.model);
    println!("reasoning:   {}", args.reasoning_effort);
    println!("run root:    {}", run_root.display());

    let exit_status = run_codex_exec(
        &repo_root,
        &full_prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        &stderr_log_path,
        "auto health",
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
    println!("health run complete");
    Ok(())
}
