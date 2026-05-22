use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::completion_artifacts::{
    git_verification_receipt_footers, shared_footer_receipt_freshness_problem,
    shared_receipt_freshness_problem,
};
use crate::util::{atomic_write, git_stdout};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ShipGateReport {
    pub(crate) blockers: Vec<String>,
}

impl ShipGateReport {
    pub(crate) fn is_blocked(&self) -> bool {
        !self.blockers.is_empty()
    }
}

#[derive(Debug, Deserialize)]
struct VerificationReceipt {
    #[serde(default)]
    commit: Option<String>,
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

pub(crate) fn evaluate_ship_gate(
    repo_root: &Path,
    branch: &str,
    base_branch: &str,
) -> ShipGateReport {
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
        .flatten()
        .or_else(|| ship_json_receipt_commit_problem(repo_root, &receipt));
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

fn ship_json_receipt_commit_problem(
    repo_root: &Path,
    receipt: &VerificationReceipt,
) -> Option<String> {
    let current = git_stdout(repo_root, ["rev-parse", "HEAD"]).ok()?;
    let current = current.trim();
    let recorded = receipt.commit.as_deref()?;
    if recorded == current {
        return None;
    }
    Some(format!(
        "commit mismatch, recorded `{recorded}` is not current HEAD `{current}`"
    ))
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

pub(crate) fn record_ship_gate_blockers_with_verdict(
    repo_root: &Path,
    branch: &str,
    base_branch: &str,
    verdict: &str,
    report: &ShipGateReport,
) -> Result<()> {
    write_ship_gate_section(repo_root, branch, base_branch, verdict, None, report)
}

#[cfg(test)]
fn record_ship_gate_bypass(
    repo_root: &Path,
    branch: &str,
    base_branch: &str,
    reason: &str,
    report: &ShipGateReport,
) -> Result<()> {
    validate_ship_gate_bypass_reason(reason)?;
    record_ship_gate_bypass_with_verdict(
        repo_root,
        branch,
        base_branch,
        "Bypassed before model execution",
        reason,
        report,
    )
}

pub(crate) fn record_ship_gate_bypass_with_verdict(
    repo_root: &Path,
    branch: &str,
    base_branch: &str,
    verdict: &str,
    reason: &str,
    report: &ShipGateReport,
) -> Result<()> {
    validate_ship_gate_bypass_reason(reason)?;
    write_ship_gate_section(
        repo_root,
        branch,
        base_branch,
        verdict,
        Some(reason),
        report,
    )
}

pub(crate) fn validate_ship_gate_bypass_reason(reason: &str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::{evaluate_ship_gate, record_ship_gate_bypass, ShipGateReport};
    use crate::ship_command::testkit::{
        init_git_repo, test_dir, write_passing_release_receipts, write_receipt_json,
        write_receipts, write_release_reports,
    };
    use crate::util::git_stdout;

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
    fn ship_gate_detects_unresolved_release_blockers() {
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
}
