//! Host-side re-execution verify gate for `auto parallel`.
//!
//! Closes the central trust hole in the completion model. The host otherwise
//! TRUSTS the worker's recorded exit codes in the verification receipt and never
//! re-runs anything itself — so a receipt that passed in the worker's lane (or a
//! worker that recorded a green it didn't actually earn) lands `[x]` unchecked.
//! Before the host marks a landed lane Done, this gate re-executes the task's
//! own declared verification commands at canonical HEAD — the *integrated*
//! state, after the cherry-pick — so "done" means "the host produced its own
//! fresh green", not "the worker said it was green". This also catches
//! integration failures: a task that passes in isolation but breaks once
//! concurrent lanes' work is combined.
//!
//! Status-downgrade gate (mirrors [`super::review_gate`]), never a fix-loop:
//! - All host-reproducible commands PASS  -> land exactly as incoming (`[x]`).
//! - A command RUNS and FAILS (clean non-zero) -> FAIL-CLOSED: the committed
//!   work still lands, but the task is held at `[~]` and the failing command +
//!   an output tail are appended to `REVIEW.md` so the next pass fixes it.
//! - No host-reproducible commands, gate disabled, spawn error, or timeout
//!   -> FAIL-CLOSED for finalization: landed work stays, but `[x]` is withheld.
//!
//! Bounded by a hard total timeout: a bug or slow verifier can never block,
//! hang, or lose a worker's committed work, but it also cannot produce `[x]`.
//!
//! Toggle:  `AUTO_PARALLEL_VERIFY_LANDINGS`      (default "1" = ON; "0" = skip)
//! Bound:   `AUTO_PARALLEL_VERIFY_TIMEOUT_SECS`  (default 1800; total across all commands)

use super::*;

// `verification_plan` and `Duration` arrive via `super::*`; only the external-
// step classifier needs an explicit import.
use crate::completion_artifacts::verification_step_looks_external;
use shlex::split as shell_split;

/// Env toggle. `"0"` skips the gate entirely (legacy behavior: trust the receipt).
const VERIFY_ENABLED_ENV: &str = "AUTO_PARALLEL_VERIFY_LANDINGS";
/// Env bound. Hard-cap the total re-execution time across all of a task's
/// commands, in seconds.
const VERIFY_TIMEOUT_ENV: &str = "AUTO_PARALLEL_VERIFY_TIMEOUT_SECS";
const DEFAULT_VERIFY_TIMEOUT_SECS: u64 = 1800;
/// Number of trailing non-blank output lines captured into the failure detail.
const FAILURE_TAIL_LINES: usize = 25;

/// Outcome of re-running a landed lane's declared verification at canonical HEAD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LaneVerifyOutcome {
    /// Every host-reproducible verification command re-passed. Land `[x]`.
    AllPassed,
    /// A command ran and returned non-zero. Hold the task at `[~]`; `detail`
    /// names the command and carries a short tail of its output for `REVIEW.md`.
    Failed { detail: String },
    /// Nothing host-reproducible to run, gate disabled, or an infra error.
    /// This is not a pass; finalization must keep the task `[~]`.
    Skipped { reason: String },
}

/// True unless explicitly disabled with `AUTO_PARALLEL_VERIFY_LANDINGS=0`
/// (trimmed). The safe default is to re-verify.
pub(crate) fn verify_gate_enabled() -> bool {
    match std::env::var(VERIFY_ENABLED_ENV) {
        Ok(value) => value.trim() != "0",
        Err(_) => true,
    }
}

