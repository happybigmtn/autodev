use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::codex_exec::run_codex_exec;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, sync_branch_with_remote, timestamp_slug,
};
use crate::ShipArgs;

const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];

pub(crate) const DEFAULT_SHIP_PROMPT_TEMPLATE: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, staging, deployment, and local-run rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, `LEARNINGS.md`, `QA.md`, `HEALTH.md`, `README.md`, `CHANGELOG.md`, and `VERSION` if they exist.
0c. Run a monolithic ship-prep pass. You may use helper workflows or GitHub/deploy tools if they are available, but you must satisfy the shipping contract below even if those helpers are missing.

1. Your task is to prepare branch `{branch}` to ship against base branch `{base_branch}`.
   - Build a release checklist from the branch diff, the current QA and review state, and the repo's actual release surfaces.
   - Treat unresolved critical issues, broken validation, and stale documentation as shipping blockers until proven otherwise.
   - Do not invent release infrastructure that the repo does not have.

2. Use this shipping workflow end-to-end:
   - Confirm the current branch diff against `{base_branch}` and identify the blast radius of what is actually shipping.
   - If it is safe and necessary, bring the branch up to date with the latest remote base branch before continuing. If that sync becomes conflicted or ambiguous, stop and report the blocker truthfully.
   - Run the real validation commands required by this repo.
   - Review the shipping diff for release risk: structural regressions, accidental leftovers, docs drift, migration risk, security issues, performance regressions, accessibility regressions on user-facing surfaces, and missing verification.
   - If `VERSION` exists and the branch genuinely warrants a version update, update it truthfully.
   - If `CHANGELOG.md` exists, update only the relevant entry for what is actually shipping. Do not clobber unrelated history.
   - If README or other project docs drifted relative to what is shipping, sync them.
   - If `QA.md` or `HEALTH.md` is missing or obviously stale relative to the branch, run enough direct verification to ship truthfully instead of trusting stale reports.
   - If the repo uses feature flags, staged rollout controls, canaries, or safe-default rollout patterns, prefer deploy-off / release-on handling over immediate full exposure.

3. Maintain `SHIP.md` as the durable release report for this branch:
   - Record the branch, base branch, and the exact validations you ran.
   - Record what changed for release bookkeeping: docs, changelog, version, or release notes.
   - Record shipping blockers, open follow-ups, and the final ship verdict.
   - Record the rollback path: what gets reverted, disabled, or rolled back first if this ship causes trouble.
   - Record the monitoring path: which metrics, logs, checks, dashboards, previews, or canary signals were actually available.
   - If a feature flag or staged rollout path exists, record the chosen rollout posture and any cleanup follow-up for that flag/control.
   - Append unresolved blockers or follow-up items to `WORKLIST.md` so they re-enter the active queue outside the release report.
   - If a PR exists or you create one, record the URL.
   - If you can perform preview, deploy, or post-push verification, record what you checked and what you observed.

4. Commit and push only truthful shipping increments:
   - Stay on branch `{branch}`.
   - Do not create or switch local branches.
   - Stage only the files relevant to shipping work plus `SHIP.md`, `CHANGELOG.md`, `VERSION`, docs, `WORKLIST.md`, `LEARNINGS.md`, `QA.md`, `HEALTH.md`, and `AGENTS.md` when they changed.
   - Commit with a message like `repo-name: ship prep`.
   - Push back to `origin/{branch}` after each successful commit-producing pass.
   - If `{branch}` is not `{base_branch}` and `gh` is available, create or refresh a PR targeting `{base_branch}`.
   - If `{branch}` already equals `{base_branch}`, skip PR creation and say so plainly in `SHIP.md`.

5. Post-push verification:
   - If the repo exposes preview URLs, deploy health checks, or a clear post-push verification path, run a lightweight verification pass and record the evidence.
   - If accessibility or performance checks are materially part of release confidence for a user-facing repo, record what you actually checked and what was not checked.
   - If deploy or canary verification is not realistically available, say so plainly instead of pretending the branch was production-verified.

6. Stop conditions:
   - If shipping blockers remain, do not fake readiness.
   - If validation is red and you cannot honestly fix it inside this pass, record the blocker in `SHIP.md` and `WORKLIST.md`, then stop.

99999. Important: shipping is a truth-telling workflow, not a ceremony workflow.
999999. Important: do not rewrite release history, changelog history, or version history casually.
9999999. Important: prefer a blocked but honest ship report over a fake green release."#;

fn render_default_ship_prompt(branch: &str, base_branch: &str) -> String {
    DEFAULT_SHIP_PROMPT_TEMPLATE
        .replace("{branch}", branch)
        .replace("{base_branch}", base_branch)
}

