use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum QaTier {
    Quick,
    Standard,
    Exhaustive,
}

impl QaTier {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Standard => "standard",
            Self::Exhaustive => "exhaustive",
        }
    }
}

#[derive(Args, Clone)]
pub(crate) struct QaArgs {
    /// Stop after this many successful QA iterations. Default is 1.
    #[arg(long, default_value_t = 1)]
    pub(crate) max_iterations: usize,

    /// Optional override for the QA prompt template
    #[arg(long)]
    pub(crate) prompt_file: Option<PathBuf>,

    /// Model to use for the QA worker
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort to pass through to the Codex QA worker
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Optional branch to require for the QA loop; defaults to the currently checked-out branch
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Directory for QA logs. Defaults to <repo>/.auto/qa
    #[arg(long)]
    pub(crate) run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// QA depth. Quick focuses on critical/high issues, Standard adds medium issues, Exhaustive includes polish and cosmetic issues.
    #[arg(long, value_enum, default_value_t = QaTier::Standard)]
    pub(crate) tier: QaTier,
}

#[derive(Args, Clone)]
pub(crate) struct QaOnlyArgs {
    /// Optional override for the report-only QA prompt template
    #[arg(long)]
    pub(crate) prompt_file: Option<PathBuf>,

    /// Model to use for the QA report worker
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort to pass through to the Codex QA report worker
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Optional branch to require for the QA report; defaults to the currently checked-out branch
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Directory for QA report logs. Defaults to <repo>/.auto/qa-only
    #[arg(long)]
    pub(crate) run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// QA depth. Quick focuses on critical/high issues, Standard adds medium issues, Exhaustive includes polish and cosmetic issues.
    #[arg(long, value_enum, default_value_t = QaTier::Standard)]
    pub(crate) tier: QaTier,
}

#[derive(Args, Clone)]
pub(crate) struct HealthArgs {
    /// Optional override for the health prompt template
    #[arg(long)]
    pub(crate) prompt_file: Option<PathBuf>,

    /// Model to use for the health worker
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort to pass through to the Codex health worker
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Optional branch to require for the health report; defaults to the currently checked-out branch
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Directory for health logs. Defaults to <repo>/.auto/health
    #[arg(long)]
    pub(crate) run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,
}

#[derive(Args, Clone)]
pub(crate) struct ShipArgs {
    /// Stop after this many successful ship iterations. Default is 1.
    #[arg(long, default_value_t = 1)]
    pub(crate) max_iterations: usize,

    /// Optional override for the ship prompt template
    #[arg(long)]
    pub(crate) prompt_file: Option<PathBuf>,

    /// Model to use for the ship worker
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort to pass through to the Codex ship worker
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Optional branch to require for the ship loop; defaults to the currently checked-out branch
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Optional explicit base branch for diff and PR targeting
    #[arg(long)]
    pub(crate) base_branch: Option<String>,

    /// Directory for ship logs. Defaults to <repo>/.auto/ship
    #[arg(long)]
    pub(crate) run_root: Option<PathBuf>,

    /// Bypass the pre-model release gate and record the operator reason in SHIP.md
    #[arg(long, value_name = "REASON")]
    pub(crate) bypass_release_gate: Option<String>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,
}

#[derive(Args, Clone)]
pub(crate) struct StewardArgs {
    /// Directory for steward artifacts. Defaults to <repo>/steward
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,

    /// Additional repo roots the steward may inspect. Use for the other side
    /// of a two-repo project (e.g. `--reference-repo ../bitino` when stewarding
    /// autonomy) so cross-repo contracts get audited in the same pass.
    #[arg(long = "reference-repo")]
    pub(crate) reference_repos: Vec<PathBuf>,

    /// Read-only mode. Produce the steward artifacts but never edit active
    /// planning files or specs.
    #[arg(long)]
    pub(crate) report_only: bool,

