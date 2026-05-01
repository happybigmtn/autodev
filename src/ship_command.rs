use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::codex_exec::run_codex_exec;
use crate::completion_artifacts::{
    git_verification_receipt_footers, shared_footer_receipt_freshness_problem,
    shared_receipt_freshness_problem,
};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, sync_branch_with_remote, timestamp_slug,
};
use crate::ShipArgs;

const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];

pub(crate) const DEFAULT_SHIP_PROMPT_TEMPLATE: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, staging, deployment, and local-run rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, `LEARNINGS.md`, `QA.md`, `HEALTH.md`, `README.md`, `CHANGELOG.md`, and `VERSION` if they exist.
0c. Run a monolithic ship-prep pass after the mechanical release gate has passed or an operator bypass has been recorded in `SHIP.md`. You may use helper workflows or GitHub/deploy tools if they are available, but you must satisfy the shipping contract below even if those helpers are missing.

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
   - Preserve any mechanical release-gate bypass reason already recorded by `auto ship`.
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
9999991. Important: an operator bypass is not readiness; keep the bypass reason visible in `SHIP.md` until the missing evidence is replaced.
9999999. Important: prefer a blocked but honest ship report over a fake green release."#;

fn render_default_ship_prompt(branch: &str, base_branch: &str) -> String {
    DEFAULT_SHIP_PROMPT_TEMPLATE
        .replace("{branch}", branch)
        .replace("{base_branch}", base_branch)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ShipGateReport {
    blockers: Vec<String>,
}

impl ShipGateReport {
    fn is_blocked(&self) -> bool {
        !self.blockers.is_empty()
    }
}

#[derive(Debug, Deserialize)]
struct VerificationReceipt {
    #[serde(default)]
    commands: Vec<VerificationReceiptCommand>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct VerificationReceiptCommand {
    command: String,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    runner_summary: Option<RunnerSummary>,
    #[serde(default, skip)]
    freshness_problem: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RunnerSummary {
    #[serde(default)]
    tests_run: Option<u64>,
    #[serde(default)]
    zero_test_detected: Option<bool>,
}

fn evaluate_ship_gate(repo_root: &Path, branch: &str, base_branch: &str) -> ShipGateReport {
    let receipts = load_verification_receipts(repo_root);
    let mut blockers = Vec::new();

    require_receipt(
        &receipts,
        command_is_cargo_fmt,
        "missing validation receipt: `cargo fmt --check`",
        "stale validation receipt: `cargo fmt --check`",
        "red validation receipt: `cargo fmt --check`",
        &mut blockers,
    );
    require_receipt(
        &receipts,
        command_is_cargo_clippy,
        "missing validation receipt: `cargo clippy --all-targets --all-features -- -D warnings`",
        "stale validation receipt: `cargo clippy --all-targets --all-features -- -D warnings`",
        "red validation receipt: `cargo clippy --all-targets --all-features -- -D warnings`",
        &mut blockers,
    );
    require_receipt(
        &receipts,
        command_is_broad_cargo_test,
        "missing validation receipt: `cargo test`",
        "stale validation receipt: `cargo test`",
        "red validation receipt: `cargo test`",
        &mut blockers,
    );
    require_receipt(
        &receipts,
        command_is_cargo_install_auto,
        "missing installed-binary proof: no passing receipt for `cargo install --path . --root ...`",
        "stale installed-binary proof: `cargo install --path . --root ...`",
        "red installed-binary proof: `cargo install --path . --root ...`",
        &mut blockers,
    );
    require_receipt(
        &receipts,
        command_is_auto_version,
        "missing installed-binary proof: no passing receipt for PATH-resolved `auto --version`",
        "stale installed-binary proof: PATH-resolved `auto --version`",
        "red installed-binary proof: PATH-resolved `auto --version`",
        &mut blockers,
    );

    check_release_report_freshness(repo_root, "QA.md", branch, base_branch, &mut blockers);
    check_release_report_freshness(repo_root, "HEALTH.md", branch, base_branch, &mut blockers);
    check_ship_report(repo_root, &mut blockers);
    check_unresolved_release_blockers(repo_root, &mut blockers);

    ShipGateReport { blockers }
}

fn require_receipt(
    receipts: &[VerificationReceiptCommand],
    matches: fn(&str) -> bool,
    missing_message: &str,
    stale_message: &str,
    red_message: &str,
    blockers: &mut Vec<String>,
) {
    let matching = receipts
        .iter()
        .filter(|receipt| matches(&receipt.command))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        blockers.push(missing_message.to_string());
    } else if let Some(problem) = matching
        .iter()
        .find_map(|receipt| receipt.freshness_problem.as_deref())
    {
        blockers.push(format!("{stale_message}: {problem}"));
    } else if !matching.iter().any(|receipt| receipt_passed(receipt)) {
        blockers.push(red_message.to_string());
    }
}

fn receipt_passed(receipt: &VerificationReceiptCommand) -> bool {
    if receipt.freshness_problem.is_some() {
        return false;
    }
    let status_passed = match receipt.status.as_deref() {
        Some("passed") => receipt.exit_code.unwrap_or(0) == 0,
        Some(_) => false,
        None => receipt.exit_code == Some(0),
    };
    let zero_test = receipt
        .runner_summary
        .as_ref()
        .map(|summary| {
            summary.zero_test_detected == Some(true)
                || summary.tests_run == Some(0) && command_is_cargo_test_like(&receipt.command)
        })
        .unwrap_or(false);
    status_passed && !zero_test
}

fn load_verification_receipts(repo_root: &Path) -> Vec<VerificationReceiptCommand> {
    let receipt_root = repo_root.join(".auto/symphony/verification-receipts");
    let mut receipts = Vec::new();
    let mut footer_task_ids = BTreeSet::new();
    for footer in git_verification_receipt_footers(repo_root) {
        footer_task_ids.insert(footer.task_id.clone());
        let Some(receipt) = serde_json::from_str::<VerificationReceipt>(&footer.receipt_text).ok()
        else {
            continue;
        };
        let expected_commands = receipt
            .commands
            .iter()
            .map(|command| command.command.clone())
            .collect::<Vec<_>>();
        let freshness_problem =
            shared_footer_receipt_freshness_problem(repo_root, &footer, &expected_commands, &[])
                .ok()
                .flatten();
        receipts.extend(receipt.commands.into_iter().map(|mut command| {
            command.freshness_problem = freshness_problem.clone();
            command
        }));
    }

    let Ok(entries) = fs::read_dir(receipt_root) else {
        return receipts;
    };

    receipts.extend(entries.filter_map(|entry| entry.ok()).flat_map(|entry| {
        let path = entry.path();
        if path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|task_id| footer_task_ids.contains(task_id))
        {
            return Vec::new();
        }
        let Some(receipt_text) = fs::read_to_string(&path).ok() else {
            return Vec::new();
        };
        let Some(receipt) = serde_json::from_str::<VerificationReceipt>(&receipt_text).ok() else {
            return Vec::new();
        };
        let expected_commands = receipt
            .commands
            .iter()
            .map(|command| command.command.clone())
            .collect::<Vec<_>>();
        let freshness_problem = shared_receipt_freshness_problem(
            repo_root,
            &path,
            &receipt_text,
            &expected_commands,
            &[],
        )
        .ok()
        .flatten();
        receipt
            .commands
            .into_iter()
            .map(|mut command| {
                command.freshness_problem = freshness_problem.clone();
                command
            })
            .collect::<Vec<_>>()
    }));
    receipts
}

fn normalized_command(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn command_is_cargo_fmt(command: &str) -> bool {
    normalized_command(command) == "cargo fmt --check"
}

fn command_is_cargo_clippy(command: &str) -> bool {
    normalized_command(command) == "cargo clippy --all-targets --all-features -- -d warnings"
}

fn command_is_broad_cargo_test(command: &str) -> bool {
    normalized_command(command) == "cargo test"
}

fn command_is_cargo_test_like(command: &str) -> bool {
    normalized_command(command).starts_with("cargo test")
}

fn command_is_cargo_install_auto(command: &str) -> bool {
    let command = normalized_command(command);
    command.contains("cargo install") && command.contains("--path .") && command.contains("--root")
}

fn command_is_auto_version(command: &str) -> bool {
    normalized_command(command)
        .trim_start_matches("command -v auto && ")
        .ends_with("auto --version")
}

fn check_release_report_freshness(
    repo_root: &Path,
    file_name: &str,
    branch: &str,
    base_branch: &str,
    blockers: &mut Vec<String>,
) {
    let path = repo_root.join(file_name);
    let Ok(content) = fs::read_to_string(&path) else {
        blockers.push(format!("`{file_name}` is missing"));
        return;
    };
    if !content.contains(branch) || !content.contains(base_branch) {
        blockers.push(format!(
            "`{file_name}` is stale: it does not name branch `{branch}` and base branch `{base_branch}`"
        ));
    }
    if report_is_partial_for_release_diff(&content) {
        blockers.push(format!(
            "`{file_name}` is stale: it records partial coverage or untested release surfaces"
        ));
    }
    if report_predates_release_diff(repo_root, &path, base_branch) {
        blockers.push(format!(
            "`{file_name}` is stale: it predates source, test, workflow, build, or release-doc changes in the branch diff"
        ));
    }
}

fn report_is_partial_for_release_diff(content: &str) -> bool {
    let normalized = content.to_lowercase();
    (normalized.contains("partial") || normalized.contains("untested"))
        && (normalized.contains("release")
            || normalized.contains("diff")
            || normalized.contains("ship")
            || normalized.contains("surface"))
}

fn report_predates_release_diff(repo_root: &Path, report_path: &Path, base_branch: &str) -> bool {
    let Ok(report_modified) = report_path.metadata().and_then(|meta| meta.modified()) else {
        return false;
    };
    newest_release_diff_mtime(repo_root, base_branch)
        .map(|newest| report_modified < newest)
        .unwrap_or(false)
}

fn newest_release_diff_mtime(repo_root: &Path, base_branch: &str) -> Option<SystemTime> {
    let diff = git_stdout(
        repo_root,
        ["diff", "--name-only", &format!("{base_branch}...HEAD")],
    )
    .or_else(|_| {
        git_stdout(
            repo_root,
            ["diff", "--name-only", &format!("{base_branch}..HEAD")],
        )
    })
    .ok()?;
    diff.lines()
        .map(str::trim)
        .filter(|path| release_relevant_path(path))
        .filter_map(|path| repo_root.join(path).metadata().ok()?.modified().ok())
        .max()
}

fn release_relevant_path(path: &str) -> bool {
    path.starts_with("src/")
        || path.starts_with("tests/")
        || path.starts_with(".github/workflows/")
        || matches!(
            path,
            "Cargo.toml" | "Cargo.lock" | "README.md" | "CHANGELOG.md" | "VERSION" | "AGENTS.md"
        )
}

fn check_ship_report(repo_root: &Path, blockers: &mut Vec<String>) {
    let ship_path = repo_root.join("SHIP.md");
    let Ok(content) = fs::read_to_string(&ship_path) else {
        blockers.push("`SHIP.md` is missing rollback notes".to_string());
        blockers.push("`SHIP.md` is missing monitoring notes".to_string());
        blockers.push("`SHIP.md` is missing PR URL or explicit no-PR reason".to_string());
        return;
    };
    if !contains_meaningful_note(&content, "rollback") {
        blockers.push("`SHIP.md` is missing rollback notes".to_string());
    }
    if !contains_meaningful_note(&content, "monitoring") {
        blockers.push("`SHIP.md` is missing monitoring notes".to_string());
    }
    let normalized = content.to_lowercase();
    if !(normalized.contains("http://")
        || normalized.contains("https://")
        || normalized.contains("no-pr")
        || normalized.contains("no pr")
        || normalized.contains("no pull request"))
    {
        blockers.push("`SHIP.md` is missing PR URL or explicit no-PR reason".to_string());
    }
}

fn contains_meaningful_note(content: &str, keyword: &str) -> bool {
    content.lines().any(|line| {
        let normalized = line.to_lowercase();
        normalized.contains(keyword)
            && line
                .split_once(':')
                .map(|(_, value)| !value.trim().is_empty())
                .unwrap_or(true)
    })
}

fn check_unresolved_release_blockers(repo_root: &Path, blockers: &mut Vec<String>) {
    for file_name in ["REVIEW.md", "QA.md", "HEALTH.md", "WORKLIST.md", "SHIP.md"] {
        let Ok(content) = fs::read_to_string(repo_root.join(file_name)) else {
            continue;
        };
        if let Some(line) = content.lines().find(|line| line_is_release_blocker(line)) {
            blockers.push(format!(
                "unresolved release blocker in `{file_name}`: {}",
                line.trim()
            ));
        }
    }
}

fn line_is_release_blocker(line: &str) -> bool {
    let normalized = line.to_lowercase();
    normalized.contains("not ready")
        || normalized.contains("red validation")
        || normalized.contains("validation: red")
        || normalized.contains("release blocker")
        || normalized.contains("shipping blocker")
        || normalized.contains("ship blocker")
        || normalized.contains("critical blocker")
        || normalized.contains("unresolved blocker")
}

fn record_ship_gate_blockers(
    repo_root: &Path,
    branch: &str,
    base_branch: &str,
    report: &ShipGateReport,
) -> Result<()> {
    write_ship_gate_section(
        repo_root,
        branch,
        base_branch,
        "Blocked before model execution",
        None,
        report,
    )
}

fn record_ship_gate_bypass(
    repo_root: &Path,
    branch: &str,
    base_branch: &str,
    reason: &str,
    report: &ShipGateReport,
) -> Result<()> {
    validate_ship_gate_bypass_reason(reason)?;
    write_ship_gate_section(
        repo_root,
        branch,
        base_branch,
        "Bypassed before model execution",
        Some(reason),
        report,
    )
}

fn validate_ship_gate_bypass_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() {
        bail!("--bypass-release-gate requires a non-empty reason");
    }
    if reason.contains('\n') || reason.contains('\r') {
        bail!("--bypass-release-gate reason must be a single line");
    }
    Ok(())
}

fn write_ship_gate_section(
    repo_root: &Path,
    branch: &str,
    base_branch: &str,
    verdict: &str,
    bypass_reason: Option<&str>,
    report: &ShipGateReport,
) -> Result<()> {
    let ship_path = repo_root.join("SHIP.md");
    let mut content = fs::read_to_string(&ship_path).unwrap_or_else(|_| "# SHIP\n".to_string());
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str("\n## Mechanical Release Gate\n\n");
    content.push_str(&format!("- Branch: `{branch}`\n"));
    content.push_str(&format!("- Base branch: `{base_branch}`\n"));
    content.push_str(&format!("- Verdict: {verdict}\n"));
    if let Some(reason) = bypass_reason {
        content.push_str(&format!("- Operator bypass reason: {reason}\n"));
    }
    if report.blockers.is_empty() {
        content.push_str("- Blockers: none detected by the mechanical gate\n");
    } else {
        content.push_str("- Blockers:\n");
        for blocker in &report.blockers {
            content.push_str(&format!("  - {blocker}\n"));
        }
    }
    atomic_write(&ship_path, content.as_bytes())
        .with_context(|| format!("failed to write {}", ship_path.display()))?;
    Ok(())
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

    let ship_gate = evaluate_ship_gate(&repo_root, &push_branch, &base_branch);
    if let Some(reason) = args.bypass_release_gate.as_deref() {
        let reason = reason.trim();
        validate_ship_gate_bypass_reason(reason)?;
        record_ship_gate_bypass(&repo_root, &push_branch, &base_branch, reason, &ship_gate)?;
        println!("release gate: bypassed; reason recorded in SHIP.md");
    } else if ship_gate.is_blocked() {
        record_ship_gate_blockers(&repo_root, &push_branch, &base_branch, &ship_gate)?;
        bail!(
            "auto ship release gate failed before model execution:\n{}",
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

    if KNOWN_PRIMARY_BRANCHES.contains(&current_branch) {
        return Ok(current_branch.to_string());
    }

    if let Some(branch) = KNOWN_PRIMARY_BRANCHES
        .into_iter()
        .find(|candidate| git_branch_exists(repo_root, candidate))
    {
        return Ok(branch.to_string());
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
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_dir(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "auto-ship-test-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn init_git_repo(repo_root: &std::path::Path) {
        Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .arg("init")
            .output()
            .expect("git init failed");
        Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["config", "user.email", "test@example.com"])
            .output()
            .expect("git config email failed");
        Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["config", "user.name", "Test User"])
            .output()
            .expect("git config name failed");
        Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["commit", "--allow-empty", "-m", "initial"])
            .output()
            .expect("git commit failed");
    }

    fn write_release_reports(repo_root: &std::path::Path, branch: &str, base_branch: &str) {
        fs::write(
            repo_root.join("QA.md"),
            format!(
                "# QA\n\nBranch: `{branch}`\nBase branch: `{base_branch}`\nCommands: `cargo test`\n"
            ),
        )
        .expect("failed to write QA.md");
        fs::write(
            repo_root.join("HEALTH.md"),
            format!(
                "# HEALTH\n\nBranch: `{branch}`\nBase branch: `{base_branch}`\nObservations: healthy release surface.\n"
            ),
        )
        .expect("failed to write HEALTH.md");
        fs::write(
            repo_root.join("SHIP.md"),
            format!(
                "# SHIP\n\nBranch: `{branch}`\nBase branch: `{base_branch}`\nRollback: revert the release commit.\nMonitoring: run `auto health` and inspect CI.\nPR: no PR because this is a base-branch release.\n"
            ),
        )
        .expect("failed to write SHIP.md");
    }

    fn write_receipts(repo_root: &std::path::Path, commands: &[&str]) {
        let receipt_dir = repo_root.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("failed to create receipt dir");
        let commands = commands
            .iter()
            .map(|command| format!(r#"{{"command":"{command}","exit_code":0,"status":"passed"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        fs::write(
            receipt_dir.join("release.json"),
            format!(r#"{{"commands":[{commands}]}}"#),
        )
        .expect("failed to write receipt");
    }

    fn write_passing_release_receipts(repo_root: &std::path::Path) {
        write_receipts(
            repo_root,
            &[
                "cargo fmt --check",
                "cargo clippy --all-targets --all-features -- -D warnings",
                "cargo test",
                "cargo install --path . --root /tmp/autodev-install-proof",
                "auto --version",
            ],
        );
    }

    fn write_receipt_json(repo_root: &std::path::Path, json: &str) {
        let receipt_dir = repo_root.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("failed to create receipt dir");
        fs::write(receipt_dir.join("release.json"), json).expect("failed to write receipt");
    }

    #[test]
    fn ship_gate_fails_without_installed_binary_proof() {
        let repo = test_dir("missing-installed-proof");
        write_release_reports(&repo, "feature/ship", "main");
        write_receipts(
            &repo,
            &[
                "cargo fmt --check",
                "cargo clippy --all-targets --all-features -- -D warnings",
                "cargo test",
            ],
        );

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(report.is_blocked());
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("missing installed-binary proof")));
        assert!(!report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("QA.md")));
    }

    #[test]
    fn ship_gate_uses_shared_receipt_inspector() {
        let repo = test_dir("shared-receipt-gate");
        init_git_repo(&repo);
        let stale_commit = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse failed");
        let stale_commit = String::from_utf8_lossy(&stale_commit.stdout)
            .trim()
            .to_string();
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), "# changed\n")
            .expect("failed to update plan");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["add", "IMPLEMENTATION_PLAN.md"])
            .output()
            .expect("git add failed");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-m", "changed"])
            .output()
            .expect("git commit failed");
        write_release_reports(&repo, "feature/ship", "main");
        write_receipt_json(
            &repo,
            &format!(
                r#"{{"commit":"{}","commands":[{{"command":"cargo fmt --check","expected_argv":["cargo","fmt","--check"],"exit_code":0,"status":"passed"}}]}}"#,
                stale_commit
            ),
        );

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("commit mismatch")));
    }

    #[test]
    fn ship_gate_rejects_failed_status_even_with_zero_exit() {
        let repo = test_dir("failed-status-zero-exit");
        write_release_reports(&repo, "feature/ship", "main");
        write_receipt_json(
            &repo,
            r#"{"commands":[
{"command":"cargo fmt --check","exit_code":0,"status":"passed"},
{"command":"cargo clippy --all-targets --all-features -- -D warnings","exit_code":0,"status":"passed"},
{"command":"cargo test","exit_code":0,"status":"failed"},
{"command":"cargo install --path . --root /tmp/autodev-install-proof","exit_code":0,"status":"passed"},
{"command":"auto --version","exit_code":0,"status":"passed"}
]}"#,
        );

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(report.is_blocked());
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker == "red validation receipt: `cargo test`"));
    }

    #[test]
    fn ship_gate_runs_after_remote_sync_before_model() {
        let repo = test_dir("post-sync-gate");
        write_release_reports(&repo, "feature/ship", "main");
        write_passing_release_receipts(&repo);

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(
            !report.is_blocked(),
            "passing receipts and fresh reports should allow model execution: {:?}",
            report.blockers
        );
    }

    #[test]
    fn ship_gate_reruns_after_model_iteration_changes() {
        let repo = test_dir("rerun-gate");
        write_release_reports(&repo, "feature/ship", "main");
        write_passing_release_receipts(&repo);
        fs::write(
            repo.join("SHIP.md"),
            "# SHIP\n\nRelease Blockers:\n- unresolved production blocker\nRollback: revert.\nMonitoring: inspect CI.\nPR: none.\n",
        )
        .expect("failed to write SHIP");

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(report.is_blocked());
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("unresolved release blocker")));
    }

    #[test]
    fn ship_gate_reports_stale_qa_or_health() {
        let repo = test_dir("stale-qa-health");
        write_passing_release_receipts(&repo);
        fs::write(
            repo.join("QA.md"),
            "# QA\n\nBranch: `old-branch`\nBase branch: `main`\nCommands: `cargo test`\n",
        )
        .expect("failed to write QA.md");
        fs::write(
            repo.join("HEALTH.md"),
            "# HEALTH\n\nBranch: `feature/ship`\nBase branch: `main`\nPartial release surface untested.\n",
        )
        .expect("failed to write HEALTH.md");
        fs::write(
            repo.join("SHIP.md"),
            "# SHIP\n\nRollback: revert the release commit.\nMonitoring: inspect CI.\nPR: no PR because this is a base-branch release.\n",
        )
        .expect("failed to write SHIP.md");

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(report.is_blocked());
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("`QA.md` is stale")));
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("`HEALTH.md` is stale")));
    }

    #[test]
    fn ship_gate_rejects_stale_completion_receipt() {
        let repo = test_dir("stale-completion-receipt");
        init_git_repo(&repo);
        let stale_commit = git_stdout(&repo, ["rev-parse", "HEAD"])
            .expect("git rev-parse failed")
            .trim()
            .to_string();
        fs::write(repo.join("release.txt"), "new release content\n")
            .expect("failed to write release file");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["add", "release.txt"])
            .output()
            .expect("git add failed");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-m", "release change"])
            .output()
            .expect("git commit failed");
        write_release_reports(&repo, "feature/ship", "main");
        write_receipt_json(
            &repo,
            &format!(
                r#"{{"commit":"{stale_commit}","commands":[
{{"command":"cargo fmt --check","expected_argv":["cargo","fmt","--check"],"exit_code":0,"status":"passed"}},
{{"command":"cargo clippy --all-targets --all-features -- -D warnings","expected_argv":["cargo","clippy","--all-targets","--all-features","--","-D","warnings"],"exit_code":0,"status":"passed"}},
{{"command":"cargo test","expected_argv":["cargo","test"],"exit_code":0,"status":"passed"}},
{{"command":"cargo install --path . --root /tmp/autodev-install-proof","expected_argv":["cargo","install","--path",".","--root","/tmp/autodev-install-proof"],"exit_code":0,"status":"passed"}},
{{"command":"auto --version","expected_argv":["auto","--version"],"exit_code":0,"status":"passed"}}
]}}"#
            ),
        );

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(report.is_blocked());
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker.contains("stale validation receipt")));
    }

    #[test]
    fn ship_gate_bypass_records_operator_reason() {
        let repo = test_dir("bypass-record");
        let report = ShipGateReport {
            blockers: vec!["missing validation receipt: `cargo test`".to_string()],
        };

        record_ship_gate_bypass(
            &repo,
            "feature/ship",
            "main",
            "release manager accepted live CI evidence",
            &report,
        )
        .expect("failed to record bypass");

        let ship = fs::read_to_string(repo.join("SHIP.md")).expect("failed to read SHIP.md");
        assert!(ship.contains("Bypassed before model execution"));
        assert!(ship.contains("Operator bypass reason: release manager accepted live CI evidence"));
        assert!(ship.contains("missing validation receipt: `cargo test`"));
    }

    #[test]
    fn ship_gate_bypass_rejects_multiline_operator_reason() {
        let repo = test_dir("bypass-multiline");
        let report = ShipGateReport {
            blockers: vec!["missing validation receipt: `cargo test`".to_string()],
        };

        let err = record_ship_gate_bypass(
            &repo,
            "feature/ship",
            "main",
            "live CI is green\n- Blockers: none",
            &report,
        )
        .expect_err("multiline bypass reason should fail");

        assert!(err.to_string().contains("single line"));
        assert!(
            !repo.join("SHIP.md").exists(),
            "invalid bypass reason should not write SHIP.md"
        );
    }

    #[test]
    fn default_ship_prompt_includes_operational_release_controls() {
        let prompt = render_default_ship_prompt("main", "trunk");
        assert!(prompt.contains("mechanical release gate"));
        assert!(prompt.contains("bypass reason"));
        assert!(prompt.contains("rollback path"));
        assert!(prompt.contains("monitoring path"));
        assert!(prompt.contains("accessibility regressions"));
        assert!(prompt.contains("feature flags"));
        assert!(prompt.contains("branch `main`"));
        assert!(prompt.contains("base branch `trunk`"));
    }

    #[test]
    fn resolve_base_branch_prefers_current_branch_when_it_is_primary() {
        let repo = test_dir("base-branch-prefers-current");
        init_git_repo(&repo);

        // Create both main and master branches, checkout main
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "master"])
            .output()
            .expect("git branch master failed");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["checkout", "-b", "main"])
            .output()
            .expect("git checkout main failed");

        let current = git_stdout(&repo, ["branch", "--show-current"])
            .expect("git branch --show-current failed");
        assert_eq!(current.trim(), "main");

        let base = resolve_base_branch(&repo, None, "main").expect("resolve_base_branch failed");
        assert_eq!(
            base, "main",
            "expected main when currently on main, got {base}"
        );
    }

    #[test]
    fn resolve_base_branch_falls_back_to_other_primary_when_current_is_not_primary() {
        let repo = test_dir("base-branch-fallback");
        init_git_repo(&repo);

        // Create a feature branch from master, leaving master as the only primary
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["checkout", "-b", "feature"])
            .output()
            .expect("git checkout feature failed");

        let base = resolve_base_branch(&repo, None, "feature").expect("resolve_base_branch failed");
        assert_eq!(
            base, "master",
            "expected master when on feature branch, got {base}"
        );
    }
}
