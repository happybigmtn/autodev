use std::path::PathBuf;

use clap::Args;

#[derive(Args, Clone)]
pub(crate) struct CorpusArgs {
    /// Planning corpus root. Defaults to <repo>/genesis
    #[arg(long)]
    pub(crate) planning_root: Option<PathBuf>,

    /// Seed corpus generation with a product idea and run an office-hours-style shaping pass
    #[arg(long)]
    pub(crate) idea: Option<String>,

    /// Steer corpus attention toward specific repo concerns without skipping the full sweep
    #[arg(long)]
    pub(crate) focus: Option<String>,

    /// Additional repository roots that corpus must inspect as reference material
    #[arg(long = "reference-repo")]
    pub(crate) reference_repos: Vec<PathBuf>,

    /// Model used for corpus authoring
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort used for corpus authoring
    #[arg(long, default_value = "xhigh")]
    pub(crate) reasoning_effort: String,

    /// Model used for the independent review pass after corpus authoring
    #[arg(long, visible_alias = "review-model", default_value = "gpt-5.5")]
    pub(crate) codex_review_model: String,

    /// Reasoning effort used for the independent review pass
    #[arg(long, visible_alias = "review-effort", default_value = "xhigh")]
    pub(crate) codex_review_effort: String,

    /// Codex executable for Codex-backed phases. Kimi/MiniMax model aliases use kimi-cli/pi discovery.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// gbrain executable used for automatic shared-memory context collection
    #[arg(long, default_value = "gbrain")]
    pub(crate) gbrain_bin: PathBuf,

    /// Do not auto-load gbrain shared-memory context into the corpus prompt
    #[arg(long)]
    pub(crate) no_gbrain_context: bool,

    /// Skip the independent review pass
    #[arg(long)]
    pub(crate) skip_codex_review: bool,

    /// Sanitize and verify the existing planning corpus without invoking authoring or review models
    #[arg(long)]
    pub(crate) verify_only: bool,

    /// Maximum Claude turns when an explicit Claude authoring model is selected
    #[arg(long, default_value_t = 200)]
    pub(crate) max_turns: usize,

    /// Maximum parallel subagents to encourage during corpus authoring
    #[arg(long, default_value_t = 5)]
    pub(crate) parallelism: usize,

    /// Preview the corpus pass without invoking the model
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Args, Clone)]
pub(crate) struct GenerationArgs {
    /// Planning corpus root. Defaults to <repo>/genesis
    #[arg(long)]
    pub(crate) planning_root: Option<PathBuf>,

    /// Generated output directory. Defaults to <repo>/gen-<timestamp>
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,

    /// Model used for spec and plan authoring
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort used for spec and plan authoring
    #[arg(long, default_value = "xhigh")]
    pub(crate) reasoning_effort: String,

    /// Model used for the independent review pass after generation
    #[arg(long, visible_alias = "review-model", default_value = "gpt-5.5")]
    pub(crate) codex_review_model: String,

    /// Reasoning effort used for the independent review pass
    #[arg(long, visible_alias = "review-effort", default_value = "xhigh")]
    pub(crate) codex_review_effort: String,

    /// Codex executable for Codex-backed phases. Kimi/MiniMax model aliases use kimi-cli/pi discovery.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// gbrain executable used for automatic shared-memory context collection
    #[arg(long, default_value = "gbrain")]
    pub(crate) gbrain_bin: PathBuf,

    /// Do not auto-load gbrain shared-memory context into the spec and plan prompts
    #[arg(long)]
    pub(crate) no_gbrain_context: bool,

    /// Skip the independent review pass
    #[arg(long)]
    pub(crate) skip_codex_review: bool,

    /// Maximum Claude turns when an explicit Claude authoring model is selected
    #[arg(long, default_value_t = 200)]
    pub(crate) max_turns: usize,

    /// Maximum parallel subagents to encourage during generation
    #[arg(long, default_value_t = 5)]
    pub(crate) parallelism: usize,

    /// Skip spec regeneration and only refresh the plan inside an existing gen-* dir
    #[arg(long)]
    pub(crate) plan_only: bool,

    /// Write a reviewable gen-* snapshot without syncing root specs or the root plan
    #[arg(long, conflicts_with = "sync_only")]
    pub(crate) snapshot_only: bool,

    /// Skip authoring and only verify/sync an existing gen-* output dir
    #[arg(long)]
    pub(crate) sync_only: bool,
}

#[derive(Args, Clone)]
pub(crate) struct SpecArgs {
    /// High-level request to turn into a conformant spec and plan items
    pub(crate) prompt: Option<String>,

    /// Explicit spec output path. Defaults to specs/<ddmmyy-prompt-slug>.md
    #[arg(long)]
    pub(crate) spec_path: Option<PathBuf>,

    /// Implementation plan path. Defaults to IMPLEMENTATION_PLAN.md
    #[arg(long)]
    pub(crate) plan_path: Option<PathBuf>,