    /// Preview the steward prompt without invoking the model.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Optional branch to require for the steward pass; defaults to the current branch.
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Codex model for the first steward pass — writes drift + hinge + retire +
    /// hazard artifacts and promotes active plan/spec work.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Codex reasoning effort for the first steward pass.
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Codex model for the finalizer pass — reviews the first pass's proposed
    /// edits against the live tree and applies the ones that hold.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) finalizer_model: String,

    /// Codex finalizer reasoning effort.
    #[arg(long, default_value = "high")]
    pub(crate) finalizer_effort: String,

    /// Codex executable used by both steward passes.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Skip the finalizer pass and stop after the first Codex pass writes its
    /// deliverables. Useful when you want a quick audit without the review-and-apply step.
    #[arg(long)]
    pub(crate) skip_finalizer: bool,
}

#[derive(Args, Clone)]
pub(crate) struct AuditArgs {
    /// Run the professional whole-repo audit pipeline: context engineering,
    /// per-file analysis, cross-file synthesis, crate remediation, final review,
    /// and optional primary-branch merge.
    #[arg(long)]
    pub(crate) everything: bool,

    /// Professional audit phase to run. Used only with --everything.
    #[arg(long, value_enum, default_value_t = AuditEverythingPhase::All)]
    pub(crate) everything_phase: AuditEverythingPhase,

    /// Resume an existing professional audit run. Defaults to the latest run
    /// recorded under .auto/audit-everything.
    #[arg(long)]
    pub(crate) everything_run_id: Option<String>,

    /// Root directory for professional audit runtime state.
    #[arg(long)]
    pub(crate) everything_run_root: Option<PathBuf>,

    /// Run the professional audit directly in the current checkout instead of
    /// creating a separate canonical audit worktree. New in-place runs require
    /// a clean checkout and commit the GO audit result directly in place.
    #[arg(long)]
    pub(crate) everything_in_place: bool,

    /// Maximum concurrent Codex workers for read-only professional audit phases.
    #[arg(long, default_value_t = 15)]
    pub(crate) everything_threads: usize,

    /// Maximum concurrent Codex remediation lanes. Each lane runs in an
    /// isolated worktree and the host lands commits back onto the audit branch.
    #[arg(long, default_value_t = 5)]
    pub(crate) remediation_threads: usize,

    /// Model for professional audit first-pass file analysis.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) first_pass_model: String,

    /// Reasoning effort for professional audit first-pass file analysis.
    #[arg(long, default_value = "low")]
    pub(crate) first_pass_effort: String,

    /// Number of retry rounds for first-pass files that fail to produce
    /// `analysis.md` (silent codex timeouts, transient API errors). Each
    /// retry only re-runs files still missing their analysis artifact, so
    /// completed work is preserved. Default 3 rounds.
    #[arg(long, default_value_t = 3)]
    pub(crate) first_pass_retries: usize,

    /// Model for professional audit cross-file synthesis.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) synthesis_model: String,

    /// Reasoning effort for professional audit cross-file synthesis.
    #[arg(long, default_value = "high")]
    pub(crate) synthesis_effort: String,

    /// Model for professional audit crate-by-crate remediation.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) remediation_model: String,

    /// Reasoning effort for professional audit crate-by-crate remediation.
    #[arg(long, default_value = "high")]
    pub(crate) remediation_effort: String,

    /// Model for professional audit final review.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) final_review_model: String,

    /// Reasoning effort for professional audit final review.
    #[arg(long, default_value = "xhigh")]
    pub(crate) final_review_effort: String,

    /// Number of final-review repair attempts to run when final review writes
    /// Verdict: NO-GO with actionable blockers.
    #[arg(long, default_value_t = 1)]
    pub(crate) final_review_retries: usize,

    /// Maximum file-quality rerating/remediation passes after a GO final
    /// review. Each pass rerates every first-pass file and runs per-file
    /// deliverables for files below 9/10 before the final review is rerun.
    #[arg(long, default_value_t = 10)]
    pub(crate) file_quality_passes: usize,

    /// Do not attempt to merge the professional audit branch back into the
    /// primary branch after final review, even if the final review is GO.
    #[arg(long)]
    pub(crate) no_everything_merge: bool,

