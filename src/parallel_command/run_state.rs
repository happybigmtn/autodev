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
const COMPLETED_SWEEP_FILE: &str = ".completed-drift-sweep";
const WORKSPACE_BASELINE_FILE: &str = ".workspace-baseline.json";

/// Best-observed baseline of the shared workspace's health for a single
/// `auto parallel` run, persisted under the run root so the baseline-aware
/// landing gate (see [`super::verify_gate`]) can tell a NEW regression apart
/// from a pre-existing failure.
///
/// Monotonicity is the whole point of `ever_*`: once a test is observed passing
/// (or a crate observed compiling) anywhere in the run, it must STAY that way —
/// a later failure of it is a regression even if it was red in the original
/// baseline snapshot. The `baseline_*` fields hold only the first (pre-existing)
/// snapshot and are informational for host logs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct WorkspaceBaseline {
    /// Set once the first workspace probe of the run has been folded in.
    #[serde(default)]
    pub(crate) captured: bool,
    /// Whether the workspace compiled cleanly at first capture (informational).
    #[serde(default)]
    pub(crate) baseline_compiles: bool,
    /// Crate names that failed to compile at first capture (informational).
    #[serde(default)]
    pub(crate) baseline_broken_crates: BTreeSet<String>,
    /// Test IDs failing at first capture: the pre-existing failures that are
    /// ALLOWED to remain failing without blocking any task (informational; the
    /// live allow decision is "absent from `ever_passed_tests`").
    #[serde(default)]
    pub(crate) baseline_failing_tests: BTreeSet<String>,
    /// Every test id EVER observed passing this run. A member here that now fails
    /// is a regression.
    #[serde(default)]
    pub(crate) ever_passed_tests: BTreeSet<String>,
    /// Every crate/target stem EVER observed compiling this run. A member here
    /// that now fails to compile is a regression.
    #[serde(default)]
    pub(crate) ever_compiled_crates: BTreeSet<String>,
    /// A short excerpt of the actual `cargo` compiler error lines captured at
    /// FIRST snapshot when the workspace did not compile (e.g. the missing
    /// `include_str!` fixture path). Purely diagnostic: surfaced verbatim so a
    /// human reading a "no code lanes dispatchable" stop sees the real cause
    /// (a broken build) instead of decoding it. Empty when the workspace
    /// compiled at first capture.
    #[serde(default)]
    pub(crate) compile_error_excerpt: Vec<String>,
}

fn workspace_baseline_path(run_root: &Path) -> PathBuf {
    run_root.join(WORKSPACE_BASELINE_FILE)
}

/// Restore the persisted workspace baseline, or a default (uncaptured) one when
/// absent or unreadable. Never fails: a corrupt baseline degrades to "recapture".
pub(crate) fn load_workspace_baseline(run_root: &Path) -> WorkspaceBaseline {
    match std::fs::read_to_string(workspace_baseline_path(run_root)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            eprintln!(
                "warning: ignoring unreadable workspace baseline {}: {err:#}",
                workspace_baseline_path(run_root).display()
            );
            WorkspaceBaseline::default()
        }),
        Err(_) => WorkspaceBaseline::default(),
    }
}

/// Persist the workspace baseline (best-effort atomic write).
pub(crate) fn save_workspace_baseline(run_root: &Path, baseline: &WorkspaceBaseline) {
    match serde_json::to_string_pretty(baseline) {
        Ok(json) => {
            if let Err(err) = atomic_write(&workspace_baseline_path(run_root), json.as_bytes()) {
                eprintln!("warning: failed persisting workspace baseline: {err:#}");
            }
        }
        Err(err) => eprintln!("warning: failed serializing workspace baseline: {err:#}"),
    }
}

/// Drop the persisted workspace baseline so a fresh run on the same run root
/// recaptures its own pre-existing snapshot rather than inheriting a finished
/// run's best-observed sets.
pub(crate) fn clear_workspace_baseline(run_root: &Path) {
    let path = workspace_baseline_path(run_root);
    if path.exists() {
        if let Err(err) = std::fs::remove_file(&path) {
            eprintln!("warning: failed clearing workspace baseline: {err:#}");
        }
    }
}

/// Fingerprint of the last EXHAUSTIVE (non-deferred) drift-reverify sweep's
/// input surface: the tracked source tree + plan text + receipts state. When
/// the current fingerprint matches, no verification-relevant input has changed
/// since that sweep, so re-running the whole sweep would reproduce the same
/// result — it is skipped. This is repo-agnostic and provably safe: a
/// cross-task regression can only exist if some source changed, which changes
/// the fingerprint and forces a fresh sweep. Persisted across restarts so a
/// config-only relaunch (model/flag change, incident recovery) does not re-run
/// the sweep, which was the dominant wasted-cycle source (1500s+ per restart).
fn completed_sweep_path(run_root: &Path) -> PathBuf {
    run_root.join(COMPLETED_SWEEP_FILE)
}

