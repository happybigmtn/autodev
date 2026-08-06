use super::*;

fn scope_effort_routing_enabled() -> bool {
    std::env::var("AUTO_PARALLEL_SCOPE_EFFORT")
        .map(|value| value.trim() != "0")
        .unwrap_or(true)
}

/// Whether a lane-local Cargo target dir is kept OUTSIDE the disposable worktree
/// so incremental compilation survives assignment churn (default on; `=0`
/// restores the legacy in-worktree `<lane_root>/cargo-target` that is deleted
/// with every fresh clone, forcing a cold compile per task).
pub(crate) fn lane_persistent_cargo_target_enabled() -> bool {
    std::env::var("AUTO_LANE_PERSISTENT_TARGET")
        .map(|value| value.trim() != "0")
        .unwrap_or(true)
}

/// Resolve the persistent per-lane Cargo target for a lane-local layout, derived
/// purely from the lane-root layout invariant `<run_root>/lanes/lane-<n>`.
///
/// Every fresh assignment deletes and re-clones `<run_root>/lanes/lane-<n>` (see
/// [`reset_parallel_lane_root`]), so a target dir *inside* that worktree is thrown
/// away every task and each compile starts cold. Relocating it to a stable
/// `<run_root>/lane-caches/lane-<n>/cargo-target` — a sibling of `lanes/`, outside
/// the disposable worktree — lets incremental compilation be reused across every
/// task that runs on the same lane.
///
/// Keyed per lane index: a lane index is held by at most one active worker at a
/// time (see [`next_free_lane_index`]), so concurrent lanes never share a target
/// dir and there is no cargo lock contention or artifact corruption.
///
/// Returns `None` (caller falls back to the legacy in-worktree target) when the
/// feature is disabled or the lane-root layout cannot be parsed — a safe
/// degradation, never a hard failure.
fn lane_persistent_cargo_target_for(lane_root: &Path) -> Option<PathBuf> {
    if !lane_persistent_cargo_target_enabled() {
        return None;
    }
    let lane_name = lane_root.file_name()?.to_str()?;
    let lane_index = parse_lane_index(lane_name)?;
    // `<run_root>/lanes/lane-<n>` -> `<run_root>`.
    let run_root = lane_root.parent()?.parent()?;
    Some(
        run_root
            .join("lane-caches")
            .join(format!("lane-{lane_index}"))
            .join("cargo-target"),
    )
}

const GIB: u64 = 1024 * 1024 * 1024;

/// Default per-lane cap on the persistent Cargo-target cache, in GiB. The
/// persistent target (P1, commit 8024d30) survives worktree churn so a resuming
/// run reuses warm artifacts, but it therefore also grows unbounded across a
/// long run — stale test/bin artifacts and `debug/incremental` accumulate with
/// every HEAD advance and never get GC'd by cargo. Left uncapped it filled the
/// shared build volume and killed the fleet. This caps it so total lane-caches
/// disk is bounded by roughly `cap × lanes`.
const DEFAULT_LANE_CACHE_MAX_GIB: u64 = 24;

/// Parse the `AUTO_LANE_CACHE_MAX_GB` per-lane cap. `Some(bytes)` = enforce that
/// cap; `None` = disabled (unbounded, the pre-cap behavior). Empty/garbage falls
/// back to the safe default rather than disabling — a typo must never silently
/// re-arm the unbounded-growth failure. `0` explicitly disables.
fn parse_lane_cache_max_bytes(raw: Option<&str>) -> Option<u64> {
    match raw.map(str::trim) {
        None | Some("") => Some(DEFAULT_LANE_CACHE_MAX_GIB * GIB),
        Some(value) => match value.parse::<u64>() {
            Ok(0) => None,
            Ok(gib) => Some(gib.saturating_mul(GIB)),
            Err(_) => Some(DEFAULT_LANE_CACHE_MAX_GIB * GIB),
        },
    }
}

fn lane_cache_max_bytes() -> Option<u64> {
    parse_lane_cache_max_bytes(std::env::var("AUTO_LANE_CACHE_MAX_GB").ok().as_deref())
}

/// Whether first-party incremental compilation is kept for lane builds. Default
/// OFF: `*/incremental` is the fastest-growing, fully regenerable component of a
/// lane cache (measured ~12-14 GiB/lane in production), and the P1 speedup comes
/// from the warm `deps/` rlib cache — dependency compilation — which
/// `CARGO_INCREMENTAL=0` does NOT touch. Because each task starts from a fresh
/// clone (new file mtimes), cross-task incremental reuse is weak anyway, so the
/// tradeoff is small: within a task, non-incremental first-party recompiles vs.
/// the still-warm dependency graph. Set `AUTO_LANE_CARGO_INCREMENTAL=1` to
/// restore incremental (the pre-cap behavior).
fn parse_lane_cargo_incremental_enabled(raw: Option<&str>) -> bool {
    raw.map(str::trim) == Some("1")
}

fn lane_cargo_incremental_enabled() -> bool {
    parse_lane_cargo_incremental_enabled(
        std::env::var("AUTO_LANE_CARGO_INCREMENTAL").ok().as_deref(),
    )
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LaneCachePrune {
    /// Under the cap (or nothing to prune) — warm cache left intact.
    UnderCap,
    /// Over the cap; dropped the regenerable `*/incremental` dirs and that
    /// brought it back under. Warm `deps/` rlibs preserved.
    PrunedIncremental,
    /// Over the cap even without incremental (bulk is `deps/`); removed the
    /// whole target. The next task on this lane recompiles cold once — the
    /// accepted worst case that guarantees the hard bound.
    Reset,
}

/// Delete every `incremental` directory anywhere under `target` (typically
/// `<target>/debug/incremental` and `<target>/release/incremental`, plus any
/// target-triple-nested variants). Fully regenerable; preserves `deps/` rlibs.
/// Returns bytes reclaimed.
fn prune_incremental_dirs(target: &Path) -> u64 {
    let mut freed = 0u64;
    let mut stack = vec![target.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let path = entry.path();
            if entry.file_name() == "incremental" {
                freed += dir_size_bytes(&path);
                if let Err(err) = fs::remove_dir_all(&path) {
                    eprintln!(
                        "warning: lane-cache: failed pruning incremental {}: {err:#}",
                        path.display()
                    );
                }
            } else {
                stack.push(path);
            }
        }
    }
    freed
}

/// Pure, testable core of the size cap: given a lane's cargo-target and a byte
/// cap, prune tiered so warm dependency artifacts survive when possible.
pub(crate) fn prune_lane_cache_over_cap(target: &Path, cap: u64) -> LaneCachePrune {
    if !target.exists() {
        return LaneCachePrune::UnderCap;
    }
    if dir_size_bytes(target) <= cap {
        return LaneCachePrune::UnderCap;
    }
    // Tier 1: drop incremental (fastest-growing, fully regenerable); keep deps warm.
    prune_incremental_dirs(target);
    if dir_size_bytes(target) <= cap {
        return LaneCachePrune::PrunedIncremental;
    }
    // Tier 2: the bulk is deps/ (accumulated stale artifacts) — reset the target.
    if let Err(err) = fs::remove_dir_all(target) {
        eprintln!(
            "warning: lane-cache: failed resetting over-cap target {}: {err:#}",
            target.display()
        );
    }
    LaneCachePrune::Reset
}

/// Bound the persistent lane-cache for `lane_root` at (re)assignment time.
///
/// Safe by construction: this runs on the fresh-assignment path, immediately
/// before [`reset_parallel_lane_root`] wipes the lane worktree. The lane index
/// has just been freed by [`next_free_lane_index`], so the previous worker on
/// this index has finished and the next has not started — nothing is compiling
/// into this lane's persistent target, so pruning it cannot corrupt a live
/// build. No-ops when the persistent-target feature is off (`None` target) or
/// the cap is disabled (`AUTO_LANE_CACHE_MAX_GB=0`).
pub(crate) fn enforce_lane_cache_size_cap(lane_root: &Path) {
    let Some(cap) = lane_cache_max_bytes() else {
        return;
    };
    let Some(target) = lane_persistent_cargo_target_for(lane_root) else {
        return;
    };
    if !target.exists() {
        return;
    }
    let before = dir_size_bytes(&target);
    match prune_lane_cache_over_cap(&target, cap) {
        LaneCachePrune::UnderCap => {}
        LaneCachePrune::PrunedIncremental => {
            println!(
                "lane-cache: {} over cap {} — pruned incremental, now {} (warm deps kept)",
                target.display(),
                human_bytes(cap),
                human_bytes(dir_size_bytes(&target))
            );
        }
        LaneCachePrune::Reset => {
            println!(
                "lane-cache: {} was {} over cap {} — reset (cold recompile next task)",
                target.display(),
                human_bytes(before),
                human_bytes(cap)
            );
        }
    }
}

