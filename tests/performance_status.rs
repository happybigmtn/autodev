use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn large_plan_status_renders_under_measured_observation() {
    let repo = unique_temp_dir("large-plan-status");
    let run_root = unique_temp_dir("large-plan-status-run");
    init_git_repo(&repo);
    fs::write(repo.join("IMPLEMENTATION_PLAN.md"), large_plan(180))
        .expect("failed to write large plan");

    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_auto"))
        .current_dir(&repo)
        .args([
            "parallel",
            "--run-root",
            run_root.to_str().expect("run root should be utf-8"),
            "status",
        ])
        .output()
        .expect("failed to run status");
    let elapsed = started.elapsed();

    assert!(
        output.status.success(),
        "status command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "performance observation: large plan status took {:?}",
        elapsed
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("safety verdict:"), "{stdout}");
    fs::remove_dir_all(repo).ok();
    fs::remove_dir_all(run_root).ok();
}

#[test]
fn large_audit_status_renders_under_measured_observation() {
    let repo = unique_temp_dir("large-audit-status");
    let run_root = repo.join(".auto/audit-everything/run");
    init_git_repo(&repo);
    fs::create_dir_all(&run_root).expect("failed to create audit run");
    fs::write(repo.join(".auto/audit-everything/latest-run"), "run\n")
        .expect("failed to write latest run");
    fs::write(
        run_root.join("RUN-STATUS.md"),
        "# RUN STATUS\n\n- tasks: 400\n",
    )
    .expect("failed to write audit status");

    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_auto"))
        .current_dir(&repo)
        .args(["audit", "--everything", "--everything-phase", "status"])
        .output()
        .expect("failed to run audit status");
    let elapsed = started.elapsed();

    assert!(
        output.status.success(),
        "audit status command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "performance observation: large audit status took {:?}",
        elapsed
    );
    fs::remove_dir_all(repo).ok();
}

fn large_plan(count: usize) -> String {
    let mut plan = "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n".to_string();
    for index in 0..count {
        plan.push_str(&format!(
            "- [ ] `TASK-{index:03}` Measured status task {index}\n  Spec: `specs/status.md`\n  Why now: performance fixture.\n  Codebase evidence: `src/parallel_command.rs`\n  Source of truth: `src/parallel_command.rs`\n  Runtime owner: `src/parallel_command.rs`\n  UI consumers: none\n  Generated artifacts: none\n  Fixture boundary: test fixture only.\n  Retired surfaces: none\n  Owns: `src/parallel_command.rs`\n  Integration touchpoints: `src/parallel_command.rs`\n  Scope boundary: status rendering only.\n  Acceptance criteria: status renders.\n  Verification: `cargo test --test performance_status large_plan_status_renders_under_measured_observation`\n  Required tests: `cargo test --test performance_status large_plan_status_renders_under_measured_observation`\n  Contract generation: none\n  Cross-surface tests: none\n  Review/closeout: performance observation recorded.\n  Completion artifacts: `tests/performance_status.rs`\n  Dependencies: none\n  Estimated scope: XS\n  Completion signal: test passes.\n\n"
        ));
    }
    plan.push_str("## Follow-On Work\n\n## Completed / Already Satisfied\n");
    plan
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
