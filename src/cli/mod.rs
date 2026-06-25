mod args_exec;
mod args_ops;
mod args_plan;
mod args_quota;

use clap::{Parser, Subcommand};

use crate::util::CLI_LONG_VERSION;

pub(crate) use args_exec::{
    BugArgs, HardeningProfile, NemesisArgs, ParallelAction, ParallelArgs, ParallelCargoTarget,
    ReviewArgs,
};
pub(crate) use args_ops::{
    AuditArgs, AuditEverythingPhase, AuditResumeMode, HealthArgs, QaArgs, QaOnlyArgs, QaTier,
    ShipArgs, StewardArgs, SymphonyArgs, SymphonyRunArgs, SymphonySubcommand, SymphonySyncArgs,
    SymphonyWorkflowArgs,
};
pub(crate) use args_plan::{
    AuditHarvestArgs, BookArgs, CorpusArgs, DesignArgs, GenerationArgs, SpecArgs, SuperArgs,
};
pub(crate) use args_quota::{AccountsCommand, QuotaArgs, QuotaSubcommand};

#[derive(Parser)]
#[command(
    name = "auto",
    version,
    long_version = CLI_LONG_VERSION,
    about = "Lightweight repo-root planning and execution workflow"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Command {
    /// Review the repo and author a fresh planning corpus under genesis/
    Corpus(CorpusArgs),
    /// Generate specs and a new implementation plan from genesis/
    Gen(GenerationArgs),
    /// Turn a prompt into a conformant spec plus IMPLEMENTATION_PLAN.md task items
    Spec(SpecArgs),
    /// Audit and improve frontend design doctrine with runtime/UI contract proof
    Design(DesignArgs),
    /// Run the CEO 14-day production race: corpus, design gate, functional reviews, gen, gates, parallel
    Super(SuperArgs),
    /// Reverse-engineer specs from code reality using genesis/ as supporting context
    Reverse(GenerationArgs),
    /// Run a chunked multi-pass bug-finding, invalidation, verification, and implementation pipeline
    Bug(BugArgs),
    /// Run the multi-lane implementation executor
    Parallel(ParallelArgs),
    /// Run a runtime QA and ship-readiness pass on the current branch
    Qa(QaArgs),
    /// Run a report-only runtime QA pass on the current branch
    QaOnly(QaOnlyArgs),
    /// Run a repo-wide quality and verification health report
    Health(HealthArgs),
    /// Rewrite the last audit's CODEBASE-BOOK as a detailed narrative walkthrough
    Book(BookArgs),
    /// Run a no-model first-run preflight for baseline and execution readiness
    Doctor(crate::doctor_command::DoctorArgs),
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
    /// Harvest findings from a completed `auto audit --everything` run into
    /// IMPLEMENTATION_PLAN.md as actionable task rows so `auto parallel`
    /// proceeds to actually address them.
    AuditHarvest(AuditHarvestArgs),
    /// Sync implementation-plan items into Linear and run the local Symphony runtime
    Symphony(SymphonyArgs),
}
