mod audit_command;
mod audit_everything;
mod backend_policy;
mod book_command;
mod bug_command;
mod claude_exec;
mod codex_exec;
mod codex_stream;
mod completion_artifacts;
mod corpus;
mod doctor_command;
mod generation;
mod health_command;
mod kimi_backend;
mod linear_tracker;
mod loop_command;
mod nemesis;
mod parallel_command;
mod pi_backend;
mod qa_command;
mod qa_only_command;
mod quota_accounts;
mod quota_config;
mod quota_exec;
mod quota_patterns;
mod quota_selector;
mod quota_state;
mod quota_status;
mod quota_usage;
mod review_command;
mod ship_command;
mod state;
mod steward_command;
mod super_command;
mod symphony_command;
mod task_parser;
mod util;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::util::CLI_LONG_VERSION;

#[derive(Parser)]
#[command(
    name = "auto",
    version,
    long_version = CLI_LONG_VERSION,
    about = "Lightweight repo-root planning and execution workflow"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Review the repo and author a fresh planning corpus under genesis/
    Corpus(CorpusArgs),
    /// Generate specs and a new implementation plan from genesis/
    Gen(GenerationArgs),
    /// Run the all-in-one production-grade workflow: corpus, gen, gates, then parallel
    Super(SuperArgs),
    /// Reverse-engineer specs from code reality using genesis/ as supporting context
    Reverse(GenerationArgs),
    /// Run a chunked multi-pass bug-finding, invalidation, verification, and implementation pipeline
    Bug(BugArgs),
    /// Run the implementation loop on the repo's primary branch
    Loop(LoopArgs),
    /// Run the experimental multi-lane implementation executor
    Parallel(ParallelArgs),
    /// Run a runtime QA and ship-readiness pass on the current branch
    Qa(QaArgs),
    /// Run a report-only runtime QA pass on the current branch
    QaOnly(QaOnlyArgs),
    /// Run a repo-wide quality and verification health report
    Health(HealthArgs),
    /// Rewrite the last audit's CODEBASE-BOOK as a detailed narrative walkthrough
    Book(BookArgs),
    /// Run a no-model first-run preflight for local layout, binary metadata, and help surfaces
    Doctor(doctor_command::DoctorArgs),
    /// Review completed work on the current branch
    Review(ReviewArgs),
    /// Stewardship pass for a mid-flight repo. Two-pass Codex (gpt-5.5)
    /// pipeline: reconciles plan claims against the live code, surfaces
    /// hinge items, and applies approved IMPLEMENTATION_PLAN.md /
    /// WORKLIST.md / LEARNINGS.md updates in-place. Replaces `auto corpus`
    /// and `auto gen` for repos that already have an active planning
    /// surface; greenfield repos should keep using those.
    Steward(StewardArgs),
    /// File-by-file audit of a mature codebase against an operator-authored
    /// doctrine. Produces per-file verdicts (CLEAN / DRIFT / SLOP / RETIRE /
    /// REFACTOR), applies safe fixes atomically, batches large work into
    /// WORKLIST.md, and resumes cleanly from partial runs via a manifest.
    /// Doctrine is whatever the operator writes in `audit/DOCTRINE.md` — the
    /// command stays agnostic.
    Audit(AuditArgs),
    /// Prepare the current branch to ship, push it, and open or refresh a PR when appropriate
    Ship(ShipArgs),
    /// Run a disposable Nemesis audit and append its outputs into root specs and plan
    Nemesis(NemesisArgs),
    /// Manage quota-aware account multiplexing for Claude and Codex
    Quota(QuotaArgs),
    /// Sync implementation-plan items into Linear and run the local Symphony runtime
    Symphony(SymphonyArgs),
}

#[derive(Args, Clone)]
struct QuotaArgs {
    #[command(subcommand)]
    command: QuotaSubcommand,
}

#[derive(Args, Clone)]
struct SymphonyArgs {
    #[command(subcommand)]
    command: SymphonySubcommand,
}

#[derive(Subcommand, Clone)]
enum SymphonySubcommand {
    /// Sync unchecked implementation-plan items into a Linear project
    Sync(SymphonySyncArgs),
    /// Render a repo-specific Symphony WORKFLOW.md
    Workflow(SymphonyWorkflowArgs),
    /// Render the workflow if needed, then launch Symphony in the foreground dashboard
    Run(SymphonyRunArgs),
}

#[derive(Args, Clone)]
struct SymphonySyncArgs {
    /// Repository root whose IMPLEMENTATION_PLAN.md should be synced. Defaults to the current git repo root.
    #[arg(long)]
    repo_root: Option<PathBuf>,

    /// Linear project slug that should receive this repo's synced tasks. Defaults to the generated WORKFLOW.md after first setup.
    #[arg(long)]
    project_slug: Option<String>,

    /// Linear state name used for newly created or reopened issues
    #[arg(long, default_value = "Todo")]
    todo_state: String,

    /// Codex model used for sync planning analysis
    #[arg(long, default_value = "gpt-5.5")]
    planner_model: String,

    /// Codex reasoning effort used for sync planning analysis
    #[arg(long, default_value = "high")]
    planner_reasoning_effort: String,

    /// Codex executable used for sync planning analysis
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Disable the Codex planner and fall back to deterministic dependency parsing only
    #[arg(long)]
    no_ai_planner: bool,
}

#[derive(Args, Clone)]
struct SymphonyWorkflowArgs {
    /// Repository root whose Symphony workflow should be rendered. Defaults to the current git repo root.
    #[arg(long)]
    repo_root: Option<PathBuf>,