pub(crate) fn load_completed_sweep_fingerprint(run_root: &Path) -> Option<String> {
    std::fs::read_to_string(completed_sweep_path(run_root))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(crate) fn save_completed_sweep_fingerprint(run_root: &Path, fingerprint: &str) {
    let _ = crate::util::atomic_write(&completed_sweep_path(run_root), fingerprint.as_bytes());
}

pub(crate) fn clear_completed_sweep_fingerprint(run_root: &Path) {
    let _ = std::fs::remove_file(completed_sweep_path(run_root));
}

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
    let mut state = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            eprintln!(
                "warning: ignoring unreadable parallel run-state ledger {}: {err:#}",
                path.display()
            );
            ParallelRunState::default()
        }),
        Err(_) => ParallelRunState::default(),
    };
    apply_retry_shelved_override(&mut state, retry_shelved_requested());
    state
}

/// Whether the operator asked to retry shelved/deferred tasks this run.
fn retry_shelved_requested() -> bool {
    std::env::var("AUTO_PARALLEL_RETRY_SHELVED")
        .ok()
        .as_deref()
        == Some("1")
}

/// Operator escape hatch. When a task was shelved or deferred during a prior run
/// (e.g. a transient dirty-tree cherry-pick conflict that the operator has since
/// fixed) the durable ledger would keep it shelved on every later invocation —
/// previously only `rm .run-state.json` recovered it. With
/// `AUTO_PARALLEL_RETRY_SHELVED=1`, re-invoking `auto parallel` gives every
/// shelved/deferred task a fresh attempt. Attempt counters are reset so the
/// retry is real, not immediately re-exhausted.
fn apply_retry_shelved_override(state: &mut ParallelRunState, requested: bool) {
    if !requested {
        return;
    }
    if state.shelved_tasks.is_empty() && state.deferred_partial_tasks.is_empty() {
        return;
    }
    eprintln!(
        "resume: AUTO_PARALLEL_RETRY_SHELVED=1 -> retrying {} shelved + {} deferred task(s) fresh",
        state.shelved_tasks.len(),
        state.deferred_partial_tasks.len()
    );
    state.shelved_tasks.clear();
    state.deferred_partial_tasks.clear();
    state.unblock_attempt_counts.clear();
    state.attempted_partial_followups.clear();
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

/// Mint and persist a fresh run id for this host at `<run_root>/.current-run-id`.
/// Format is `<unix_millis>-<pid>`, unique per host start. Best-effort: on write
/// failure the returned id is still usable in-process, lanes just won't be able
/// to compare against it (they degrade to "stale", never to a false "live").
pub(crate) fn stamp_current_parallel_run_id(run_root: &Path) -> String {
    let millis = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let run_id = format!("{millis}-{}", std::process::id());
    let path = run_root.join(CURRENT_RUN_ID_FILE);
    if let Err(err) = atomic_write(&path, run_id.as_bytes()) {
        eprintln!("warning: failed persisting current run id: {err:#}");
    }
    run_id
}

/// Read the current run id for `run_root`, if a host has stamped one.
pub(crate) fn current_parallel_run_id(run_root: &Path) -> Option<String> {
    std::fs::read_to_string(run_root.join(CURRENT_RUN_ID_FILE))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Copy the current run id into a lane's `.run-id` at assignment. Derives the
/// run root from `<run_root>/lanes/lane-N`. Best-effort.
pub(crate) fn stamp_lane_run_id(lane_root: &Path) {
    let Some(run_root) = lane_root.parent().and_then(|lanes| lanes.parent()) else {
        return;
    };
    let Some(run_id) = current_parallel_run_id(run_root) else {
        return;
    };
    if let Err(err) = atomic_write(&lane_root.join(LANE_RUN_ID_FILE), run_id.as_bytes()) {
        eprintln!("warning: failed stamping lane run id: {err:#}");
    }
}

/// Read a lane's stamped run id, if present.
pub(crate) fn lane_run_id(lane_root: &Path) -> Option<String> {
    std::fs::read_to_string(lane_root.join(LANE_RUN_ID_FILE))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// A lane is stale when the current run has an id and the lane's id is missing
/// or differs. If the current run has no id (older host, or status invoked with
/// no run ever started), nothing is classified stale — preserving prior
/// behavior rather than over-hiding lanes.
pub(crate) fn lane_is_from_previous_run(run_root: &Path, lane_root: &Path) -> bool {
    match current_parallel_run_id(run_root) {
        Some(current) => lane_run_id(lane_root).map(|id| id != current).unwrap_or(true),
        None => false,
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
    fn lane_from_current_run_is_not_stale() {
        let run_root = temp_dir("run-current");
        let run_id = stamp_current_parallel_run_id(&run_root);
        assert!(!run_id.is_empty());
        let lane_root = run_root.join("lanes").join("lane-1");
        std::fs::create_dir_all(&lane_root).expect("create lane");
        stamp_lane_run_id(&lane_root);
        assert_eq!(lane_run_id(&lane_root).as_deref(), Some(run_id.as_str()));
        assert!(!lane_is_from_previous_run(&run_root, &lane_root));
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn lane_from_previous_run_is_stale() {
        let run_root = temp_dir("run-prev");
        // Simulate a lane stamped by an earlier run, then a new host start.
        std::fs::create_dir_all(run_root.join("lanes").join("lane-1")).expect("create lane");
        let lane_root = run_root.join("lanes").join("lane-1");
        stamp_current_parallel_run_id(&run_root);
        stamp_lane_run_id(&lane_root);
        // New host start mints a different current run id.
        std::fs::write(run_root.join(CURRENT_RUN_ID_FILE), b"999999-1").expect("rewrite run id");
        assert!(lane_is_from_previous_run(&run_root, &lane_root));
        std::fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn no_current_run_id_means_no_lane_is_stale() {
        // Preserve prior behavior when no host ever stamped a run id.
        let run_root = temp_dir("run-none");
        let lane_root = run_root.join("lanes").join("lane-1");
        std::fs::create_dir_all(&lane_root).expect("create lane");
        assert!(!lane_is_from_previous_run(&run_root, &lane_root));
        std::fs::remove_dir_all(&run_root).ok();
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

    #[test]
    fn workspace_baseline_round_trips_and_clears() {
        let dir = temp_dir("workspace-baseline");
        assert!(
            !load_workspace_baseline(&dir).captured,
            "absent baseline is uncaptured"
        );
        let baseline = WorkspaceBaseline {
            captured: true,
            baseline_compiles: false,
            baseline_broken_crates: ["boardlab_tui".to_string()].into_iter().collect(),
            baseline_failing_tests: ["ludii_core::board::flaky".to_string()]
                .into_iter()
                .collect(),
            ever_passed_tests: ["ludii_core::board::stable".to_string()]
                .into_iter()
                .collect(),
            ever_compiled_crates: ["ludii_core".to_string()].into_iter().collect(),
            compile_error_excerpt: vec![
                "error: couldn't read a.lud: No such file or directory (os error 2)".to_string(),
            ],
        };

        save_workspace_baseline(&dir, &baseline);
        let restored = load_workspace_baseline(&dir);
        assert!(restored.captured);
        assert!(!restored.baseline_compiles);
        assert!(restored.baseline_broken_crates.contains("boardlab_tui"));
        assert!(restored
            .ever_passed_tests
            .contains("ludii_core::board::stable"));
        assert!(restored.ever_compiled_crates.contains("ludii_core"));
        assert_eq!(
            restored.compile_error_excerpt,
            baseline.compile_error_excerpt,
            "compile-error excerpt round-trips through the persisted baseline"
        );

        clear_workspace_baseline(&dir);
        assert!(!load_workspace_baseline(&dir).captured);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_workspace_baseline_degrades_to_default() {
        let dir = temp_dir("workspace-baseline-corrupt");
        std::fs::write(workspace_baseline_path(&dir), b"{not json").expect("write");
        assert!(!load_workspace_baseline(&dir).captured);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn retry_shelved_override_clears_shelved_and_deferred_when_requested() {
        let mut state = ParallelRunState::default();
        state
            .shelved_tasks
            .insert("TASK-006".to_string(), "task md".to_string());
        state.deferred_partial_tasks.insert("TASK-001".to_string());
        state.unblock_attempt_counts.insert("TASK-006".to_string(), 4);
        state
            .attempted_partial_followups
            .insert("TASK-001".to_string(), 1);

        // Not requested: state is untouched.
        let mut untouched = state.clone();
        apply_retry_shelved_override(&mut untouched, false);
        assert_eq!(untouched.shelved_tasks.len(), 1);
        assert_eq!(untouched.deferred_partial_tasks.len(), 1);

        // Requested: shelved/deferred + attempt counters are cleared for a fresh retry.
        apply_retry_shelved_override(&mut state, true);
        assert!(state.shelved_tasks.is_empty());
        assert!(state.deferred_partial_tasks.is_empty());
        assert!(state.unblock_attempt_counts.is_empty());
        assert!(state.attempted_partial_followups.is_empty());
    }
}
