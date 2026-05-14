//! Session-survivable launch + per-lane checkpoints.
//!
//! Background: when `auto super` is invoked inside a Claude Code session, the
//! session boundary reaps descendants synchronously when the user disconnects
//! or the session ends. Two production super runs died this way before the
//! workaround landed. The workaround (a shell wrapper `auto-super-opus` that
//! re-execs under `systemd-run --user --scope`) lived outside autodev so
//! users had to discover and install it themselves.
//!
//! This module folds that workaround into the binary: when we detect we are
//! running as a Claude descendant, we re-exec the same invocation under a
//! transient user-scoped systemd unit so the process survives the session
//! reaper. The decision is conservative; if `systemd-run` is missing or the
//! marker shows we have already re-execed, we proceed in-process.
//!
//! The lane-checkpoint primitive is a small helper for parallel/super lane
//! workers to persist phase progress so a survived process can resume after
//! a crash or operator restart. Call-site migration is owned by
//! `parallel_command.rs` and not done here.

use std::env;
use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::util::{atomic_write, timestamp_slug};

const REEXEC_DONE_ENV: &str = "AUTODEV_REEXEC_DONE";
const FORCE_REEXEC_ENV: &str = "AUTODEV_FORCE_REEXEC";
const CLAUDE_SESSION_ENV: &str = "CLAUDE_SESSION_ID";

/// Why we believe the current process is reapable by a parent session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReapableReason {
    ClaudeSessionEnv,
    ClaudeAncestor,
    ForcedByOperator,
}

#[derive(Clone, Debug)]
pub(crate) struct ReapableContext {
    pub(crate) reason: ReapableReason,
}

/// Return `Some(ctx)` if any heuristic indicates we are running under a
/// Claude session boundary (and therefore at risk of being synchronously
/// reaped). Returns `None` when the environment looks clean.
pub(crate) fn detect_reapable_parent() -> Option<ReapableContext> {
    if env::var(REEXEC_DONE_ENV).is_ok() {
        return None;
    }
    if env::var(FORCE_REEXEC_ENV).map(|v| v == "1").unwrap_or(false) {
        return Some(ReapableContext {
            reason: ReapableReason::ForcedByOperator,
        });
    }
    if env::var(CLAUDE_SESSION_ENV).is_ok() {
        return Some(ReapableContext {
            reason: ReapableReason::ClaudeSessionEnv,
        });
    }
    if ancestor_command_contains("claude") {
        return Some(ReapableContext {
            reason: ReapableReason::ClaudeAncestor,
        });
    }
    None
}

/// Walk `/proc/<pid>/status` upward (current -> parent -> grandparent) and
/// return true if any ancestor's command name contains `needle`. We stop at
/// init (PPid 1 or 0) to bound the walk. Falls through silently on any I/O
/// error -- failing closed (False) is safe: we just won't re-exec.
fn ancestor_command_contains(needle: &str) -> bool {
    let mut pid: u32 = match read_ppid(std::process::id()) {
        Some(parent) => parent,
        None => return false,
    };
    for _ in 0..4 {
        if pid <= 1 {
            return false;
        }
        if let Some(name) = read_proc_comm(pid) {
            if name.contains(needle) {
                return true;
            }
        }
        match read_ppid(pid) {
            Some(next) => pid = next,
            None => return false,
        }
    }
    false
}

fn read_ppid(pid: u32) -> Option<u32> {
    let path = format!("/proc/{pid}/status");
    let raw = fs::read_to_string(&path).ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            return rest.trim().parse::<u32>().ok();
        }
    }
    None
}

fn read_proc_comm(pid: u32) -> Option<String> {
    let path = format!("/proc/{pid}/comm");
    fs::read_to_string(&path)
        .ok()
        .map(|raw| raw.trim().to_string())
}

/// Re-exec the current invocation under a transient user-scoped systemd unit
/// so it survives Claude-session reaping. On success the call replaces the
/// current process and never returns. On graceful fallback (no `systemd-run`,
/// missing argv, already re-execed) the function logs a warning and returns
/// Ok(()) so the caller proceeds in-process.
pub(crate) fn reexec_under_systemd_scope(
    ctx: ReapableContext,
    instance: &str,
) -> Result<()> {
    if env::var(REEXEC_DONE_ENV).is_ok() {
        return Ok(());
    }
    if which("systemd-run").is_none() {
        eprintln!(
            "session-survival: systemd-run not on PATH; staying in current process (reason: {:?})",
            ctx.reason
        );
        return Ok(());
    }
    let exe = env::current_exe()
        .context("session-survival: failed to resolve current executable path")?;
    let argv: Vec<String> = env::args().collect();
    if argv.is_empty() {
        eprintln!("session-survival: empty argv; staying in current process");
        return Ok(());
    }
    let unit_name = format!("auto-super-{}-{}", sanitize_unit(instance), timestamp_slug());

    let mut cmd = Command::new("systemd-run");
    cmd.arg("--user")
        .arg("--scope")
        .arg("--quiet")
        .arg("-u")
        .arg(&unit_name)
        .arg(&exe);
    for arg in argv.iter().skip(1) {
        cmd.arg(arg);
    }
    cmd.env(REEXEC_DONE_ENV, "1");
    eprintln!(
        "session-survival: re-execing under transient unit {unit_name} (reason: {:?})",
        ctx.reason
    );
    // exec() replaces the current process on success. If it returns at all,
    // it's an error -- there's no successful return path.
    let err = cmd.exec();
    Err(err).context("session-survival: systemd-run exec failed")
}