    /// Linear project slug used by Symphony for this repo. Defaults to the generated WORKFLOW.md after first setup.
    #[arg(long)]
    project_slug: Option<String>,

    /// Output path for the generated WORKFLOW.md
    #[arg(long)]
    output: Option<PathBuf>,

    /// Root directory where Symphony should create per-issue workspaces for this repo
    #[arg(long)]
    workspace_root: Option<PathBuf>,

    /// Branch that the generated workflow should treat as the integration branch
    #[arg(long)]
    base_branch: Option<String>,

    /// Maximum concurrent Symphony agents for this repo
    #[arg(long, default_value_t = 1)]
    max_concurrent_agents: usize,

    /// Poll interval in milliseconds
    #[arg(long, default_value_t = 5_000)]
    poll_interval_ms: u64,

    /// Model passed to Codex app-server through quota routing
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort passed to Codex app-server through quota routing
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Linear state name used when work begins
    #[arg(long, default_value = "In Progress")]
    in_progress_state: String,

    /// Linear terminal state name used after successful landing
    #[arg(long, default_value = "Done")]
    done_state: String,

    /// Optional non-active state name used when the worker encounters a true external blocker
    #[arg(long)]
    blocked_state: Option<String>,
}

#[derive(Args, Clone)]
struct SymphonyRunArgs {
    /// Repository root whose Symphony workflow should be rendered and run. Defaults to the current git repo root.
    #[arg(long)]
    repo_root: Option<PathBuf>,

    /// Linear project slug used by Symphony for this repo. Defaults to the generated WORKFLOW.md after first setup.
    #[arg(long)]
    project_slug: Option<String>,

    /// Output path for the generated WORKFLOW.md
    #[arg(long)]
    output: Option<PathBuf>,

    /// Root directory where Symphony should create per-issue workspaces for this repo
    #[arg(long)]
    workspace_root: Option<PathBuf>,

    /// Branch that the generated workflow should treat as the integration branch
    #[arg(long)]
    base_branch: Option<String>,

    /// Maximum concurrent Symphony agents for this repo
    #[arg(long, default_value_t = 1)]
    max_concurrent_agents: usize,

    /// Poll interval in milliseconds
    #[arg(long, default_value_t = 5_000)]
    poll_interval_ms: u64,

    /// Model passed to Codex app-server through quota routing
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort passed to Codex app-server through quota routing
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Sync Linear issues from IMPLEMENTATION_PLAN.md before launching Symphony
    #[arg(long)]
    sync_first: bool,

    /// Linear state name used for newly created or reopened issues when --sync-first is set
    #[arg(long, default_value = "Todo")]
    todo_state: String,

    /// Codex model used for sync planning analysis when --sync-first is set
    #[arg(long, default_value = "gpt-5.5")]
    planner_model: String,

    /// Codex reasoning effort used for sync planning analysis when --sync-first is set
    #[arg(long, default_value = "high")]
    planner_reasoning_effort: String,

    /// Codex executable used for sync planning analysis when --sync-first is set
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Disable the Codex planner and fall back to deterministic dependency parsing only when --sync-first is set
    #[arg(long)]
    no_ai_planner: bool,

    /// Linear state name used when work begins
    #[arg(long, default_value = "In Progress")]
    in_progress_state: String,

    /// Linear terminal state name used after successful landing
    #[arg(long, default_value = "Done")]
    done_state: String,

    /// Optional non-active state name used when the worker encounters a true external blocker
    #[arg(long)]
    blocked_state: Option<String>,

    /// Local Symphony Elixir root directory. Overrides AUTODEV_SYMPHONY_ROOT; required when the env var is unset.
    #[arg(long, value_name = "PATH")]
    symphony_root: Option<PathBuf>,

    /// Directory where Symphony should write its own log files
    #[arg(long)]
    logs_root: Option<PathBuf>,

    /// Optional Symphony dashboard port
    #[arg(long)]
    port: Option<u16>,
}

#[derive(Subcommand, Clone)]
enum QuotaSubcommand {
    /// Show quota status for all accounts
    Status,
    /// Select the primary account and activate its credentials for the provider
    Select(QuotaSelectArgs),
    /// Manage accounts
    Accounts(AccountsSubcommand),
    /// Force-clear exhausted status (all accounts, or one by name)
    Reset(QuotaResetArgs),
    /// Select the best account and launch the provider CLI
    Open(QuotaOpenArgs),
}

#[derive(Args, Clone)]
struct QuotaOpenArgs {
    /// Provider: "claude" or "codex"
    provider: String,
    /// Arguments passed through to the provider CLI
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Args, Clone)]
struct QuotaSelectArgs {
    /// Provider: "claude" or "codex"
    provider: String,
}

#[derive(Args, Clone)]
struct QuotaResetArgs {
    /// Account name to reset. Omit to reset all.
    name: Option<String>,
}

#[derive(Args, Clone)]
struct AccountsSubcommand {
    #[command(subcommand)]
    command: AccountsCommand,
}

#[derive(Subcommand, Clone)]
enum AccountsCommand {
    /// Add a new account profile
    Add(AccountsAddArgs),
    /// List all configured accounts
    List,
    /// Remove an account profile
    Remove(AccountsRemoveArgs),
    /// Re-capture credentials from the current session into a profile
    Capture(AccountsCaptureArgs),
}

#[derive(Args, Clone)]
struct AccountsAddArgs {
    /// Account name (e.g., "work-codex-1")
    name: String,
    /// Provider: "claude" or "codex"
    provider: String,
}

