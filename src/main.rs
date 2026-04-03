mod bug_command;
mod codex_stream;
mod corpus;
mod generation;
mod loop_command;
mod nemesis;
mod review_command;
mod state;
mod util;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "auto",
    version,
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
    /// Reverse-engineer specs from code reality using genesis/ as supporting context
    Reverse(GenerationArgs),
    /// Run a chunked multi-pass bug-finding, invalidation, remediation, and review pipeline
    Bug(BugArgs),
    /// Run the single-worker implementation loop on main
    Loop(LoopArgs),
    /// Review completed work on main
    Review(ReviewArgs),
    /// Run a disposable Nemesis audit and append its outputs into root specs and plan
    Nemesis(NemesisArgs),
}

#[derive(Args, Clone)]
pub(crate) struct CorpusArgs {
    /// Planning corpus root. Defaults to <repo>/genesis
    #[arg(long)]
    planning_root: Option<PathBuf>,

    /// Model used for corpus authoring
    #[arg(long, default_value = "claude-opus-4-6")]
    model: String,

    /// Maximum Claude turns
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
    #[arg(long, default_value = "claude-opus-4-6")]
    model: String,

    /// Maximum Claude turns
    #[arg(long, default_value_t = 200)]
    max_turns: usize,

    /// Maximum parallel subagents to encourage during generation
    #[arg(long, default_value_t = 5)]
    parallelism: usize,

    /// Skip spec regeneration and only refresh the plan inside an existing gen-* dir
    #[arg(long)]
    plan_only: bool,
}

#[derive(Args, Clone)]
pub(crate) struct BugArgs {
    /// Output directory for bug pipeline artifacts. Defaults to <repo>/bug
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Maximum files per audit chunk
    #[arg(long, default_value_t = 24)]
    chunk_size: usize,

    /// Optional cap on how many chunks to process
    #[arg(long)]
    max_chunks: Option<usize>,

    /// Stop after skeptical validation and summary generation
    #[arg(long)]
    report_only: bool,

    /// Allow remediation to run on a dirty worktree
    #[arg(long)]
    allow_dirty: bool,

    /// Preview the chunk plan without invoking any models
    #[arg(long)]
    dry_run: bool,

    /// Model for the initial finder pass
    #[arg(long, default_value = "minimax/MiniMax-M2.7-highspeed")]
    finder_model: String,

    /// Effort / variant for the initial finder pass
    #[arg(long, default_value = "high")]
    finder_effort: String,

    /// Model for the adversarial skeptic pass
    #[arg(long, default_value = "kimi")]
    skeptic_model: String,

    /// Effort / variant for the skeptic pass
    #[arg(long, default_value = "high")]
    skeptic_effort: String,

    /// Model for the remediation pass
    #[arg(long, default_value = "minimax/MiniMax-M2.7-highspeed")]
    fixer_model: String,

    /// Effort / variant for the remediation pass
    #[arg(long, default_value = "high")]
    fixer_effort: String,

    /// Model for the remediation review pass
    #[arg(long, default_value = "kimi")]
    reviewer_model: String,

    /// Effort / variant for the remediation review pass
    #[arg(long, default_value = "high")]
    reviewer_effort: String,

    /// Codex executable to invoke for non-OpenCode models
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// OpenCode executable to invoke for MiniMax/Kimi models
    #[arg(long, default_value = "opencode")]
    opencode_bin: PathBuf,
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
    #[arg(long, default_value = "gpt-5.4")]
    model: String,

    /// Reasoning effort to pass through to the Codex worker
    #[arg(long, default_value = "xhigh")]
    reasoning_effort: String,

    /// Branch that the loop is allowed to run on
    #[arg(long, default_value = "main")]
    branch: String,

    /// Directory for loop logs. Defaults to <repo>/.auto/loop
    #[arg(long)]
    run_root: Option<PathBuf>,

    /// Codex executable to invoke
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,
}

#[derive(Args, Clone)]
pub(crate) struct ReviewArgs {
    /// Stop after this many successful review iterations. Default is 1.
    #[arg(long, default_value_t = 1)]
    max_iterations: usize,

    /// Optional override for the review prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Model to use for the review worker
    #[arg(long, default_value = "gpt-5.4")]
    model: String,

    /// Reasoning effort to pass through to the Codex review worker
    #[arg(long, default_value = "xhigh")]
    reasoning_effort: String,

    /// Optional branch to require for the review loop; defaults to the currently checked-out branch
    #[arg(long)]
    branch: Option<String>,

    /// Directory for review logs. Defaults to <repo>/.auto/review
    #[arg(long)]
    run_root: Option<PathBuf>,

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

    /// Model to use for the Nemesis run. Values like `minimax` or `kimi` automatically use OpenCode.
    #[arg(long, default_value = "gpt-5.4")]
    model: String,

    /// Reasoning effort / variant for the Nemesis backend
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Use OpenCode with the Kimi 2.5 model instead of Codex
    #[arg(long, conflicts_with = "minimax")]
    kimi: bool,

    /// Use OpenCode with the MiniMax M2.5 model instead of Codex
    #[arg(long, conflicts_with = "kimi")]
    minimax: bool,

    /// Preview the Nemesis run without invoking a model
    #[arg(long)]
    dry_run: bool,

    /// Codex executable to invoke for the default backend
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// OpenCode executable to invoke for the Kimi/MiniMax backends
    #[arg(long, default_value = "opencode")]
    opencode_bin: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Corpus(args) => generation::run_corpus(args).await,
        Command::Gen(args) => generation::run_gen(args).await,
        Command::Reverse(args) => generation::run_reverse(args).await,
        Command::Bug(args) => bug_command::run_bug(args).await,
        Command::Loop(args) => loop_command::run_loop(args).await,
        Command::Review(args) => review_command::run_review(args).await,
        Command::Nemesis(args) => nemesis::run_nemesis(args).await,
    }
}
