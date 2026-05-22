//! Shared test fixtures for the `ship_command` submodules.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::util::git_stdout;
use crate::ShipArgs;

pub(crate) fn test_dir(label: &str) -> PathBuf {
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

pub(crate) fn init_git_repo(repo_root: &Path) {
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

pub(crate) fn init_main_git_repo(repo_root: &Path) {
    init_git_repo(repo_root);
    command_ok(repo_root, ["branch", "-M", "main"]);
}

pub(crate) fn command_ok<'a>(repo_root: &Path, args: impl IntoIterator<Item = &'a str>) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .expect("git command failed to launch");
    assert!(
        output.status.success(),
        "git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) fn commit_all(repo_root: &Path, message: &str) {
    command_ok(repo_root, ["add", "."]);
    command_ok(repo_root, ["commit", "-m", message]);
}

pub(crate) fn setup_origin(repo: &Path, label: &str) -> PathBuf {
    let origin = test_dir(label);
    command_ok(&origin, ["init", "--bare", "--initial-branch=main"]);
    command_ok(repo, ["remote", "add", "origin", origin.to_str().unwrap()]);
    command_ok(repo, ["push", "-u", "origin", "main"]);
    origin
}

pub(crate) fn write_release_reports(repo_root: &Path, branch: &str, base_branch: &str) {
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

pub(crate) fn write_receipts(repo_root: &Path, commands: &[&str]) {
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

pub(crate) fn write_passing_release_receipts(repo_root: &Path) {
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

pub(crate) fn write_passing_release_receipts_for_head(repo_root: &Path) {
    let head = git_stdout(repo_root, ["rev-parse", "HEAD"])
        .expect("git rev-parse failed")
        .trim()
        .to_string();
    write_receipt_json(repo_root, &passing_release_receipt_json(&head, "pending"));
    let dirty_fingerprint = dirty_state_fingerprint(repo_root);
    write_receipt_json(
        repo_root,
        &passing_release_receipt_json(&head, &dirty_fingerprint),
    );
}

pub(crate) fn passing_release_receipt_json(head: &str, dirty_fingerprint: &str) -> String {
    format!(
        r#"{{"commit":"{head}","dirty_state":{{"fingerprint":"{dirty_fingerprint}"}},"commands":[
{{"command":"cargo fmt --check","expected_argv":["cargo","fmt","--check"],"exit_code":0,"status":"passed"}},
{{"command":"cargo clippy --all-targets --all-features -- -D warnings","expected_argv":["cargo","clippy","--all-targets","--all-features","--","-D","warnings"],"exit_code":0,"status":"passed"}},
{{"command":"cargo test","expected_argv":["cargo","test"],"exit_code":0,"status":"passed"}},
{{"command":"cargo install --path . --root /tmp/autodev-install-proof","expected_argv":["cargo","install","--path",".","--root","/tmp/autodev-install-proof"],"exit_code":0,"status":"passed"}},
{{"command":"auto --version","expected_argv":["auto","--version"],"exit_code":0,"status":"passed"}}
]}}"#
    )
}

pub(crate) fn dirty_state_fingerprint(repo_root: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain=v1", "-z"])
        .output()
        .expect("git status failed");
    assert!(
        output.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    format!("{:x}", Sha256::digest(&output.stdout))
}

pub(crate) fn write_receipt_json(repo_root: &Path, json: &str) {
    let receipt_dir = repo_root.join(".auto/symphony/verification-receipts");
    fs::create_dir_all(&receipt_dir).expect("failed to create receipt dir");
    fs::write(receipt_dir.join("release.json"), json).expect("failed to write receipt");
}

pub(crate) fn write_fake_codex_script(path: &Path, body: &str) {
    fs::write(path, body).expect("failed to write fake codex");
    Command::new("chmod")
        .arg("+x")
        .arg(path)
        .output()
        .expect("chmod fake codex failed");
}

pub(crate) fn ship_args(repo: &Path, codex_bin: PathBuf) -> ShipArgs {
    ShipArgs {
        max_iterations: 1,
        prompt_file: None,
        model: "gpt-5.5".to_string(),
        reasoning_effort: "high".to_string(),
        branch: Some("main".to_string()),
        base_branch: Some("main".to_string()),
        run_root: Some(repo.join(".auto/ship-test")),
        bypass_release_gate: None,
        codex_bin,
    }
}