#[derive(Args, Clone)]
struct AccountsRemoveArgs {
    /// Account name to remove
    name: String,
    /// Skip confirmation prompt
    #[arg(long)]
    force: bool,
}

#[derive(Args, Clone)]
struct AccountsCaptureArgs {
    /// Account name to update credentials for
    name: String,
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum HardeningProfile {
    Fast,
    Balanced,
    MaxQuality,
}

#[derive(Args, Clone)]
pub(crate) struct CorpusArgs {
    /// Planning corpus root. Defaults to <repo>/genesis
    #[arg(long)]
    planning_root: Option<PathBuf>,

    /// Seed corpus generation with a product idea and run an office-hours-style shaping pass
    #[arg(long)]
    idea: Option<String>,

    /// Steer corpus attention toward specific repo concerns without skipping the full sweep
    #[arg(long)]
    focus: Option<String>,

    /// Additional repository roots that corpus must inspect as reference material
    #[arg(long = "reference-repo")]
    reference_repos: Vec<PathBuf>,

    /// Model used for corpus authoring
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort used for corpus authoring
    #[arg(long, default_value = "xhigh")]
    reasoning_effort: String,

    /// Model used for the independent Codex review pass after corpus authoring
    #[arg(long, default_value = "gpt-5.5")]
    codex_review_model: String,

    /// Reasoning effort used for the independent Codex review pass
    #[arg(long, default_value = "xhigh")]
    codex_review_effort: String,

    /// Codex executable to invoke for the independent review pass
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Skip the independent Codex review pass
    #[arg(long)]
    skip_codex_review: bool,

    /// Sanitize and verify the existing planning corpus without invoking authoring or review models
    #[arg(long)]
    verify_only: bool,

    /// Maximum Claude turns when an explicit Claude authoring model is selected
    #[arg(long, default_value_t = 200)]
    max_turns: usize,

    /// Maximum parallel subagents to encourage during corpus authoring
    #[arg(long, default_value_t = 5)]
    parallelism: usize,

    /// Preview the corpus pass without invoking the model
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Clone)]
pub(crate) struct GenerationArgs {
    /// Planning corpus root. Defaults to <repo>/genesis
    #[arg(long)]
    planning_root: Option<PathBuf>,

    /// Generated output directory. Defaults to <repo>/gen-<timestamp>
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Model used for spec and plan authoring
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort used for spec and plan authoring
    #[arg(long, default_value = "xhigh")]
    reasoning_effort: String,

    /// Model used for the independent Codex review pass after generation
    #[arg(long, default_value = "gpt-5.5")]
    codex_review_model: String,

    /// Reasoning effort used for the independent Codex review pass
    #[arg(long, default_value = "xhigh")]
    codex_review_effort: String,

    /// Codex executable to invoke for the independent review pass
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Skip the independent Codex review pass
    #[arg(long)]
    skip_codex_review: bool,

    /// Maximum Claude turns when an explicit Claude authoring model is selected
    #[arg(long, default_value_t = 200)]
    max_turns: usize,

    /// Maximum parallel subagents to encourage during generation
    #[arg(long, default_value_t = 5)]
    parallelism: usize,

    /// Skip spec regeneration and only refresh the plan inside an existing gen-* dir
    #[arg(long)]
    plan_only: bool,

    /// Write a reviewable gen-* snapshot without syncing root specs or the root plan
    #[arg(long, conflicts_with = "sync_only")]
    snapshot_only: bool,

    /// Skip authoring and only verify/sync an existing gen-* output dir
    #[arg(long)]
    sync_only: bool,
}

#[derive(Args, Clone)]
pub(crate) struct SuperArgs {
    /// Single high-level instruction for the production-grade workflow
    prompt: Option<String>,

    /// Planning corpus root. Defaults to <repo>/genesis
    #[arg(long)]
    planning_root: Option<PathBuf>,

    /// Generated output directory. Defaults to <repo>/gen-<timestamp>
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Seed corpus generation with product direction
    #[arg(long)]
    idea: Option<String>,

    /// Additional focus text to combine with the positional prompt
    #[arg(long)]
    focus: Option<String>,

    /// Additional repository roots that all planning phases may inspect as reference material
    #[arg(long = "reference-repo")]
    reference_repos: Vec<PathBuf>,

    /// Model used for corpus, generation, and super review gates
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort used for corpus, generation, and super review gates
    #[arg(long, default_value = "xhigh")]
    reasoning_effort: String,

    /// Codex executable used for Codex-backed phases
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Maximum Claude turns if an explicit Claude authoring model is selected
    #[arg(long, default_value_t = 200)]
    max_turns: usize,

    /// Maximum parallel subagents to encourage during corpus and generation
    #[arg(long, default_value_t = 8)]
    planning_parallelism: usize,

    /// Maximum concurrent `auto parallel` worker lanes
    #[arg(
        long = "threads",
        visible_alias = "max-concurrent-workers",
        default_value_t = 5
    )]
    max_concurrent_workers: usize,

    /// Stop `auto parallel` after this many successful lands. Default is unlimited.
    #[arg(long)]
    max_iterations: Option<usize>,

    /// Model used by implementation workers after the gates pass
    #[arg(long, default_value = "gpt-5.5")]
    worker_model: String,

    /// Reasoning effort used by implementation workers after the gates pass
    #[arg(long, default_value = "high")]
    worker_reasoning_effort: String,

    /// Branch that `auto parallel` is allowed to run on. Defaults to the repo's primary branch.
    #[arg(long)]
    branch: Option<String>,

    /// Skip launching `auto parallel` after the production-grade gates pass
    #[arg(long)]
    no_execute: bool,

    /// Skip the additional super-only model review gates, leaving corpus/gen controls in place
    #[arg(long)]
    skip_super_review: bool,

    /// Preview the planned super workflow without invoking models or launching workers
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Clone)]
pub(crate) struct BugArgs {
    /// Output directory for bug pipeline artifacts. Defaults to <repo>/bug
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Reuse existing bug artifacts and continue from the first incomplete or invalid phase output
    #[arg(long)]
    resume: bool,

