//! Durable run-state ledger for `auto parallel`.
//!
//! The plan checkboxes in `IMPLEMENTATION_PLAN.md` are already durable (git), so
//! a resumed host knows what is Done / Partial / Pending. What it otherwise
//! LOSES on a crash/restart is the in-memory scheduling bookkeeping: which `[~]`
//! tasks it had shelved or parked for the run, and how many retry/unblock
//! attempts each had already consumed. Without that, a resumed host resets every
//! budget and re-thrashes through the same failures, burning expensive workers.
//!
//! This persists those four maps to `<run_root>/.run-state.json` after each main
//! loop iteration and restores them at startup. It is purely protective: every
//! restored entry is re-pruned against the freshly-read plan by the existing
//! per-iteration `retain` calls, so a stale ledger can never resurrect a task the
//! plan says is Done or whose spec changed. Persistence failures are logged and
//! ignored — the ledger never blocks a run.

use super::*;

use std::collections::{BTreeMap, BTreeSet};

const RUN_STATE_FILE: &str = ".run-state.json";

/// Snapshot of the host's per-run scheduling bookkeeping. Field names are stable
/// (serialized to disk); add new fields with `#[serde(default)]` to stay
/// backward-compatible with ledgers written by older binaries.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ParallelRunState {
    /// task id -> task markdown at shelve time ("don't redispatch this run").
    #[serde(default)]
    pub(crate) shelved_tasks: BTreeMap<String, String>,
    /// Partial tasks parked for the run after exhausting in-run follow-ups.
    #[serde(default)]
    pub(crate) deferred_partial_tasks: BTreeSet<String>,
    /// task id -> autonomous unblock attempts already consumed.
    #[serde(default)]
    pub(crate) unblock_attempt_counts: BTreeMap<String, usize>,
    /// task id -> in-run partial follow-up attempts already consumed.
    #[serde(default)]
    pub(crate) attempted_partial_followups: BTreeMap<String, usize>,
}

fn run_state_path(run_root: &Path) -> PathBuf {
    run_root.join(RUN_STATE_FILE)
}

/// Restore the run-state ledger, or a default (empty) state when none exists or
/// it cannot be parsed. Never fails: a corrupt ledger degrades to "start fresh".
pub(crate) fn load_parallel_run_state(run_root: &Path) -> ParallelRunState {
    let path = run_state_path(run_root);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            eprintln!(
                "warning: ignoring unreadable parallel run-state ledger {}: {err:#}",
                path.display()
            );
            ParallelRunState::default()
        }),
        Err(_) => ParallelRunState::default(),
    }
}

/// Persist the current scheduling bookkeeping. Best-effort: a write error is
/// logged and ignored so the ledger can never wedge or slow a real run.
pub(crate) fn save_parallel_run_state(
    run_root: &Path,
    shelved_tasks: &BTreeMap<String, String>,
    deferred_partial_tasks: &BTreeSet<String>,
    unblock_attempt_counts: &BTreeMap<String, usize>,
    attempted_partial_followups: &BTreeMap<String, usize>,
) {
    let state = ParallelRunState {
        shelved_tasks: shelved_tasks.clone(),
        deferred_partial_tasks: deferred_partial_tasks.clone(),
        unblock_attempt_counts: unblock_attempt_counts.clone(),
        attempted_partial_followups: attempted_partial_followups.clone(),
    };
    match serde_json::to_string_pretty(&state) {
        Ok(json) => {
            if let Err(err) = atomic_write(&run_state_path(run_root), json.as_bytes()) {
                eprintln!("warning: failed persisting parallel run-state ledger: {err:#}");
            }
        }
        Err(err) => eprintln!("warning: failed serializing parallel run-state ledger: {err:#}"),
    }
}

/// Remove the ledger after a clean completion so a later run on the same
/// `run_root` starts fresh rather than reloading a finished run's bookkeeping.
pub(crate) fn clear_parallel_run_state(run_root: &Path) {
    let path = run_state_path(run_root);
    if path.exists() {
        if let Err(err) = std::fs::remove_file(&path) {
            eprintln!("warning: failed clearing parallel run-state ledger: {err:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-run-state-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn load_returns_default_when_absent() {
        let dir = temp_dir("absent");
        let state = load_parallel_run_state(&dir);
        assert!(state.shelved_tasks.is_empty());
        assert!(state.deferred_partial_tasks.is_empty());
        assert!(state.unblock_attempt_counts.is_empty());
        assert!(state.attempted_partial_followups.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp_dir("roundtrip");
        let mut shelved = BTreeMap::new();
        shelved.insert("TASK-1".to_string(), "- [~] `TASK-1` t\n".to_string());
        let mut deferred = BTreeSet::new();
        deferred.insert("TASK-2".to_string());
        let mut unblock = BTreeMap::new();
        unblock.insert("TASK-2".to_string(), 3usize);
        let mut followups = BTreeMap::new();
        followups.insert("TASK-1".to_string(), 2usize);

        save_parallel_run_state(&dir, &shelved, &deferred, &unblock, &followups);
        let restored = load_parallel_run_state(&dir);
        assert_eq!(restored.shelved_tasks, shelved);
        assert_eq!(restored.deferred_partial_tasks, deferred);
        assert_eq!(restored.unblock_attempt_counts, unblock);
        assert_eq!(restored.attempted_partial_followups, followups);

        clear_parallel_run_state(&dir);
        assert!(load_parallel_run_state(&dir).shelved_tasks.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_ledger_degrades_to_default() {
        let dir = temp_dir("corrupt");
        std::fs::write(run_state_path(&dir), b"{not valid json").expect("write");
        let state = load_parallel_run_state(&dir);
        assert!(state.shelved_tasks.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
