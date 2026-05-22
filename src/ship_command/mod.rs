mod branch;
mod gate;
mod prompt;
#[cfg(test)]
mod testkit;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::codex_exec::run_codex_exec;
use crate::ship_command::branch::resolve_base_branch;
use crate::ship_command::gate::{
    evaluate_ship_gate, record_ship_gate_blockers_with_verdict,
    record_ship_gate_bypass_with_verdict, validate_ship_gate_bypass_reason,
};
use crate::ship_command::prompt::render_default_ship_prompt;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, sync_branch_with_remote, timestamp_slug,
};
use crate::ShipArgs;

#[derive(Clone, Copy)]
enum ShipGatePhase {
    BeforeModel,
    AfterModelIteration,
}

impl ShipGatePhase {
    fn blocked_verdict(self) -> &'static str {
        match self {
            Self::BeforeModel => "Blocked before model execution",
            Self::AfterModelIteration => "Blocked after model iteration before readiness",
        }
    }

    fn bypassed_verdict(self) -> &'static str {
        match self {
            Self::BeforeModel => "Bypassed before model execution",
            Self::AfterModelIteration => "Bypassed after model iteration before readiness",
        }
    }

    fn failure_context(self) -> &'static str {
        match self {
            Self::BeforeModel => "before model execution",
            Self::AfterModelIteration => "after model iteration before readiness",
        }
    }
}