    /// Execution preset. Explicit model/effort flags still win over the preset.
    #[arg(long, value_enum, default_value_t = HardeningProfile::Balanced)]
    profile: HardeningProfile,

    /// Maximum files per audit chunk
    #[arg(long, default_value_t = 24)]
    chunk_size: usize,

    /// Optional cap on how many chunks to process
    #[arg(long)]
    max_chunks: Option<usize>,

    /// Maximum concurrent read-only chunk pipelines before serial implementation begins
    #[arg(long, default_value_t = 4)]
    read_parallelism: usize,

    /// Stop after the verification review and summary generation
    #[arg(long)]
    report_only: bool,

    /// Allow the implementation and final review passes to run on a dirty worktree
    #[arg(long)]
    allow_dirty: bool,

    /// Preview the chunk plan without invoking any models
    #[arg(long)]
    dry_run: bool,

    /// Model for the initial finder pass
    #[arg(long, default_value = "gpt-5.5")]
    finder_model: String,

    /// Effort / variant for the initial finder pass
    #[arg(long, default_value = "high")]
    finder_effort: String,

    /// Model for the adversarial skeptic pass
    #[arg(long, default_value = "gpt-5.5")]
    skeptic_model: String,

    /// Effort / variant for the skeptic pass
    #[arg(long, default_value = "high")]
    skeptic_effort: String,

    /// Model for the implementation pass after review verification
    #[arg(long, default_value = "gpt-5.5")]
    fixer_model: String,

    /// Effort / variant for the implementation pass after review verification
    #[arg(long, default_value = "high")]
    fixer_effort: String,

    /// Model for the verification review pass
    #[arg(long, default_value = "gpt-5.5")]
    reviewer_model: String,

    /// Effort / variant for the verification review pass
    #[arg(long, default_value = "high")]
    reviewer_effort: String,

    /// Model for the final Codex review pass. This stays pinned to gpt-5.5.
    #[arg(long, default_value = "gpt-5.5")]
    finalizer_model: String,

    /// Effort / variant for the final Codex review pass. This stays pinned to high.
    #[arg(long, default_value = "high")]
    finalizer_effort: String,

    /// Codex executable to invoke for the finalizer / fallback path
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Legacy PI executable. Retained for explicit Kimi/PI opt-ins.
    #[arg(long = "pi-bin", visible_alias = "opencode-bin", default_value = "pi")]
    pi_bin: PathBuf,

    /// kimi-cli executable used for explicit Kimi model opt-ins.
    #[arg(long, default_value = "kimi-cli")]
    kimi_bin: PathBuf,

    /// Route explicit Kimi phases through `kimi-cli --yolo` instead of the legacy `pi` binary.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    use_kimi_cli: bool,
}

#[derive(Args, Clone)]
pub(crate) struct LoopArgs {
    /// Stop after this many successful loop iterations. Default is unlimited.
    #[arg(long)]
    max_iterations: Option<usize>,

    /// Optional override for the worker prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Model to use for the implementation worker
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort to pass through to the Codex worker
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Branch that the loop is allowed to run on. Defaults to the repo's primary branch.
    #[arg(long)]
    branch: Option<String>,

    /// Additional repository roots the loop worker may inspect or edit
    #[arg(long = "reference-repo")]
    reference_repos: Vec<PathBuf>,

    /// Auto-discover sibling git repos in the parent directory as reference repos
    #[arg(long)]
    include_siblings: bool,

    /// Directory for loop logs. Defaults to <repo>/.auto/loop
    #[arg(long)]
    run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Use Claude instead of Codex
    #[arg(long)]
    claude: bool,

    /// Maximum Claude turns (only used with --claude). Omit for unlimited.
    #[arg(long)]
    max_turns: Option<usize>,

    /// Maximum retries when Claude exits non-zero before bailing
    #[arg(long, default_value_t = 2)]
    max_retries: usize,
}

#[derive(Args, Clone)]
pub(crate) struct ParallelArgs {
    /// Optional action. `auto parallel status` prints the current tmux/lane health.
    #[arg(value_enum)]
    action: Option<ParallelAction>,

    /// Stop after this many successful parallel lands. Default is unlimited.
    #[arg(long)]
    max_iterations: Option<usize>,