/// Resolve the hard total timeout, honoring the env override. Invalid or zero
/// values fall back to the default rather than disabling the bound.
fn verify_timeout() -> Duration {
    let secs = std::env::var(VERIFY_TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_VERIFY_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// The task's exact executable verification commands. External/live steps are
/// rejected by [`run_lane_verify_gate`] rather than silently filtered, because
/// a task can only be `[x]` after its canonical verification story is complete.
pub(crate) fn host_reproducible_commands(task_markdown: &str) -> Vec<String> {
    verification_plan(task_markdown)
        .executable_commands
        .into_iter()
        .collect()
}

/// Re-run the task's declared verification commands at canonical HEAD, bounded
/// by a total timeout. Never returns `Err`; classifies every path into a
/// [`LaneVerifyOutcome`].
pub(crate) async fn run_lane_verify_gate(
    repo_root: &Path,
    task_id: &str,
    task_markdown: &str,
) -> LaneVerifyOutcome {
    let verification = verification_plan(task_markdown);
    if verification
        .steps
        .iter()
        .any(|step| verification_step_looks_external(step))
    {
        return LaneVerifyOutcome::Skipped {
            reason: "verification includes external/live step(s); host cannot mark [x] until they are cleared explicitly".to_string(),
        };
    }
    let commands = host_reproducible_commands(task_markdown);
    if commands.is_empty() {
        return LaneVerifyOutcome::Skipped {
            reason: "no host-reproducible verification commands to re-run".to_string(),
        };
    }
    if !repo_root.join("scripts/run-task-verification.sh").is_file() {
        return LaneVerifyOutcome::Failed {
            detail: "missing `scripts/run-task-verification.sh`; cannot produce current-tree verification receipt".to_string(),
        };
    }
    let deadline = verify_timeout();
    match tokio::time::timeout(
        deadline,
        run_verify_commands(repo_root.to_path_buf(), task_id.to_string(), commands),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => LaneVerifyOutcome::Skipped {
            reason: format!("host re-execution timed out after {}s", deadline.as_secs()),
        },
    }
}

/// Run each command through the repo's receipt wrapper. `kill_on_drop` ensures
/// a timeout terminates an in-flight build rather than orphaning it.
async fn run_verify_commands(
    repo_root: PathBuf,
    task_id: String,
    commands: Vec<String>,
) -> LaneVerifyOutcome {
    for command in &commands {
        let Some(argv) = shell_split(command) else {
            return LaneVerifyOutcome::Failed {
                detail: format!("could not parse verification command `{command}` as shell argv"),
            };
        };
        let result = tokio::process::Command::new("scripts/run-task-verification.sh")
            .arg(&task_id)
            .arg("--")
            .args(&argv)
            .current_dir(&repo_root)
            .kill_on_drop(true)
            .output()
            .await;
        match result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let status = output
                    .status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string());
                return LaneVerifyOutcome::Failed {
                    detail: format!(
                        "`{command}` exited with status {status}\n{}",
                        output_tail(&output.stdout, &output.stderr)
                    ),
                };
            }
            Err(err) => {
                return LaneVerifyOutcome::Failed {
                    detail: format!("could not run verification wrapper for `{command}`: {err}"),
                };
            }
        }
    }
    LaneVerifyOutcome::AllPassed
}