fn enforce_ship_gate(
    repo_root: &Path,
    branch: &str,
    base_branch: &str,
    bypass_reason: Option<&str>,
    phase: ShipGatePhase,
) -> Result<()> {
    let ship_gate = evaluate_ship_gate(repo_root, branch, base_branch);
    if let Some(reason) = bypass_reason {
        record_ship_gate_bypass_with_verdict(
            repo_root,
            branch,
            base_branch,
            phase.bypassed_verdict(),
            reason,
            &ship_gate,
        )?;
        println!("release gate: bypassed; reason recorded in SHIP.md");
    } else if ship_gate.is_blocked() {
        record_ship_gate_blockers_with_verdict(
            repo_root,
            branch,
            base_branch,
            phase.blocked_verdict(),
            &ship_gate,
        )?;
        bail!(
            "auto ship release gate failed {}:\n{}",
            phase.failure_context(),
            ship_gate
                .blockers
                .iter()
                .map(|blocker| format!("- {blocker}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    } else {
        println!("release gate: passed");
    }
    Ok(())
}

pub(crate) async fn run_ship(args: ShipArgs) -> Result<()> {
    run_ship_in_repo(git_repo_root()?, args).await
}

async fn run_ship_in_repo(repo_root: PathBuf, args: ShipArgs) -> Result<()> {
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

    let bypass_reason = args.bypass_release_gate.as_deref().map(str::trim);
    if let Some(reason) = bypass_reason {
        validate_ship_gate_bypass_reason(reason)?;
    }

    if let Some(commit) =
        auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "ship checkpoint")?
    {
        println!("checkpoint:  committed pre-existing ship changes at {commit}");
    } else if sync_branch_with_remote(&repo_root, push_branch.as_str())? {
        println!("remote sync: rebased onto origin/{}", push_branch);
    }

    enforce_ship_gate(
        &repo_root,
        &push_branch,
        &base_branch,
        bypass_reason,
        ShipGatePhase::BeforeModel,
    )?;

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
                enforce_ship_gate(
                    &repo_root,
                    &push_branch,
                    &base_branch,
                    bypass_reason,
                    ShipGatePhase::AfterModelIteration,
                )?;
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
        enforce_ship_gate(
            &repo_root,
            &push_branch,
            &base_branch,
            bypass_reason,
            ShipGatePhase::AfterModelIteration,
        )?;
        iteration += 1;
        println!();
        println!("================ SHIP {} ================", iteration);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::run_ship_in_repo;
    use crate::ship_command::gate::evaluate_ship_gate;
    use crate::ship_command::testkit::{
        command_ok, commit_all, init_main_git_repo, setup_origin, ship_args, test_dir,
        write_fake_codex_script, write_passing_release_receipts_for_head, write_release_reports,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn ship_gate_runs_after_checkpoint_before_model() {
        let repo = test_dir("checkpoint-before-gate");
        init_main_git_repo(&repo);
        fs::create_dir_all(repo.join("src")).expect("failed to create src");
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn release_value() -> u8 { 1 }\n",
        )
        .expect("failed to write source");
        write_release_reports(&repo, "main", "main");
        commit_all(&repo, "release baseline");
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn release_value() -> u8 { 2 }\n",
        )
        .expect("failed to dirty source");
        setup_origin(&repo, "checkpoint-before-gate-origin");
        let fake_codex = repo.join(".auto/fake-codex.sh");
        fs::create_dir_all(repo.join(".auto")).expect("failed to create .auto");
        write_fake_codex_script(
            &fake_codex,
            "#!/bin/sh\ncat >/dev/null\n: > codex-invoked\nexit 0\n",
        );
        write_passing_release_receipts_for_head(&repo);
        let pre_checkpoint_report = evaluate_ship_gate(&repo, "main", "main");
        assert!(
            !pre_checkpoint_report.is_blocked(),
            "pre-checkpoint gate should pass so this test proves ordering; blockers: {:?}",
            pre_checkpoint_report.blockers
        );

        let err = run_ship_in_repo(repo.clone(), ship_args(&repo, fake_codex))
            .await
            .expect_err("stale post-checkpoint receipts should stop ship before model work");

        assert!(
            err.to_string().contains("release gate failed"),
            "unexpected error: {err}"
        );
        assert!(
            !repo.join("codex-invoked").exists(),
            "model ship prep must not run after post-checkpoint gate failure"
        );
        let ship = fs::read_to_string(repo.join("SHIP.md")).expect("failed to read SHIP.md");
        assert!(ship.contains("Blocked before model execution"));
        assert!(ship.contains("stale validation receipt"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ship_gate_runs_after_remote_sync_before_model() {
        let repo = test_dir("remote-sync-before-gate");
        init_main_git_repo(&repo);
        fs::create_dir_all(repo.join("src")).expect("failed to create src");
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn release_value() -> u8 { 1 }\n",
        )
        .expect("failed to write source");
        write_release_reports(&repo, "main", "main");
        commit_all(&repo, "release baseline");
        let fake_codex = repo.join(".auto/fake-codex.sh");
        fs::create_dir_all(repo.join(".auto")).expect("failed to create .auto");
        write_fake_codex_script(
            &fake_codex,
            "#!/bin/sh\ncat >/dev/null\n: > codex-invoked\nexit 0\n",
        );
        let origin = setup_origin(&repo, "remote-sync-before-gate-origin");
        write_passing_release_receipts_for_head(&repo);
        let updater = test_dir("remote-sync-before-gate-updater");
        let clone_output = Command::new("git")
            .args(["clone", origin.to_str().unwrap(), updater.to_str().unwrap()])
            .output()
            .expect("git clone failed");
        assert!(
            clone_output.status.success(),
            "git clone failed: {}",
            String::from_utf8_lossy(&clone_output.stderr)
        );
        command_ok(&updater, ["config", "user.email", "test@example.com"]);
        command_ok(&updater, ["config", "user.name", "Test User"]);
        fs::write(
            updater.join("src/lib.rs"),
            "pub fn release_value() -> u8 { 2 }\n",
        )
        .expect("failed to write remote source");
        commit_all(&updater, "remote release change");
        command_ok(&updater, ["push", "origin", "main"]);

        let err = run_ship_in_repo(repo.clone(), ship_args(&repo, fake_codex))
            .await
            .expect_err("stale post-sync receipts should stop ship before model work");

        assert!(
            err.to_string().contains("release gate failed"),
            "unexpected error: {err}"
        );
        assert!(
            !repo.join("codex-invoked").exists(),
            "model ship prep must not run after post-sync gate failure"
        );
        let ship = fs::read_to_string(repo.join("SHIP.md")).expect("failed to read SHIP.md");
        assert!(ship.contains("Blocked before model execution"));
        assert!(ship.contains("stale validation receipt"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ship_gate_reruns_after_model_iteration_changes() {
        let repo = test_dir("rerun-after-model-changes");
        init_main_git_repo(&repo);
        write_release_reports(&repo, "main", "main");
        commit_all(&repo, "release reports");
        setup_origin(&repo, "rerun-after-model-changes-origin");
        let fake_codex = repo.join(".auto/fake-codex.sh");
        fs::create_dir_all(repo.join(".auto")).expect("failed to create .auto");
        write_fake_codex_script(
            &fake_codex,
            "#!/bin/sh\ncat >/dev/null\ncat > SHIP.md <<'EOF'\n# SHIP\n\nRelease Blockers:\n- unresolved production blocker\nRollback: revert.\nMonitoring: inspect CI.\nPR: none.\nEOF\nexit 0\n",
        );
        write_passing_release_receipts_for_head(&repo);

        let err = run_ship_in_repo(repo.clone(), ship_args(&repo, fake_codex))
            .await
            .expect_err("post-model release blockers should be gated before readiness");

        assert!(
            err.to_string().contains("release gate failed"),
            "unexpected error: {err}"
        );
        let ship = fs::read_to_string(repo.join("SHIP.md")).expect("failed to read SHIP.md");
        assert!(ship.contains("Blocked after model iteration before readiness"));
        assert!(ship.contains("unresolved production blocker"));
    }
}