/// Remove `<run_root>/lane-caches/lane-M` for `M > max_concurrent_workers` at
/// startup. These are orphans from a prior run that used more lanes (e.g. after
/// dialing 8 lanes down to 6, `lane-7`/`lane-8` linger); the current run never
/// assigns those indices, so their persistent targets are pure dead weight.
/// Always safe: no lane with index > `max_concurrent_workers` is ever active
/// this run. Best-effort; failures are logged and never block the run.
pub(crate) fn prune_orphan_lane_caches(run_root: &Path, max_concurrent_workers: usize) {
    let caches = run_root.join("lane-caches");
    let Ok(entries) = fs::read_dir(&caches) else {
        return;
    };
    let mut freed = 0u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(index) = parse_lane_index(name) else {
            continue;
        };
        if index > max_concurrent_workers {
            let path = entry.path();
            freed += dir_size_bytes(&path);
            if let Err(err) = fs::remove_dir_all(&path) {
                eprintln!(
                    "warning: lane-cache: failed removing orphan {}: {err:#}",
                    path.display()
                );
            }
        }
    }
    if freed > 0 {
        println!(
            "lane-cache: reclaimed {} from orphan lane-caches (lanes > {})",
            human_bytes(freed),
            max_concurrent_workers
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScopeClass {
    Small,
    Medium,
    Large,
}

/// Map a plan row's `Estimated scope:` value to a class. Accepts single-letter
/// (S/M/L, XS->Small, XL->Large) and word forms (small/medium/large).
fn normalize_scope(scope: Option<&str>) -> Option<ScopeClass> {
    let raw = scope?.trim().to_ascii_lowercase();
    let token = raw.split_whitespace().next().unwrap_or("");
    match token {
        "xs" | "s" | "small" | "trivial" | "tiny" => Some(ScopeClass::Small),
        "m" | "med" | "medium" => Some(ScopeClass::Medium),
        "l" | "xl" | "xxl" | "large" | "big" | "huge" => Some(ScopeClass::Large),
        _ => None,
    }
}

/// Total order over Codex/Claude effort levels, low to high. Unknown strings
/// rank at the top so an unrecognized ceiling never silently caps routing down.
fn effort_rank(effort: &str) -> u8 {
    match effort.trim().to_ascii_lowercase().as_str() {
        "none" => 0,
        "minimal" => 1,
        "low" => 2,
        "medium" => 3,
        "high" => 4,
        "xhigh" => 5,
        _ => u8::MAX,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LaneRunConfig {
    pub(crate) claude: bool,
    pub(crate) max_turns: Option<usize>,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
    pub(crate) codex_bin: PathBuf,
    pub(crate) extra_env: Vec<(String, String)>,
    pub(crate) lane_local_cargo_target: bool,
    pub(crate) cargo_target_prompt_clause: String,
    pub(crate) preflight_prompt_clause: String,
}

impl LaneRunConfig {
    pub(crate) fn new(
        args: &ParallelArgs,
        worker_env: &LoopWorkerEnv,
        preflight_prompt_clause: String,
    ) -> Self {
        Self {
            claude: args.claude,
            max_turns: effective_parallel_claude_max_turns(args),
            model: args.model.clone(),
            reasoning_effort: args.reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            extra_env: worker_env.extra_env.clone(),
            lane_local_cargo_target: worker_env.lane_local_cargo_target,
            cargo_target_prompt_clause: worker_env.cargo_target_prompt_clause.clone(),
            preflight_prompt_clause,
        }
    }

    /// Reasoning effort to actually run this attempt at. Scope-based routing
    /// (on by default; disable with `AUTO_PARALLEL_SCOPE_EFFORT=0`) spends less
    /// on mechanical tasks: S/XS -> medium, M -> high, L/unknown -> the
    /// operator's `--reasoning-effort`. The operator value is a hard ceiling —
    /// routing never exceeds it. Any retry (attempt > 1) uses the ceiling, so a
    /// task that failed cheap is re-run at full strength.
    pub(crate) fn effective_reasoning_effort(&self, scope: Option<&str>, attempt: usize) -> String {
        if !scope_effort_routing_enabled() || attempt > 1 {
            return self.reasoning_effort.clone();
        }
        let target = match normalize_scope(scope) {
            Some(ScopeClass::Small) => "medium",
            Some(ScopeClass::Medium) => "high",
            Some(ScopeClass::Large) | None => return self.reasoning_effort.clone(),
        };
        // Cap at the operator ceiling.
        if effort_rank(target) <= effort_rank(&self.reasoning_effort) {
            target.to_string()
        } else {
            self.reasoning_effort.clone()
        }
    }

    pub(crate) fn env_for_lane(&self, lane_root: &Path) -> Vec<(String, String)> {
        let mut extra_env = self.extra_env.clone();
        if extra_env
            .iter()
            .any(|(key, _)| key == WORKER_CARGO_LEASE_ENV)
        {
            extra_env.push((
                WORKER_LANE_CARGO_LEASE_ENV.to_string(),
                lane_cargo_lease_path(lane_root)
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        if self.lane_local_cargo_target {
            let target = lane_persistent_cargo_target_for(lane_root)
                .unwrap_or_else(|| lane_root.join("cargo-target"));
            extra_env.push((
                "CARGO_TARGET_DIR".to_string(),
                target.to_string_lossy().into_owned(),
            ));
            if !lane_cargo_incremental_enabled() {
                // Bound the fastest-growing, fully-regenerable slice of the
                // persistent lane cache. Warm `deps/` rlibs (the P1 win) are
                // unaffected by CARGO_INCREMENTAL.
                extra_env.push(("CARGO_INCREMENTAL".to_string(), "0".to_string()));
            }
        }
        extra_env
    }

    pub(crate) fn assignment_worker_metadata(&self) -> LaneWorkerMetadata {
        if self.claude {
            let mut command = vec![
                "claude".to_string(),
                "-p".to_string(),
                "--verbose".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--model".to_string(),
                self.model.clone(),
                "--effort".to_string(),
                self.reasoning_effort.clone(),
                "--output-format".to_string(),
                "stream-json".to_string(),
            ];
            if let Some(max_turns) = self.max_turns {
                command.push("--max-turns".to_string());
                command.push(max_turns.to_string());
            }
            return LaneWorkerMetadata {
                harness: "claude".to_string(),
                command,
                model: self.model.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
                max_turns: self.max_turns,
            };
        }

        LaneWorkerMetadata {
            harness: "codex".to_string(),
            command: vec![
                self.codex_bin.display().to_string(),
                "exec".to_string(),
                "--json".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--skip-git-repo-check".to_string(),
                "-m".to_string(),
                self.model.clone(),
                "-c".to_string(),
                format!("model_reasoning_effort=\"{}\"", self.reasoning_effort),
            ],
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            max_turns: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveLaneAssignment {
    pub(crate) lane_index: usize,
    pub(crate) attempts: usize,
    pub(crate) task: LoopTask,
    pub(crate) resumed: bool,
    pub(crate) lane_root: PathBuf,
    pub(crate) lane_repo_root: PathBuf,
    pub(crate) base_commit: String,
    pub(crate) stdout_log_path: PathBuf,
    pub(crate) stderr_log_path: PathBuf,
    pub(crate) worker_pid_path: PathBuf,
    pub(crate) clean_commit_since: Option<Instant>,
    pub(crate) terminate_requested_at: Option<Instant>,
    pub(crate) host_recovery_note: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct LaneResumeCandidate {
    pub(crate) lane_index: usize,
    pub(crate) task: LoopTask,
    pub(crate) lane_root: PathBuf,
    pub(crate) lane_repo_root: PathBuf,
    pub(crate) base_commit: String,
    pub(crate) stdout_log_path: PathBuf,
    pub(crate) stderr_log_path: PathBuf,
    pub(crate) worker_pid_path: PathBuf,
    pub(crate) host_recovery_note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct LaneWorkerMetadata {
    pub(crate) harness: String,
    pub(crate) command: Vec<String>,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
    pub(crate) max_turns: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct LaneAssignmentMetadata {
    pub(crate) task_id: String,
    pub(crate) target_branch: String,
    pub(crate) base_commit: String,
    pub(crate) task_hash: u64,
    pub(crate) dependency_hash: u64,
    pub(crate) verification_hash: u64,
    pub(crate) worker: LaneWorkerMetadata,
    pub(crate) assignment_hash: u64,
}

#[derive(Debug)]
pub(crate) struct LaneAttemptResult {
    pub(crate) lane_index: usize,
    pub(crate) exit_status: Option<ExitStatus>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CherryPickFailurePolicy {
    Abort,
    LeaveInProgress,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LaneLandingOutcome {
    Landed {
        auto_repaired: bool,
        completion_status: LoopTaskStatus,
    },
    NeedsRecovery {
        recovery_note: String,
        conflict_paths: Vec<String>,
    },
    DivergenceExhausted {
        detail: String,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LaneLandingRecoveryPrep {
    RebasedCleanly,
    NeedsWorkerResolution {
        recovery_note: String,
        conflict_paths: Vec<String>,
    },
}

pub(crate) fn next_free_lane_index(
    max_concurrent_workers: usize,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
) -> Option<usize> {
    (1..=max_concurrent_workers).find(|lane_index| !active_lanes.contains_key(lane_index))
}

pub(crate) fn prepare_parallel_lane_assignment(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_config: &LaneRunConfig,
    lane_index: usize,
    task: LoopTask,
    resume_candidate: Option<LaneResumeCandidate>,
) -> Result<ActiveLaneAssignment> {
    let worker_metadata = lane_config.assignment_worker_metadata();
    if let Some(candidate) = resume_candidate {
        write_lane_task_id(&candidate.lane_root, &task.id)?;
        write_lane_assignment_metadata(
            &candidate.lane_root,
            target_branch,
            &candidate.base_commit,
            &task,
            &worker_metadata,
        )?;
        return Ok(ActiveLaneAssignment {
            lane_index: candidate.lane_index,
            attempts: 0,
            task,
            resumed: true,
            lane_root: candidate.lane_root,
            lane_repo_root: candidate.lane_repo_root,
            base_commit: candidate.base_commit,
            stdout_log_path: candidate.stdout_log_path,
            stderr_log_path: candidate.stderr_log_path,
            worker_pid_path: candidate.worker_pid_path,
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: candidate.host_recovery_note,
        });
    }

    let lane_root = run_root.join("lanes").join(format!("lane-{lane_index}"));
    // Bound the persistent lane-cache before the next task compiles into it.
    // The lane is idle here (index just freed, previous worker done), so this
    // only ever prunes a cache no build is using. See enforce_lane_cache_size_cap.
    if lane_config.lane_local_cargo_target {
        enforce_lane_cache_size_cap(&lane_root);
    }
    reset_parallel_lane_root(&lane_root)?;
    let lane_repo_root = lane_root.join("repo");
    clone_loop_lane_repo(repo_root, target_branch, &lane_repo_root)?;
    let base_commit = git_stdout(&lane_repo_root, ["rev-parse", "HEAD"])?;
    write_lane_task_id(&lane_root, &task.id)?;
    write_lane_assignment_metadata(
        &lane_root,
        target_branch,
        base_commit.trim(),
        &task,
        &worker_metadata,
    )?;
    Ok(ActiveLaneAssignment {
        lane_index,
        attempts: 0,
        task,
        resumed: false,
        lane_root: lane_root.clone(),
        lane_repo_root,
        base_commit: base_commit.trim().to_string(),
        stdout_log_path: lane_root.join("stdout.log"),
        stderr_log_path: lane_root.join("stderr.log"),
        worker_pid_path: lane_root.join("worker.pid"),
        clean_commit_since: None,
        terminate_requested_at: None,
        host_recovery_note: None,
    })
}

pub(crate) fn reset_parallel_lane_root(lane_root: &Path) -> Result<()> {
    if lane_root.exists() {
        let stale_root = reserve_stale_lane_root_path(lane_root)?;
        fs::rename(lane_root, &stale_root).with_context(|| {
            format!(
                "failed to move stale lane root {} aside",
                lane_root.display()
            )
        })?;
        if let Err(err) = fs::remove_dir_all(&stale_root) {
            eprintln!(
                "warning: failed removing stale lane root {} after reset: {err}",
                stale_root.display()
            );
        }
    }
    fs::create_dir_all(lane_root)
        .with_context(|| format!("failed to create {}", lane_root.display()))?;
    Ok(())
}

pub(crate) fn reserve_stale_lane_root_path(lane_root: &Path) -> Result<PathBuf> {
    let parent = lane_root
        .parent()
        .with_context(|| format!("lane root {} had no parent", lane_root.display()))?;
    let stem = lane_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .with_context(|| format!("lane root {} had no file name", lane_root.display()))?;
    for attempt in 0..100usize {
        let candidate = if attempt == 0 {
            format!("{stem}.stale-{}", timestamp_slug())
        } else {
            format!("{stem}.stale-{}-{attempt}", timestamp_slug())
        };
        let path = parent.join(candidate);
        if !path.exists() {
            return Ok(path);
        }
    }
    bail!(
        "failed reserving stale lane root path near {}",
        lane_root.display()
    );
}

pub(crate) fn prepare_parallel_lane_assignment_with_fallback(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_config: &LaneRunConfig,
    lane_index: usize,
    task: LoopTask,
    resume_candidate: Option<LaneResumeCandidate>,
) -> Result<ActiveLaneAssignment> {
    let resumable_snapshot = resume_candidate.clone();
    match prepare_parallel_lane_assignment(
        repo_root,
        run_root,
        target_branch,
        lane_config,
        lane_index,
        task.clone(),
        resume_candidate,
    ) {
        Ok(assignment) => Ok(assignment),
        Err(err) => {
            let Some(candidate) = resumable_snapshot else {
                return Err(err);
            };
            eprintln!(
                "warning: failed resuming lane-{} `{}`; retrying with a fresh clone: {err:#}",
                candidate.lane_index, task.id
            );
            prepare_parallel_lane_assignment(
                repo_root,
                run_root,
                target_branch,
                lane_config,
                lane_index,
                task,
                None,
            )
        }
    }
}

pub(crate) fn discover_resume_candidates(
    repo_root: &Path,
    run_root: &Path,
    target_branch: &str,
    lane_config: &LaneRunConfig,
    plan: &LoopPlanSnapshot,
    parallel_logger: &ParallelEventLogger,
) -> Result<BTreeMap<usize, LaneResumeCandidate>> {
    let lanes_root = run_root.join("lanes");
    if !lanes_root.exists() {
        return Ok(BTreeMap::new());
    }

    let pending_tasks = plan
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                LoopTaskStatus::Pending | LoopTaskStatus::Partial
            )
        })
        .map(|task| (task.id.clone(), task.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = BTreeMap::new();

    for entry in fs::read_dir(&lanes_root)
        .with_context(|| format!("failed to read {}", lanes_root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", lanes_root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if !file_type.is_dir() {
            continue;
        }

        let lane_root = entry.path();
        let lane_name = entry.file_name();
        let Some(lane_index) = parse_lane_index(&lane_name.to_string_lossy()) else {
            continue;
        };
        let lane_repo_root = lane_root.join("repo");
        if !lane_repo_root.join(".git").exists() {
            continue;
        }

        let Some(task_id) = read_lane_task_id(&lane_root)? else {
            continue;
        };
        let Some(task) = pending_tasks.get(&task_id).cloned() else {
            continue;
        };
        let base_commit = match infer_lane_base_commit(&lane_repo_root, target_branch) {
            Ok(base_commit) => base_commit,
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because its base commit could not be inferred: {err:#}",
                    lane_index
                );
                continue;
            }
        };
        if let Err(err) = validate_lane_assignment_metadata(
            &lane_root,
            target_branch,
            &base_commit,
            &lane_config.assignment_worker_metadata(),
            &task,
        ) {
            eprintln!(
                "warning: skipping resumable lane-{} `{}` because assignment metadata is stale or missing: {err:#}",
                lane_index, task_id
            );
            continue;
        }

        let stdout_log_path = lane_root.join("stdout.log");
        let stderr_log_path = lane_root.join("stderr.log");
        let worker_pid_path = lane_root.join("worker.pid");
        if let Err(err) = clear_stale_worker_pid(&worker_pid_path) {
            eprintln!(
                "warning: skipping resumable lane-{} because its worker pid file could not be cleaned up: {err:#}",
                lane_index
            );
            continue;
        }
        match read_worker_pid(&worker_pid_path) {
            Ok(Some(pid)) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because identity-verified worker pid {} is still alive in {}",
                    lane_index,
                    pid,
                    lane_root.display()
                );
                continue;
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because its worker pid file is unreadable: {err:#}",
                    lane_index
                );
                continue;
            }
        }

        match retire_superseded_lane_cherry_pick_recovery(repo_root, &lane_repo_root, &task_id) {
            Ok(Some(superseded)) => {
                parallel_logger.info(format!(
                    "recovery-retire: lane-{} `{}` had stale duplicate landing recovery; {}",
                    lane_index,
                    task_id,
                    superseded.summary()
                ));
                continue;
            }
            Ok(None) => {}
            Err(err) => {
                parallel_logger.warn(format!(
                    "warning: lane-{} `{}` stale-recovery retirement check failed; keeping lane resumable: {err:#}",
                    lane_index, task_id
                ));
            }
        }

        let progress = match inspect_lane_repo_progress(&lane_repo_root, &base_commit) {
            Ok(progress) => progress,
            Err(err) => {
                eprintln!(
                    "warning: skipping resumable lane-{} because repo progress inspection failed: {err:#}",
                    lane_index
                );
                continue;
            }
        };
        let mut host_recovery_note = match &progress {
            LaneRepoProgress::None
                if resume_lane_progress_is_harvestable(
                    repo_root,
                    &lane_repo_root,
                    &task_id,
                    &progress,
                ) => Some(format!(
                    "host restart found a clean lane receipt for `{task_id}`; reconcile the existing proof before dispatching duplicate work"
                )),
            LaneRepoProgress::None => continue,
            LaneRepoProgress::Dirty(status) | LaneRepoProgress::NewCommitsWithDirty(status) => {
                Some(lane_repo_recovery_note(
                    &lane_repo_root,
                    target_branch,
                    status,
                ))
            }
            LaneRepoProgress::NewCommits => None,
        };
        if host_recovery_note.is_none() {
            host_recovery_note =
                salvage_recovery_note(&lane_root, lane_index, &task_id, target_branch);
        }

        candidates.insert(
            lane_index,
            LaneResumeCandidate {
                lane_index,
                task,
                lane_root,
                lane_repo_root,
                base_commit,
                stdout_log_path,
                stderr_log_path,
                worker_pid_path,
                host_recovery_note,
            },
        );
    }

    Ok(candidates)
}

fn resume_lane_progress_is_harvestable(
    repo_root: &Path,
    lane_repo_root: &Path,
    task_id: &str,
    progress: &LaneRepoProgress,
) -> bool {
    matches!(progress, LaneRepoProgress::NewCommits)
        || (matches!(progress, LaneRepoProgress::None)
            && [repo_root, lane_repo_root].iter().any(|root| {
                root.join(".auto/symphony/verification-receipts")
                    .join(format!("{task_id}.json"))
                    .is_file()
            }))
}

pub(crate) fn live_resume_workers(run_root: &Path) -> Result<Vec<(usize, String, u32)>> {
    let lanes_root = run_root.join("lanes");
    if !lanes_root.exists() {
        return Ok(Vec::new());
    }

    let mut live = Vec::new();
    for entry in fs::read_dir(&lanes_root)
        .with_context(|| format!("failed to read {}", lanes_root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", lanes_root.display()))?;
        if !entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?
            .is_dir()
        {
            continue;
        }
        let lane_root = entry.path();
        let Some(lane_index) = entry.file_name().to_str().and_then(parse_lane_index) else {
            continue;
        };
        let Some(pid) = read_worker_pid(&lane_root.join("worker.pid"))? else {
            continue;
        };
        let task_id = read_lane_task_id(&lane_root)?.unwrap_or_else(|| "unknown".to_string());
        live.push((lane_index, task_id, pid));
    }
    live.sort();
    Ok(live)
}

pub(crate) async fn wait_for_live_resume_workers(
    run_root: &Path,
    parallel_logger: &ParallelEventLogger,
) -> Result<()> {
    let mut last_summary = None;
    loop {
        let live = live_resume_workers(run_root)?;
        if live.is_empty() {
            if last_summary.is_some() {
                parallel_logger.info(
                    "resume-wait: prior-host workers exited; harvesting their lanes before dispatch",
                );
            }
            return Ok(());
        }
        let summary = live
            .iter()
            .map(|(lane, task, pid)| format!("lane-{lane} `{task}` pid {pid}"))
            .collect::<Vec<_>>()
            .join(", ");
        if last_summary.as_deref() != Some(summary.as_str()) {
            parallel_logger.info(format!(
                "resume-wait: preserving {} live prior-host worker(s) before duplicate dispatch: {summary}",
                live.len()
            ));
            last_summary = Some(summary);
        }
        tokio::time::sleep(LANE_POLL_INTERVAL).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn harvest_resumable_lane_results(
    repo_root: &Path,
    target_branch: &str,
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
    attempted_partial_followups: &mut BTreeMap<String, usize>,
    deferred_partial_tasks: &mut BTreeSet<String>,
    linear_tracker: &mut Option<LinearTracker>,
    parallel_logger: &ParallelEventLogger,
    review_config: &LaneReviewConfig,
) -> Result<usize> {
    let mut landed = 0usize;
    let lane_indexes = resumable_lanes.keys().copied().collect::<Vec<_>>();
    for lane_index in lane_indexes {
        let should_land = match resumable_lanes.get(&lane_index) {
            Some(candidate) => {
                match inspect_lane_repo_progress(&candidate.lane_repo_root, &candidate.base_commit)
                {
                    Ok(progress) => resume_lane_progress_is_harvestable(
                        repo_root,
                        &candidate.lane_repo_root,
                        &candidate.task.id,
                        &progress,
                    ),
                    Err(err) => {
                        eprintln!(
                            "warning: skipping resumable lane-{} because repo progress inspection failed: {err:#}",
                            lane_index
                        );
                        false
                    }
                }
            }
            None => false,
        };
        if !should_land {
            continue;
        }
        let Some(candidate) = resumable_lanes.remove(&lane_index) else {
            continue;
        };
        let mut assignment = ActiveLaneAssignment {
            lane_index: candidate.lane_index,
            attempts: 0,
            task: candidate.task,
            resumed: true,
            lane_root: candidate.lane_root,
            lane_repo_root: candidate.lane_repo_root,
            base_commit: candidate.base_commit,
            stdout_log_path: candidate.stdout_log_path,
            stderr_log_path: candidate.stderr_log_path,
            worker_pid_path: candidate.worker_pid_path,
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: candidate.host_recovery_note,
        };

        let clean_no_commit = matches!(
            inspect_lane_repo_progress(&assignment.lane_repo_root, &assignment.base_commit),
            Ok(LaneRepoProgress::None)
        );
        if clean_no_commit {
            match reconcile_parallel_clean_no_commit(
                repo_root,
                target_branch,
                &mut assignment,
                parallel_logger,
                review_config,
            )
            .await
            {
                Ok(true) => {
                    if push_parallel_clean_no_commit_closeout(
                        repo_root,
                        target_branch,
                        &assignment,
                    )? {
                        parallel_logger.info(format!(
                            "remote sync: rebased onto origin/{target_branch} after resumed clean-no-commit closeout"
                        ));
                    }
                    if let Some(tracker) = linear_tracker.as_mut() {
                        if let Err(err) = tracker.note_done(&assignment.task.id).await {
                            eprintln!(
                                "warning: failed to archive `{}` in Linear: {err:#}",
                                assignment.task.id
                            );
                        }
                    }
                    landed += 1;
                    attempted_partial_followups.remove(&assignment.task.id);
                    deferred_partial_tasks.remove(&assignment.task.id);
                    parallel_logger.info(format!(
                        "resumed:     reconciled {} from clean lane-{} receipt before duplicate dispatch (total landed: {})",
                        assignment.task.id, assignment.lane_index, landed
                    ));
                    continue;
                }
                Ok(false) => {
                    parallel_logger.warn(format!(
                        "warning: resumed clean lane-{} `{}` receipt did not satisfy current-tree gates; keeping lane resumable",
                        assignment.lane_index, assignment.task.id
                    ));
                }
                Err(error) => {
                    parallel_logger.warn(format!(
                        "warning: resumed clean lane-{} `{}` reconciliation failed; keeping lane resumable: {error:#}",
                        assignment.lane_index, assignment.task.id
                    ));
                }
            }
            resumable_lanes.insert(
                lane_index,
                LaneResumeCandidate {
                    lane_index: assignment.lane_index,
                    task: assignment.task,
                    lane_root: assignment.lane_root,
                    lane_repo_root: assignment.lane_repo_root,
                    base_commit: assignment.base_commit,
                    stdout_log_path: assignment.stdout_log_path,
                    stderr_log_path: assignment.stderr_log_path,
                    worker_pid_path: assignment.worker_pid_path,
                    host_recovery_note: Some(
                        "clean-lane receipt did not pass resumed reconciliation; rerun only the missing current-tree gates"
                            .to_string(),
                    ),
                },
            );
            continue;
        }

        match land_parallel_lane_result(repo_root, target_branch, &mut assignment, review_config)
            .await
        {
            Ok(LaneLandingOutcome::Landed {
                auto_repaired,
                completion_status,
            }) => {
                if completion_status == LoopTaskStatus::Done {
                    if let Some(tracker) = linear_tracker.as_mut() {
                        if let Err(err) = tracker.note_done(&assignment.task.id).await {
                            eprintln!(
                                "warning: failed to archive `{}` in Linear: {err:#}",
                                assignment.task.id
                            );
                        }
                    }
                }
                landed += 1;
                let status_suffix = completion_status_suffix(
                    &assignment.task.id,
                    completion_status,
                    attempted_partial_followups,
                    deferred_partial_tasks,
                );
                parallel_logger.info(format!(
                    "resumed:     landed {} from lane-{} before dispatch{}{} (total landed: {})",
                    assignment.task.id,
                    assignment.lane_index,
                    if auto_repaired {
                        " after host auto-repair"
                    } else {
                        ""
                    },
                    status_suffix,
                    landed
                ));
            }
            Ok(LaneLandingOutcome::NeedsRecovery {
                recovery_note,
                conflict_paths,
            }) => {
                parallel_logger.warn(format!(
                    "warning: resume harvest for lane-{} `{}` prepared a landing-recovery attempt instead of landing; keeping lane resumable; conflict paths: {}",
                    assignment.lane_index,
                    assignment.task.id,
                    if conflict_paths.is_empty() {
                        "unknown".to_string()
                    } else {
                        conflict_paths.join(", ")
                    }
                ));
                resumable_lanes.insert(
                    lane_index,
                    LaneResumeCandidate {
                        lane_index: assignment.lane_index,
                        task: assignment.task,
                        lane_root: assignment.lane_root,
                        lane_repo_root: assignment.lane_repo_root,
                        base_commit: assignment.base_commit,
                        stdout_log_path: assignment.stdout_log_path,
                        stderr_log_path: assignment.stderr_log_path,
                        worker_pid_path: assignment.worker_pid_path,
                        host_recovery_note: Some(recovery_note),
                    },
                );
            }
            Ok(LaneLandingOutcome::DivergenceExhausted { detail }) => {
                parallel_logger.warn(format!(
                    "warning: resume harvest for lane-{} `{}` exhausted bounded landing-divergence retries; keeping lane resumable: {}",
                    assignment.lane_index, assignment.task.id, detail
                ));
                resumable_lanes.insert(
                    lane_index,
                    LaneResumeCandidate {
                        lane_index: assignment.lane_index,
                        task: assignment.task,
                        lane_root: assignment.lane_root,
                        lane_repo_root: assignment.lane_repo_root,
                        base_commit: assignment.base_commit,
                        stdout_log_path: assignment.stdout_log_path,
                        stderr_log_path: assignment.stderr_log_path,
                        worker_pid_path: assignment.worker_pid_path,
                        host_recovery_note: Some(landing_recovery_note(target_branch, &detail)),
                    },
                );
            }
            Err(error) => {
                parallel_logger.warn(format!(
                    "warning: resume harvest for lane-{} `{}` failed; keeping lane resumable instead: {error:#}",
                    assignment.lane_index, assignment.task.id
                ));
                resumable_lanes.insert(
                    lane_index,
                    LaneResumeCandidate {
                        lane_index: assignment.lane_index,
                        task: assignment.task,
                        lane_root: assignment.lane_root,
                        lane_repo_root: assignment.lane_repo_root,
                        base_commit: assignment.base_commit,
                        stdout_log_path: assignment.stdout_log_path,
                        stderr_log_path: assignment.stderr_log_path,
                        worker_pid_path: assignment.worker_pid_path,
                        host_recovery_note: Some(landing_recovery_note(
                            target_branch,
                            &format!("{error:#}"),
                        )),
                    },
                );
            }
        }
    }
    Ok(landed)
}

pub(crate) fn take_resume_candidate_for_task(
    resumable_lanes: &mut BTreeMap<usize, LaneResumeCandidate>,
    task_id: &str,
    active_lanes: &BTreeMap<usize, ActiveLaneAssignment>,
) -> Option<(usize, LaneResumeCandidate)> {
    let lane_index = resumable_lanes
        .iter()
        .find(|(lane_index, candidate)| {
            !active_lanes.contains_key(lane_index) && candidate.task.id == task_id
        })
        .map(|(lane_index, _)| *lane_index)?;
    let candidate = resumable_lanes.remove(&lane_index)?;
    Some((lane_index, candidate))
}

pub(crate) fn refresh_assignment_task_from_plan(
    plan: &LoopPlanSnapshot,
    assignment: &mut ActiveLaneAssignment,
) {
    if let Some(task) = plan
        .tasks
        .iter()
        .find(|task| task.id == assignment.task.id)
        .cloned()
    {
        assignment.task = task;
    }
}

pub(crate) fn parse_lane_index(name: &str) -> Option<usize> {
    name.strip_prefix("lane-")?.parse::<usize>().ok()
}

pub(crate) fn write_lane_task_id(lane_root: &Path, task_id: &str) -> Result<()> {
    stamp_lane_run_id(lane_root);
    atomic_write(&lane_root.join(LANE_TASK_ID_FILE), task_id.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            lane_root.join(LANE_TASK_ID_FILE).display()
        )
    })
}

pub(crate) fn write_lane_assignment_metadata(
    lane_root: &Path,
    target_branch: &str,
    base_commit: &str,
    task: &LoopTask,
    worker: &LaneWorkerMetadata,
) -> Result<()> {
    let task_hash = hash_stable(&task.markdown);
    let dependency_hash = hash_stable(&task.dependencies);
    let verification_hash = hash_stable(&task_field_body(
        &task.markdown,
        "Verification:",
        "Required tests:",
    ));
    let metadata = LaneAssignmentMetadata {
        task_id: task.id.clone(),
        target_branch: target_branch.to_string(),
        base_commit: base_commit.to_string(),
        task_hash,
        dependency_hash,
        verification_hash,
        worker: worker.clone(),
        assignment_hash: lane_assignment_hash(
            &task.id,
            target_branch,
            base_commit,
            task_hash,
            dependency_hash,
            verification_hash,
            worker,
        ),
    };
    let json = serde_json::to_vec_pretty(&metadata)?;
    atomic_write(&lane_root.join(LANE_ASSIGNMENT_FILE), &json).with_context(|| {
        format!(
            "failed to write {}",
            lane_root.join(LANE_ASSIGNMENT_FILE).display()
        )
    })
}

pub(crate) fn validate_lane_assignment_metadata(
    lane_root: &Path,
    target_branch: &str,
    base_commit: &str,
    worker: &LaneWorkerMetadata,
    task: &LoopTask,
) -> Result<LaneAssignmentMetadata> {
    let metadata_path = lane_root.join(LANE_ASSIGNMENT_FILE);
    let text = fs::read_to_string(&metadata_path)
        .with_context(|| format!("failed to read {}", metadata_path.display()))?;
    let metadata: LaneAssignmentMetadata = serde_json::from_str(&text)
        .with_context(|| format!("invalid {}", metadata_path.display()))?;
    if metadata.task_id != task.id {
        bail!(
            "task id changed from `{}` to `{}`",
            metadata.task_id,
            task.id
        );
    }
    if metadata.target_branch != target_branch {
        bail!(
            "target branch changed from `{}` to `{target_branch}`",
            metadata.target_branch
        );
    }
    if metadata.base_commit != base_commit {
        bail!(
            "base commit changed from `{}` to `{base_commit}`",
            metadata.base_commit
        );
    }
    if metadata.worker.model != worker.model {
        bail!(
            "worker model changed from `{}` to `{}`",
            metadata.worker.model,
            worker.model
        );
    }
    if metadata.worker.command != worker.command {
        bail!("worker command changed");
    }
    if metadata.worker.reasoning_effort != worker.reasoning_effort {
        bail!(
            "worker reasoning effort changed from `{}` to `{}`",
            metadata.worker.reasoning_effort,
            worker.reasoning_effort
        );
    }
    if metadata.worker.max_turns != worker.max_turns {
        bail!(
            "worker max turns changed from `{:?}` to `{:?}`",
            metadata.worker.max_turns,
            worker.max_turns
        );
    }
    if metadata.verification_hash
        != hash_stable(&task_field_body(
            &task.markdown,
            "Verification:",
            "Required tests:",
        ))
    {
        bail!("verification text hash changed");
    }
    if metadata.task_hash != hash_stable(&task.markdown) {
        bail!("task body hash changed");
    }
    if metadata.dependency_hash != hash_stable(&task.dependencies) {
        bail!("dependency hash changed");
    }
    let expected_assignment_hash = lane_assignment_hash(
        &task.id,
        target_branch,
        &metadata.base_commit,
        metadata.task_hash,
        metadata.dependency_hash,
        metadata.verification_hash,
        worker,
    );
    if metadata.assignment_hash != expected_assignment_hash {
        bail!("assignment hash changed");
    }
    Ok(metadata)
}

pub(crate) fn hash_stable<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn lane_assignment_hash(
    task_id: &str,
    target_branch: &str,
    base_commit: &str,
    task_hash: u64,
    dependency_hash: u64,
    verification_hash: u64,
    worker: &LaneWorkerMetadata,
) -> u64 {
    hash_stable(&(
        task_id,
        target_branch,
        base_commit,
        task_hash,
        dependency_hash,
        verification_hash,
        worker,
    ))
}

pub(crate) fn read_lane_task_id(lane_root: &Path) -> Result<Option<String>> {
    let task_id_path = lane_root.join(LANE_TASK_ID_FILE);
    if task_id_path.exists() {
        let task_id = fs::read_to_string(&task_id_path)
            .with_context(|| format!("failed to read {}", task_id_path.display()))?;
        let task_id = task_id.trim();
        if !task_id.is_empty() {
            return Ok(Some(task_id.to_string()));
        }
    }

    let mut latest_prompt: Option<(std::time::SystemTime, String)> = None;
    for entry in fs::read_dir(lane_root)
        .with_context(|| format!("failed to read {}", lane_root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", lane_root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some(task_id) = task_id_from_prompt_filename(&file_name) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        match &latest_prompt {
            Some((latest_modified, _)) if &modified <= latest_modified => {}
            _ => latest_prompt = Some((modified, task_id)),
        }
    }

    Ok(latest_prompt.map(|(_, task_id)| task_id))
}

pub(crate) fn lane_status_task_id(
    stored_task_id: &str,
    worker_running: bool,
    log_line: Option<&str>,
) -> String {
    if worker_running {
        return stored_task_id.to_string();
    }
    if log_line
        .map(str::trim)
        .is_some_and(|line| line.contains("] idle:"))
    {
        return "[idle]".to_string();
    }
    stored_task_id.to_string()
}

pub(crate) fn lane_worker_status(
    lane_root: &Path,
    lane_repo_root: &Path,
) -> Result<(bool, String)> {
    let pid_path = lane_root.join("worker.pid");
    let pid_state = match read_worker_pid(&pid_path) {
        Ok(Some(pid)) => return Ok((true, format!("running verified pid {pid}"))),
        Ok(None) => None,
        Err(err) => Some(format!("worker pid unreadable: {err:#}")),
    };

    let descendant_pids = lane_repo_process_pids(lane_repo_root)?;
    if !descendant_pids.is_empty() {
        return Ok((
            true,
            format!(
                "running descendant pid(s) {}{}",
                descendant_pids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                pid_state
                    .map(|state| format!(" ({state})"))
                    .unwrap_or_default()
            ),
        ));
    }

    Ok((
        false,
        pid_state.unwrap_or_else(|| "no worker pid".to_string()),
    ))
}

pub(crate) fn task_id_from_prompt_filename(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix("-prompt.md")?;
    let (task_id, attempt) = stem.rsplit_once("-attempt-")?;
    if attempt.parse::<usize>().is_err() || task_id.is_empty() {
        return None;
    }
    Some(task_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        parse_lane_cache_max_bytes, parse_lane_cargo_incremental_enabled,
        resume_lane_progress_is_harvestable, DEFAULT_LANE_CACHE_MAX_GIB, GIB,
    };
    use crate::parallel_command::*;
    use std::time::UNIX_EPOCH;

    fn effort_config(ceiling: &str) -> LaneRunConfig {
        LaneRunConfig {
            claude: false,
            max_turns: None,
            model: "gpt-5.6-sol".to_string(),
            reasoning_effort: ceiling.to_string(),
            codex_bin: std::path::PathBuf::from("codex"),
            extra_env: Vec::new(),
            lane_local_cargo_target: false,
            cargo_target_prompt_clause: String::new(),
            preflight_prompt_clause: String::new(),
        }
    }

    // All scope-effort assertions live in one test: the feature is gated by a
    // process-global env var, and splitting across parallel tests would race on
    // it. Sequential phases here set the var explicitly before each block.
    #[test]
    fn scope_effort_routing_behaviour() {
        // Routing ON (explicit): S/M route down under a high ceiling.
        std::env::set_var("AUTO_PARALLEL_SCOPE_EFFORT", "1");
        let high = effort_config("xhigh");
        assert_eq!(high.effective_reasoning_effort(Some("S"), 1), "medium");
        assert_eq!(high.effective_reasoning_effort(Some("small"), 1), "medium");
        assert_eq!(high.effective_reasoning_effort(Some("M"), 1), "high");
        // Large / unknown fall back to the operator ceiling.
        assert_eq!(high.effective_reasoning_effort(Some("L"), 1), "xhigh");
        assert_eq!(high.effective_reasoning_effort(None, 1), "xhigh");

        // Ceiling caps routing: an M task capped at a medium ceiling.
        let medium = effort_config("medium");
        assert_eq!(medium.effective_reasoning_effort(Some("M"), 1), "medium");
        assert_eq!(medium.effective_reasoning_effort(Some("S"), 1), "medium");

        // Retry escalates to the ceiling regardless of scope.
        assert_eq!(high.effective_reasoning_effort(Some("S"), 2), "xhigh");

        // Routing OFF: always the ceiling.
        std::env::set_var("AUTO_PARALLEL_SCOPE_EFFORT", "0");
        assert_eq!(high.effective_reasoning_effort(Some("S"), 1), "xhigh");

        std::env::remove_var("AUTO_PARALLEL_SCOPE_EFFORT");
    }

    #[test]
    fn lane_local_cargo_target_persists_outside_disposable_worktree() {
        // AUTO_LANE_PERSISTENT_TARGET is process-global; drive all phases
        // sequentially in one test to avoid racing on it.
        let lane_root = PathBuf::from("/srv/run/lanes/lane-3");
        let mut lane_local = effort_config("high");
        lane_local.lane_local_cargo_target = true;

        // Default (persistence on): the target lives under
        // <run_root>/lane-caches/lane-3, a sibling of lanes/, so it survives the
        // per-assignment worktree wipe and incremental compilation is reused.
        std::env::set_var("AUTO_LANE_PERSISTENT_TARGET", "1");
        let target = lane_local
            .env_for_lane(&lane_root)
            .into_iter()
            .find(|(key, _)| key == "CARGO_TARGET_DIR")
            .map(|(_, value)| value)
            .expect("lane-local run must set CARGO_TARGET_DIR");
        assert_eq!(target, "/srv/run/lane-caches/lane-3/cargo-target");

        // Disabled: legacy in-worktree target, deleted with every fresh clone.
        std::env::set_var("AUTO_LANE_PERSISTENT_TARGET", "0");
        let target = lane_local
            .env_for_lane(&lane_root)
            .into_iter()
            .find(|(key, _)| key == "CARGO_TARGET_DIR")
            .map(|(_, value)| value)
            .expect("lane-local run must still set CARGO_TARGET_DIR when disabled");
        assert_eq!(target, "/srv/run/lanes/lane-3/cargo-target");
        std::env::remove_var("AUTO_LANE_PERSISTENT_TARGET");

        // A non-lane-local config never sets CARGO_TARGET_DIR: Fixed/None layouts
        // already carry their own target in extra_env and must not be clobbered.
        let shared = effort_config("high"); // lane_local_cargo_target: false
        assert!(shared
            .env_for_lane(&lane_root)
            .iter()
            .all(|(key, _)| key != "CARGO_TARGET_DIR"));
    }

    #[test]
    fn lane_environment_assigns_a_distinct_cargo_serialization_lease() {
        let mut config = effort_config("high");
        config.extra_env.push((
            WORKER_CARGO_LEASE_ENV.to_string(),
            "/srv/run/validation-resource.lock".to_string(),
        ));
        let lane_one = PathBuf::from("/srv/run/lanes/lane-1");
        let lane_two = PathBuf::from("/srv/run/lanes/lane-2");
        let lease_for = |lane_root: &Path| {
            config
                .env_for_lane(lane_root)
                .into_iter()
                .find(|(key, _)| key == WORKER_LANE_CARGO_LEASE_ENV)
                .map(|(_, value)| value)
                .expect("Cargo-enabled lane must receive a serialization lease")
        };
        assert_eq!(
            lease_for(&lane_one),
            "/srv/run/lanes/lane-1/cargo-resource.lock"
        );
        assert_eq!(
            lease_for(&lane_two),
            "/srv/run/lanes/lane-2/cargo-resource.lock"
        );
    }

    fn sample_worker_metadata() -> LaneWorkerMetadata {
        LaneWorkerMetadata {
            harness: "codex".to_string(),
            command: vec![
                "codex".to_string(),
                "exec".to_string(),
                "--json".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--skip-git-repo-check".to_string(),
                "-m".to_string(),
                "gpt-5.6-sol".to_string(),
                "-c".to_string(),
                "model_reasoning_effort=\"high\"".to_string(),
            ],
            model: "gpt-5.6-sol".to_string(),
            reasoning_effort: "high".to_string(),
            max_turns: None,
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    #[test]
    fn lane_status_task_id_reports_idle_when_latest_log_is_idle() {
        assert_eq!(
            lane_status_task_id(
                "OLD-TASK",
                false,
                Some("[auto parallel host lane-5 [idle]] idle: waiting on dependencies"),
            ),
            "[idle]"
        );
        assert_eq!(
            lane_status_task_id("OLD-TASK", true, Some("anything")),
            "OLD-TASK"
        );
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_task_body() {
        let lane_root = unique_temp_dir("lane-assignment-body");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed = task.clone();
        changed.markdown.push_str("Extra body\n");
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &worker, &changed)
                .expect_err("changed body rejected");
        assert!(format!("{err:#}").contains("task body hash changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_dependencies() {
        let lane_root = unique_temp_dir("lane-assignment-deps");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec!["TASK-000".to_string()],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: `TASK-000`\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed = task.clone();
        changed.dependencies = vec![];
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &worker, &changed)
                .expect_err("changed dependencies rejected");
        assert!(format!("{err:#}").contains("dependency hash changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_verification_text() {
        let lane_root = unique_temp_dir("lane-assignment-verification");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed = task.clone();
        changed.markdown = changed
            .markdown
            .replace("cargo test task_one", "cargo test task_two");
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &worker, &changed)
                .expect_err("changed verification rejected");
        assert!(format!("{err:#}").contains("verification text hash changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_base_commit() {
        let lane_root = unique_temp_dir("lane-assignment-base-commit");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let err = validate_lane_assignment_metadata(&lane_root, "main", "def456", &worker, &task)
            .expect_err("changed base commit rejected");
        assert!(format!("{err:#}").contains("base commit changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_worker_model() {
        let lane_root = unique_temp_dir("lane-assignment-worker-model");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed_worker = worker.clone();
        changed_worker.model = "gpt-6".to_string();
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &changed_worker, &task)
                .expect_err("changed worker model rejected");
        assert!(format!("{err:#}").contains("worker model changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_worker_command() {
        let lane_root = unique_temp_dir("lane-assignment-worker-command");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        let worker = sample_worker_metadata();
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task, &worker)
            .expect("metadata should write");

        let mut changed_worker = worker.clone();
        changed_worker.command.push("--new-worker-flag".to_string());
        let err =
            validate_lane_assignment_metadata(&lane_root, "main", "abc123", &changed_worker, &task)
                .expect_err("changed worker command rejected");
        assert!(format!("{err:#}").contains("worker command changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn prompt_filename_task_id_round_trips() {
        assert_eq!(
            task_id_from_prompt_filename("P-029C-attempt-03-prompt.md"),
            Some("P-029C".to_string())
        );
        assert_eq!(
            task_id_from_prompt_filename("WEB-CRAPS-D-attempt-1-prompt.md"),
            Some("WEB-CRAPS-D".to_string())
        );
        assert_eq!(task_id_from_prompt_filename("stderr.log"), None);
    }

    #[test]
    fn lane_task_id_prefers_metadata_and_falls_back_to_latest_prompt() {
        let lane_root = unique_temp_dir("parallel-lane-task-id");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        fs::write(lane_root.join("P-018B-attempt-01-prompt.md"), "")
            .expect("failed to write prompt");
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(lane_root.join("P-021-attempt-02-prompt.md"), "")
            .expect("failed to write prompt");

        assert_eq!(
            read_lane_task_id(&lane_root).expect("lane task id should read"),
            Some("P-021".to_string())
        );

        fs::write(lane_root.join(super::LANE_TASK_ID_FILE), "P-029C\n")
            .expect("failed to write metadata");
        assert_eq!(
            read_lane_task_id(&lane_root).expect("lane task id should read"),
            Some("P-029C".to_string())
        );

        fs::remove_dir_all(&lane_root).expect("failed to remove lane root");
    }

    #[test]
    fn reset_parallel_lane_root_rehomes_existing_contents() {
        let lane_root = unique_temp_dir("parallel-lane-reset");
        fs::create_dir_all(lane_root.join("repo")).expect("failed to create lane repo");
        fs::write(lane_root.join("repo").join("stale.txt"), "stale")
            .expect("failed to write stale file");

        reset_parallel_lane_root(&lane_root).expect("lane root should reset");

        assert!(lane_root.exists(), "lane root should exist after reset");
        assert!(
            fs::read_dir(&lane_root)
                .expect("lane root should be readable")
                .next()
                .is_none(),
            "lane root should be recreated empty"
        );

        let parent = lane_root.parent().expect("lane root should have parent");
        let prefix = format!(
            "{}.stale-",
            lane_root
                .file_name()
                .expect("lane root should have file name")
                .to_string_lossy()
        );
        let stale_dirs = fs::read_dir(parent)
            .expect("parent should be readable")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().starts_with(&prefix))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        assert!(
            stale_dirs.is_empty(),
            "stale lane roots should be pruned after reset"
        );

        fs::remove_dir_all(&lane_root).expect("failed to remove lane root");
    }

    fn write_blob(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().expect("blob parent")).expect("create blob parent");
        fs::write(path, vec![0u8; bytes]).expect("write blob");
    }

    #[test]
    fn lane_cache_max_bytes_parses_default_disable_and_override() {
        // Empty / unset / garbage -> safe default (never silently unbounded).
        assert_eq!(
            parse_lane_cache_max_bytes(None),
            Some(DEFAULT_LANE_CACHE_MAX_GIB * GIB)
        );
        assert_eq!(
            parse_lane_cache_max_bytes(Some("   ")),
            Some(DEFAULT_LANE_CACHE_MAX_GIB * GIB)
        );
        assert_eq!(
            parse_lane_cache_max_bytes(Some("not-a-number")),
            Some(DEFAULT_LANE_CACHE_MAX_GIB * GIB)
        );
        // Explicit 0 disables the cap (opt back into unbounded behavior).
        assert_eq!(parse_lane_cache_max_bytes(Some("0")), None);
        // A concrete GiB value.
        assert_eq!(parse_lane_cache_max_bytes(Some("10")), Some(10 * GIB));
        assert_eq!(parse_lane_cache_max_bytes(Some(" 3 ")), Some(3 * GIB));
    }

    #[test]
    fn lane_cargo_incremental_disabled_by_default() {
        // Default (unset) disables incremental; only an explicit "1" restores it.
        assert!(!parse_lane_cargo_incremental_enabled(None));
        assert!(!parse_lane_cargo_incremental_enabled(Some("0")));
        assert!(!parse_lane_cargo_incremental_enabled(Some("")));
        assert!(parse_lane_cargo_incremental_enabled(Some("1")));
        assert!(parse_lane_cargo_incremental_enabled(Some(" 1 ")));
    }

    #[test]
    fn prune_lane_cache_keeps_warm_cache_under_cap() {
        let target = unique_temp_dir("lane-cache-under");
        write_blob(&target.join("debug").join("deps").join("libfoo.rlib"), 4096);
        write_blob(
            &target.join("debug").join("incremental").join("state.bin"),
            4096,
        );

        // 1 GiB cap, ~8 KiB of content -> nothing pruned.
        assert_eq!(
            prune_lane_cache_over_cap(&target, GIB),
            LaneCachePrune::UnderCap
        );
        assert!(target.join("debug").join("incremental").exists());
        assert!(target
            .join("debug")
            .join("deps")
            .join("libfoo.rlib")
            .exists());

        fs::remove_dir_all(&target).expect("cleanup");
    }

    #[test]
    fn prune_lane_cache_drops_incremental_first_and_keeps_deps() {
        let target = unique_temp_dir("lane-cache-incremental");
        // deps: 4 KiB, incremental: 64 KiB. Cap between the two.
        write_blob(&target.join("debug").join("deps").join("libfoo.rlib"), 4096);
        write_blob(
            &target.join("debug").join("incremental").join("state.bin"),
            64 * 1024,
        );
        write_blob(
            &target.join("release").join("incremental").join("state.bin"),
            64 * 1024,
        );

        // Cap = 16 KiB: over cap with incremental, under cap once it's gone.
        assert_eq!(
            prune_lane_cache_over_cap(&target, 16 * 1024),
            LaneCachePrune::PrunedIncremental
        );
        assert!(
            !target.join("debug").join("incremental").exists(),
            "debug incremental should be pruned"
        );
        assert!(
            !target.join("release").join("incremental").exists(),
            "release incremental should be pruned"
        );
        assert!(
            target
                .join("debug")
                .join("deps")
                .join("libfoo.rlib")
                .exists(),
            "warm deps rlibs must survive an incremental prune"
        );

        fs::remove_dir_all(&target).expect("cleanup");
    }

    #[test]
    fn prune_lane_cache_resets_whole_target_when_deps_exceed_cap() {
        let target = unique_temp_dir("lane-cache-reset");
        // deps alone (no incremental) exceeds the cap -> only the nuke bounds it.
        write_blob(
            &target.join("debug").join("deps").join("big.rlib"),
            64 * 1024,
        );

        assert_eq!(
            prune_lane_cache_over_cap(&target, 16 * 1024),
            LaneCachePrune::Reset
        );
        assert!(
            !target.exists(),
            "over-cap deps-heavy target must be fully removed"
        );
    }

    #[test]
    fn prune_orphan_lane_caches_removes_higher_indexed_lanes_only() {
        let run_root = unique_temp_dir("orphan-lane-caches");
        let caches = run_root.join("lane-caches");
        for lane in 1..=8usize {
            write_blob(
                &caches
                    .join(format!("lane-{lane}"))
                    .join("cargo-target")
                    .join("marker"),
                1024,
            );
        }
        // A non-lane dir must be ignored, not deleted.
        write_blob(&caches.join("scratch").join("keep"), 1024);

        prune_orphan_lane_caches(&run_root, 6);

        for lane in 1..=6usize {
            assert!(
                caches.join(format!("lane-{lane}")).exists(),
                "lane-{lane} within budget must survive"
            );
        }
        for lane in 7..=8usize {
            assert!(
                !caches.join(format!("lane-{lane}")).exists(),
                "orphan lane-{lane} above budget must be removed"
            );
        }
        assert!(caches.join("scratch").exists(), "non-lane dirs untouched");

        fs::remove_dir_all(&run_root).expect("cleanup");
    }

    #[test]
    fn enforce_lane_cache_size_cap_noops_when_disabled() {
        // AUTO_LANE_CACHE_MAX_GB=0 disables the cap: an over-sized cache is kept.
        // Env is process-global, so isolate the whole assertion in one test.
        let run_root = unique_temp_dir("lane-cache-disabled");
        let lane_root = run_root.join("lanes").join("lane-2");
        fs::create_dir_all(&lane_root).expect("create lane root");
        let target = run_root
            .join("lane-caches")
            .join("lane-2")
            .join("cargo-target");
        write_blob(
            &target.join("debug").join("deps").join("big.rlib"),
            64 * 1024,
        );

        // enforce reads AUTO_LANE_PERSISTENT_TARGET via lane_persistent_cargo_target_for,
        // but with the cap disabled it returns before consulting the target at all,
        // so this assertion is robust regardless of that (parallel-test-shared) env.
        std::env::set_var("AUTO_LANE_CACHE_MAX_GB", "0");
        enforce_lane_cache_size_cap(&lane_root);
        assert!(
            target.join("debug").join("deps").join("big.rlib").exists(),
            "disabled cap (=0) must leave the cache untouched"
        );
        std::env::remove_var("AUTO_LANE_CACHE_MAX_GB");

        // With a real cap below the content, the pure core bounds the same cache.
        assert_eq!(
            prune_lane_cache_over_cap(&target, 16 * 1024),
            LaneCachePrune::Reset
        );

        fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn resume_candidate_matches_requested_task() {
        let ready_tasks = [
            LoopTask {
                id: "P-019D".to_string(),
                title: "first".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: String::new(),
            },
            LoopTask {
                id: "P-021".to_string(),
                title: "second".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: String::new(),
            },
        ];
        let mut resumable = BTreeMap::new();
        resumable.insert(
            2,
            LaneResumeCandidate {
                lane_index: 2,
                task: ready_tasks[1].clone(),
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: None,
            },
        );
        resumable.insert(
            5,
            LaneResumeCandidate {
                lane_index: 5,
                task: ready_tasks[0].clone(),
                lane_root: PathBuf::from("/tmp/lane-5"),
                lane_repo_root: PathBuf::from("/tmp/lane-5/repo"),
                base_commit: "def456".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-5/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-5/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-5/worker.pid"),
                host_recovery_note: Some("recover this lane".to_string()),
            },
        );

        let matched = take_resume_candidate_for_task(
            &mut resumable,
            &ready_tasks[0].id,
            &BTreeMap::<usize, ActiveLaneAssignment>::new(),
        )
        .expect("expected a matching resumable lane");
        assert_eq!(matched.0, 5);
        assert_eq!(matched.1.task.id, "P-019D");
        assert_eq!(
            matched.1.host_recovery_note.as_deref(),
            Some("recover this lane")
        );
        assert!(resumable.contains_key(&2));
        assert!(!resumable.contains_key(&5));

        let mut rediscovered = BTreeMap::new();
        rediscovered.insert(
            2,
            LaneResumeCandidate {
                lane_index: 2,
                task: ready_tasks[1].clone(),
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: None,
            },
        );
        resumable
            .get_mut(&2)
            .expect("lane-2 should remain resumable")
            .host_recovery_note = Some("preserve this note".to_string());
        preserve_resume_recovery_notes(&mut rediscovered, &resumable);
        assert_eq!(
            rediscovered
                .get(&2)
                .and_then(|candidate| candidate.host_recovery_note.as_deref()),
            Some("preserve this note")
        );

        let mut active = BTreeMap::new();
        active.insert(
            2,
            ActiveLaneAssignment {
                lane_index: 2,
                attempts: 1,
                task: ready_tasks[1].clone(),
                resumed: true,
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                clean_commit_since: None,
                terminate_requested_at: None,
                host_recovery_note: None,
            },
        );
        assert!(
            take_resume_candidate_for_task(&mut resumable, &ready_tasks[1].id, &active).is_none()
        );
    }

    #[test]
    fn clean_no_commit_lane_with_receipt_is_harvestable_after_host_restart() {
        let canonical_repo = unique_temp_dir("parallel-clean-receipt-canonical");
        let lane_repo = unique_temp_dir("parallel-clean-receipt-lane");
        // The repository verification wrapper deliberately publishes receipts
        // from `.auto/parallel/lanes/lane-N/repo` into the canonical checkout's
        // shared `.auto/symphony` root. Host-restart discovery must inspect that
        // real location instead of assuming the ignored receipt lives inside
        // the disposable lane clone.
        let receipt = canonical_repo.join(".auto/symphony/verification-receipts/TASK-RECOVER.json");
        fs::create_dir_all(receipt.parent().expect("receipt parent"))
            .expect("create receipt directory");
        fs::write(&receipt, "{}\n").expect("write generated receipt");

        assert!(resume_lane_progress_is_harvestable(
            &canonical_repo,
            &lane_repo,
            "TASK-RECOVER",
            &LaneRepoProgress::None,
        ));
        assert!(!resume_lane_progress_is_harvestable(
            &canonical_repo,
            &lane_repo,
            "TASK-WITHOUT-RECEIPT",
            &LaneRepoProgress::None,
        ));

        fs::remove_dir_all(canonical_repo).ok();
        fs::remove_dir_all(lane_repo).ok();
    }

    #[test]
    fn live_resume_worker_scan_protects_prior_host_processes() {
        let run_root = unique_temp_dir("parallel-live-resume-worker");
        let lane_root = run_root.join("lanes/lane-3");
        fs::create_dir_all(&lane_root).expect("create lane root");
        fs::write(lane_root.join("task-id"), "TASK-LIVE\n").expect("write task id");
        fs::write(
            lane_root.join("worker.pid"),
            format!("{}\n", std::process::id()),
        )
        .expect("write live worker pid");

        let error = live_resume_workers(&run_root)
            .expect_err("a live legacy pid-only record must require explicit recovery");
        assert!(
            error.to_string().contains("legacy pid-only"),
            "unexpected legacy recovery error: {error:#}"
        );

        fs::write(lane_root.join("worker.pid"), "4294967295\n").expect("write dead worker pid");
        assert!(live_resume_workers(&run_root)
            .expect("scan dead workers")
            .is_empty());

        fs::remove_dir_all(run_root).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_resume_worker_scan_rejects_a_reused_pid_identity() {
        let run_root = unique_temp_dir("parallel-reused-worker-pid");
        let lane_root = run_root.join("lanes/lane-4");
        fs::create_dir_all(&lane_root).expect("create lane root");
        fs::write(lane_root.join("task-id"), "TASK-STALE\n").expect("write task id");
        let pid = std::process::id();
        let pid_path = lane_root.join("worker.pid");
        let guard = crate::backend_process::WorkerPidGuard::new(Some(&pid_path), Some(pid))
            .expect("publish identity-bound worker lease");
        assert_eq!(
            live_resume_workers(&run_root).expect("scan verified live worker"),
            vec![(4, "TASK-STALE".to_string(), pid)]
        );

        let lease_path = lane_root.join(fs::read_link(&pid_path).expect("read worker pid lease"));
        let mut record: crate::backend_process::WorkerPidLeaseRecord =
            serde_json::from_str(&fs::read_to_string(&lease_path).expect("read worker identity"))
                .expect("parse worker identity");
        record.linux_start_time_ticks += 1;
        fs::write(
            &lease_path,
            serde_json::to_vec(&record).expect("serialize stale identity"),
        )
        .expect("write stale worker identity");

        assert!(live_resume_workers(&run_root)
            .expect("a reused pid must be treated as stale")
            .is_empty());

        drop(guard);
        fs::remove_dir_all(run_root).ok();
    }
}
