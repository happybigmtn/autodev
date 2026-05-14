// Parallel-dispatch stage: launch `auto parallel` inside the super run and
// write the branch reconciliation + final sanity notes that wrap the run.
// Spliced into `super_command.rs` via `include!`.

async fn run_super_parallel_stage(
    args: &SuperArgs,
    repo_root: &Path,
    super_root: &Path,
    gate: &DeterministicGateSummary,
) -> Result<()> {
    parallel_command::run_parallel(ParallelArgs {
        action: None::<ParallelAction>,
        max_iterations: args.max_iterations,
        max_concurrent_workers: args.max_concurrent_workers.max(1),
        cargo_build_jobs: None,
        cargo_target: ParallelCargoTarget::Auto,
        prompt_file: None,
        model: args.worker_model.clone(),
        reasoning_effort: args.worker_reasoning_effort.clone(),
        branch: args.branch.clone(),
        reference_repos: args.reference_repos.clone(),
        include_siblings: false,
        run_root: Some(super_root.join("parallel")),
        codex_bin: args.codex_bin.clone(),
        claude: false,
        max_turns: None,
        max_retries: 2,
    })
    .await?;
    write_super_branch_reconciliation_plan(super_root, repo_root, args, "post-parallel")?;
    write_super_final_sanity(super_root, repo_root, gate, args, "post-parallel")?;
    Ok(())
}
