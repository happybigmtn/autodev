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

/// Run each command through the repo's receipt wrapper when present, otherwise
/// run it directly through the shell. Wrapper absence is a compatibility path,
/// not a verification failure by itself.
async fn run_verify_commands(
    repo_root: PathBuf,
    task_id: String,
    commands: Vec<String>,
) -> LaneVerifyOutcome {
    let wrapper = repo_root.join("scripts/run-task-verification.sh");
    let wrapper_present = wrapper.is_file();
    for command in &commands {
        let result = if wrapper_present {
            let Some(argv) = shell_split(command) else {
                return LaneVerifyOutcome::Failed {
                    detail: format!(
                        "could not parse verification command `{command}` as shell argv"
                    ),
                };
            };
            tokio::process::Command::new("scripts/run-task-verification.sh")
                .arg(&task_id)
                .arg("--")
                .args(&argv)
                .current_dir(&repo_root)
                .kill_on_drop(true)
                .output()
                .await
        } else {
            tokio::process::Command::new("bash")
                .arg("-lc")
                .arg(command)
                .current_dir(&repo_root)
                .kill_on_drop(true)
                .output()
                .await
        };
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
                    detail: format!("could not run verification command `{command}`: {err}"),
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

/// STRICT-mode gate: the legacy "whole workspace must be green" behavior,
/// preserved verbatim behind `AUTO_PARALLEL_WORKSPACE_GATE_MODE=strict`. Derives
/// its coarse pass/fail from the same structured probe the baseline gate uses so
/// there is a single source of truth for how the workspace is exercised.
pub(crate) async fn run_workspace_test_gate(repo_root: &Path) -> WorkspaceTestOutcome {
    match run_workspace_probe(repo_root).await {
        WorkspaceProbe::Skipped { reason } => WorkspaceTestOutcome::Skipped { reason },
        WorkspaceProbe::Ran(obs) => {
            if obs.compiled && obs.failing_tests.is_empty() {
                WorkspaceTestOutcome::Passed
            } else {
                WorkspaceTestOutcome::Failed {
                    detail: summarize_workspace_failure(&obs),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Baseline-aware workspace gate (default `AUTO_PARALLEL_WORKSPACE_GATE_MODE`).
//
// Problem it solves: a single workspace shared by many concurrent waves means
// any PRE-EXISTING test failure or ANY crate's compile break — even in a crate
// unrelated to the landing task — makes `cargo test --workspace` red and, under
// the strict gate, blocks EVERY task's `[~]->[x]` promotion across every wave.
// A non-green baseline renders the whole repo unlandable.
//
// The baseline gate narrows the question from "is the whole workspace green?" to
// "did THIS task introduce a NEW regression vs the run's best-observed baseline?"
// A test that was already failing at the run's base (and never observed passing)
// staying failed does NOT block. A test that was passing (ever, this run) now
// failing, or a crate that compiled (ever, this run) now failing to compile, IS
// a regression — and blocks the task when it falls in the task's blast radius
// (a crate the task touched). The task's OWN declared verification remains a
// separate hard gate (`apply_lane_verify_gate`); this gate is defense-in-depth
// against cross-lane integration regressions, not a replacement for it.
// ---------------------------------------------------------------------------

/// Structured result of one `cargo test --workspace --no-fail-fast` run, parsed
/// from the interleaved (`2>&1`) cargo + libtest text output. This is what makes
/// baseline-relative regression detection possible: a coarse exit code cannot
/// tell a pre-existing failure apart from a newly-introduced one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkspaceObservation {
    /// True when the workspace built every test target it attempted (no
    /// `error: could not compile` was emitted). When false, cargo aborts the run
    /// and NO test results are available, so `passing_tests`/`failing_tests` are
    /// empty and only `broken_crates` is meaningful.
    pub(crate) compiled: bool,
    /// Normalized crate names (`-` -> `_`) that failed to compile.
    pub(crate) broken_crates: BTreeSet<String>,
    /// Normalized crate/target stems that produced a `Running`/`Doc-tests` line
    /// (i.e. their test target compiled and executed) this run.
    pub(crate) compiled_targets: BTreeSet<String>,
    /// Test IDs (`<target_stem>::<test_name>`) that ran and passed.
    pub(crate) passing_tests: BTreeSet<String>,
    /// Test IDs that ran and FAILED.
    pub(crate) failing_tests: BTreeSet<String>,
    /// Verbatim `cargo`/`rustc` error lines (those beginning with `error`)
    /// captured when the workspace did not compile, capped for log sanity. This
    /// is what lets a "workspace won't compile" diagnostic name the ACTUAL cause
    /// (e.g. `error: couldn't read .../Cannon.lud: No such file (os error 2)`)
    /// rather than only the crate name. Empty on a clean compile.
    pub(crate) compile_error_excerpt: Vec<String>,
}

/// Maximum number of `error` lines retained in [`WorkspaceObservation::compile_error_excerpt`].
const MAX_COMPILE_ERROR_EXCERPT_LINES: usize = 12;

/// Either a parsed observation, or an infra-level skip (non-Rust repo, spawn
/// error, timeout, or an ambiguous non-zero exit with no parseable signal).
pub(crate) enum WorkspaceProbe {
    Ran(WorkspaceObservation),
    Skipped { reason: String },
}

/// Run `cargo test --workspace --no-fail-fast` (merging stderr into stdout so the
/// `Running <target>` lines and the `test ... ok/FAILED` lines stay in true
/// order for positional attribution) and parse the result. `--no-fail-fast` is
/// essential: it lets us enumerate EVERY passing test across all binaries, so a
/// later failure of any of them is recognized as a regression instead of being
/// missed because cargo stopped at the first red binary.
pub(crate) async fn run_workspace_probe(repo_root: &Path) -> WorkspaceProbe {
    if !repo_root.join("Cargo.toml").is_file() {
        return WorkspaceProbe::Skipped {
            reason: "no Cargo.toml found; workspace cargo test gate is not applicable".to_string(),
        };
    }
    let deadline = workspace_test_timeout();
    match tokio::time::timeout(deadline, run_workspace_test_capture(repo_root.to_path_buf())).await
    {
        Ok(Some((success, text))) => {
            let obs = parse_workspace_test_output(&text);
            // A non-zero exit with neither a compile break nor a failing test is
            // an ambiguous infra failure (e.g. cargo/rustup misconfig). Do not
            // manufacture a phantom regression from it; surface it as a skip so
            // the baseline gate fails OPEN rather than holding a task hostage.
            if !success
                && obs.compiled
                && obs.failing_tests.is_empty()
                && obs.broken_crates.is_empty()
            {
                return WorkspaceProbe::Skipped {
                    reason: format!(
                        "`cargo test --workspace` exited non-zero with no parseable test/compile signal\n{}",
                        tail_lines(&text)
                    ),
                };
            }
            WorkspaceProbe::Ran(obs)
        }
        Ok(None) => WorkspaceProbe::Skipped {
            reason: "could not spawn `cargo test --workspace`".to_string(),
        },
        Err(_) => WorkspaceProbe::Skipped {
            reason: format!(
                "`cargo test --workspace` timed out after {}s",
                deadline.as_secs()
            ),
        },
    }
}

async fn run_workspace_test_capture(repo_root: PathBuf) -> Option<(bool, String)> {
    let result = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg("cargo test --workspace --no-fail-fast 2>&1")
        .current_dir(&repo_root)
        .kill_on_drop(true)
        .output()
        .await;
    match result {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.stderr.is_empty() {
                // With `2>&1` stderr is already folded into stdout; this only
                // catches the rare case where the shell wrote to stderr itself.
                text.push('\n');
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            Some((output.status.success(), text))
        }
        Err(_) => None,
    }
}

/// Normalize a crate/package name to cargo's compiled-artifact form (`-` -> `_`)
/// so `boardlab-tui` (Cargo.toml) and `boardlab_tui` (deps/ binary) compare equal.
pub(crate) fn normalize_crate_name(name: &str) -> String {
    name.replace('-', "_")
}

/// Parse interleaved `cargo test` output into a [`WorkspaceObservation`].
///
/// Association rule: every `test <name> ... ok|FAILED` line is attributed to the
/// most recent `Running <target> (…/deps/<stem>-<hash>)` or `Doc-tests <crate>`
/// line, yielding a per-crate-unique test id `<stem>::<name>`. Cargo runs test
/// binaries sequentially, so this positional attribution is stable.
pub(crate) fn parse_workspace_test_output(text: &str) -> WorkspaceObservation {
    let mut obs = WorkspaceObservation {
        compiled: true,
        ..Default::default()
    };
    let mut current_target: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();

        // Any `error`/`error[E…]`/`error:` line is retained verbatim (capped) so
        // the compile-block diagnostic can show the real cause — the missing
        // fixture path, the unresolved import, etc. — not just the crate name.
        if trimmed.starts_with("error")
            && obs.compile_error_excerpt.len() < MAX_COMPILE_ERROR_EXCERPT_LINES
        {
            obs.compile_error_excerpt.push(trimmed.to_string());
        }

        // `error: could not compile `<crate>` (…) due to …` -> compile break.
        if let Some(rest) = trimmed.strip_prefix("error: could not compile `") {
            if let Some(end) = rest.find('`') {
                let crate_name = normalize_crate_name(&rest[..end]);
                obs.broken_crates.insert(crate_name);
            }
            obs.compiled = false;
            continue;
        }

        // `Running <desc> (<path>/deps/<stem>-<hash>)`
        if let Some(rest) = trimmed.strip_prefix("Running ") {
            if let Some(stem) = running_line_target_stem(rest) {
                obs.compiled_targets.insert(stem.clone());
                current_target = Some(stem);
            }
            continue;
        }

        // `Doc-tests <crate>`
        if let Some(rest) = trimmed.strip_prefix("Doc-tests ") {
            let stem = normalize_crate_name(rest.trim());
            if !stem.is_empty() {
                obs.compiled_targets.insert(stem.clone());
                current_target = Some(stem);
            }
            continue;
        }

        // `test <name> ... ok|FAILED|ignored` (excludes the `test result:` line,
        // which has no ` ... ` separator).
        if let Some((name, status)) = parse_test_result_line(trimmed) {
            let stem = current_target.as_deref().unwrap_or("<unknown>");
            let id = format!("{stem}::{name}");
            match status {
                TestLineStatus::Ok => {
                    obs.passing_tests.insert(id);
                }
                TestLineStatus::Failed => {
                    obs.failing_tests.insert(id);
                }
                TestLineStatus::Ignored => {}
            }
        }
    }
    // A test can appear as both ok (in one target) and failed (in another) only
    // via distinct stems, so the two sets never collide. But if a name somehow
    // landed in both (e.g. a retry), prefer "failing" as the safe reading.
    for id in obs.failing_tests.clone() {
        obs.passing_tests.remove(&id);
    }
    obs
}

/// Extract the deps-binary stem from a `Running` line's parenthesized path,
/// e.g. `unittests src/lib.rs (target/debug/deps/boardlab_tui-1a2b3c4d)` ->
/// `boardlab_tui`. Returns the file stem with the trailing `-<hash>` removed.
fn running_line_target_stem(rest: &str) -> Option<String> {
    let open = rest.rfind('(')?;
    let close = rest[open..].find(')')? + open;
    let path = &rest[open + 1..close];
    let file = path.rsplit('/').next().unwrap_or(path).trim();
    // Strip the trailing `-<hash>` (hex) cargo appends to deps binaries.
    let stem = match file.rfind('-') {
        Some(idx)
            if idx + 1 < file.len()
                && file[idx + 1..].chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            &file[..idx]
        }
        _ => file,
    };
    if stem.is_empty() {
        None
    } else {
        Some(normalize_crate_name(stem))
    }
}

enum TestLineStatus {
    Ok,
    Failed,
    Ignored,
}

fn parse_test_result_line(line: &str) -> Option<(String, TestLineStatus)> {
    let rest = line.strip_prefix("test ")?;
    // Exclude the summary line `test result: ok. N passed; …`.
    if rest.starts_with("result:") {
        return None;
    }
    let sep = rest.find(" ... ")?;
    let name = rest[..sep].trim();
    if name.is_empty() {
        return None;
    }
    let tail = rest[sep + 5..].trim_start();
    let status = if tail == "ok" || tail.starts_with("ok ") {
        TestLineStatus::Ok
    } else if tail.starts_with("FAILED") {
        TestLineStatus::Failed
    } else if tail.starts_with("ignored") {
        TestLineStatus::Ignored
    } else {
        return None;
    };
    Some((name.to_string(), status))
}

/// A one-line-per-item summary for `REVIEW.md` when the strict gate (or the
/// baseline gate's blocking path) demotes a task.
pub(crate) fn summarize_workspace_failure(obs: &WorkspaceObservation) -> String {
    let mut lines = Vec::new();
    if !obs.compiled {
        if obs.broken_crates.is_empty() {
            lines.push("workspace failed to compile".to_string());
        } else {
            lines.push(format!(
                "workspace failed to compile: {}",
                obs.broken_crates
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if !obs.failing_tests.is_empty() {
        let shown: Vec<String> = obs.failing_tests.iter().take(25).cloned().collect();
        lines.push(format!(
            "{} failing test(s): {}",
            obs.failing_tests.len(),
            shown.join(", ")
        ));
    }
    if lines.is_empty() {
        lines.push("workspace gate reported failure with no parseable detail".to_string());
    }
    lines.join("\n")
}

fn tail_lines(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(FAILURE_TAIL_LINES);
    lines[start..].join("\n")
}

/// Which workspace gate the operator selected. Baseline (the default) narrows the
/// gate to NEW regressions; strict restores the legacy whole-workspace-green bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceGateMode {
    Baseline,
    Strict,
}

const WORKSPACE_GATE_MODE_ENV: &str = "AUTO_PARALLEL_WORKSPACE_GATE_MODE";

pub(crate) fn workspace_gate_mode() -> WorkspaceGateMode {
    match std::env::var(WORKSPACE_GATE_MODE_ENV)
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("strict") => WorkspaceGateMode::Strict,
        Some("baseline") | Some("") | None => WorkspaceGateMode::Baseline,
        Some(other) => {
            eprintln!(
                "warning: unknown {WORKSPACE_GATE_MODE_ENV}={other:?}; defaulting to baseline"
            );
            WorkspaceGateMode::Baseline
        }
    }
}

/// The classification of an observation against a persisted best-observed
/// baseline. `blocking` regressions fall in the task's blast radius (a crate it
/// touched) and demote it; `nonblocking` regressions are real but attributed to
/// another lane (the task did not touch the crate) — logged, never used to hold
/// THIS task hostage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkspaceRegressionDecision {
    pub(crate) blocking: Vec<String>,
    pub(crate) nonblocking: Vec<String>,
}

impl WorkspaceRegressionDecision {
    pub(crate) fn is_blocked(&self) -> bool {
        !self.blocking.is_empty()
    }
}

/// True if `obs` contains any candidate regression against `baseline` (a broken
/// crate that ever compiled, or a failing test that ever passed). Cheap set math
/// used to keep `cargo metadata` (needed only for attribution) off the clean path.
pub(crate) fn has_candidate_regression(
    baseline: &WorkspaceBaseline,
    obs: &WorkspaceObservation,
) -> bool {
    obs.broken_crates
        .iter()
        .any(|c| baseline.ever_compiled_crates.contains(c))
        || obs
            .failing_tests
            .iter()
            .any(|t| baseline.ever_passed_tests.contains(t))
}

/// Partition the observation's regressions (vs the best-observed baseline) into
/// blocking (in the task's blast radius) vs nonblocking (another lane's crate).
///
/// A regression is: a crate in `ever_compiled_crates` that now fails to compile,
/// or a test in `ever_passed_tests` that now fails. Pre-existing baseline
/// failures that were never observed passing/compiling this run are, by
/// construction, absent from the `ever_*` sets and so are NOT regressions.
pub(crate) fn classify_workspace_regressions(
    baseline: &WorkspaceBaseline,
    obs: &WorkspaceObservation,
    touched_crates: &BTreeSet<String>,
) -> WorkspaceRegressionDecision {
    let mut decision = WorkspaceRegressionDecision::default();
    for crate_name in &obs.broken_crates {
        if !baseline.ever_compiled_crates.contains(crate_name) {
            continue; // never compiled this run -> not a regression (pre-existing break)
        }
        let msg = format!(
            "crate `{crate_name}` compiled earlier this run but no longer compiles"
        );
        if touched_crates.contains(crate_name) {
            decision
                .blocking
                .push(format!("{msg} (task touched `{crate_name}`)"));
        } else {
            decision.nonblocking.push(format!(
                "{msg} (task did not touch `{crate_name}`; attributed to the owning lane)"
            ));
        }
    }
    for test_id in &obs.failing_tests {
        if !baseline.ever_passed_tests.contains(test_id) {
            continue; // never passed this run -> not a regression (pre-existing failure)
        }
        let stem = test_id.split("::").next().unwrap_or("").to_string();
        let msg = format!("test `{test_id}` passed earlier this run but now FAILS");
        if touched_crates.contains(&stem) {
            decision
                .blocking
                .push(format!("{msg} (task touched crate `{stem}`)"));
        } else {
            decision.nonblocking.push(format!(
                "{msg} (task did not touch crate `{stem}`; attributed to the owning lane)"
            ));
        }
    }
    decision
}

/// Fold an observation into the best-observed baseline (monotonic). The FIRST
/// call records the original pre-existing snapshot (`baseline_*`, for logging).
/// Every call where the workspace compiled ADDS the run's passing tests and
/// compiled targets to the `ever_*` sets and never removes anything — so once a
/// test is seen passing, a later failure of it is a regression even if it was in
/// the original baseline's failing set.
pub(crate) fn advance_workspace_baseline(
    baseline: &mut WorkspaceBaseline,
    obs: &WorkspaceObservation,
) {
    if !baseline.captured {
        baseline.captured = true;
        baseline.baseline_compiles = obs.compiled;
        baseline.baseline_broken_crates = obs.broken_crates.clone();
        baseline.baseline_failing_tests = obs.failing_tests.clone();
        baseline.compile_error_excerpt = obs.compile_error_excerpt.clone();
    }
    if obs.compiled {
        for id in &obs.passing_tests {
            baseline.ever_passed_tests.insert(id.clone());
        }
        for stem in &obs.compiled_targets {
            baseline.ever_compiled_crates.insert(stem.clone());
        }
    }
}

/// A prominent, human-actionable diagnostic when the shared workspace has NEVER
/// compiled this run. A compile break makes `cargo test --workspace` and every
/// task whose own verification builds a dependent crate fail, so tasks correctly
/// stall at `[~]`/shelved rather than promoting to `[x]`. The bare "no executable
/// dependency-ready code tasks remain" stop then leaves a human decoding why —
/// with zero indication the real cause is a broken build (classically a
/// missing/renamed source or fixture referenced by `include_str!`). This turns
/// that into an explicit, copy-pasteable explanation.
///
/// Returns `None` when the workspace has compiled at some point this run (every
/// crate broken at first capture was later observed compiling) or no baseline was
/// captured — i.e. nothing to warn about.
pub(crate) fn workspace_compile_block_diagnostic(baseline: &WorkspaceBaseline) -> Option<String> {
    if !baseline.captured {
        return None;
    }
    // Crates broken at first capture that were NEVER observed compiling this run
    // are still a hard blocker for every dependent task.
    let still_broken: Vec<String> = baseline
        .baseline_broken_crates
        .iter()
        .filter(|c| !baseline.ever_compiled_crates.contains(*c))
        .cloned()
        .collect();
    if still_broken.is_empty() {
        return None;
    }
    let mut msg = format!(
        "the shared workspace has NOT compiled at any point this run: crate(s) {} fail to compile. \
Every task whose own verification builds a dependent crate cannot pass, so tasks correctly stall at `[~]`/shelved instead of promoting to `[x]` — a broken build, NOT a scheduler defect, is why no code lanes are dispatchable.",
        still_broken.join(", ")
    );
    if !baseline.compile_error_excerpt.is_empty() {
        msg.push_str("\n  first compiler error line(s):");
        for line in &baseline.compile_error_excerpt {
            msg.push_str("\n    ");
            msg.push_str(line);
        }
    }
    msg.push_str(
        "\n  Recovery: fix the compile error — a common cause is a missing/renamed/deleted source or fixture file referenced by `include_str!`/`include_bytes!`; run `cargo build --workspace` for the exact path and check `git status` for deleted untracked files. \
Once the workspace compiles, re-run `auto parallel`; if tasks were shelved this run, add `AUTO_PARALLEL_RETRY_SHELVED=1` to give them a fresh attempt.",
    );
    Some(msg)
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
    async fn run_lane_verify_gate_falls_back_when_wrapper_missing() {
        let dir =
            std::env::temp_dir().join(format!("autodev-verify-wrapper-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let markdown = "- [ ] `TASK-1` t\n\nVerification:\n- Run `bash -c true`\n";
        assert_eq!(
            run_lane_verify_gate(&dir, "TASK-1", markdown).await,
            LaneVerifyOutcome::AllPassed
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_lane_verify_gate_uses_wrapper_when_present() {
        let dir = std::env::temp_dir().join(format!(
            "autodev-verify-wrapper-present-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let scripts = dir.join("scripts");
        fs::create_dir_all(&scripts).expect("create scripts dir");
        let wrapper = scripts.join("run-task-verification.sh");
        fs::write(
            &wrapper,
            "#!/usr/bin/env bash\nset -euo pipefail\necho \"$1:$3\" > wrapper-ran.txt\nshift\nif [[ ${1:-} == \"--\" ]]; then shift; fi\n\"$@\"\n",
        )
        .expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper)
            .expect("wrapper metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        let markdown = "- [ ] `TASK-1` t\n\nVerification:\n- Run `bash -c true`\n";
        assert_eq!(
            run_lane_verify_gate(&dir, "TASK-1", markdown).await,
            LaneVerifyOutcome::AllPassed
        );
        let marker = fs::read_to_string(dir.join("wrapper-ran.txt")).expect("wrapper marker");
        assert_eq!(marker.trim(), "TASK-1:bash");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn baseline_with(
        ever_passed: &[&str],
        ever_compiled: &[&str],
        baseline_failing: &[&str],
    ) -> WorkspaceBaseline {
        WorkspaceBaseline {
            captured: true,
            baseline_compiles: true,
            baseline_broken_crates: BTreeSet::new(),
            baseline_failing_tests: baseline_failing.iter().map(|s| s.to_string()).collect(),
            ever_passed_tests: ever_passed.iter().map(|s| s.to_string()).collect(),
            ever_compiled_crates: ever_compiled.iter().map(|s| s.to_string()).collect(),
            compile_error_excerpt: Vec::new(),
        }
    }

    fn set_of(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_associates_tests_with_their_running_target() {
        let out = "\
   Compiling boardlab-tui v0.1.0
    Finished test [unoptimized] in 3.4s
     Running unittests src/lib.rs (target/debug/deps/ludii_core-1a2b3c4d)
running 2 tests
test board::passes ... ok
test board::regresses ... FAILED
test result: FAILED. 1 passed; 1 failed; 0 ignored
     Running unittests src/lib.rs (target/debug/deps/boardlab_tui-99887766)
running 1 test
test ui::renders ... ok
test result: ok. 1 passed; 0 failed; 0 ignored
   Doc-tests boardlab-tui
running 1 test
test src/lib.rs - foo (line 3) ... ok
";
        let obs = parse_workspace_test_output(out);
        assert!(obs.compiled, "no compile error present");
        assert!(obs.passing_tests.contains("ludii_core::board::passes"));
        assert!(obs.failing_tests.contains("ludii_core::board::regresses"));
        assert!(obs.passing_tests.contains("boardlab_tui::ui::renders"));
        // Doc-tests target stem is normalized to underscores.
        assert!(obs
            .passing_tests
            .iter()
            .any(|t| t.starts_with("boardlab_tui::")));
        assert!(obs.compiled_targets.contains("ludii_core"));
        assert!(obs.compiled_targets.contains("boardlab_tui"));
        // The `test result:` summary line must not be captured as a test.
        assert!(!obs.passing_tests.iter().any(|t| t.contains("result:")));
        assert!(!obs.failing_tests.iter().any(|t| t.contains("result:")));
    }

    #[test]
    fn parse_detects_compile_break_and_yields_no_tests() {
        let out = "\
   Compiling boardlab-tui v0.1.0
error[E0425]: cannot find value `x` in this scope
 --> crates/boardlab-tui/src/lib.rs:1:1
error: could not compile `boardlab-tui` (lib test) due to 1 previous error
";
        let obs = parse_workspace_test_output(out);
        assert!(!obs.compiled);
        assert!(obs.broken_crates.contains("boardlab_tui"));
        assert!(obs.passing_tests.is_empty());
        assert!(obs.failing_tests.is_empty());
    }

    #[test]
    fn pre_existing_failure_does_not_block() {
        // `ludii_core::board::flaky` was failing at baseline and NEVER observed
        // passing this run -> not in ever_passed -> its continued failure is not
        // a regression.
        let baseline = baseline_with(
            &["ludii_core::board::stable"],
            &["ludii_core"],
            &["ludii_core::board::flaky"],
        );
        let obs = WorkspaceObservation {
            compiled: true,
            failing_tests: set_of(&["ludii_core::board::flaky"]),
            passing_tests: set_of(&["ludii_core::board::stable"]),
            compiled_targets: set_of(&["ludii_core"]),
            ..Default::default()
        };
        assert!(!has_candidate_regression(&baseline, &obs));
        let decision =
            classify_workspace_regressions(&baseline, &obs, &set_of(&["ludii_core"]));
        assert!(!decision.is_blocked(), "{decision:?}");
    }

    #[test]
    fn new_test_regression_in_touched_crate_blocks() {
        let baseline = baseline_with(&["ludii_core::board::stable"], &["ludii_core"], &[]);
        let obs = WorkspaceObservation {
            compiled: true,
            failing_tests: set_of(&["ludii_core::board::stable"]),
            compiled_targets: set_of(&["ludii_core"]),
            ..Default::default()
        };
        assert!(has_candidate_regression(&baseline, &obs));
        let decision =
            classify_workspace_regressions(&baseline, &obs, &set_of(&["ludii_core"]));
        assert!(decision.is_blocked(), "{decision:?}");
        assert!(decision.blocking[0].contains("board::stable"));
    }

    #[test]
    fn new_test_regression_in_untouched_crate_is_nonblocking() {
        // Real regression, but the landing task did not touch the crate; it is
        // attributed elsewhere and must not hold THIS task hostage.
        let baseline = baseline_with(&["ludii_core::board::stable"], &["ludii_core"], &[]);
        let obs = WorkspaceObservation {
            compiled: true,
            failing_tests: set_of(&["ludii_core::board::stable"]),
            compiled_targets: set_of(&["ludii_core"]),
            ..Default::default()
        };
        let decision =
            classify_workspace_regressions(&baseline, &obs, &set_of(&["some_other_crate"]));
        assert!(!decision.is_blocked(), "{decision:?}");
        assert_eq!(decision.nonblocking.len(), 1);
    }

    #[test]
    fn best_observed_monotonicity_blocks_refailed_baseline_test() {
        // A test that was RED in the original baseline, then observed passing,
        // then re-fails, IS a regression despite being in baseline_failing_tests.
        let mut baseline = baseline_with(&[], &[], &["ludii_core::board::t"]);
        // Observe it passing this run -> enters ever_passed.
        let passing = WorkspaceObservation {
            compiled: true,
            passing_tests: set_of(&["ludii_core::board::t"]),
            compiled_targets: set_of(&["ludii_core"]),
            ..Default::default()
        };
        advance_workspace_baseline(&mut baseline, &passing);
        assert!(baseline.ever_passed_tests.contains("ludii_core::board::t"));
        // Now it fails again.
        let refail = WorkspaceObservation {
            compiled: true,
            failing_tests: set_of(&["ludii_core::board::t"]),
            compiled_targets: set_of(&["ludii_core"]),
            ..Default::default()
        };
        assert!(has_candidate_regression(&baseline, &refail));
        let decision =
            classify_workspace_regressions(&baseline, &refail, &set_of(&["ludii_core"]));
        assert!(
            decision.is_blocked(),
            "re-failing an ever-passed test is a regression even if in baseline: {decision:?}"
        );
    }

    #[test]
    fn compile_broken_baseline_does_not_block_task_with_compiling_crates() {
        // Baseline never saw `boardlab_tui` compile (broken by another wave).
        // The workspace still won't compile at this landing, but the task's own
        // crate compiled+tested (via the separate verify gate); the pre-existing
        // break is not a regression and must not block.
        let baseline = baseline_with(&["mycrate::works"], &["mycrate"], &[]);
        let obs = WorkspaceObservation {
            compiled: false,
            broken_crates: set_of(&["boardlab_tui"]),
            ..Default::default()
        };
        assert!(
            !has_candidate_regression(&baseline, &obs),
            "boardlab_tui never compiled this run -> not a regression"
        );
        let decision = classify_workspace_regressions(&baseline, &obs, &set_of(&["mycrate"]));
        assert!(!decision.is_blocked(), "{decision:?}");
    }

    #[test]
    fn breaking_own_previously_compiling_crate_blocks() {
        // `mycrate` compiled earlier this run; the task touched it and now it
        // fails to compile -> a NEW compile regression in the task's blast radius.
        let baseline = baseline_with(&["mycrate::works"], &["mycrate"], &[]);
        let obs = WorkspaceObservation {
            compiled: false,
            broken_crates: set_of(&["mycrate"]),
            ..Default::default()
        };
        assert!(has_candidate_regression(&baseline, &obs));
        let decision = classify_workspace_regressions(&baseline, &obs, &set_of(&["mycrate"]));
        assert!(decision.is_blocked(), "{decision:?}");
        assert!(decision.blocking[0].contains("mycrate"));
    }

    #[test]
    fn parse_workspace_test_output_captures_compile_error_excerpt() {
        // Regression coverage for the compile-block diagnostic: the parser must
        // retain the real error line (a missing `include_str!` fixture), not just
        // the crate name, so the operator sees the actual cause.
        let out = "   Compiling ludii-core v0.1.0\n\
error: couldn't read crates/ludii-core/src/../Cannon.lud: No such file or directory (os error 2)\n\
error: could not compile `ludii-core` (lib test) due to 1 previous error\n";
        let obs = parse_workspace_test_output(out);
        assert!(!obs.compiled, "missing include_str! target breaks the build");
        assert!(obs.broken_crates.contains("ludii_core"));
        assert!(
            obs.compile_error_excerpt
                .iter()
                .any(|line| line.contains("Cannon.lud") && line.contains("No such file")),
            "excerpt must retain the verbatim compiler error: {:?}",
            obs.compile_error_excerpt
        );
    }

    #[test]
    fn workspace_compile_block_diagnostic_surfaces_persistent_break() {
        // A crate broken at first capture and NEVER observed compiling this run is
        // a persistent build blocker — the true reason tasks are shelved and no
        // code lanes are dispatchable. The diagnostic must fire, name the crate,
        // echo the captured compiler error, and point at the recovery.
        let baseline = WorkspaceBaseline {
            captured: true,
            baseline_broken_crates: ["ludii_core".to_string()].into_iter().collect(),
            compile_error_excerpt: vec![
                "error: couldn't read crates/ludii-core/src/../Cannon.lud: No such file or directory (os error 2)".to_string(),
            ],
            ..Default::default()
        };
        let diag = workspace_compile_block_diagnostic(&baseline)
            .expect("a persistent compile break must produce a diagnostic");
        assert!(diag.contains("ludii_core"), "names the broken crate: {diag}");
        assert!(diag.contains("Cannon.lud"), "echoes the real compiler error: {diag}");
        assert!(
            diag.contains("AUTO_PARALLEL_RETRY_SHELVED=1"),
            "points at the shelved-task recovery: {diag}"
        );

        // Once the crate is observed compiling later in the run, it is no longer a
        // blocker and the diagnostic goes silent (best-observed monotonicity).
        let mut recovered = baseline.clone();
        recovered.ever_compiled_crates.insert("ludii_core".to_string());
        assert!(
            workspace_compile_block_diagnostic(&recovered).is_none(),
            "a crate that compiled later this run is not a persistent blocker"
        );

        // An uncaptured baseline (strict mode / probe skipped) never warns.
        assert!(workspace_compile_block_diagnostic(&WorkspaceBaseline::default()).is_none());
    }

    #[test]
    fn newly_broken_crate_untouched_is_nonblocking() {
        let baseline = baseline_with(&[], &["mycrate", "othercrate"], &[]);
        let obs = WorkspaceObservation {
            compiled: false,
            broken_crates: set_of(&["othercrate"]),
            ..Default::default()
        };
        let decision = classify_workspace_regressions(&baseline, &obs, &set_of(&["mycrate"]));
        assert!(!decision.is_blocked(), "{decision:?}");
        assert_eq!(decision.nonblocking.len(), 1);
    }

    #[test]
    fn advance_baseline_records_first_snapshot_and_is_monotonic() {
        let mut baseline = WorkspaceBaseline::default();
        let first = WorkspaceObservation {
            compiled: true,
            passing_tests: set_of(&["c::a"]),
            failing_tests: set_of(&["c::b"]),
            compiled_targets: set_of(&["c"]),
            ..Default::default()
        };
        advance_workspace_baseline(&mut baseline, &first);
        assert!(baseline.captured);
        assert!(baseline.baseline_failing_tests.contains("c::b"));
        assert!(baseline.ever_passed_tests.contains("c::a"));
        // A later broken-compile observation must not erase best-observed sets.
        let broken = WorkspaceObservation {
            compiled: false,
            broken_crates: set_of(&["c"]),
            ..Default::default()
        };
        advance_workspace_baseline(&mut baseline, &broken);
        assert!(baseline.ever_passed_tests.contains("c::a"));
        assert!(baseline.ever_compiled_crates.contains("c"));
        // First snapshot is not overwritten by later observations.
        assert!(baseline.baseline_compiles);
    }

    #[test]
    fn workspace_gate_mode_defaults_to_baseline() {
        let prev = std::env::var(WORKSPACE_GATE_MODE_ENV).ok();
        std::env::remove_var(WORKSPACE_GATE_MODE_ENV);
        assert_eq!(workspace_gate_mode(), WorkspaceGateMode::Baseline);
        std::env::set_var(WORKSPACE_GATE_MODE_ENV, "strict");
        assert_eq!(workspace_gate_mode(), WorkspaceGateMode::Strict);
        std::env::set_var(WORKSPACE_GATE_MODE_ENV, "baseline");
        assert_eq!(workspace_gate_mode(), WorkspaceGateMode::Baseline);
        std::env::set_var(WORKSPACE_GATE_MODE_ENV, "nonsense");
        assert_eq!(workspace_gate_mode(), WorkspaceGateMode::Baseline);
        match prev {
            Some(value) => std::env::set_var(WORKSPACE_GATE_MODE_ENV, value),
            None => std::env::remove_var(WORKSPACE_GATE_MODE_ENV),
        }
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