    /// Operator-authored doctrine markdown. This is the judgment framework
    /// the auditor applies. The command stays agnostic — whatever you put
    /// here is what "clean" means for this repo. Required; will NOT be
    /// auto-generated (auto-gen defeats operator ownership).
    #[arg(long, default_value = "audit/DOCTRINE.md")]
    pub(crate) doctrine_prompt: PathBuf,

    /// Override the bundled verdicts / output rubric. Rare — changing this
    /// will break the Rust-side parser unless you also maintain the shape.
    #[arg(long)]
    pub(crate) rubric_prompt: Option<PathBuf>,

    /// Glob patterns to include. Repeatable. Defaults to sensible code +
    /// spec globs; override to scope a run (e.g. `--paths 'node/src/bridge_*'`).
    #[arg(long = "paths")]
    pub(crate) include_paths: Vec<String>,

    /// Glob patterns to exclude. Repeatable. Applied after `--paths`.
    #[arg(long = "exclude")]
    pub(crate) exclude_paths: Vec<String>,

    /// Cap the number of files audited this run. 0 means unlimited.
    /// Use to control cost on large codebases.
    #[arg(long, default_value_t = 0)]
    pub(crate) max_files: usize,

    /// Maximum concurrent workers for the legacy per-file audit first pass.
    /// The host still applies verdicts and writes the manifest centrally.
    #[arg(long = "audit-threads", alias = "threads", default_value_t = 15)]
    pub(crate) audit_threads: usize,

    /// Directory for audit artifacts. Defaults to <repo>/audit
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,

    /// Verify that every previously flagged legacy audit finding has either
    /// been re-audited clean or removed from the current tree.
    #[arg(long)]
    pub(crate) verify_findings: bool,

    /// Remediate existing legacy audit findings from MANIFEST.json without
    /// producing a fresh first-pass audit, then re-audit drifted files and run
    /// the finding verifier.
    #[arg(long)]
    pub(crate) resolve_findings: bool,

    /// Maximum Cargo validation concurrency each resolve lane should use.
    /// This is passed to lane prompts and CARGO_BUILD_JOBS.
    #[arg(long, default_value_t = 2)]
    pub(crate) resolve_validation_threads: usize,

    /// Number of finding-resolution run logs to keep after successful
    /// resolution. Older run directories and lane target directories are pruned.
    #[arg(long, default_value_t = 2)]
    pub(crate) resolve_keep_runs: usize,

    /// Maximum remediation/verification passes for --resolve-findings before
    /// failing with the remaining open findings.
    #[arg(long, default_value_t = 10)]
    pub(crate) resolve_passes: usize,

    /// Disable pruning of completed finding-resolution Cargo target dirs.
    #[arg(long)]
    pub(crate) no_resolve_target_prune: bool,

    /// Allow resolve-findings to continue when a tracked legacy doctrine file
    /// was intentionally deleted and the repo entrypoint names a successor.
    #[arg(long)]
    pub(crate) allow_missing_resolve_roots: bool,

    /// Resume mode. `resume` (default) picks up at first pending file;
    /// `fresh` archives the old manifest and starts over; `only-drifted`
    /// re-audits files whose content or doctrine hash changed.
    #[arg(long, value_enum, default_value_t = AuditResumeMode::Resume)]
    pub(crate) resume_mode: AuditResumeMode,

    /// Read-only. Write verdicts + manifest but never apply patches, append
    /// to WORKLIST.md, or commit.
    #[arg(long)]
    pub(crate) report_only: bool,

    /// Print the per-file prompt for the first pending file and exit.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Optional branch to require.
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Auditor model.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Auditor reasoning effort / thinking.
    #[arg(long, default_value = "low")]
    pub(crate) reasoning_effort: String,

    /// Escalation model for DRIFT-LARGE / REFACTOR verdicts that write
    /// worklist entries. Codex gives a second-opinion on high-impact calls.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) escalation_model: String,

    /// Escalation reasoning effort.
    #[arg(long, default_value = "high")]
    pub(crate) escalation_effort: String,