pub(crate) async fn run_ship(args: ShipArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    let push_branch = args
        .branch
        .clone()
        .unwrap_or_else(|| current_branch.clone());
    if current_branch != push_branch {
        bail!(
            "auto ship must run on branch `{}` (current: `{}`)",
            push_branch,
            current_branch
        );
    }

    let base_branch =
        resolve_base_branch(&repo_root, args.base_branch.as_deref(), &current_branch)?;
    let prompt_template = match &args.prompt_file {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display()))?,
        None => render_default_ship_prompt(&push_branch, &base_branch),
    };
    let full_prompt = format!("{prompt_template}\n\nExecute the instructions above.");

    let run_root = args
        .run_root
        .unwrap_or_else(|| repo_root.join(".auto").join("ship"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let stderr_log_path = run_root.join("codex.stderr.log");

    println!("auto ship");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", push_branch);
    println!("base branch: {}", base_branch);
    println!("model:       {}", args.model);
    println!("reasoning:   {}", args.reasoning_effort);
    println!("run root:    {}", run_root.display());

    if let Some(commit) =
        auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "ship checkpoint")?
    {
        println!("checkpoint:  committed pre-existing ship changes at {commit}");
    } else if sync_branch_with_remote(&repo_root, push_branch.as_str())? {
        println!("remote sync: rebased onto origin/{}", push_branch);
    }

    let mut iteration = 0usize;
    while iteration < args.max_iterations {
        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("ship-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        println!("prompt log:  {}", prompt_path.display());

        let commit_before = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        println!();
        println!("running ship iteration {}", iteration + 1);

        let exit_status = run_codex_exec(
            &repo_root,
            &full_prompt,
            &args.model,
            &args.reasoning_effort,
            &args.codex_bin,
            &stderr_log_path,
            None,
            "auto ship",
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
        println!("ship iteration complete");

        let commit_after = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        if commit_before.trim() == commit_after.trim() {
            if let Some(commit) =
                auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "ship checkpoint")?
            {
                iteration += 1;
                println!("checkpoint:  committed iteration changes at {commit}");
                println!();
                println!("================ SHIP {} ================", iteration);
                continue;
            }
            println!("no new commit detected; stopping.");
            break;
        }

        if push_branch_with_remote_sync(&repo_root, push_branch.as_str())? {
            println!("remote sync: rebased onto origin/{}", push_branch);
        }
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "ship checkpoint")?
        {
            println!("checkpoint:  committed trailing changes at {commit}");
        }
        iteration += 1;
        println!();
        println!("================ SHIP {} ================", iteration);
    }

    Ok(())
}

fn resolve_base_branch(
    repo_root: &Path,
    requested_base_branch: Option<&str>,
    current_branch: &str,
) -> Result<String> {
    if let Some(branch) = requested_base_branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
    {
        return Ok(branch.to_string());
    }

    let origin_head = git_stdout(
        repo_root,
        [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok();
    if let Some(branch) = origin_head.and_then(|value| parse_origin_head_branch(&value)) {
        return Ok(branch);
    }

    if let Some(branch) = KNOWN_PRIMARY_BRANCHES
        .into_iter()
        .find(|candidate| git_branch_exists(repo_root, candidate) && *candidate != current_branch)
    {
        return Ok(branch.to_string());
    }

    if KNOWN_PRIMARY_BRANCHES.contains(&current_branch) {
        return Ok(current_branch.to_string());
    }

    bail!(
        "auto ship could not resolve the repo's base branch; pass `--base-branch <name>` explicitly"
    );
}

fn parse_origin_head_branch(origin_head: &str) -> Option<String> {
    let trimmed = origin_head.trim();
    let branch = trimmed.strip_prefix("origin/").unwrap_or(trimmed).trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

fn git_branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_ref_exists(repo_root, &format!("refs/heads/{branch}"))
        || git_ref_exists(repo_root, &format!("refs/remotes/origin/{branch}"))
}

fn git_ref_exists(repo_root: &Path, git_ref: &str) -> bool {
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
    use super::render_default_ship_prompt;

    #[test]
    fn default_ship_prompt_includes_operational_release_controls() {
        let prompt = render_default_ship_prompt("main", "trunk");
        assert!(prompt.contains("rollback path"));
        assert!(prompt.contains("monitoring path"));
        assert!(prompt.contains("accessibility regressions"));
        assert!(prompt.contains("feature flags"));
        assert!(prompt.contains("branch `main`"));
        assert!(prompt.contains("base branch `trunk`"));
    }
}