    /// Model used for spec authoring. Default `opus` resolves to the latest installed Claude Opus alias.
    #[arg(long, default_value = "opus")]
    pub(crate) model: String,

    /// Reasoning effort used for spec authoring
    #[arg(long, default_value = "xhigh")]
    pub(crate) reasoning_effort: String,

    /// Maximum Claude turns when the spec authoring model is Claude/Opus
    #[arg(long, default_value_t = 200)]
    pub(crate) max_turns: usize,

    /// Codex executable for Codex-backed phases. Kimi/MiniMax model aliases use kimi-cli/pi discovery.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Preview the generated prompt without invoking the model
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Args, Clone)]
pub(crate) struct DesignArgs {
    /// Optional focus prompt for the design pass
    pub(crate) prompt: Option<String>,

    /// Planning corpus root. Defaults to <repo>/genesis when present
    #[arg(long)]
    pub(crate) planning_root: Option<PathBuf>,

    /// Output directory for design artifacts. Defaults to <repo>/.auto/design/<timestamp>
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,

    /// Apply bounded repo edits to DESIGN.md, specs, or IMPLEMENTATION_PLAN.md instead of report-only artifacts
    #[arg(long)]
    pub(crate) apply: bool,

    /// Resolve design/runtime findings by adding queue-ready plan items, running auto parallel, and re-verifying until GO.
    #[arg(long)]
    pub(crate) resolve: bool,

    /// Maximum design audit/implementation/reverification passes for --resolve.
    /// In auto super, final NO-GO reports with dependency-ready repair tasks may
    /// receive bounded extra repair continuations instead of failing immediately.
    #[arg(long, default_value_t = 3)]
    pub(crate) resolve_passes: usize,

    /// Maximum concurrent implementation lanes when --resolve launches auto parallel.
    #[arg(
        long = "threads",
        visible_alias = "max-concurrent-workers",
        default_value_t = 5
    )]
    pub(crate) max_concurrent_workers: usize,

    /// Stop each --resolve auto parallel pass after this many successful lands. Default is unlimited.
    #[arg(long)]
    pub(crate) max_iterations: Option<usize>,

    /// Model used by implementation workers during --resolve.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) worker_model: String,

    /// Reasoning effort used by implementation workers during --resolve.
    #[arg(long, default_value = "high")]
    pub(crate) worker_reasoning_effort: String,

    /// Branch that --resolve auto parallel is allowed to run on. Defaults to the repo's primary branch.
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Additional repository roots implementation workers may inspect as read-only context during --resolve.
    #[arg(long = "reference-repo")]
    pub(crate) reference_repos: Vec<PathBuf>,

    /// Skip browser/runtime QA attempts and produce the design + contract audit only
    #[arg(long)]
    pub(crate) skip_qa: bool,

    /// Model used for design analysis
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort used for design analysis
    #[arg(long, default_value = "high")]
    pub(crate) reasoning_effort: String,

    /// Codex executable for Codex-backed phases. Kimi/MiniMax model aliases use kimi-cli/pi discovery.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Preview the design prompt without invoking the model
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Args, Clone)]
pub(crate) struct SuperArgs {
    /// Single high-level instruction for the CEO production-race workflow
    pub(crate) prompt: Option<String>,

    /// Planning corpus root. Defaults to <repo>/genesis
    #[arg(long)]
    pub(crate) planning_root: Option<PathBuf>,

    /// Generated output directory. Defaults to <repo>/gen-<timestamp>
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,

    /// Resume an existing auto super run from its .auto/super/<run-id> directory.
    /// Completed manifest stages are skipped; the first incomplete stage continues.
    #[arg(long)]
    pub(crate) resume: Option<PathBuf>,

    /// Seed corpus generation with product direction
    #[arg(long)]
    pub(crate) idea: Option<String>,

    /// Additional focus text to combine with the positional prompt
    #[arg(long)]
    pub(crate) focus: Option<String>,

    /// Additional repository roots that all planning phases may inspect as reference material
    #[arg(long = "reference-repo")]
    pub(crate) reference_repos: Vec<PathBuf>,

    /// Model used for corpus, generation, and super review gates
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Reasoning effort used for corpus, generation, and super review gates
    #[arg(long, default_value = "xhigh")]
    pub(crate) reasoning_effort: String,

    /// Codex executable for Codex-backed phases. Kimi/MiniMax model aliases use kimi-cli/pi discovery.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Maximum Claude turns if an explicit Claude authoring model is selected
    #[arg(long, default_value_t = 200)]
    pub(crate) max_turns: usize,

    /// Maximum parallel subagents to encourage during corpus and generation
    #[arg(long, default_value_t = 8)]
    pub(crate) planning_parallelism: usize,

