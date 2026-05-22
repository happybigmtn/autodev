//! `auto symphony` command: Linear sync, workflow rendering, and the foreground runner.

mod linear;
mod planner;
mod queries;
mod sync;
mod task;
mod workflow;

use anyhow::Result;

use crate::{SymphonyArgs, SymphonySubcommand};

pub(crate) use sync::run_sync;
pub(crate) use task::{
    parse_tasks, render_issue_description, task_contract_fingerprint, TaskStatus,
};

pub(crate) async fn run_symphony(args: SymphonyArgs) -> Result<()> {
    match args.command {
        SymphonySubcommand::Sync(args) => run_sync(args).await,
        SymphonySubcommand::Workflow(args) => {
            let rendered = workflow::render_workflow(args).await?;
            println!("workflow: {}", rendered.output_path.display());
            println!("base_branch: {}", rendered.base_branch);
            println!("workspace_root: {}", rendered.workspace_root.display());
            Ok(())
        }
        SymphonySubcommand::Run(args) => workflow::run_foreground(args).await,
    }
}