    /// Maximum concurrent worker lanes.
    #[arg(
        long = "threads",
        visible_alias = "max-concurrent-workers",
        default_value_t = 5
    )]
    max_concurrent_workers: usize,

    /// Override CARGO_BUILD_JOBS for parallel workers. Defaults to a conservative automatic cap.
    #[arg(long)]
    cargo_build_jobs: Option<usize>,

    /// Cargo target layout for workers. `auto` uses lane-local targets for multi-lane Rust repos.
    #[arg(long = "cargo-target", value_enum, default_value = "auto")]
    cargo_target: ParallelCargoTarget,

    /// Optional override for the worker prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Model to use for the implementation worker
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort to pass through to the Codex worker
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Branch that the parallel executor is allowed to run on. Defaults to the repo's primary branch.
    #[arg(long)]
    branch: Option<String>,

    /// Additional repository roots the parallel worker may inspect as read-only context
    #[arg(long = "reference-repo")]
    reference_repos: Vec<PathBuf>,

    /// Auto-discover sibling git repos in the parent directory as reference repos
    #[arg(long)]
    include_siblings: bool,

    /// Directory for parallel executor logs. Defaults to <repo>/.auto/parallel
    #[arg(long)]
    run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Use Claude instead of Codex
    #[arg(long)]
    claude: bool,

    /// Maximum Claude turns (only used with --claude). Omit for unlimited.
    #[arg(long)]
    max_turns: Option<usize>,

    /// Maximum retries when Claude exits non-zero before bailing
    #[arg(long, default_value_t = 2)]
    max_retries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ParallelAction {
    /// Print host, tmux, and lane health for the current repo's parallel run.
    Status,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ParallelCargoTarget {
    /// Inherit CARGO_TARGET_DIR when set; otherwise use lane-local targets for multi-lane Rust repos.
    Auto,
    /// Force a shared target directory under .auto/parallel.
    Shared,
    /// Force one target directory per lane.
    Lane,
    /// Do not set CARGO_TARGET_DIR for workers.
    None,
}

#[derive(Args, Clone)]
pub(crate) struct ReviewArgs {
    /// Stop after this many successful review iterations. 0 means run until
    /// the review queue is empty.
    #[arg(long, default_value_t = 0)]
    max_iterations: usize,

    /// Number of REVIEW.md items to feed the reviewer per iteration. 0 means
    /// "all items in one call" (legacy behavior — brittle on large queues).
    #[arg(long, default_value_t = 5)]
    batch_size: usize,

    /// Optional override for the review prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Model to use for the review worker
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort to pass through to the Codex review worker
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Optional branch to require for the review loop; defaults to the currently checked-out branch
    #[arg(long)]
    branch: Option<String>,

    /// Additional repo roots the reviewer may inspect or edit beyond the queue repo.
    #[arg(long = "reference-repo")]
    reference_repos: Vec<PathBuf>,

    /// Auto-discover sibling git repos in the parent directory as reference repos.
    /// Enabled by default so `auto review` can reconcile queue items whose owned
    /// surfaces landed in sibling repos.
    #[arg(long, default_value_t = true)]
    include_siblings: bool,

    /// Directory for review logs. Defaults to <repo>/.auto/review
    #[arg(long)]
    run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Use Claude instead of Codex
    #[arg(long)]
    claude: bool,

    /// Maximum Claude turns (only used with --claude). Omit for unlimited.
    #[arg(long)]
    max_turns: Option<usize>,

    /// Build the per-iteration prompt, write it to the logs, and print the
    /// batch + live-tree block to stdout — but do not invoke the model.
    /// Useful for inspecting what will be sent before burning tokens.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Clone)]
pub(crate) struct StewardArgs {
    /// Directory for steward artifacts. Defaults to <repo>/steward
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Additional repo roots the steward may inspect. Use for the other side
    /// of a two-repo project (e.g. `--reference-repo ../bitino` when stewarding
    /// autonomy) so cross-repo contracts get audited in the same pass.
    #[arg(long = "reference-repo")]
    reference_repos: Vec<PathBuf>,

    /// Read-only mode. Produce the steward artifacts but never edit active
    /// planning files or specs.
    #[arg(long)]
    report_only: bool,

    /// Preview the steward prompt without invoking the model.
    #[arg(long)]
    dry_run: bool,

    /// Optional branch to require for the steward pass; defaults to the current branch.
    #[arg(long)]
    branch: Option<String>,

    /// Codex model for the first steward pass — writes drift + hinge + retire +
    /// hazard artifacts and promotes active plan/spec work.
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Codex reasoning effort for the first steward pass.
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Codex model for the finalizer pass — reviews the first pass's proposed
    /// edits against the live tree and applies the ones that hold.
    #[arg(long, default_value = "gpt-5.5")]
    finalizer_model: String,

    /// Codex finalizer reasoning effort.
    #[arg(long, default_value = "high")]
    finalizer_effort: String,

    /// Codex executable used by both steward passes.
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Skip the finalizer pass and stop after the first Codex pass writes its
    /// deliverables. Useful when you want a quick audit without the review-and-apply step.
    #[arg(long)]
    skip_finalizer: bool,
}

#[derive(Args, Clone)]
pub(crate) struct AuditArgs {
    /// Run the professional whole-repo audit pipeline: context engineering,
    /// per-file analysis, cross-file synthesis, crate remediation, final review,
    /// and optional primary-branch merge.
    #[arg(long)]
    everything: bool,

    /// Professional audit phase to run. Used only with --everything.
    #[arg(long, value_enum, default_value_t = AuditEverythingPhase::All)]
    everything_phase: AuditEverythingPhase,

    /// Resume an existing professional audit run. Defaults to the latest run
    /// recorded under .auto/audit-everything.
    #[arg(long)]
    everything_run_id: Option<String>,

    /// Root directory for professional audit runtime state.
    #[arg(long)]
    everything_run_root: Option<PathBuf>,

    /// Run the professional audit directly in the current checkout instead of
    /// creating a separate canonical audit worktree. New in-place runs require
    /// a clean checkout and commit the GO audit result directly in place.
    #[arg(long)]
    everything_in_place: bool,

    /// Maximum concurrent Codex workers for read-only professional audit phases.
    #[arg(long, default_value_t = 15)]
    everything_threads: usize,

    /// Maximum concurrent Codex remediation lanes. Each lane runs in an
    /// isolated worktree and the host lands commits back onto the audit branch.
    #[arg(long, default_value_t = 5)]
    remediation_threads: usize,

    /// Model for professional audit first-pass file analysis.
    #[arg(long, default_value = "gpt-5.5")]
    first_pass_model: String,

    /// Reasoning effort for professional audit first-pass file analysis.
    #[arg(long, default_value = "low")]
    first_pass_effort: String,

    /// Model for professional audit cross-file synthesis.
    #[arg(long, default_value = "gpt-5.5")]
    synthesis_model: String,

    /// Reasoning effort for professional audit cross-file synthesis.
    #[arg(long, default_value = "high")]
    synthesis_effort: String,

    /// Model for professional audit crate-by-crate remediation.
    #[arg(long, default_value = "gpt-5.5")]
    remediation_model: String,

    /// Reasoning effort for professional audit crate-by-crate remediation.
    #[arg(long, default_value = "high")]
    remediation_effort: String,

    /// Model for professional audit final review.
    #[arg(long, default_value = "gpt-5.5")]
    final_review_model: String,

    /// Reasoning effort for professional audit final review.
    #[arg(long, default_value = "xhigh")]
    final_review_effort: String,

    /// Number of final-review repair attempts to run when final review writes
    /// Verdict: NO-GO with actionable blockers.
    #[arg(long, default_value_t = 1)]
    final_review_retries: usize,

    /// Maximum file-quality rerating/remediation passes after a GO final
    /// review. Each pass rerates every first-pass file and runs per-file
    /// deliverables for files below 9/10 before the final review is rerun.
    #[arg(long, default_value_t = 10)]
    file_quality_passes: usize,

    /// Do not attempt to merge the professional audit branch back into the
    /// primary branch after final review, even if the final review is GO.
    #[arg(long)]
    no_everything_merge: bool,

    /// Operator-authored doctrine markdown. This is the judgment framework
    /// the auditor applies. The command stays agnostic — whatever you put
    /// here is what "clean" means for this repo. Required; will NOT be
    /// auto-generated (auto-gen defeats operator ownership).
    #[arg(long, default_value = "audit/DOCTRINE.md")]
    doctrine_prompt: PathBuf,

    /// Override the bundled verdicts / output rubric. Rare — changing this
    /// will break the Rust-side parser unless you also maintain the shape.
    #[arg(long)]
    rubric_prompt: Option<PathBuf>,

    /// Glob patterns to include. Repeatable. Defaults to sensible code +
    /// spec globs; override to scope a run (e.g. `--paths 'node/src/bridge_*'`).
    #[arg(long = "paths")]
    include_paths: Vec<String>,

    /// Glob patterns to exclude. Repeatable. Applied after `--paths`.
    #[arg(long = "exclude")]
    exclude_paths: Vec<String>,

    /// Cap the number of files audited this run. 0 means unlimited.
    /// Use to control cost on large codebases.
    #[arg(long, default_value_t = 0)]
    max_files: usize,

    /// Maximum concurrent workers for the legacy per-file audit first pass.
    /// The host still applies verdicts and writes the manifest centrally.
    #[arg(long = "audit-threads", alias = "threads", default_value_t = 15)]
    audit_threads: usize,

    /// Directory for audit artifacts. Defaults to <repo>/audit
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Resume mode. `resume` (default) picks up at first pending file;
    /// `fresh` archives the old manifest and starts over; `only-drifted`
    /// re-audits files whose content or doctrine hash changed.
    #[arg(long, value_enum, default_value_t = AuditResumeMode::Resume)]
    resume_mode: AuditResumeMode,

    /// Read-only. Write verdicts + manifest but never apply patches, append
    /// to WORKLIST.md, or commit.
    #[arg(long)]
    report_only: bool,

    /// Print the per-file prompt for the first pending file and exit.
    #[arg(long)]
    dry_run: bool,

    /// Optional branch to require.
    #[arg(long)]
    branch: Option<String>,

    /// Auditor model.
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Auditor reasoning effort / thinking.
    #[arg(long, default_value = "low")]
    reasoning_effort: String,

    /// Escalation model for DRIFT-LARGE / REFACTOR verdicts that write
    /// worklist entries. Codex gives a second-opinion on high-impact calls.
    #[arg(long, default_value = "gpt-5.5")]
    escalation_model: String,

    /// Escalation reasoning effort.
    #[arg(long, default_value = "high")]
    escalation_effort: String,

    /// Codex executable used for audit and escalation passes.
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// kimi-cli executable used for explicit Kimi model opt-ins.
    #[arg(long, default_value = "kimi-cli")]
    kimi_bin: PathBuf,

    /// Legacy PI binary retained for compatibility.
    #[arg(long = "pi-bin", default_value = "pi")]
    pi_bin: PathBuf,

    /// Route explicit Kimi audit models through `kimi-cli --yolo`.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    use_kimi_cli: bool,
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
pub(crate) struct QaArgs {
    /// Stop after this many successful QA iterations. Default is 1.
    #[arg(long, default_value_t = 1)]
    max_iterations: usize,

    /// Optional override for the QA prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Model to use for the QA worker
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort to pass through to the Codex QA worker
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Optional branch to require for the QA loop; defaults to the currently checked-out branch
    #[arg(long)]
    branch: Option<String>,

    /// Directory for QA logs. Defaults to <repo>/.auto/qa
    #[arg(long)]
    run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// QA depth. Quick focuses on critical/high issues, Standard adds medium issues, Exhaustive includes polish and cosmetic issues.
    #[arg(long, value_enum, default_value_t = QaTier::Standard)]
    tier: QaTier,
}

#[derive(Args, Clone)]
pub(crate) struct QaOnlyArgs {
    /// Optional override for the report-only QA prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Model to use for the QA report worker
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort to pass through to the Codex QA report worker
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Optional branch to require for the QA report; defaults to the currently checked-out branch
    #[arg(long)]
    branch: Option<String>,

    /// Directory for QA report logs. Defaults to <repo>/.auto/qa-only
    #[arg(long)]
    run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// QA depth. Quick focuses on critical/high issues, Standard adds medium issues, Exhaustive includes polish and cosmetic issues.
    #[arg(long, value_enum, default_value_t = QaTier::Standard)]
    tier: QaTier,
}

#[derive(Args, Clone)]
pub(crate) struct HealthArgs {
    /// Optional override for the health prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Model to use for the health worker
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort to pass through to the Codex health worker
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Optional branch to require for the health report; defaults to the currently checked-out branch
    #[arg(long)]
    branch: Option<String>,

    /// Directory for health logs. Defaults to <repo>/.auto/health
    #[arg(long)]
    run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,
}

#[derive(Args, Clone)]
pub(crate) struct ShipArgs {
    /// Stop after this many successful ship iterations. Default is 1.
    #[arg(long, default_value_t = 1)]
    max_iterations: usize,

    /// Optional override for the ship prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Model to use for the ship worker
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort to pass through to the Codex ship worker
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Optional branch to require for the ship loop; defaults to the currently checked-out branch
    #[arg(long)]
    branch: Option<String>,

    /// Optional explicit base branch for diff and PR targeting
    #[arg(long)]
    base_branch: Option<String>,

    /// Directory for ship logs. Defaults to <repo>/.auto/ship
    #[arg(long)]
    run_root: Option<PathBuf>,

    /// Bypass the pre-model release gate and record the operator reason in SHIP.md
    #[arg(long, value_name = "REASON")]
    bypass_release_gate: Option<String>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,
}

#[derive(Args, Clone)]
pub(crate) struct NemesisArgs {
    /// Optional override for the Nemesis prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Output directory for disposable Nemesis artifacts. Defaults to <repo>/nemesis
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Reuse valid nemesis artifacts and continue from the first missing or invalid phase
    #[arg(long)]
    resume: bool,

    /// Execution preset. Explicit model/effort flags still win over the preset.
    #[arg(long, value_enum, default_value_t = HardeningProfile::Balanced)]
    profile: HardeningProfile,

    /// Model for the initial Nemesis audit pass.
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Reasoning effort / variant for the initial Nemesis audit pass
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Model for the Nemesis synthesis pass.
    #[arg(long, default_value = "gpt-5.5")]
    reviewer_model: String,

    /// Reasoning effort / variant for the final Nemesis synthesis pass
    #[arg(long, default_value = "high")]
    reviewer_effort: String,

    /// Legacy opt-in for the Kimi audit model.
    #[arg(long, conflicts_with = "minimax")]
    kimi: bool,

    /// Opt back into the retired MiniMax audit model. Kept for operators who
    /// deliberately want a second-opinion run against legacy output.
    #[arg(long, conflicts_with = "kimi")]
    minimax: bool,

    /// Stop after audit and synthesis without running the implementation pass
    #[arg(long)]
    report_only: bool,

    /// Optional branch to require for the Nemesis implementation pass; defaults to the current branch
    #[arg(long)]
    branch: Option<String>,

    /// Preview the Nemesis run without invoking a model
    #[arg(long)]
    dry_run: bool,

    /// Model to use for the Nemesis implementation / fixer pass.
    #[arg(long, default_value = "gpt-5.5")]
    fixer_model: String,

    /// Reasoning effort / variant for the Nemesis implementation pass
    #[arg(long, default_value = "high")]
    fixer_effort: String,

    /// Model used by the final Codex review pass. Stays on gpt-5.5.
    #[arg(long, default_value = "gpt-5.5")]
    finalizer_model: String,

    /// Reasoning effort / variant for the Codex finalizer pass
    #[arg(long, default_value = "high")]
    finalizer_effort: String,

    /// Number of Nemesis auditor passes to run. 2+ passes surface more findings
    /// because each pass explores the codebase differently.
    #[arg(long, default_value_t = 1)]
    audit_passes: usize,

    /// Codex executable used for the finalizer + fallback path
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Legacy PI executable. Retained for explicit Kimi/PI opt-ins.
    #[arg(long = "pi-bin", visible_alias = "opencode-bin", default_value = "pi")]
    pi_bin: PathBuf,

    /// kimi-cli executable used for explicit Kimi model opt-ins.
    #[arg(long, default_value = "kimi-cli")]
    kimi_bin: PathBuf,

    /// Route explicit Kimi phases through `kimi-cli --yolo`.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    use_kimi_cli: bool,
}

#[derive(Args, Clone)]
pub(crate) struct BookArgs {
    /// Audit run id under audit/everything/<run-id>. Defaults to the latest
    /// run recorded by .auto/audit-everything/latest-run, then the newest
    /// directory under audit/everything.
    #[arg(long)]
    audit_run_id: Option<String>,

    /// Override the audit/everything root. Defaults to <repo>/audit/everything.
    #[arg(long)]
    audit_root: Option<PathBuf>,

    /// Override the CODEBASE-BOOK output directory. Defaults to
    /// <audit-root>/<run-id>/CODEBASE-BOOK.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Codex model used to rewrite the narrative book.
    #[arg(long, default_value = "gpt-5.5")]
    model: String,

    /// Codex reasoning effort used to rewrite the narrative book.
    #[arg(long, default_value = "xhigh")]
    reasoning_effort: String,

    /// Codex executable used for the book rewrite.
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// Print the generated book prompt and exit without invoking Codex.
    #[arg(long)]
    dry_run: bool,

    /// Skip the post-write quality review. By default `auto book` asks Codex
    /// to judge whether the book is deep enough for a junior developer to
    /// understand the codebase without reading source files.
    #[arg(long)]
    skip_quality_review: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Corpus(args) => generation::run_corpus(args).await,
        Command::Gen(args) => generation::run_gen(args).await,
        Command::Super(args) => super_command::run_super(args).await,
        Command::Reverse(args) => generation::run_reverse(args).await,
        Command::Bug(args) => bug_command::run_bug(args).await,
        Command::Loop(args) => loop_command::run_loop(args).await,
        Command::Parallel(args) => parallel_command::run_parallel(args).await,
        Command::Qa(args) => qa_command::run_qa(args).await,
        Command::QaOnly(args) => qa_only_command::run_qa_only(args).await,
        Command::Health(args) => health_command::run_health(args).await,
        Command::Book(args) => book_command::run_book(args).await,
        Command::Doctor(args) => doctor_command::run_doctor(args).await,
        Command::Review(args) => review_command::run_review(args).await,
        Command::Steward(args) => steward_command::run_steward(args).await,
        Command::Audit(args) => audit_command::run_audit(args).await,
        Command::Ship(args) => ship_command::run_ship(args).await,
        Command::Nemesis(args) => nemesis::run_nemesis(args).await,
        Command::Quota(args) => match args.command {
            QuotaSubcommand::Status => quota_status::run_status().await,
            QuotaSubcommand::Select(args) => {
                let provider: quota_config::Provider = args.provider.parse()?;
                quota_exec::run_quota_select(provider).await
            }
            QuotaSubcommand::Reset(args) => quota_status::run_reset(args.name.as_deref()),
            QuotaSubcommand::Open(args) => {
                let provider: quota_config::Provider = args.provider.parse()?;
                let code = quota_exec::run_quota_open(provider, &args.args).await?;
                std::process::exit(code);
            }
            QuotaSubcommand::Accounts(a) => match a.command {
                AccountsCommand::Add(args) => {
                    quota_accounts::run_accounts_add(&args.name, &args.provider)
                }
                AccountsCommand::List => quota_accounts::run_accounts_list(),
                AccountsCommand::Remove(args) => {
                    quota_accounts::run_accounts_remove(&args.name, args.force)
                }
                AccountsCommand::Capture(args) => quota_accounts::run_accounts_capture(&args.name),
            },
        },
        Command::Symphony(args) => symphony_command::run_symphony(args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, SymphonySubcommand};
    use clap::{CommandFactory, Parser};

    #[test]
    fn top_level_command_surface_matches_live_enum() {
        let expected = [
            "corpus", "gen", "super", "reverse", "bug", "loop", "parallel", "qa", "qa-only",
            "health", "book", "doctor", "review", "steward", "audit", "ship", "nemesis", "quota",
            "symphony",
        ];
        let cli_command = Cli::command();
        let actual = cli_command
            .get_subcommands()
            .map(|command| command.get_name().to_string())
            .collect::<Vec<_>>();

        assert_eq!(actual, expected);

        for command in expected {
            let help = Cli::try_parse_from(["auto", command, "--help"]);
            assert!(help.is_err(), "expected help output for auto {command}");
        }
    }

    #[test]
    fn symphony_run_does_not_sync_by_default() {
        let cli = Cli::try_parse_from(["auto", "symphony", "run"]).expect("cli parse");
        let Command::Symphony(args) = cli.command else {
            panic!("expected symphony command");
        };
        let SymphonySubcommand::Run(args) = args.command else {
            panic!("expected symphony run");
        };
        assert!(!args.sync_first);
    }

    #[test]
    fn symphony_run_accepts_sync_first_flag() {
        let cli =
            Cli::try_parse_from(["auto", "symphony", "run", "--sync-first"]).expect("cli parse");
        let Command::Symphony(args) = cli.command else {
            panic!("expected symphony command");
        };
        let SymphonySubcommand::Run(args) = args.command else {
            panic!("expected symphony run");
        };
        assert!(args.sync_first);
    }

    #[test]
    fn review_includes_siblings_by_default() {
        let cli = Cli::try_parse_from(["auto", "review"]).expect("cli parse");
        let Command::Review(args) = cli.command else {
            panic!("expected review command");
        };
        assert!(args.include_siblings);
    }

    #[test]
    fn doctor_command_is_parseable() {
        let cli = Cli::try_parse_from(["auto", "doctor"]).expect("cli parse");
        let Command::Doctor(_) = cli.command else {
            panic!("expected doctor command");
        };

        let help = match Cli::try_parse_from(["auto", "doctor", "--help"]) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected help output"),
        };
        assert!(help.contains("Usage: auto doctor"));
    }

    #[test]
    fn symphony_run_help_mentions_symphony_root_env() {
        let help = match Cli::try_parse_from(["auto", "symphony", "run", "--help"]) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected help output"),
        };

        assert!(help.contains("--symphony-root <PATH>"));
        assert!(help.contains("AUTODEV_SYMPHONY_ROOT"));
    }
}