    /// Codex executable used for audit and escalation passes.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// kimi-cli executable used for explicit Kimi model opt-ins.
    #[arg(long, default_value = "kimi-cli")]
    pub(crate) kimi_bin: PathBuf,

    /// Legacy PI binary retained for compatibility.
    #[arg(long = "pi-bin", default_value = "pi")]
    pub(crate) pi_bin: PathBuf,

    /// Route explicit Kimi audit models through `kimi-cli --yolo`.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub(crate) use_kimi_cli: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum AuditEverythingPhase {
    /// Run all professional audit phases in order.
    All,
    /// Create/reuse worktree and generate AGENTS.md / ARCHITECTURE.md context.
    InitContext,
    /// Run one clean Codex iteration per tracked file.
    FirstPass,
    /// Build and revise crate/module markdown reports from per-file analysis.
    Synthesize,
    /// Generate the dependency graph used by parallel remediation lanes.
    PlanRemediation,
    /// Apply code/doc/test revisions via dependency-ready isolated remediation lanes.
    Remediate,
    /// Run the final xhigh review over reports and diff.
    FinalReview,
    /// Attempt to merge the professional audit branch back to the primary branch.
    Merge,
    /// Request a graceful pause for the run. Active remediation lanes drain;
    /// no new lanes are dispatched while the request exists.
    Pause,
    /// Clear a professional audit pause request so the next run can resume.
    Unpause,
    /// Print current professional audit status.
    Status,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum AuditResumeMode {
    /// Resume from the existing manifest (default). Skips files already
    /// audited if content + doctrine hashes still match; re-audits if
    /// either has drifted.
    Resume,
    /// Archive the current manifest and start a fresh full pass.
    Fresh,
    /// Only re-audit files whose content or doctrine hash has drifted
    /// since their last audit. Skips all files never audited.
    OnlyDrifted,
}

#[derive(Args, Clone)]
pub(crate) struct SymphonyArgs {
    #[command(subcommand)]
    pub(crate) command: SymphonySubcommand,
}

#[derive(Subcommand, Clone)]
pub(crate) enum SymphonySubcommand {
    /// Sync unchecked implementation-plan items into a Linear project
    Sync(SymphonySyncArgs),
    /// Render a repo-specific Symphony WORKFLOW.md
    Workflow(SymphonyWorkflowArgs),
    /// Render the workflow if needed, then launch Symphony in the foreground dashboard
    Run(SymphonyRunArgs),
}

#[derive(Args, Clone)]
pub(crate) struct SymphonySyncArgs {
    /// Repository root whose IMPLEMENTATION_PLAN.md should be synced. Defaults to the current git repo root.
    #[arg(long)]
    pub(crate) repo_root: Option<PathBuf>,

    /// Linear project slug that should receive this repo's synced tasks. Defaults to the generated WORKFLOW.md after first setup.
    #[arg(long)]
    pub(crate) project_slug: Option<String>,

    /// Linear state name used for newly created or reopened issues
    #[arg(long, default_value = "Todo")]
    pub(crate) todo_state: String,

    /// Codex model used for sync planning analysis
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) planner_model: String,

    /// Codex reasoning effort used for sync planning analysis
    #[arg(long, default_value = "high")]
    pub(crate) planner_reasoning_effort: String,

    /// Codex executable used for sync planning analysis
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Disable the Codex planner and fall back to deterministic dependency parsing only
    #[arg(long)]
    pub(crate) no_ai_planner: bool,
}

#[derive(Args, Clone)]
pub(crate) struct SymphonyWorkflowArgs {
    /// Repository root whose Symphony workflow should be rendered. Defaults to the current git repo root.
    #[arg(long)]
    pub(crate) repo_root: Option<PathBuf>,

    /// Linear project slug used by Symphony for this repo. Defaults to the generated WORKFLOW.md after first setup.
    #[arg(long)]
    pub(crate) project_slug: Option<String>,

    /// Output path for the generated WORKFLOW.md
    #[arg(long)]
    pub(crate) output: Option<PathBuf>,

