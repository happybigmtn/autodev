mod bug_command;
mod codex_exec;
mod codex_stream;
mod corpus;
mod generation;
mod health_command;
mod loop_command;
mod nemesis;
mod pi_backend;
mod qa_command;
mod qa_only_command;
mod review_command;
mod ship_command;
mod state;
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
    /// Reverse-engineer specs from code reality using genesis/ as supporting context
    Reverse(GenerationArgs),
    /// Run a chunked multi-pass bug-finding, invalidation, verification, and implementation pipeline
    Bug(BugArgs),
    /// Run the single-worker implementation loop on the repo's primary branch
    Loop(LoopArgs),
    /// Run a runtime QA and ship-readiness pass on the current branch
    Qa(QaArgs),
    /// Run a report-only runtime QA pass on the current branch
    QaOnly(QaOnlyArgs),
    /// Run a repo-wide quality and verification health report
    Health(HealthArgs),
    /// Review completed work on the current branch
    Review(ReviewArgs),
    /// Prepare the current branch to ship, push it, and open or refresh a PR when appropriate
    Ship(ShipArgs),
    /// Run a disposable Nemesis audit and append its outputs into root specs and plan
    Nemesis(NemesisArgs),
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

    /// Reuse existing bug artifacts and continue from the first incomplete or invalid phase output
    #[arg(long)]
    resume: bool,

    /// Maximum files per audit chunk
    #[arg(long, default_value_t = 24)]
    chunk_size: usize,

    /// Optional cap on how many chunks to process
    #[arg(long)]
    max_chunks: Option<usize>,

    /// Stop after the verification review and summary generation
    #[arg(long)]
    report_only: bool,

    /// Allow the final implementation pass to run on a dirty worktree
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

    /// Model for the final implementation pass. This stays pinned to gpt-5.4.
    #[arg(long, default_value = "gpt-5.4")]
    fixer_model: String,

    /// Effort / variant for the final implementation pass. This stays pinned to high.
    #[arg(long, default_value = "high")]
    fixer_effort: String,

    /// Model for the verification review pass
    #[arg(long, default_value = "kimi")]
    reviewer_model: String,

    /// Effort / variant for the verification review pass
    #[arg(long, default_value = "high")]
    reviewer_effort: String,

    /// Codex executable to invoke for non-PI models
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// PI executable to invoke for MiniMax/Kimi models
    #[arg(long = "pi-bin", visible_alias = "opencode-bin", default_value = "pi")]
    pi_bin: PathBuf,
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
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Branch that the loop is allowed to run on. Defaults to the repo's primary branch.
    #[arg(long)]
    branch: Option<String>,

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
    #[arg(long, default_value = "high")]
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
pub(crate) struct QaArgs {
    /// Stop after this many successful QA iterations. Default is 1.
    #[arg(long, default_value_t = 1)]
    max_iterations: usize,

    /// Optional override for the QA prompt template
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Model to use for the QA worker
    #[arg(long, default_value = "gpt-5.4")]
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
    #[arg(long, default_value = "gpt-5.4")]
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
    #[arg(long, default_value = "gpt-5.4")]
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
    #[arg(long, default_value = "gpt-5.4")]
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

    /// Model to use for the initial Nemesis audit pass. Values like `minimax` or `kimi` automatically use PI.
    #[arg(long, default_value = "minimax/MiniMax-M2.7-highspeed")]
    model: String,

    /// Reasoning effort / variant for the initial Nemesis audit pass
    #[arg(long, default_value = "high")]
    reasoning_effort: String,

    /// Model to use for the final Nemesis synthesis pass. Values like `minimax` or `kimi` automatically use PI.
    #[arg(long, default_value = "kimi")]
    reviewer_model: String,

    /// Reasoning effort / variant for the final Nemesis synthesis pass
    #[arg(long, default_value = "high")]
    reviewer_effort: String,

    /// Use PI with the Kimi 2.5 model for the initial Nemesis audit pass
    #[arg(long, conflicts_with = "minimax")]
    kimi: bool,

    /// Use PI with the MiniMax M2.7-highspeed model for the initial Nemesis audit pass
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

    /// Model to use for the Nemesis implementation pass
    #[arg(long, default_value = "gpt-5.4")]
    fixer_model: String,

    /// Reasoning effort / variant for the Nemesis implementation pass
    #[arg(long, default_value = "high")]
    fixer_effort: String,

    /// Codex executable to invoke for the default backend
    #[arg(long, default_value = "codex")]
    codex_bin: PathBuf,

    /// PI executable to invoke for the Kimi/MiniMax backends
    #[arg(long = "pi-bin", visible_alias = "opencode-bin", default_value = "pi")]
    pi_bin: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Corpus(args) => generation::run_corpus(args).await,
        Command::Gen(args) => generation::run_gen(args).await,
        Command::Reverse(args) => generation::run_reverse(args).await,
        Command::Bug(args) => bug_command::run_bug(args).await,
        Command::Loop(args) => loop_command::run_loop(args).await,
        Command::Qa(args) => qa_command::run_qa(args).await,
        Command::QaOnly(args) => qa_only_command::run_qa_only(args).await,
        Command::Health(args) => health_command::run_health(args).await,
        Command::Review(args) => review_command::run_review(args).await,
        Command::Ship(args) => ship_command::run_ship(args).await,
        Command::Nemesis(args) => nemesis::run_nemesis(args).await,
    }
}
