use std::path::PathBuf;

use clap::{Args, ValueEnum};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum HardeningProfile {
    Fast,
    Balanced,
    MaxQuality,
}

#[derive(Args, Clone)]
pub(crate) struct BugArgs {
    /// Output directory for bug pipeline artifacts. Defaults to <repo>/bug
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,

    /// Reuse existing bug artifacts and continue from the first incomplete or invalid phase output
    #[arg(long)]
    pub(crate) resume: bool,

    /// Execution preset. Explicit model/effort flags still win over the preset.
    #[arg(long, value_enum, default_value_t = HardeningProfile::Balanced)]
    pub(crate) profile: HardeningProfile,

    /// Maximum files per audit chunk
    #[arg(long, default_value_t = 24)]
    pub(crate) chunk_size: usize,

    /// Optional cap on how many chunks to process
    #[arg(long)]
    pub(crate) max_chunks: Option<usize>,

    /// Maximum concurrent read-only chunk pipelines before serial implementation begins
    #[arg(long, default_value_t = 4)]
    pub(crate) read_parallelism: usize,

    /// Stop after the verification review and summary generation
    #[arg(long)]
    pub(crate) report_only: bool,

    /// Allow the implementation and final review passes to run on a dirty worktree
    #[arg(long)]
    pub(crate) allow_dirty: bool,

    /// Preview the chunk plan without invoking any models
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Model for the initial finder pass
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) finder_model: String,

    /// Effort / variant for the initial finder pass
    #[arg(long, default_value = "low")]
    pub(crate) finder_effort: String,

    /// Model for the adversarial skeptic pass
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) skeptic_model: String,

    /// Effort / variant for the skeptic pass
    #[arg(long, default_value = "low")]
    pub(crate) skeptic_effort: String,

    /// Model for the implementation pass after review verification
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) fixer_model: String,

    /// Effort / variant for the implementation pass after review verification
    #[arg(long, default_value = "high")]
    pub(crate) fixer_effort: String,

    /// Model for the verification review pass
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) reviewer_model: String,

    /// Effort / variant for the verification review pass
    #[arg(long, default_value = "high")]
    pub(crate) reviewer_effort: String,

    /// Model for the final Codex review pass. This stays pinned to gpt-5.5.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) finalizer_model: String,

    /// Effort / variant for the final Codex review pass. This stays pinned to high.
    #[arg(long, default_value = "high")]
    pub(crate) finalizer_effort: String,

    /// Codex executable to invoke for the finalizer / fallback path
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Legacy PI executable. Retained for explicit Kimi/PI opt-ins.
    #[arg(long = "pi-bin", visible_alias = "opencode-bin", default_value = "pi")]
    pub(crate) pi_bin: PathBuf,

    /// kimi-cli executable used for explicit Kimi model opt-ins.
    #[arg(long, default_value = "kimi-cli")]
    pub(crate) kimi_bin: PathBuf,

    /// Route explicit Kimi phases through `kimi-cli --yolo` instead of the legacy `pi` binary.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub(crate) use_kimi_cli: bool,
}

#[derive(Args, Clone)]
pub(crate) struct LoopArgs {
    /// Stop after this many successful loop iterations. Default is unlimited.
    #[arg(long)]
    pub(crate) max_iterations: Option<usize>,

    /// Optional override for the worker prompt template
    #[arg(long)]
    pub(crate) prompt_file: Option<PathBuf>,

    /// Model to use for the implementation worker
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort to pass through to the Codex worker
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Branch that the loop is allowed to run on. Defaults to the repo's primary branch.
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Additional repository roots the loop worker may inspect or edit
    #[arg(long = "reference-repo")]
    pub(crate) reference_repos: Vec<PathBuf>,

    /// Auto-discover sibling git repos in the parent directory as reference repos
    #[arg(long)]
    pub(crate) include_siblings: bool,