    /// Root directory where Symphony should create per-issue workspaces for this repo
    #[arg(long)]
    pub(crate) workspace_root: Option<PathBuf>,

    /// Branch that the generated workflow should treat as the integration branch
    #[arg(long)]
    pub(crate) base_branch: Option<String>,

    /// Maximum concurrent Symphony agents for this repo
    #[arg(long, default_value_t = 1)]
    pub(crate) max_concurrent_agents: usize,

    /// Poll interval in milliseconds
    #[arg(long, default_value_t = 5_000)]
    pub(crate) poll_interval_ms: u64,

    /// Model passed to Codex app-server through quota routing
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort passed to Codex app-server through quota routing
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Linear state name used when work begins
    #[arg(long, default_value = "In Progress")]
    pub(crate) in_progress_state: String,

    /// Linear terminal state name used after successful landing
    #[arg(long, default_value = "Done")]
    pub(crate) done_state: String,

    /// Optional non-active state name used when the worker encounters a true external blocker
    #[arg(long)]
    pub(crate) blocked_state: Option<String>,
}

#[derive(Args, Clone)]
pub(crate) struct SymphonyRunArgs {
    /// Repository root whose Symphony workflow should be rendered and run. Defaults to the current git repo root.
    #[arg(long)]
    pub(crate) repo_root: Option<PathBuf>,

    /// Linear project slug used by Symphony for this repo. Defaults to the generated WORKFLOW.md after first setup.
    #[arg(long)]
    pub(crate) project_slug: Option<String>,

    /// Output path for the generated WORKFLOW.md
    #[arg(long)]
    pub(crate) output: Option<PathBuf>,

    /// Root directory where Symphony should create per-issue workspaces for this repo
    #[arg(long)]
    pub(crate) workspace_root: Option<PathBuf>,

    /// Branch that the generated workflow should treat as the integration branch
    #[arg(long)]
    pub(crate) base_branch: Option<String>,

    /// Maximum concurrent Symphony agents for this repo
    #[arg(long, default_value_t = 1)]
    pub(crate) max_concurrent_agents: usize,

    /// Poll interval in milliseconds
    #[arg(long, default_value_t = 5_000)]
    pub(crate) poll_interval_ms: u64,

    /// Model passed to Codex app-server through quota routing
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort passed to Codex app-server through quota routing
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Sync Linear issues from IMPLEMENTATION_PLAN.md before launching Symphony
    #[arg(long)]
    pub(crate) sync_first: bool,

    /// Linear state name used for newly created or reopened issues when --sync-first is set
    #[arg(long, default_value = "Todo")]
    pub(crate) todo_state: String,

    /// Codex model used for sync planning analysis when --sync-first is set
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) planner_model: String,

    /// Codex reasoning effort used for sync planning analysis when --sync-first is set
    #[arg(long, default_value = "high")]
    pub(crate) planner_reasoning_effort: String,

    /// Codex executable used for sync planning analysis when --sync-first is set
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Disable the Codex planner and fall back to deterministic dependency parsing only when --sync-first is set
    #[arg(long)]
    pub(crate) no_ai_planner: bool,

    /// Linear state name used when work begins
    #[arg(long, default_value = "In Progress")]
    pub(crate) in_progress_state: String,

    /// Linear terminal state name used after successful landing
    #[arg(long, default_value = "Done")]
    pub(crate) done_state: String,

    /// Optional non-active state name used when the worker encounters a true external blocker
    #[arg(long)]
    pub(crate) blocked_state: Option<String>,

    /// Local Symphony Elixir root directory. Overrides AUTODEV_SYMPHONY_ROOT; required when the env var is unset.
    #[arg(long, value_name = "PATH")]
    pub(crate) symphony_root: Option<PathBuf>,

    /// Directory where Symphony should write its own log files
    #[arg(long)]
    pub(crate) logs_root: Option<PathBuf>,

    /// Optional Symphony dashboard port
    #[arg(long)]
    pub(crate) port: Option<u16>,
}