    /// Maximum concurrent `auto parallel` worker lanes
    #[arg(
        long = "threads",
        visible_alias = "max-concurrent-workers",
        default_value_t = 5
    )]
    pub(crate) max_concurrent_workers: usize,

    /// Stop `auto parallel` after this many successful lands. Default is unlimited.
    #[arg(long)]
    pub(crate) max_iterations: Option<usize>,

    /// Model used by implementation workers after the gates pass
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) worker_model: String,

    /// Reasoning effort used by implementation workers after the gates pass
    #[arg(long, default_value = "high")]
    pub(crate) worker_reasoning_effort: String,

    /// Branch that `auto parallel` is allowed to run on. Defaults to the repo's primary branch.
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Skip launching `auto parallel` after the production-race gates pass
    #[arg(long)]
    pub(crate) no_execute: bool,

    /// Skip the CEO functional review and execution gates, leaving corpus/gen controls in place
    #[arg(long)]
    pub(crate) skip_super_review: bool,

    /// Skip the design perfection gate before functional reviews and generation
    #[arg(long)]
    pub(crate) skip_design: bool,

    /// Maximum design repair passes before auto super starts bounded final NO-GO recovery.
    #[arg(long, default_value_t = 3)]
    pub(crate) design_resolve_passes: usize,

    /// Run `auto audit --everything` after gen, then harvest its findings
    /// into IMPLEMENTATION_PLAN.md so the parallel stage actually addresses
    /// the issues the audit identifies. The audit runs with the same model
    /// stack that powers the rest of super; failure rounds are retried per
    /// `--audit-first-pass-retries`.
    #[arg(long)]
    pub(crate) with_audit: bool,

    /// Maximum concurrent first-pass / synthesis Codex workers when
    /// --with-audit is set. Same semantics as `auto audit --everything-threads`.
    #[arg(long, default_value_t = 10)]
    pub(crate) audit_threads: usize,

    /// Number of retry rounds for files that fail the audit's first pass
    /// when --with-audit is set. Each round only re-runs files still missing
    /// their `analysis.md` artifact.
    #[arg(long, default_value_t = 3)]
    pub(crate) audit_first_pass_retries: usize,

    /// Reuse an existing audit run-id under `.auto/audit-everything/` instead
    /// of starting a new one. Use to resume a partial audit when --with-audit
    /// is set; otherwise super creates a fresh run-id per invocation.
    #[arg(long)]
    pub(crate) audit_run_id: Option<String>,

    /// Preview the planned super workflow without invoking models or launching workers
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Args, Clone)]
pub(crate) struct AuditHarvestArgs {
    /// `auto audit --everything` run-id to harvest. Defaults to the latest
    /// run under `.auto/audit-everything/`.
    #[arg(long)]
    pub(crate) run_id: Option<String>,

    /// Codex model used to translate findings into IMPLEMENTATION_PLAN.md rows.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Codex reasoning effort.
    #[arg(long, default_value = "xhigh")]
    pub(crate) reasoning_effort: String,

    /// Codex executable.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Maximum number of findings to harvest, ranked by lowest score first.
    /// 0 means no cap. Each finding is compressed to its actionable subset
    /// (path, score, summary, recommended_actions, top deletion candidate)
    /// before going to codex, so thousands of findings fit in context.
    #[arg(long, default_value_t = 0)]
    pub(crate) max_findings: usize,

    /// Inclusive minimum audit score to include. Use with `--score-max` to
    /// scope a harvest pass to a specific cohort (e.g. `--score-min 0
    /// --score-max 7` for the acute drift, then `--score-min 8 --score-max
    /// 8` for the broad-mild drift).
    #[arg(long, default_value_t = 0)]
    pub(crate) score_min: i64,

    /// Inclusive maximum audit score to include. Defaults to 8 — score-9+
    /// files are already strong and don't need plan rows.
    #[arg(long, default_value_t = 8)]
    pub(crate) score_max: i64,
}

#[derive(Args, Clone)]
pub(crate) struct BookArgs {
    /// Audit run id under audit/everything/<run-id>. Defaults to the latest
    /// run recorded by .auto/audit-everything/latest-run, then the newest
    /// directory under audit/everything.
    #[arg(long)]
    pub(crate) audit_run_id: Option<String>,

    /// Override the audit/everything root. Defaults to <repo>/audit/everything.
    #[arg(long)]
    pub(crate) audit_root: Option<PathBuf>,

    /// Override the CODEBASE-BOOK output directory. Defaults to
    /// <audit-root>/<run-id>/CODEBASE-BOOK.
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,

    /// Codex model used to rewrite the narrative book.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Codex reasoning effort used to rewrite the narrative book.
    #[arg(long, default_value = "xhigh")]
    pub(crate) reasoning_effort: String,

    /// Codex executable used for the book rewrite.
    #[arg(long, default_value = "codex")]
    pub(crate) codex_bin: PathBuf,

    /// Print the generated book prompt and exit without invoking Codex.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Skip the post-write quality review. By default `auto book` asks Codex
    /// to judge whether the book is deep enough for a junior developer to
    /// understand the codebase without reading source files.
    #[arg(long)]
    pub(crate) skip_quality_review: bool,
}
