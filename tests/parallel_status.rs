use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn parallel_status_reports_health_when_run_root_has_no_lanes() {
    let repo = unique_temp_dir("parallel-status-empty-lanes-repo");
    let run_root = unique_temp_dir("parallel-status-empty-lanes-run");
    init_git_repo(&repo);

    let output = Command::new(env!("CARGO_BIN_EXE_auto"))
        .current_dir(&repo)
        .args([
            "parallel",
            "--run-root",
            run_root.to_str().expect("run root path should be utf-8"),
            "status",
        ])
        .output()
        .expect("failed to run auto parallel status");

    assert!(
        output.status.success(),
        "status command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("auto parallel status"), "{stdout}");
    assert!(stdout.contains("repo root:"), "{stdout}");
    assert!(stdout.contains("branch:"), "{stdout}");
    assert!(stdout.contains("run root:"), "{stdout}");
    assert!(stdout.contains("tmux:"), "{stdout}");
    assert!(stdout.contains("host pids:   none detected"), "{stdout}");
    assert!(stdout.contains("lanes:       none"), "{stdout}");
    assert!(stdout.contains("health:      healthy"), "{stdout}");
    assert!(
        !run_root.exists(),
        "status should inspect the requested run root without creating it"
    );

    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn parallel_status_reports_stale_lane_recovery_without_live_host() {
    let repo = unique_temp_dir("parallel-status-stale-recovery-repo");
    let run_root = unique_temp_dir("parallel-status-stale-recovery-run");
    init_git_repo(&repo);

    let lane_root = run_root.join("lanes").join("lane-2");
    let lane_repo = lane_root.join("repo");
    init_git_repo(&lane_repo);
    fs::write(lane_root.join("task-id"), "TASK-016\n").expect("failed to write lane task id");
    fs::write(lane_root.join("worker.pid"), "999999\n").expect("failed to write stale pid");
    fs::write(
        lane_root.join("stdout.log"),
        "[auto parallel host lane-2 TASK-016] working on stale release recovery\n",
    )
    .expect("failed to write lane stdout");
    fs::create_dir_all(lane_repo.join(".git").join("rebase-merge"))
        .expect("failed to create stale rebase metadata");
    fs::write(
        run_root.join("live.log"),
        "warning: failed syncing host-owned queue state\n",
    )
    .expect("failed to write live log");

    let output = Command::new(env!("CARGO_BIN_EXE_auto"))
        .current_dir(&repo)
        .args([
            "parallel",
            "--run-root",
            run_root.to_str().expect("run root path should be utf-8"),
            "status",
        ])
        .output()
        .expect("failed to run auto parallel status");

    assert!(
        output.status.success(),
        "status command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("host pids:   none detected"), "{stdout}");
    assert!(stdout.contains("lane-2: TASK-016"), "{stdout}");
    assert!(stdout.contains("stale recovery"), "{stdout}");
    assert!(stdout.contains("recovery artifact:"), "{stdout}");
    assert!(stdout.contains("reset command:"), "{stdout}");
    assert!(stdout.contains("live.log"), "{stdout}");
    assert!(stdout.contains("ago"), "{stdout}");
    assert!(
        stdout.contains("stale recovery lanes: lane-2 TASK-016"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("active recovery lanes: lane-2 TASK-016"),
        "{stdout}"
    );

    let _ = fs::remove_dir_all(&repo);
    let _ = fs::remove_dir_all(&run_root);
}

fn init_git_repo(path: &Path) {
    fs::create_dir_all(path).expect("failed to create repo dir");
    run_git(path, ["init", "-q"]);
    run_git(path, ["config", "user.email", "test@example.com"]);
    run_git(path, ["config", "user.name", "Autodev Test"]);
    fs::write(path.join("README.md"), "# fixture\n").expect("failed to write README");
    run_git(path, ["add", "README.md"]);
    run_git(path, ["commit", "-m", "init"]);
    run_git(path, ["branch", "-M", "main"]);
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_nanos();
    std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
}
