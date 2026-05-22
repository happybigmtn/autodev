//! `auto audit --resolve-findings`: the multi-pass parallel finding-resolution
//! engine. `resolve_audit_findings` drives the pass loop; each pass builds
//! architecture-keyed lanes, clones a repo per lane, runs the remediation
//! backend, lands the lane commit, re-audits the drifted files, and verifies.

mod env;
mod git;
mod lanes;

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use tokio::process::Command as TokioCommand;
use tokio::task::JoinSet;

use crate::audit_command::auditor::run_auditor_labeled_with_env_and_timeout;
use crate::audit_command::files::slugify;
use crate::audit_command::manifest::Manifest;
use crate::audit_command::verify::{
    audit_entry_requires_closure, build_finding_verification_report, verify_audit_findings,
    write_finding_verification_report, FindingVerificationResult,
};
use crate::audit_command::FINDING_RESOLUTION_TIMEOUT_SECS;
use crate::util::{auto_checkpoint_if_needed, git_stdout};
use crate::AuditArgs;

use self::env::{prepare_finding_resolution_lane_env, resolve_auto_executable};
use self::git::{
    clone_finding_resolution_lane_repo, commit_finding_resolution_lane_changes,
    land_finding_resolution_lane_result, prune_completed_finding_resolution_lane,
};
use self::lanes::{
    build_finding_resolution_lanes, build_finding_resolution_prompt, finding_resolution_lane_dir,
    finding_resolution_lane_worktree_dir, finding_resolution_target_root,
    finding_resolution_worktree_root, prune_finding_resolution_artifacts,
    reset_finding_resolution_lane_root, write_finding_resolution_status,
    FindingResolutionLaneAssignment, FindingResolutionLaneOutcome, FindingResolutionLaneStatus,
};