    /// Directory for loop logs. Defaults to <repo>/.auto/loop
    #[arg(long)]
    pub(crate) run_root: Option<PathBuf>,

    /// Codex executable for Codex-backed phases. Kimi/MiniMax model aliases use kimi-cli/pi discovery.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Use Claude instead of Codex
    #[arg(long)]
    pub(crate) claude: bool,

    /// Maximum Claude turns (only used with --claude). Omit for unlimited.
    #[arg(long)]
    pub(crate) max_turns: Option<usize>,

    /// Maximum retries when Claude exits non-zero before bailing
    #[arg(long, default_value_t = 2)]
    pub(crate) max_retries: usize,
}

#[derive(Args, Clone)]
pub(crate) struct ParallelArgs {
    /// Optional action. `auto parallel status` prints the current tmux/lane health.
    #[arg(value_enum)]
    pub(crate) action: Option<ParallelAction>,

    /// For `auto parallel receipt-backfill`: synthesize missing REVIEW.md handoffs.
    #[arg(long)]
    pub(crate) apply_receipt_backfill_handoffs: bool,

    /// Stop after this many successful parallel lands. Default is unlimited.
    #[arg(long)]
    pub(crate) max_iterations: Option<usize>,

    /// Maximum concurrent worker lanes.
    #[arg(
        long = "threads",
        visible_alias = "max-concurrent-workers",
        default_value_t = 5
    )]
    pub(crate) max_concurrent_workers: usize,

    /// Override CARGO_BUILD_JOBS for parallel workers. Defaults to a conservative automatic cap.
    #[arg(long)]
    pub(crate) cargo_build_jobs: Option<usize>,

    /// Cargo target layout for workers. `auto` uses lane-local targets for multi-lane Rust repos.
    #[arg(long = "cargo-target", value_enum, default_value = "auto")]
    pub(crate) cargo_target: ParallelCargoTarget,

    /// Optional override for the worker prompt template
    #[arg(long)]
    pub(crate) prompt_file: Option<PathBuf>,

    /// Model to use for the implementation worker
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort to pass through to the Codex worker
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Branch that the parallel executor is allowed to run on. Defaults to the repo's primary branch.
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Additional repository roots the parallel worker may inspect as read-only context
    #[arg(long = "reference-repo")]
    pub(crate) reference_repos: Vec<PathBuf>,

    /// Auto-discover sibling git repos in the parent directory as reference repos
    #[arg(long)]
    pub(crate) include_siblings: bool,

    /// Directory for parallel executor logs. Defaults to <repo>/.auto/parallel
    #[arg(long)]
    pub(crate) run_root: Option<PathBuf>,

    /// Codex executable for Codex-backed phases. Kimi/MiniMax model aliases use kimi-cli/pi discovery.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Use Claude instead of Codex
    #[arg(long)]
    pub(crate) claude: bool,

    /// Maximum Claude turns (only used with --claude). Omit for unlimited.
    #[arg(long)]
    pub(crate) max_turns: Option<usize>,

    /// Maximum retries when Claude exits non-zero before bailing
    #[arg(long, default_value_t = 2)]
    pub(crate) max_retries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ParallelAction {
    /// Print host, tmux, and lane health for the current repo's parallel run.
    Status,
    /// Write a no-model plan for closing historical receipt drift.
    ReceiptBackfill,
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
    pub(crate) max_iterations: usize,

    /// Number of REVIEW.md items to feed the reviewer per iteration. 0 means
    /// "all items in one call" (legacy behavior — brittle on large queues).
    #[arg(long, default_value_t = 5)]
    pub(crate) batch_size: usize,

    /// Optional override for the review prompt template
    #[arg(long)]
    pub(crate) prompt_file: Option<PathBuf>,

    /// Model to use for the review worker
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort to pass through to the review worker
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Optional branch to require for the review loop; defaults to the currently checked-out branch
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Additional repo roots the reviewer may inspect or edit beyond the queue repo.
    #[arg(long = "reference-repo")]
    pub(crate) reference_repos: Vec<PathBuf>,

    /// Auto-discover sibling git repos in the parent directory as reference repos.
    /// Enabled by default so `auto review` can reconcile queue items whose owned
    /// surfaces landed in sibling repos.
    #[arg(long, default_value_t = true)]
    pub(crate) include_siblings: bool,

    /// Directory for review logs. Defaults to <repo>/.auto/review
    #[arg(long)]
    pub(crate) run_root: Option<PathBuf>,

    /// Codex executable for Codex-backed phases. Kimi/MiniMax model aliases use kimi-cli/pi discovery.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Use Claude instead of Codex
    #[arg(long)]
    pub(crate) claude: bool,

    /// Maximum Claude turns (only used with --claude). Omit for unlimited.
    #[arg(long)]
    pub(crate) max_turns: Option<usize>,

    /// Build the per-iteration prompt, write it to the logs, and print the
    /// batch + live-tree block to stdout — but do not invoke the model.
    /// Useful for inspecting what will be sent before burning tokens.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Args, Clone)]
pub(crate) struct NemesisArgs {
    /// Optional override for the Nemesis prompt template
    #[arg(long)]
    pub(crate) prompt_file: Option<PathBuf>,

    /// Output directory for disposable Nemesis artifacts. Defaults to <repo>/nemesis
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,

    /// Reuse valid nemesis artifacts and continue from the first missing or invalid phase
    #[arg(long)]
    pub(crate) resume: bool,

    /// Execution preset. Explicit model/effort flags still win over the preset.
    #[arg(long, value_enum, default_value_t = HardeningProfile::Balanced)]
    pub(crate) profile: HardeningProfile,

    /// Model for the initial Nemesis audit pass.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort / variant for the initial Nemesis audit pass
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Model for the Nemesis synthesis pass.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) reviewer_model: String,

    /// Reasoning effort / variant for the final Nemesis synthesis pass
    #[arg(long, default_value = "high")]
    pub(crate) reviewer_effort: String,

    /// Legacy opt-in for the Kimi audit model.
    #[arg(long, conflicts_with = "minimax")]
    pub(crate) kimi: bool,

    /// Opt back into the retired MiniMax audit model. Kept for operators who
    /// deliberately want a second-opinion run against legacy output.
    #[arg(long, conflicts_with = "kimi")]
    pub(crate) minimax: bool,

    /// Stop after audit and synthesis without running the implementation pass
    #[arg(long)]
    pub(crate) report_only: bool,

    /// Optional branch to require for the Nemesis implementation pass; defaults to the current branch
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Preview the Nemesis run without invoking a model
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Model to use for the Nemesis implementation / fixer pass.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) fixer_model: String,

    /// Reasoning effort / variant for the Nemesis implementation pass
    #[arg(long, default_value = "high")]
    pub(crate) fixer_effort: String,

    /// Model used by the final Codex review pass. Stays on gpt-5.5.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) finalizer_model: String,

    /// Reasoning effort / variant for the Codex finalizer pass
    #[arg(long, default_value = "high")]
    pub(crate) finalizer_effort: String,

    /// Number of Nemesis auditor passes to run. 2+ passes surface more findings
    /// because each pass explores the codebase differently.
    #[arg(long, default_value_t = 1)]
    pub(crate) audit_passes: usize,

    /// Codex executable used for the finalizer + fallback path
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Legacy PI executable. Retained for explicit Kimi/PI opt-ins.
    #[arg(long = "pi-bin", visible_alias = "opencode-bin", default_value = "pi")]
    pub(crate) pi_bin: PathBuf,

    /// kimi-cli executable used for explicit Kimi model opt-ins.
    #[arg(long, default_value = "kimi-cli")]
    pub(crate) kimi_bin: PathBuf,

    /// Route explicit Kimi phases through `kimi-cli --yolo`.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub(crate) use_kimi_cli: bool,
}
