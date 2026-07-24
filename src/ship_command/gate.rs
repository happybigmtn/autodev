use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::completion_artifacts::{
    direct_verification_receipt_freshness_problem, direct_verification_receipt_problem,
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
    commands: Vec<VerificationReceiptCommand>,
}

#[derive(Clone, Debug, Default)]
struct VerificationReceiptEvidence {
    commands: Vec<VerificationReceiptCommand>,
    content_problem: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct VerificationReceiptCommand {
    command: String,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output_summary: Option<OutputSummary>,
    #[serde(default)]
    runner_summary: Option<RunnerSummary>,
    #[serde(default, skip)]
    freshness_problem: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct OutputSummary {
    #[serde(default)]
    stdout_tail: Option<String>,
    #[serde(default)]
    stderr_tail: Option<String>,
    #[serde(default)]
    stdout_bytes: Option<u64>,
    #[serde(default)]
    stderr_bytes: Option<u64>,
    #[serde(default)]
    stdout_truncated: Option<bool>,
    #[serde(default)]
    stderr_truncated: Option<bool>,
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
    let receipt_evidence = load_verification_receipts(repo_root);
    let receipts = &receipt_evidence.commands;
    let mut blockers = Vec::new();

    require_receipt(
        receipts,
        command_is_cargo_fmt,
        "missing validation receipt: `cargo fmt --check`",
        "stale validation receipt: `cargo fmt --check`",
        "red validation receipt: `cargo fmt --check`",
        &mut blockers,
    );
    require_receipt(
        receipts,
        command_is_cargo_clippy,
        "missing validation receipt: `cargo clippy --all-targets --all-features -- -D warnings`",
        "stale validation receipt: `cargo clippy --all-targets --all-features -- -D warnings`",
        "red validation receipt: `cargo clippy --all-targets --all-features -- -D warnings`",
        &mut blockers,
    );
    require_receipt(
        receipts,
        command_is_broad_cargo_test,
        "missing validation receipt: `cargo test`",
        "stale validation receipt: `cargo test`",
        "red validation receipt: `cargo test`",
        &mut blockers,
    );
    require_installed_binary_proof(repo_root, receipts, &mut blockers);
    if let Some(problem) = receipt_evidence.content_problem {
        blockers.push(format!(
            "invalid dedicated release receipt content: {problem}"
        ));
    }

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
        Some("passed") => receipt.exit_code == Some(0),
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

fn load_verification_receipts(repo_root: &Path) -> VerificationReceiptEvidence {
    // Release authority is a distinct identity. Task receipts and historical
    // task footers prove only their own task and must never be aggregated into
    // a later release decision.
    let path = repo_root.join(".auto/symphony/verification-receipts/release.json");
    let Ok(receipt_text) = fs::read_to_string(&path) else {
        return VerificationReceiptEvidence::default();
    };
    let Ok(receipt) = serde_json::from_str::<VerificationReceipt>(&receipt_text) else {
        return VerificationReceiptEvidence::default();
    };
    let expected_commands = minimum_release_command_texts(&receipt.commands);
    let freshness_problem = match direct_verification_receipt_freshness_problem(
        repo_root,
        &path,
        &receipt_text,
        &expected_commands,
        &[],
    ) {
        Ok(problem) => problem,
        Err(err) => Some(format!("invalid dedicated release receipt: {err:#}")),
    };
    let content_problem = if freshness_problem.is_none() {
        match direct_verification_receipt_problem(
            repo_root,
            &path,
            &receipt_text,
            &expected_commands,
            &[],
        ) {
            Ok(problem) => problem,
            Err(err) => Some(format!("invalid dedicated release receipt: {err:#}")),
        }
    } else {
        None
    };
    let commands = receipt
        .commands
        .into_iter()
        .map(|mut command| {
            command.freshness_problem = freshness_problem.clone();
            command
        })
        .collect();
    VerificationReceiptEvidence {
        commands,
        content_problem,
    }
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

fn minimum_release_command_texts(receipts: &[VerificationReceiptCommand]) -> Vec<String> {
    receipts
        .iter()
        .filter(|receipt| {
            command_is_cargo_fmt(&receipt.command)
                || command_is_cargo_clippy(&receipt.command)
                || command_is_broad_cargo_test(&receipt.command)
                || locked_install_root(&receipt.argv).is_some()
                || bound_version_root(&receipt.argv).is_some()
        })
        .map(|receipt| receipt.command.clone())
        .collect()
}

fn locked_install_root(argv: &[String]) -> Option<PathBuf> {
    if argv.first().map(String::as_str) != Some("cargo")
        || argv.get(1).map(String::as_str) != Some("install")
    {
        return None;
    }

    let mut locked = false;
    let mut path = None::<&str>;
    let mut root = None::<&str>;
    let mut index = 2;
    while index < argv.len() {
        match argv[index].as_str() {
            "--locked" if !locked => {
                locked = true;
                index += 1;
            }
            "--path" if path.is_none() => {
                path = argv.get(index + 1).map(String::as_str);
                index += 2;
            }
            "--root" if root.is_none() => {
                root = argv.get(index + 1).map(String::as_str);
                index += 2;
            }
            _ => return None,
        }
    }
    let root = PathBuf::from(root?);
    if !locked || path != Some(".") || !safe_absolute_path(&root) {
        return None;
    }
    Some(root)
}

fn bound_version_root(argv: &[String]) -> Option<PathBuf> {
    let [env, path_assignment, auto, version] = argv else {
        return None;
    };
    if env != "env" || auto != "auto" || version != "--version" {
        return None;
    }
    let path_value = path_assignment.strip_prefix("PATH=")?;
    if path_value.is_empty() || path_value.contains(':') {
        return None;
    }
    let bin = PathBuf::from(path_value);
    if !safe_absolute_path(&bin) || bin.file_name()?.to_str()? != "bin" {
        return None;
    }
    let root = bin.parent()?.to_path_buf();
    safe_absolute_path(&root).then_some(root)
}

fn safe_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn require_installed_binary_proof(
    repo_root: &Path,
    receipts: &[VerificationReceiptCommand],
    blockers: &mut Vec<String>,
) {
    let installs = receipts
        .iter()
        .filter_map(|receipt| locked_install_root(&receipt.argv).map(|root| (root, receipt)))
        .collect::<Vec<_>>();
    let smokes = receipts
        .iter()
        .filter_map(|receipt| bound_version_root(&receipt.argv).map(|root| (root, receipt)))
        .collect::<Vec<_>>();

    if installs.is_empty() {
        blockers.push(
            "missing installed-binary proof: actual argv must be `cargo install --path . --locked --root ABS`"
                .to_string(),
        );
    }
    if smokes.is_empty() {
        blockers.push(
            "missing installed-binary proof: version smoke actual argv must bind `auto --version` to `PATH=<install-root>/bin` with no fallback PATH"
                .to_string(),
        );
    }
    if installs.is_empty() || smokes.is_empty() {
        return;
    }

    let mut saw_linked_pair = false;
    let mut saw_stale = None::<String>;
    let mut saw_red = false;
    let mut provenance_problems = Vec::new();
    for (install_root, install) in &installs {
        for (smoke_root, smoke) in &smokes {
            if install_root != smoke_root {
                continue;
            }
            saw_linked_pair = true;
            if let Some(problem) = install
                .freshness_problem
                .as_deref()
                .or(smoke.freshness_problem.as_deref())
            {
                saw_stale.get_or_insert_with(|| problem.to_string());
                continue;
            }
            if !receipt_passed(install) || !receipt_passed(smoke) {
                saw_red = true;
                continue;
            }
            if let Some(problem) = installed_binary_provenance_problem(repo_root, smoke) {
                provenance_problems.push(problem);
                continue;
            }
            return;
        }
    }

    if !saw_linked_pair {
        blockers.push(
            "invalid installed-binary proof: version smoke install root does not match the locked cargo install root"
                .to_string(),
        );
    } else if let Some(problem) = saw_stale {
        blockers.push(format!("stale installed-binary proof: {problem}"));
    } else if saw_red {
        blockers
            .push("red installed-binary proof: linked install/version command failed".to_string());
    } else {
        blockers.push(format!(
            "invalid installed-binary provenance: {}",
            provenance_problems
                .first()
                .map(String::as_str)
                .unwrap_or("no linked passing version smoke proved the release binary")
        ));
    }
}

fn installed_binary_provenance_problem(
    repo_root: &Path,
    smoke: &VerificationReceiptCommand,
) -> Option<String> {
    let Some(output) = smoke.output_summary.as_ref() else {
        return Some("missing output_summary".to_string());
    };
    let Some(stdout) = output.stdout_tail.as_deref() else {
        return Some("output_summary is missing stdout_tail".to_string());
    };
    let Some(stderr) = output.stderr_tail.as_deref() else {
        return Some("output_summary is missing stderr_tail".to_string());
    };
    if output.stdout_truncated != Some(false) || output.stderr_truncated != Some(false) {
        return Some("output_summary is missing truncation state or is truncated".to_string());
    }
    if output.stdout_bytes != Some(stdout.len() as u64)
        || output.stderr_bytes != Some(stderr.len() as u64)
    {
        return Some("output_summary byte counts do not match the untruncated tails".to_string());
    }
    let Some(short_head) = git_stdout(repo_root, ["rev-parse", "--short", "HEAD"])
        .ok()
        .map(|head| head.trim().to_string())
        .filter(|head| !head.is_empty())
    else {
        return Some("current short HEAD is unavailable".to_string());
    };
    let expected = format!(
        "auto {}\ncommit: {short_head}\ndirty: clean\nprofile: release",
        env!("CARGO_PKG_VERSION")
    );
    let actual = stdout.trim_end_matches(['\r', '\n']);
    (actual != expected).then_some(format!(
        "version output does not match `auto {}`, current short HEAD, `dirty: clean`, and `profile: release`",
        env!("CARGO_PKG_VERSION")
    ))
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
        dirty_state_fingerprint, init_git_repo, passing_release_receipt_json, plan_hash, test_dir,
        write_passing_release_receipts, write_receipt_json, write_receipts, write_release_reports,
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
    fn ship_gate_rejects_unlocked_unbound_installed_binary_proof() {
        let repo = test_dir("unbound-installed-proof");
        init_git_repo(&repo);
        write_release_reports(&repo, "feature/ship", "main");
        let head = git_stdout(&repo, ["rev-parse", "HEAD"])
            .expect("git rev-parse failed")
            .trim()
            .to_string();
        let short_head = git_stdout(&repo, ["rev-parse", "--short", "HEAD"])
            .expect("git short rev-parse failed")
            .trim()
            .to_string();
        let plan_hash = plan_hash(&repo);
        let unlocked_unbound_receipt = |dirty_fingerprint: &str| {
            let mut receipt: serde_json::Value = serde_json::from_str(
                &passing_release_receipt_json(&head, &short_head, dirty_fingerprint, &plan_hash),
            )
            .expect("passing fixture should deserialize");
            for command in receipt["commands"]
                .as_array_mut()
                .expect("commands should be an array")
            {
                let argv = command["argv"]
                    .as_array_mut()
                    .expect("argv should be an array");
                if argv.first().and_then(serde_json::Value::as_str) == Some("cargo")
                    && argv.get(1).and_then(serde_json::Value::as_str) == Some("install")
                {
                    argv.retain(|arg| arg.as_str() != Some("--locked"));
                    command["expected_argv"] = serde_json::Value::Array(argv.clone());
                    command["command"] = serde_json::Value::String(
                        "cargo install --path . --root /tmp/autodev-install-proof".to_string(),
                    );
                } else if argv.first().and_then(serde_json::Value::as_str) == Some("env") {
                    *argv = vec![
                        serde_json::Value::String("auto".to_string()),
                        serde_json::Value::String("--version".to_string()),
                    ];
                    command["expected_argv"] = serde_json::Value::Array(argv.clone());
                    command["command"] = serde_json::Value::String("auto --version".to_string());
                }
            }
            serde_json::to_string(&receipt).expect("mutated receipt should serialize")
        };
        write_receipt_json(&repo, &unlocked_unbound_receipt("pending"));
        let fingerprint = dirty_state_fingerprint(&repo);
        write_receipt_json(&repo, &unlocked_unbound_receipt(&fingerprint));

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(
            report
                .blockers
                .iter()
                .any(|blocker| blocker.contains("installed-binary proof")
                    && (blocker.contains("--locked")
                        || blocker.contains("install root")
                        || blocker.contains("provenance"))),
            "an unlocked install followed by an unbound PATH smoke must not prove the installed release binary: {:?}",
            report.blockers
        );
    }

    #[test]
    fn ship_gate_rejects_missing_truncated_or_mismatched_installed_provenance() {
        for case in ["missing", "truncated", "mismatched"] {
            let repo = test_dir(&format!("installed-provenance-{case}"));
            init_git_repo(&repo);
            write_release_reports(&repo, "feature/ship", "main");
            let head = git_stdout(&repo, ["rev-parse", "HEAD"])
                .expect("git rev-parse failed")
                .trim()
                .to_string();
            let short_head = git_stdout(&repo, ["rev-parse", "--short", "HEAD"])
                .expect("git short rev-parse failed")
                .trim()
                .to_string();
            let plan_hash = plan_hash(&repo);
            let mut receipt: serde_json::Value = serde_json::from_str(
                &passing_release_receipt_json(&head, &short_head, "pending", &plan_hash),
            )
            .expect("passing fixture should deserialize");
            let smoke = receipt["commands"]
                .as_array_mut()
                .expect("commands should be an array")
                .iter_mut()
                .find(|command| {
                    command["argv"]
                        .as_array()
                        .and_then(|argv| argv.first())
                        .and_then(serde_json::Value::as_str)
                        == Some("env")
                })
                .expect("passing fixture should include version smoke");
            match case {
                "missing" => {
                    smoke
                        .as_object_mut()
                        .expect("smoke should be an object")
                        .remove("output_summary");
                }
                "truncated" => {
                    smoke["output_summary"]["stdout_truncated"] = serde_json::Value::Bool(true);
                }
                "mismatched" => {
                    smoke["output_summary"]["stdout_tail"] = serde_json::Value::String(format!(
                        "auto {}\ncommit: deadbee\ndirty: clean\nprofile: release\n",
                        env!("CARGO_PKG_VERSION")
                    ));
                }
                _ => unreachable!(),
            }
            write_receipt_json(
                &repo,
                &serde_json::to_string(&receipt).expect("serialize provisional receipt"),
            );
            let fingerprint = dirty_state_fingerprint(&repo);
            receipt["dirty_state"]["fingerprint"] = serde_json::Value::String(fingerprint);
            write_receipt_json(
                &repo,
                &serde_json::to_string(&receipt).expect("serialize fresh receipt"),
            );

            let report = evaluate_ship_gate(&repo, "feature/ship", "main");

            assert!(
                report.blockers.iter().any(|blocker| {
                    blocker.contains("installed-binary provenance")
                        && (blocker.contains("output_summary")
                            || blocker.contains("version output"))
                }),
                "{case} provenance must block release: {:?}",
                report.blockers
            );
        }
    }

    #[test]
    fn ship_gate_blocks_extra_unsuperseded_failed_test_receipt() {
        let repo = test_dir("extra-failed-hard-gate");
        init_git_repo(&repo);
        write_release_reports(&repo, "feature/ship", "main");
        let head = git_stdout(&repo, ["rev-parse", "HEAD"])
            .expect("git rev-parse failed")
            .trim()
            .to_string();
        let short_head = git_stdout(&repo, ["rev-parse", "--short", "HEAD"])
            .expect("git short rev-parse failed")
            .trim()
            .to_string();
        let plan_hash = plan_hash(&repo);
        let mut receipt: serde_json::Value = serde_json::from_str(&passing_release_receipt_json(
            &head,
            &short_head,
            "pending",
            &plan_hash,
        ))
        .expect("passing fixture should deserialize");
        receipt["commands"]
            .as_array_mut()
            .expect("commands should be an array")
            .push(serde_json::json!({
                "command": "cargo test hidden_release_regression",
                "argv": ["cargo", "test", "hidden_release_regression"],
                "expected_argv": ["cargo", "test", "hidden_release_regression"],
                "exit_code": 101,
                "status": "failed",
            }));
        write_receipt_json(
            &repo,
            &serde_json::to_string(&receipt).expect("serialize provisional receipt"),
        );
        let fingerprint = dirty_state_fingerprint(&repo);
        receipt["dirty_state"]["fingerprint"] = serde_json::Value::String(fingerprint);
        write_receipt_json(
            &repo,
            &serde_json::to_string(&receipt).expect("serialize fresh receipt"),
        );

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(
            report.blockers.iter().any(|blocker| {
                blocker.contains("invalid dedicated release receipt content")
                    && blocker.contains("unsuperseded failed command")
                    && blocker.contains("hidden_release_regression")
            }),
            "an extra failed cargo test must globally block release: {:?}",
            report.blockers
        );
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
                r#"{{"task_id":"release","commit":"{}","commands":[{{"command":"cargo fmt --check","argv":["cargo","fmt","--check"],"expected_argv":["cargo","fmt","--check"],"exit_code":0,"status":"passed"}}]}}"#,
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
        init_git_repo(&repo);
        write_release_reports(&repo, "feature/ship", "main");
        let head = git_stdout(&repo, ["rev-parse", "HEAD"])
            .expect("git rev-parse failed")
            .trim()
            .to_string();
        let short_head = git_stdout(&repo, ["rev-parse", "--short", "HEAD"])
            .expect("git short rev-parse failed")
            .trim()
            .to_string();
        let plan_hash = plan_hash(&repo);
        let mut receipt: serde_json::Value = serde_json::from_str(&passing_release_receipt_json(
            &head,
            &short_head,
            "pending",
            &plan_hash,
        ))
        .expect("passing fixture should deserialize");
        let cargo_test = receipt["commands"]
            .as_array_mut()
            .expect("commands should be an array")
            .iter_mut()
            .find(|command| command["command"].as_str() == Some("cargo test"))
            .expect("passing fixture should include cargo test");
        cargo_test["status"] = serde_json::Value::String("failed".to_string());
        write_receipt_json(
            &repo,
            &serde_json::to_string(&receipt).expect("serialize provisional receipt"),
        );
        let fingerprint = dirty_state_fingerprint(&repo);
        receipt["dirty_state"]["fingerprint"] = serde_json::Value::String(fingerprint);
        write_receipt_json(
            &repo,
            &serde_json::to_string(&receipt).expect("serialize fresh receipt"),
        );

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(report.is_blocked());
        assert!(report
            .blockers
            .iter()
            .any(|blocker| blocker == "red validation receipt: `cargo test`"));
    }

    #[test]
    fn ship_gate_ignores_fresh_non_release_receipt_identity() {
        let repo = test_dir("non-release-receipt");
        init_git_repo(&repo);
        write_release_reports(&repo, "feature/ship", "main");
        let head = git_stdout(&repo, ["rev-parse", "HEAD"])
            .expect("git rev-parse failed")
            .trim()
            .to_string();
        let short_head = git_stdout(&repo, ["rev-parse", "--short", "HEAD"])
            .expect("git short rev-parse failed")
            .trim()
            .to_string();
        let plan_hash = plan_hash(&repo);
        let receipt_dir = repo.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("create receipt dir");
        let other_path = receipt_dir.join("TASK-OLD.json");
        fs::write(
            &other_path,
            passing_release_receipt_json(&head, &short_head, "pending", &plan_hash)
                .replace("\"task_id\":\"release\"", "\"task_id\":\"TASK-OLD\""),
        )
        .expect("write provisional task receipt");
        let fingerprint = dirty_state_fingerprint(&repo);
        fs::write(
            &other_path,
            passing_release_receipt_json(&head, &short_head, &fingerprint, &plan_hash)
                .replace("\"task_id\":\"release\"", "\"task_id\":\"TASK-OLD\""),
        )
        .expect("write fresh task receipt");

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(
            report
                .blockers
                .iter()
                .any(|blocker| blocker == "missing validation receipt: `cargo fmt --check`"),
            "a task receipt must not satisfy release authority: {:?}",
            report.blockers
        );
        assert!(
            report
                .blockers
                .iter()
                .any(|blocker| blocker.contains("missing installed-binary proof")),
            "task installation evidence must not satisfy release authority: {:?}",
            report.blockers
        );
    }

    #[test]
    fn ship_gate_fails_closed_when_shared_release_schema_rejects_receipt() {
        let repo = test_dir("invalid-shared-release-schema");
        write_release_reports(&repo, "feature/ship", "main");
        write_receipt_json(
            &repo,
            r#"{"task_id":7,"commands":[
{"command":"cargo fmt --check","argv":["cargo","fmt","--check"],"expected_argv":["cargo","fmt","--check"],"exit_code":0,"status":"passed"},
{"command":"cargo clippy --all-targets --all-features -- -D warnings","argv":["cargo","clippy","--all-targets","--all-features","--","-D","warnings"],"expected_argv":["cargo","clippy","--all-targets","--all-features","--","-D","warnings"],"exit_code":0,"status":"passed"},
{"command":"cargo test","argv":["cargo","test"],"expected_argv":["cargo","test"],"exit_code":0,"status":"passed"},
{"command":"cargo install --path . --root /tmp/autodev-install-proof","argv":["cargo","install","--path",".","--root","/tmp/autodev-install-proof"],"expected_argv":["cargo","install","--path",".","--root","/tmp/autodev-install-proof"],"exit_code":0,"status":"passed"},
{"command":"auto --version","argv":["auto","--version"],"expected_argv":["auto","--version"],"exit_code":0,"status":"passed"}
]}"#,
        );

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(
            report
                .blockers
                .iter()
                .any(|blocker| blocker.contains("invalid dedicated release receipt")),
            "schema errors must block release: {:?}",
            report.blockers
        );
    }

    #[test]
    fn ship_gate_rejects_echoed_install_and_version_commands() {
        let repo = test_dir("echoed-install-version");
        write_release_reports(&repo, "feature/ship", "main");
        write_receipt_json(
            &repo,
            r#"{"task_id":"release","commands":[
{"command":"cargo fmt --check","argv":["cargo","fmt","--check"],"expected_argv":["cargo","fmt","--check"],"exit_code":0,"status":"passed"},
{"command":"cargo clippy --all-targets --all-features -- -D warnings","argv":["cargo","clippy","--all-targets","--all-features","--","-D","warnings"],"expected_argv":["cargo","clippy","--all-targets","--all-features","--","-D","warnings"],"exit_code":0,"status":"passed"},
{"command":"cargo test","argv":["cargo","test"],"expected_argv":["cargo","test"],"exit_code":0,"status":"passed"},
{"command":"echo cargo install --path . --root /tmp/autodev-install-proof","argv":["echo","cargo","install","--path",".","--root","/tmp/autodev-install-proof"],"expected_argv":["echo","cargo","install","--path",".","--root","/tmp/autodev-install-proof"],"exit_code":0,"status":"passed"},
{"command":"echo auto --version","argv":["echo","auto","--version"],"expected_argv":["echo","auto","--version"],"exit_code":0,"status":"passed"}
]}"#,
        );

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(
            report
                .blockers
                .iter()
                .filter(|blocker| blocker.contains("missing installed-binary proof"))
                .count()
                >= 2,
            "echoed command text must not count as execution proof: {:?}",
            report.blockers
        );
    }

