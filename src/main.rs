mod corpus;
mod generation;
mod loop_command;
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
    /// Run the single-worker implementation loop on main
    Loop(LoopArgs),
    /// Review completed work on main
    Review(ReviewArgs),
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
    #[arg(long, default_value = "claude-opus-4-6")]
    model: String,

    /// Branch that the review loop is allowed to run on
    #[arg(long, default_value = "main")]
    branch: String,

    /// Directory for review logs. Defaults to <repo>/.auto/review
    #[arg(long)]
    run_root: Option<PathBuf>,

    /// Claude executable to invoke
    #[arg(long, default_value = "claude")]
    claude_bin: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Corpus(args) => generation::run_corpus(args).await,
        Command::Gen(args) => generation::run_gen(args).await,
        Command::Reverse(args) => generation::run_reverse(args).await,
        Command::Loop(args) => loop_command::run_loop(args).await,
        Command::Review(args) => review_command::run_review(args).await,
    }
}
