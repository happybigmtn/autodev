use std::fs;

use anyhow::{bail, Context, Result};

use crate::codex_exec::run_codex_exec;
use crate::qa_only_command::{
    allowed_report_only_dirty_paths, collect_dirty_state, print_final_status_block,
    report_only_dirty_state_report, require_nonempty_report,
};
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
    let baseline_dirty_state = collect_dirty_state(&repo_root)?;

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
    let allowed_dirty_paths =
        allowed_report_only_dirty_paths(&repo_root, &run_root, "HEALTH.md", ".auto/health");
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
        None,
        "auto health",
    )
    .await?;
    let dirty_report =
        report_only_dirty_state_report(&repo_root, &baseline_dirty_state, &allowed_dirty_paths)?;
    if dirty_report.has_violations() {
        bail!(
            "{}",
            dirty_report.render("auto health", "`HEALTH.md` and allowed health logs")
        );
    }
    if dirty_report.has_preexisting_dirty_state() {
        eprintln!("{}", dirty_report.render_preexisting());
    }
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

    require_nonempty_report(&repo_root.join("HEALTH.md"), "HEALTH.md")?;
    print_final_status_block(
        "health report complete",
        &[
            repo_root.join("HEALTH.md").display().to_string(),
            prompt_path.display().to_string(),
            stderr_log_path.display().to_string(),
        ],
        "none",
        "review HEALTH.md, then address blockers or run auto qa-only for runtime QA",
    );
    println!();
    println!("health run complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::qa_only_command::{
        format_final_status_block, report_only_dirty_state_report, require_nonempty_report,
    };

    use super::*;

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "autodev-health-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("failed to create temp dir");
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn run_git_in<'a>(repo: &std::path::Path, args: impl IntoIterator<Item = &'a str>) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to launch git");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(name: &str) -> TestTempDir {
        let repo = TestTempDir::new(name);
        run_git_in(repo.path(), ["init"]);
        run_git_in(repo.path(), ["config", "user.name", "autodev tests"]);
        run_git_in(repo.path(), ["config", "user.email", "autodev@example.com"]);
        fs::write(repo.path().join("README.md"), "# temp\n").expect("failed to write README");
        run_git_in(repo.path(), ["add", "README.md"]);
        run_git_in(repo.path(), ["commit", "-m", "init"]);
        repo
    }

    #[test]
    fn health_requires_non_empty_report() {
        let temp = TestTempDir::new("requires-report");
        let health_path = temp.path().join("HEALTH.md");

        assert!(require_nonempty_report(&health_path, "HEALTH.md").is_err());
        fs::write(&health_path, "\n\n").expect("failed to write empty health report");
        assert!(require_nonempty_report(&health_path, "HEALTH.md").is_err());
        fs::write(&health_path, "# Health\n\n- Status: checked\n")
            .expect("failed to write health report");
        require_nonempty_report(&health_path, "HEALTH.md").expect("health report should pass");
    }

    #[test]
    fn health_report_only_rejects_disallowed_dirty_state() {
        let repo = init_repo("rejects-dirty");
        let baseline = collect_dirty_state(repo.path()).expect("baseline should work");
        fs::write(repo.path().join("src.rs"), "pub fn changed() {}\n")
            .expect("failed to write source");

        let allowed = allowed_report_only_dirty_paths(
            repo.path(),
            &repo.path().join(".auto/health"),
            "HEALTH.md",
            ".auto/health",
        );
        let report =
            report_only_dirty_state_report(repo.path(), &baseline, &allowed).expect("report");

        assert!(report.has_violations());
        assert!(report
            .render("auto health", "`HEALTH.md` and allowed health logs")
            .contains("write boundary violation"));
    }

    #[test]
    fn health_final_status_block_names_operator_contract_fields() {
        let block = format_final_status_block(
            "health report complete",
            &["HEALTH.md".to_string()],
            "none",
            "review HEALTH.md",
        );

        assert!(block.contains("status:"));
        assert!(block.contains("files written:"));
        assert!(block.contains("blockers:"));
        assert!(block.contains("next step:"));
        assert!(block.contains("HEALTH.md"));
    }
}