    #[test]
    fn ship_gate_rejects_passed_status_without_zero_exit_code() {
        let repo = test_dir("missing-release-exit-code");
        write_release_reports(&repo, "feature/ship", "main");
        write_receipt_json(
            &repo,
            r#"{"task_id":"release","commands":[
{"command":"cargo fmt --check","argv":["cargo","fmt","--check"],"expected_argv":["cargo","fmt","--check"],"status":"passed"},
{"command":"cargo clippy --all-targets --all-features -- -D warnings","argv":["cargo","clippy","--all-targets","--all-features","--","-D","warnings"],"expected_argv":["cargo","clippy","--all-targets","--all-features","--","-D","warnings"],"exit_code":0,"status":"passed"},
{"command":"cargo test","argv":["cargo","test"],"expected_argv":["cargo","test"],"exit_code":0,"status":"passed"},
{"command":"cargo install --path . --root /tmp/autodev-install-proof","argv":["cargo","install","--path",".","--root","/tmp/autodev-install-proof"],"expected_argv":["cargo","install","--path",".","--root","/tmp/autodev-install-proof"],"exit_code":0,"status":"passed"},
{"command":"auto --version","argv":["auto","--version"],"expected_argv":["auto","--version"],"exit_code":0,"status":"passed"}
]}"#,
        );

        let report = evaluate_ship_gate(&repo, "feature/ship", "main");

        assert!(
            report.blockers.iter().any(|blocker| {
                blocker.contains("stale validation receipt")
                    || blocker == "red validation receipt: `cargo fmt --check`"
            }),
            "missing exit status must block release proof: {:?}",
            report.blockers
        );
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