/// Last [`FAILURE_TAIL_LINES`] non-blank lines of stdout+stderr, for the
/// `REVIEW.md` failure note.
fn output_tail(stdout: &[u8], stderr: &[u8]) -> String {
    let merged = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let lines: Vec<&str> = merged
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(FAILURE_TAIL_LINES);
    lines[start..].join("\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceTestOutcome {
    Passed,
    Failed { detail: String },
    Skipped { reason: String },
}

const WORKSPACE_TEST_TIMEOUT_ENV: &str = "AUTO_PARALLEL_WORKSPACE_TEST_TIMEOUT_SECS";
const DEFAULT_WORKSPACE_TEST_TIMEOUT_SECS: u64 = 1800;

fn workspace_test_timeout() -> Duration {
    let secs = std::env::var(WORKSPACE_TEST_TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_WORKSPACE_TEST_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

pub(crate) async fn run_workspace_test_gate(repo_root: &Path) -> WorkspaceTestOutcome {
    if !repo_root.join("Cargo.toml").is_file() {
        return WorkspaceTestOutcome::Skipped {
            reason: "no Cargo.toml found; workspace cargo test gate is not applicable".to_string(),
        };
    }
    let deadline = workspace_test_timeout();
    match tokio::time::timeout(deadline, run_workspace_test(repo_root.to_path_buf())).await {
        Ok(outcome) => outcome,
        Err(_) => WorkspaceTestOutcome::Skipped {
            reason: format!(
                "`cargo test --workspace` timed out after {}s",
                deadline.as_secs()
            ),
        },
    }
}

async fn run_workspace_test(repo_root: PathBuf) -> WorkspaceTestOutcome {
    let result = tokio::process::Command::new("cargo")
        .args(["test", "--workspace"])
        .current_dir(&repo_root)
        .kill_on_drop(true)
        .output()
        .await;
    match result {
        Ok(output) if output.status.success() => WorkspaceTestOutcome::Passed,
        Ok(output) => {
            let status = output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string());
            WorkspaceTestOutcome::Failed {
                detail: format!(
                    "`cargo test --workspace` exited with status {status}\n{}",
                    output_tail(&output.stdout, &output.stderr)
                ),
            }
        }
        Err(err) => WorkspaceTestOutcome::Failed {
            detail: format!("could not run `cargo test --workspace`: {err}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn install_test_wrapper(dir: &Path) {
        let scripts = dir.join("scripts");
        fs::create_dir_all(&scripts).expect("create scripts dir");
        let wrapper = scripts.join("run-task-verification.sh");
        fs::write(
            &wrapper,
            "#!/usr/bin/env bash\nset -euo pipefail\ntask_id=$1\nshift\nif [[ ${1:-} == \"--\" ]]; then shift; fi\n\"$@\"\n",
        )
        .expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper)
            .expect("wrapper metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
    }

    #[test]
    fn verify_gate_disabled_only_on_explicit_zero() {
        let prev = std::env::var(VERIFY_ENABLED_ENV).ok();
        std::env::set_var(VERIFY_ENABLED_ENV, "0");
        assert!(!verify_gate_enabled());
        std::env::set_var(VERIFY_ENABLED_ENV, " 0 ");
        assert!(!verify_gate_enabled(), "trimmed 0 still disables");
        std::env::set_var(VERIFY_ENABLED_ENV, "1");
        assert!(verify_gate_enabled());
        std::env::remove_var(VERIFY_ENABLED_ENV);
        assert!(verify_gate_enabled(), "default-on when unset");
        if let Some(value) = prev {
            std::env::set_var(VERIFY_ENABLED_ENV, value);
        }
    }

    #[test]
    fn host_reproducible_commands_preserves_exact_commands() {
        let markdown = "- [ ] `TASK-1` thing\n\nVerification:\n- Run `cargo test foo`\n- Check `https://example.com/health` is 200\n- Run `ssh host uptime`\n";
        let commands = host_reproducible_commands(markdown);
        assert!(
            commands.iter().any(|c| c.contains("cargo test foo")),
            "cargo command kept, got {commands:?}"
        );
        assert!(
            commands.iter().any(|c| c.starts_with("ssh")),
            "external executable command is not silently dropped, got {commands:?}"
        );
    }

    #[tokio::test]
    async fn run_lane_verify_gate_passes_when_command_succeeds() {
        let dir = std::env::temp_dir().join(format!("autodev-verify-pass-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        install_test_wrapper(&dir);
        let markdown = "- [ ] `TASK-1` t\n\nVerification:\n- Run `bash -c true`\n";
        assert_eq!(
            run_lane_verify_gate(&dir, "TASK-1", markdown).await,
            LaneVerifyOutcome::AllPassed
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_lane_verify_gate_fails_closed_on_nonzero_command() {
        let dir = std::env::temp_dir().join(format!("autodev-verify-fail-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        install_test_wrapper(&dir);
        let markdown = "- [ ] `TASK-1` t\n\nVerification:\n- Run `bash -c false`\n";
        match run_lane_verify_gate(&dir, "TASK-1", markdown).await {
            LaneVerifyOutcome::Failed { detail } => {
                assert!(detail.contains("false"), "names the command: {detail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_lane_verify_gate_fails_closed_on_missing_toolchain() {
        let dir = std::env::temp_dir().join(format!("autodev-verify-127-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        install_test_wrapper(&dir);
        // A recognized command (`./`-prefixed) whose binary does not exist exits
        // 127. That is not a passing canonical verification, so it must fail
        // closed and keep the task [~].
        let markdown =
            "- [ ] `TASK-1` t\n\nVerification:\n- Run `./nonexistent-binary-xyz check`\n";
        match run_lane_verify_gate(&dir, "TASK-1", markdown).await {
            LaneVerifyOutcome::Failed { detail } => {
                assert!(detail.contains("nonexistent-binary-xyz"), "{detail}");
            }
            other => panic!("missing toolchain must fail closed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_lane_verify_gate_skips_when_no_commands() {
        let dir = std::env::temp_dir().join(format!("autodev-verify-skip-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let markdown =
            "- [ ] `TASK-1` t\n\nVerification:\n- Manually confirm the layout looks right\n";
        match run_lane_verify_gate(&dir, "TASK-1", markdown).await {
            LaneVerifyOutcome::Skipped { .. } => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_lane_verify_gate_fails_when_wrapper_missing() {
        let dir =
            std::env::temp_dir().join(format!("autodev-verify-wrapper-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let markdown = "- [ ] `TASK-1` t\n\nVerification:\n- Run `bash -c true`\n";
        match run_lane_verify_gate(&dir, "TASK-1", markdown).await {
            LaneVerifyOutcome::Failed { detail } => {
                assert!(detail.contains("run-task-verification.sh"), "{detail}");
            }
            other => panic!("missing wrapper must fail closed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_lane_verify_gate_skips_external_live_steps() {
        let dir =
            std::env::temp_dir().join(format!("autodev-verify-external-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        install_test_wrapper(&dir);
        let markdown =
            "- [ ] `TASK-1` t\n\nVerification:\n- Check `https://example.com/health` is 200\n";
        match run_lane_verify_gate(&dir, "TASK-1", markdown).await {
            LaneVerifyOutcome::Skipped { reason } => {
                assert!(reason.contains("external/live"), "{reason}");
            }
            other => panic!("external verification should not pass, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