pub(crate) async fn resolve_audit_findings(
    repo_root: &Path,
    output_dir: &Path,
    args: AuditArgs,
) -> Result<()> {
    preflight_finding_resolution_roots(repo_root, &args)?;
    let target_branch = args
        .branch
        .clone()
        .unwrap_or_else(|| {
            git_stdout(repo_root, ["branch", "--show-current"])
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .trim()
        .to_string();
    if target_branch.is_empty() {
        bail!("auto audit --resolve-findings requires a checked-out branch");
    }
    if let Some(checkpoint) = auto_checkpoint_if_needed(
        repo_root,
        &target_branch,
        "audit finding resolution checkpoint",
    )? {
        println!("checkpoint: {checkpoint}");
    }

    let max_passes = args.resolve_passes.max(1);
    for resolve_pass in 1..=max_passes {
        if let Some(checkpoint) = auto_checkpoint_if_needed(
            repo_root,
            &target_branch,
            &format!("audit finding resolution checkpoint pass {resolve_pass}"),
        )? {
            println!("checkpoint: committed resolve pass {resolve_pass} inputs at {checkpoint}");
        }
        println!("auto audit resolve findings pass {resolve_pass}/{max_passes}");
        match resolve_audit_findings_pass(
            repo_root,
            output_dir,
            args.clone(),
            &target_branch,
            resolve_pass,
            max_passes,
        )
        .await
        {
            Ok(ResolvePassOutcome::Verified) => return Ok(()),
            Ok(ResolvePassOutcome::RetryNeeded { reason }) => {
                if let Some(checkpoint) = auto_checkpoint_if_needed(
                    repo_root,
                    &target_branch,
                    &format!(
                        "audit finding resolution verification checkpoint pass {resolve_pass}"
                    ),
                )? {
                    println!(
                        "checkpoint: committed resolve pass {resolve_pass} verification output at {checkpoint}"
                    );
                }
                if resolve_pass == max_passes {
                    bail!(
                        "audit findings are still not fully closed after {max_passes} resolve pass(es): {reason}"
                    );
                }
                eprintln!(
                    "auto audit resolve findings pass {resolve_pass}/{max_passes} did not close all findings: {reason}"
                );
            }
            Err(err) => return Err(err),
        }
    }

    bail!("audit findings are still not fully closed after {max_passes} resolve pass(es)")
}

enum ResolvePassOutcome {
    Verified,
    RetryNeeded { reason: String },
}

async fn resolve_audit_findings_pass(
    repo_root: &Path,
    output_dir: &Path,
    args: AuditArgs,
    target_branch: &str,
    resolve_pass: usize,
    max_passes: usize,
) -> Result<ResolvePassOutcome> {
    let manifest_path = output_dir.join("MANIFEST.json");
    if !manifest_path.exists() {
        bail!(
            "audit finding resolution requires an existing manifest at {}",
            manifest_path.display()
        );
    }
    fs::create_dir_all(output_dir.join("files"))
        .with_context(|| format!("failed to create {}", output_dir.join("files").display()))?;
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let verification_report = build_finding_verification_report(repo_root, output_dir)?;
    write_finding_verification_report(output_dir, &verification_report)?;
    let unresolved_paths = verification_report
        .findings
        .iter()
        .filter(|finding| {
            matches!(
                finding.result,
                FindingVerificationResult::NeedsReaudit | FindingVerificationResult::StillOpen
            )
        })
        .map(|finding| finding.path.clone())
        .collect::<HashSet<_>>();
    let resolved_before_lane_count = verification_report
        .findings
        .len()
        .saturating_sub(unresolved_paths.len());
    let findings = manifest
        .files
        .iter()
        .filter(audit_entry_requires_closure)
        .filter(|entry| unresolved_paths.contains(&entry.path))
        .filter(|entry| repo_root.join(&entry.path).exists())
        .cloned()
        .collect::<Vec<_>>();
    let finding_paths = findings
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();

    if findings.is_empty() {
        println!("auto audit resolve findings: no existing unresolved flagged files to remediate");
        verify_audit_findings(repo_root, output_dir)?;
        return Ok(ResolvePassOutcome::Verified);
    }

    let max_lanes = args.audit_threads.clamp(1, 8).min(findings.len());
    let lanes = build_finding_resolution_lanes(findings, max_lanes);
    let run_id = format!("{}-pass-{resolve_pass:02}", crate::util::timestamp_slug());
    let run_dir = output_dir.join("finding-resolution").join(&run_id);
    let target_root = finding_resolution_target_root(repo_root, &run_id);
    let worktree_root = finding_resolution_worktree_root(repo_root, &run_id);
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("failed to create {}", run_dir.display()))?;
    fs::create_dir_all(&target_root)
        .with_context(|| format!("failed to create {}", target_root.display()))?;
    fs::create_dir_all(&worktree_root)
        .with_context(|| format!("failed to create {}", worktree_root.display()))?;

    println!("auto audit resolve findings");
    println!("manifest: {}", manifest_path.display());
    println!("pass:     {resolve_pass}/{max_passes}");
    println!("lanes:    {} (max {})", lanes.len(), max_lanes);
    if resolved_before_lane_count > 0 {
        println!(
            "preclosed: {resolved_before_lane_count} finding(s) already closed by removal or clean artifacts"
        );
    }
    println!("run dir:  {}", run_dir.display());
    println!("targets:  {}", target_root.display());
    println!("worktrees: {}", worktree_root.display());

    prune_finding_resolution_artifacts(
        repo_root,
        output_dir,
        &run_id,
        args.resolve_keep_runs,
        !args.no_resolve_target_prune,
        false,
    )?;

    let lane_assignments = lanes
        .into_iter()
        .map(|lane| {
            let lane_root = finding_resolution_lane_worktree_dir(&worktree_root, &lane);
            let lane_repo_root = lane_root.join("repo");
            let lane_target_dir = target_root.join(slugify(&lane.name));
            reset_finding_resolution_lane_root(&lane_root)?;
            clone_finding_resolution_lane_repo(repo_root, target_branch, &lane_repo_root)?;
            let base_commit = git_stdout(&lane_repo_root, ["rev-parse", "HEAD"])?
                .trim()
                .to_string();
            Ok(FindingResolutionLaneAssignment {
                lane,
                lane_root,
                lane_repo_root,
                lane_target_dir,
                base_commit,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut lane_statuses = lane_assignments
        .iter()
        .map(|assignment| {
            let lane = &assignment.lane;
            let lane_dir = finding_resolution_lane_dir(&run_dir, lane);
            FindingResolutionLaneStatus {
                id: lane.id,
                name: lane.name.clone(),
                finding_count: lane.entries.len(),
                state: "running".to_string(),
                repo_dir: assignment.lane_repo_root.display().to_string(),
                target_dir: assignment.lane_target_dir.display().to_string(),
                prompt_path: lane_dir.join("prompt.md").display().to_string(),
                response_path: lane_dir.join("response.log").display().to_string(),
                landed_commit: None,
                error: None,
            }
        })
        .collect::<Vec<_>>();
    write_finding_resolution_status(
        output_dir,
        &run_id,
        "running",
        &run_dir,
        &worktree_root,
        &target_root,
        &lane_statuses,
    )?;

    let mut join_set = JoinSet::new();
    for assignment in lane_assignments {
        let output_dir = output_dir.to_path_buf();
        let run_dir = run_dir.clone();
        let args = args.clone();
        let lane_id = assignment.lane.id;
        join_set.spawn(async move {
            run_finding_resolution_lane(assignment, output_dir, run_dir, args)
                .await
                .map_err(|err| (lane_id, err.to_string()))
        });
    }

    let mut failures = Vec::new();
    while let Some(result) = join_set.join_next().await {
        let outcome = result.context("finding resolution lane panicked")?;
        match outcome {
            Ok(outcome) => {
                if let Some(status) = lane_statuses
                    .iter_mut()
                    .find(|status| status.id == outcome.lane_id)
                {
                    status.state = "landing".to_string();
                    status.error = None;
                }
                write_finding_resolution_status(
                    output_dir,
                    &run_id,
                    if failures.is_empty() {
                        "running"
                    } else {
                        "failed"
                    },
                    &run_dir,
                    &worktree_root,
                    &target_root,
                    &lane_statuses,
                )?;
                let landed_commit =
                    land_finding_resolution_lane_result(repo_root, target_branch, &outcome)
                        .with_context(|| format!("failed landing lane {}", outcome.lane_id + 1))?;
                if let Some(status) = lane_statuses
                    .iter_mut()
                    .find(|status| status.id == outcome.lane_id)
                {
                    status.state = "landed".to_string();
                    status.landed_commit = Some(landed_commit);
                    status.error = None;
                }
                prune_completed_finding_resolution_lane(&outcome.lane_repo_root)?;
            }
            Err((lane_id, error)) => {
                if let Some(status) = lane_statuses.iter_mut().find(|status| status.id == lane_id) {
                    status.state = "failed".to_string();
                    status.error = Some(error.clone());
                }
                failures.push(format!("lane {} failed: {error}", lane_id + 1));
            }
        }
        write_finding_resolution_status(
            output_dir,
            &run_id,
            if failures.is_empty() {
                "running"
            } else {
                "failed"
            },
            &run_dir,
            &worktree_root,
            &target_root,
            &lane_statuses,
        )?;
    }

    if !failures.is_empty() {
        bail!("{}", failures.join("\n"));
    }

    write_finding_resolution_status(
        output_dir,
        &run_id,
        "re-auditing-drifted",
        &run_dir,
        &worktree_root,
        &target_root,
        &lane_statuses,
    )?;
    let reaudit_status = rerun_only_drifted_audit(repo_root, output_dir, &args, &finding_paths)
        .await
        .context("failed to launch drifted finding re-audit")?;
    if let ReauditOutcome::NoGo { status } = &reaudit_status {
        eprintln!(
            "auto audit resolve findings: only-drifted re-audit exited with {status}; verifying findings and continuing the resolve loop if needed"
        );
    }
    match verify_audit_findings(repo_root, output_dir) {
        Ok(()) => {
            write_finding_resolution_status(
                output_dir,
                &run_id,
                "verified",
                &run_dir,
                &worktree_root,
                &target_root,
                &lane_statuses,
            )?;
            prune_finding_resolution_artifacts(
                repo_root,
                output_dir,
                &run_id,
                args.resolve_keep_runs,
                !args.no_resolve_target_prune,
                true,
            )?;
            Ok(ResolvePassOutcome::Verified)
        }
        Err(err) => {
            write_finding_resolution_status(
                output_dir,
                &run_id,
                "verification-no-go",
                &run_dir,
                &worktree_root,
                &target_root,
                &lane_statuses,
            )?;
            Ok(ResolvePassOutcome::RetryNeeded {
                reason: err.to_string(),
            })
        }
    }
}

enum ReauditOutcome {
    Success,
    NoGo { status: String },
}

async fn run_finding_resolution_lane(
    assignment: FindingResolutionLaneAssignment,
    output_dir: std::path::PathBuf,
    run_dir: std::path::PathBuf,
    args: AuditArgs,
) -> Result<FindingResolutionLaneOutcome> {
    let FindingResolutionLaneAssignment {
        lane,
        lane_root: _lane_root,
        lane_repo_root,
        lane_target_dir,
        base_commit,
    } = assignment;
    let lane_env = prepare_finding_resolution_lane_env(
        &lane_repo_root,
        &lane_target_dir,
        args.resolve_validation_threads,
    )?;
    let prompt = build_finding_resolution_prompt(
        &lane_repo_root,
        &output_dir,
        &lane,
        &lane_target_dir,
        &args,
    )?;
    let lane_dir = finding_resolution_lane_dir(&run_dir, &lane);
    fs::create_dir_all(&lane_dir)
        .with_context(|| format!("failed to create {}", lane_dir.display()))?;
    crate::util::atomic_write(&lane_dir.join("prompt.md"), prompt.as_bytes())?;
    println!(
        "[resolve:{}/{}] {} finding(s) across {}",
        lane.id + 1,
        lane.entries.len(),
        lane.entries.len(),
        lane.name
    );
    let label = format!("audit-resolve:{}/{}", lane.id + 1, lane.name);
    let response = run_auditor_labeled_with_env_and_timeout(
        &lane_repo_root,
        &prompt,
        &args,
        Some(&label),
        &lane_env,
        FINDING_RESOLUTION_TIMEOUT_SECS,
    )
    .await?;
    crate::util::atomic_write(&lane_dir.join("response.log"), response.as_bytes())?;
    commit_finding_resolution_lane_changes(&lane_repo_root, &lane, &base_commit)?;
    Ok(FindingResolutionLaneOutcome {
        lane_id: lane.id,
        lane_repo_root,
        base_commit,
    })
}

fn preflight_finding_resolution_roots(repo_root: &Path, args: &AuditArgs) -> Result<()> {
    if args.allow_missing_resolve_roots {
        return Ok(());
    }
    let tracked_candidates = [
        "AGENTS.md",
        "IMPLEMENTATION_PLAN.md",
        "REVIEW.md",
        "audit/DOCTRINE.md",
        "AUTONOMY-GDD.md",
    ];
    let agents_text = fs::read_to_string(repo_root.join("AGENTS.md")).unwrap_or_default();
    let mut missing = Vec::new();
    for path in tracked_candidates {
        if !tracked_path_exists(repo_root, path)? {
            continue;
        }
        if repo_root.join(path).exists() {
            continue;
        }
        if path == "AUTONOMY-GDD.md" && agents_text.contains("RSOCIETY-GDD.md") {
            eprintln!(
                "auto audit resolve findings: tracked AUTONOMY-GDD.md is missing, but AGENTS.md names RSOCIETY-GDD.md as canonical successor"
            );
            continue;
        }
        missing.push(path.to_string());
    }
    if !missing.is_empty() {
        bail!(
            "auto audit --resolve-findings refuses to run with missing source-of-truth file(s): {}. Restore them, update AGENTS.md to name the successor doctrine, or pass --allow-missing-resolve-roots.",
            missing.join(", ")
        );
    }
    Ok(())
}

fn tracked_path_exists(repo_root: &Path, path: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(path)
        .output()
        .with_context(|| format!("failed to check whether {path} is tracked"))?;
    Ok(output.status.success())
}

async fn rerun_only_drifted_audit(
    repo_root: &Path,
    output_dir: &Path,
    args: &AuditArgs,
    focus_paths: &[String],
) -> Result<ReauditOutcome> {
    let exe = resolve_auto_executable()?;
    let mut command = TokioCommand::new(exe);
    command
        .arg("audit")
        .arg("--resume-mode")
        .arg("only-drifted")
        .arg("--audit-threads")
        .arg(args.audit_threads.clamp(1, 8).to_string())
        .arg("--output-dir")
        .arg(output_dir)
        .arg("--model")
        .arg(&args.model)
        .arg("--reasoning-effort")
        .arg(&args.reasoning_effort)
        .arg("--escalation-model")
        .arg(&args.escalation_model)
        .arg("--escalation-effort")
        .arg(&args.escalation_effort)
        .arg("--codex-bin")
        .arg(&args.codex_bin)
        .arg("--kimi-bin")
        .arg(&args.kimi_bin)
        .arg("--pi-bin")
        .arg(&args.pi_bin)
        .arg("--use-kimi-cli")
        .arg(if args.use_kimi_cli { "true" } else { "false" })
        .current_dir(repo_root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(path) = args.rubric_prompt.as_deref() {
        command.arg("--rubric-prompt").arg(path);
    }
    command.arg("--doctrine-prompt").arg(&args.doctrine_prompt);
    if args.report_only {
        command.arg("--report-only");
    }
    if let Some(branch) = args.branch.as_deref() {
        command.arg("--branch").arg(branch);
    }
    let include_paths = if focus_paths.is_empty() {
        &args.include_paths
    } else {
        focus_paths
    };
    for path in include_paths {
        command.arg("--paths").arg(path);
    }
    for path in &args.exclude_paths {
        command.arg("--exclude").arg(path);
    }

    let status = command
        .status()
        .await
        .context("failed to launch only-drifted audit subprocess")?;
    if !status.success() {
        return Ok(ReauditOutcome::NoGo {
            status: status.to_string(),
        });
    }
    Ok(ReauditOutcome::Success)
}