/// Convenience wrapper: detect and re-exec if appropriate. Safe to call
/// unconditionally at the top of an entry point.
pub(crate) fn reexec_if_reapable(instance: &str) -> Result<()> {
    if let Some(ctx) = detect_reapable_parent() {
        reexec_under_systemd_scope(ctx, instance)?;
    }
    Ok(())
}

fn which(bin: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn sanitize_unit(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Per-lane checkpoint payload. The blob is opaque JSON owned by the lane
/// worker; this module only handles atomic persistence + read-back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LaneCheckpoint {
    pub(crate) phase: String,
    pub(crate) blob: serde_json::Value,
    pub(crate) written_at: DateTime<Utc>,
}

pub(crate) fn write_lane_checkpoint(
    path: &Path,
    phase: &str,
    blob: serde_json::Value,
) -> Result<()> {
    let checkpoint = LaneCheckpoint {
        phase: phase.to_string(),
        blob,
        written_at: Utc::now(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create checkpoint dir {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(&checkpoint)
        .context("serialize lane checkpoint")?;
    atomic_write(path, &bytes)?;
    Ok(())
}

pub(crate) fn read_lane_checkpoint(path: &Path) -> Result<Option<LaneCheckpoint>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read checkpoint {}", path.display()))?;
    let checkpoint: LaneCheckpoint = serde_json::from_str(&raw)
        .with_context(|| format!("parse checkpoint {}", path.display()))?;
    Ok(Some(checkpoint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // env var manipulation must be serialized across tests in the same process.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        prior: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let prior = keys
                .iter()
                .map(|k| (*k, env::var(k).ok()))
                .collect::<Vec<_>>();
            for k in keys {
                env::remove_var(k);
            }
            Self { prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.prior {
                match v {
                    Some(value) => env::set_var(k, value),
                    None => env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn session_survival_detect_returns_some_when_claude_session_id_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new(&[CLAUDE_SESSION_ENV, REEXEC_DONE_ENV, FORCE_REEXEC_ENV]);
        env::set_var(CLAUDE_SESSION_ENV, "abc-123");
        let ctx = detect_reapable_parent().expect("expected reapable ctx");
        assert_eq!(ctx.reason, ReapableReason::ClaudeSessionEnv);
    }

    #[test]
    fn session_survival_detect_returns_none_when_clean_and_no_claude_ancestor() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new(&[CLAUDE_SESSION_ENV, REEXEC_DONE_ENV, FORCE_REEXEC_ENV]);
        // If a real claude ancestor is present in the test runner's pid tree
        // we skip rather than fail; the heuristic is doing its job.
        if ancestor_command_contains("claude") {
            return;
        }
        assert!(detect_reapable_parent().is_none());
    }

    #[test]
    fn session_survival_reexec_is_noop_when_already_done() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new(&[CLAUDE_SESSION_ENV, REEXEC_DONE_ENV, FORCE_REEXEC_ENV]);
        env::set_var(REEXEC_DONE_ENV, "1");
        // Forcing should still no-op because the done marker wins.
        env::set_var(FORCE_REEXEC_ENV, "1");
        assert!(detect_reapable_parent().is_none());
        // reexec_if_reapable should also be a no-op.
        reexec_if_reapable("test-instance").expect("noop should succeed");
    }

    #[test]
    fn session_survival_lane_checkpoint_round_trips() {
        let tmp = tempdir();
        let path = tmp.join("lane-checkpoint.json");
        let blob = serde_json::json!({"completed": ["step-a", "step-b"], "cursor": 42});
        write_lane_checkpoint(&path, "implement", blob.clone()).expect("write checkpoint");
        let loaded = read_lane_checkpoint(&path)
            .expect("read checkpoint")
            .expect("checkpoint must exist");
        assert_eq!(loaded.phase, "implement");
        assert_eq!(loaded.blob, blob);
        cleanup(&tmp);
    }

    #[test]
    fn session_survival_lane_checkpoint_missing_returns_none() {
        let tmp = tempdir();
        let path = tmp.join("does-not-exist.json");
        let loaded = read_lane_checkpoint(&path).expect("missing is Ok(None)");
        assert!(loaded.is_none());
        cleanup(&tmp);
    }

    #[test]
    fn session_survival_sanitize_unit_strips_unsafe_chars() {
        assert_eq!(sanitize_unit("super-bitino"), "super-bitino");
        assert_eq!(sanitize_unit("super/bitino:1"), "super-bitino-1");
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn tempdir() -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let base = std::env::temp_dir().join(format!(
            "autodev-session-survival-{}-{nanos}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&base).expect("mkdir tempdir");
        base
    }

    fn cleanup(p: &Path) {
        let _ = fs::remove_dir_all(p);
    }
}
