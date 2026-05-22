//! `auto nemesis` — Nemesis-style deep hardening audit and remediation.
//!
//! `run_nemesis` drives the audit, synthesis, implementation, and finalizer
//! phases, then syncs the produced spec and plan into the repo root. Submodules
//! own the cohesive concerns: `prompts` builds the phase prompts, `outputs`
//! verifies model artifacts, `plan` parses and merges the markdown plan,
//! `commit` records the outputs, and `backend` is the process layer. The shared
//! JSON-repair engine lives in `crate::bug_command::llm_json`.

mod backend;
mod commit;
mod outputs;
mod plan;
mod prompts;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::kimi_backend::{preflight_kimi_cli, resolve_kimi_bin};
use crate::nemesis::backend::{
    is_kimi_model, print_phase_header, run_nemesis_backend, select_backend, NemesisBackend,
};
use crate::nemesis::commit::commit_nemesis_outputs_if_needed;
use crate::nemesis::outputs::{
    draft_nemesis_outputs_valid, nonempty_file, verify_nemesis_implementation_results,
    verify_nemesis_implementation_results_once, verify_nemesis_outputs, VerifiedNemesisOutputs,
};
use crate::nemesis::plan::{
    append_nemesis_plan_to_root, load_unchecked_nemesis_task_ids, sync_nemesis_spec_to_root,
};
use crate::nemesis::prompts::{
    build_audit_prompt, build_finalizer_prompt, build_implementation_prompt, build_review_prompt,
    DEFAULT_NEMESIS_PROMPT,
};
use crate::pi_backend::PiProvider;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, copy_tree, ensure_repo_layout, git_repo_root,
    git_stdout, push_branch_with_remote_sync, sync_branch_with_remote, timestamp_slug,
};
use crate::{HardeningProfile, NemesisArgs};

pub(crate) const DEFAULT_CODEX_NEMESIS_MODEL: &str = "gpt-5.5";
#[allow(dead_code)]
const DEFAULT_NEMESIS_AUDIT_MODEL: &str = "gpt-5.5";

#[derive(Clone, Debug)]
struct PhaseConfig {
    model: String,
    effort: String,
}

fn apply_nemesis_profile(
    profile: HardeningProfile,
    auditor: &mut PhaseConfig,
    reviewer: &mut PhaseConfig,
    fixer: &mut PhaseConfig,
    finalizer: &mut PhaseConfig,
) {
    match profile {
        HardeningProfile::Balanced => {}
        HardeningProfile::Fast => {
            set_default_effort(auditor, "medium");
            set_default_effort(reviewer, "medium");
            set_default_effort(fixer, "high");
            set_default_effort(finalizer, "high");
        }
        HardeningProfile::MaxQuality => {
            for config in [auditor, reviewer, fixer, finalizer] {
                set_default_effort(config, "xhigh");
            }
        }
    }
}

fn set_default_effort(config: &mut PhaseConfig, effort: &str) {
    if config.model == DEFAULT_CODEX_NEMESIS_MODEL && config.effort == "high" {
        config.effort = effort.to_string();
    }
}

pub(crate) async fn run_nemesis(args: NemesisArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    if !args.dry_run && !args.report_only && current_branch.is_empty() {
        bail!(
            "auto nemesis requires a checked-out branch so implementation commits can push to origin"
        );
    }
    if let Some(required_branch) = args.branch.as_deref() {
        if current_branch != required_branch {
            bail!(
                "auto nemesis must run on branch `{}` (current: `{}`)",
                required_branch,
                current_branch
            );
        }
    }

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join("nemesis"));
    let mut auditor = PhaseConfig {
        model: resolve_auditor_model(&args),
        effort: args.reasoning_effort.clone(),
    };
    let mut reviewer = PhaseConfig {
        model: args.reviewer_model.clone(),
        effort: args.reviewer_effort.clone(),
    };
    let mut fixer = PhaseConfig {
        model: args.fixer_model.clone(),
        effort: args.fixer_effort.clone(),
    };
    let mut finalizer = PhaseConfig {
        model: args.finalizer_model.clone(),
        effort: args.finalizer_effort.clone(),
    };
    apply_nemesis_profile(
        args.profile,
        &mut auditor,
        &mut reviewer,
        &mut fixer,
        &mut finalizer,
    );
    ensure_nemesis_phase_config("auto nemesis audit pass", &auditor)?;
    ensure_nemesis_phase_config("auto nemesis synthesis pass", &reviewer)?;
    ensure_nemesis_fixer_config(&fixer)?;
    ensure_nemesis_finalizer_config(&finalizer)?;
    let kimi_preflight_model = [&auditor, &reviewer, &fixer]
        .iter()
        .find(|config| is_kimi_model(&config.model))
        .map(|config| config.model.as_str());
    if args.use_kimi_cli {
        if let Some(model) = kimi_preflight_model {
            let kimi_bin = resolve_kimi_bin(&args.kimi_bin);
            preflight_kimi_cli(&kimi_bin, model)?;
        }
    }
    let audit_backend = select_backend(
        &auditor.model,
        &auditor.effort,
        &args.codex_bin,
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    let review_backend = select_backend(
        &reviewer.model,
        &reviewer.effort,
        &args.codex_bin,
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    let fix_backend = select_backend(
        &fixer.model,
        &fixer.effort,
        &args.codex_bin,
        &args.pi_bin,
        &args.kimi_bin,
        args.use_kimi_cli,
    );
    validate_nemesis_backend_binaries(
        &audit_backend,
        &review_backend,
        &fix_backend,
        args.report_only,
        &args,
    )?;
    validate_nemesis_execution_contract(&args)?;
    let previous_snapshot =
        maybe_prepare_output_dir(&repo_root, &output_dir, args.dry_run, args.resume)?;

    let prompt_template = match &args.prompt_file {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display()))?,
        None => DEFAULT_NEMESIS_PROMPT.to_string(),
    };
    let draft_audit_path = output_dir.join("draft-nemesis-audit.md");
    let draft_plan_path = output_dir.join("draft-IMPLEMENTATION_PLAN.md");
    let final_audit_path = output_dir.join("nemesis-audit.md");
    let final_plan_path = output_dir.join("IMPLEMENTATION_PLAN.md");
    let implementation_results_json_path = output_dir.join("implementation-results.json");
    let implementation_results_md_path = output_dir.join("implementation-results.md");
    let audit_prompt = build_audit_prompt(&prompt_template, &draft_audit_path, &draft_plan_path);
    let review_prompt = build_review_prompt(
        &prompt_template,
        &draft_audit_path,
        &draft_plan_path,
        &final_audit_path,
        &final_plan_path,
    );
    let audit_prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-audit-prompt.md", timestamp_slug()));
    atomic_write(&audit_prompt_path, audit_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", audit_prompt_path.display()))?;
    let review_prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-review-prompt.md", timestamp_slug()));
    atomic_write(&review_prompt_path, review_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", review_prompt_path.display()))?;
    let implementation_prompt = build_implementation_prompt(
        &final_audit_path,
        &final_plan_path,
        &implementation_results_json_path,
        &implementation_results_md_path,
        args.branch.as_deref().unwrap_or(&current_branch),
    );
    let implementation_prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-implement-prompt.md", timestamp_slug()));
    atomic_write(
        &implementation_prompt_path,
        implementation_prompt.as_bytes(),
    )
    .with_context(|| format!("failed to write {}", implementation_prompt_path.display()))?;

    println!("auto nemesis");
    println!("repo root:   {}", repo_root.display());
    println!("output dir:  {}", output_dir.display());
    println!("profile:     {:?}", args.profile);
    println!(
        "auditor:     {} ({})",
        audit_backend.model(),
        audit_backend.variant()
    );
    println!(
        "reviewer:    {} ({})",
        review_backend.model(),
        review_backend.variant()
    );
    if !args.report_only {
        println!("fixer:       {} ({})", fixer.model, fixer.effort);
        println!(
            "branch:      {}",
            args.branch.as_deref().unwrap_or(&current_branch)
        );
    }
    if let Some(previous) = &previous_snapshot {
        println!("prior input: {}", previous.display());
    }
    if args.resume {
        println!("resume:      reusing valid nemesis artifacts when present");
    }
    if args.dry_run {
        println!("mode:        dry-run");
        return Ok(());
    }
    if !args.report_only {
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, current_branch.as_str(), "nemesis checkpoint")?
        {
            println!("checkpoint:  committed pre-existing changes at {commit}");
        } else if sync_branch_with_remote(&repo_root, current_branch.as_str())? {
            println!("remote sync: rebased onto origin/{}", current_branch);
        }
    } else {
        println!("mode:        report-only");
    }

    let audit_response_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-audit-response.log", timestamp_slug()));
    let review_response_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-review-response.log", timestamp_slug()));

    let final_outputs_reusable = args.resume && verify_nemesis_outputs(&output_dir).is_ok();
    if final_outputs_reusable {
        println!("resume:      reusing verified final audit and plan");
    } else {
        let draft_outputs_reusable =
            args.resume && draft_nemesis_outputs_valid(&draft_audit_path, &draft_plan_path).is_ok();
        if draft_outputs_reusable {
            println!("resume:      reusing draft audit and plan");
        } else {
            print_phase_header("auditor", &audit_backend);
            let audit_response =
                run_nemesis_backend(&repo_root, &audit_prompt, &audit_backend, &args.codex_bin)
                    .await
                    .map_err(|err| {
                        annotate_output_recovery(
                            err,
                            &output_dir,
                            previous_snapshot.as_deref(),
                            "Nemesis audit pass failed",
                        )
                    })?;
            if !audit_response.trim().is_empty() {
                atomic_write(&audit_response_path, audit_response.as_bytes()).with_context(
                    || format!("failed to write {}", audit_response_path.display()),
                )?;
            }
        }

        print_phase_header("reviewer", &review_backend);
        let review_response =
            run_nemesis_backend(&repo_root, &review_prompt, &review_backend, &args.codex_bin)
                .await
                .map_err(|err| {
                    annotate_output_recovery(
                        err,
                        &output_dir,
                        previous_snapshot.as_deref(),
                        "Nemesis synthesis pass failed",
                    )
                })?;
        if !review_response.trim().is_empty() {
            atomic_write(&review_response_path, review_response.as_bytes())
                .with_context(|| format!("failed to write {}", review_response_path.display()))?;
        }
    }

    let VerifiedNemesisOutputs {
        spec_path,
        plan_path,
    } = verify_nemesis_outputs(&output_dir).map_err(|err| {
        annotate_output_recovery(
            err,
            &output_dir,
            previous_snapshot.as_deref(),
            "Nemesis output verification failed",
        )
    })?;
    let mut implementation_results = None::<PathBuf>;
    let mut implementation_summary = "report-only".to_string();
    if !args.report_only {
        let pending_tasks = load_unchecked_nemesis_task_ids(&plan_path)?;
        let commit_before = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
        println!();
        println!("phase:       implementer");
        println!("backend:     {}", fix_backend.label());
        println!("model:       {}", fix_backend.model());
        println!("variant:     {}", fix_backend.variant());
        if pending_tasks.is_empty() {
            println!("status:      no unchecked Nemesis tasks; skipping implementer");
            implementation_summary =
                format!("skipped (no unchecked tasks in {})", plan_path.display());
        } else if args.resume
            && verify_nemesis_implementation_results_once(
                &implementation_results_json_path,
                &implementation_results_md_path,
                &plan_path,
            )
            .is_ok()
        {
            println!("resume:      reusing implementation results");
            implementation_summary = implementation_results_json_path.display().to_string();
            implementation_results = Some(implementation_results_json_path.clone());
        } else {
            // Route the implementer through the selected backend. Codex stays
            // as the finalizer that reviews the landed diff after this phase.
            let stderr_log = output_dir.join("implementer.stderr.log");
            let response = run_nemesis_backend(
                &repo_root,
                &implementation_prompt,
                &fix_backend,
                &args.codex_bin,
            )
            .await?;
            let response_path = output_dir.join("implementation-response.log");
            if !response.trim().is_empty() {
                atomic_write(&response_path, response.as_bytes())
                    .with_context(|| format!("failed to write {}", response_path.display()))?;
            }
            let _ = stderr_log; // stderr capture already handled by backend helpers

            let implementation_path = verify_nemesis_implementation_results(
                &repo_root,
                &fix_backend,
                &args.codex_bin,
                &spec_path,
                &implementation_results_json_path,
                &implementation_results_md_path,
                &plan_path,
            )
            .await?;
            implementation_summary = implementation_path.display().to_string();
            implementation_results = Some(implementation_path);
        }
        if implementation_results.is_some() {
            // Codex finalizer: independent review of the diff just produced.
            // Fails loudly if it finds regressions; audit record is written to
            // `nemesis/final-review.md`.
            let finalizer_backend = NemesisBackend::Codex {
                model: finalizer.model.clone(),
                reasoning_effort: finalizer.effort.clone(),
                codex_bin: args.codex_bin.clone(),
            };
            let finalizer_prompt = build_finalizer_prompt(
                &spec_path,
                &plan_path,
                &implementation_results_json_path,
                &implementation_results_md_path,
                args.branch.as_deref().unwrap_or(&current_branch),
            );
            let finalizer_prompt_path = repo_root
                .join(".auto")
                .join("logs")
                .join(format!("nemesis-{}-finalizer-prompt.md", timestamp_slug()));
            atomic_write(&finalizer_prompt_path, finalizer_prompt.as_bytes())
                .with_context(|| format!("failed to write {}", finalizer_prompt_path.display()))?;
            let finalizer_response_path = output_dir.join("final-review.md");
            if args.resume && nonempty_file(&finalizer_response_path) {
                println!("resume:      reusing finalizer review");
            } else {
                print_phase_header("finalizer", &finalizer_backend);
                let finalizer_response = run_nemesis_backend(
                    &repo_root,
                    &finalizer_prompt,
                    &finalizer_backend,
                    &args.codex_bin,
                )
                .await?;
                atomic_write(&finalizer_response_path, finalizer_response.as_bytes())
                    .with_context(|| {
                        format!("failed to write {}", finalizer_response_path.display())
                    })?;
            }
            println!(
                "finalizer:   wrote review to {}",
                finalizer_response_path.display()
            );

            let commit_after = git_stdout(&repo_root, ["rev-parse", "HEAD"])?;
            if commit_before.trim() != commit_after.trim()
                && push_branch_with_remote_sync(&repo_root, current_branch.as_str())?
            {
                println!("remote sync: rebased onto origin/{}", current_branch);
            }
        }
    }
    let root_spec = sync_nemesis_spec_to_root(&repo_root, &spec_path)?;
    let appended = append_nemesis_plan_to_root(&repo_root, &plan_path)?;
    let trailing_commit = if args.report_only {
        None
    } else {
        commit_nemesis_outputs_if_needed(
            &repo_root,
            current_branch.as_str(),
            &output_dir,
            &root_spec,
            &repo_root.join("IMPLEMENTATION_PLAN.md"),
        )?
    };

    println!();
    println!("nemesis complete");
    println!("spec:        {}", spec_path.display());
    println!("plan:        {}", plan_path.display());
    println!("root spec:   {}", root_spec.display());
    println!("root tasks:  {} appended", appended);
    if let Some(path) = implementation_results {
        println!("implementation: {}", path.display());
    } else {
        println!("implementation: {}", implementation_summary);
    }
    println!("audit prompt: {}", audit_prompt_path.display());
    println!("review prompt: {}", review_prompt_path.display());
    if !args.report_only {
        println!("implement prompt: {}", implementation_prompt_path.display());
    }
    if audit_response_path.exists() {
        println!("audit log:   {}", audit_response_path.display());
    }
    if review_response_path.exists() {
        println!("review log:  {}", review_response_path.display());
    }
    if let Some(commit) = trailing_commit {
        println!("outputs commit: {}", commit);
    }

    Ok(())
}

fn ensure_nemesis_phase_config(label: &str, config: &PhaseConfig) -> Result<()> {
    if config.model.trim().is_empty() {
        bail!("{label} model is required");
    }
    Ok(())
}

/// Accept any concrete model for remediation. The finalizer phase has its own
/// Codex-only gate so implementation can use Codex by default or an explicit
/// Kimi/PI opt-in.
fn ensure_nemesis_fixer_config(config: &PhaseConfig) -> Result<()> {
    if config.model.trim().is_empty() {
        bail!("auto nemesis fixer model is required");
    }
    Ok(())
}

/// Finalizer MUST be Codex so the last pass is independent of any optional
/// Kimi/PI implementation backend.
fn ensure_nemesis_finalizer_config(config: &PhaseConfig) -> Result<()> {
    if is_kimi_model(&config.model) || PiProvider::detect(&config.model).is_some() {
        bail!(
            "auto nemesis finalizer must use a Codex model (e.g. `gpt-5.5`); got `{}`",
            config.model
        );
    }
    Ok(())
}

fn resolve_auditor_model(args: &NemesisArgs) -> String {
    if args.model != DEFAULT_NEMESIS_AUDIT_MODEL {
        return args.model.clone();
    }
    // Explicit legacy opt-in still honoured so operators who want a MiniMax
    // second-opinion run can force it with `--minimax`.
    if args.minimax {
        return "minimax".to_string();
    }
    // Explicit legacy opt-in for the Kimi audit model remains available.
    if args.kimi {
        return "k2.6".to_string();
    }
    args.model.clone()
}

fn validate_nemesis_backend_binaries(
    audit_backend: &NemesisBackend,
    review_backend: &NemesisBackend,
    fix_backend: &NemesisBackend,
    report_only: bool,
    args: &NemesisArgs,
) -> Result<()> {
    validate_backend_binary("Nemesis audit backend", audit_backend)?;
    validate_backend_binary("Nemesis synthesis backend", review_backend)?;
    if !report_only {
        validate_backend_binary("Nemesis implementation backend", fix_backend)?;
        ensure_executable_available("Nemesis finalizer backend", &args.codex_bin)?;
    }
    Ok(())
}

fn validate_nemesis_execution_contract(args: &NemesisArgs) -> Result<()> {
    if args.report_only && args.audit_passes > 1 {
        bail!(
            "auto nemesis --report-only cannot claim multi-pass audit execution; run without --report-only to execute remediation/finalizer phases"
        );
    }
    if args.audit_passes == 0 {
        bail!("auto nemesis --audit-passes must be at least 1");
    }
    Ok(())
}

fn validate_backend_binary(label: &str, backend: &NemesisBackend) -> Result<()> {
    match backend {
        NemesisBackend::Codex { codex_bin, .. } => ensure_executable_available(label, codex_bin),
        NemesisBackend::Pi { pi_bin, .. } => ensure_executable_available(label, pi_bin),
        NemesisBackend::KimiCli { kimi_bin, .. } => ensure_executable_available(label, kimi_bin),
    }
}

fn ensure_executable_available(label: &str, executable: &Path) -> Result<()> {
    if executable.components().count() > 1 || executable.is_absolute() {
        let metadata = fs::metadata(executable).with_context(|| {
            format!(
                "{label} executable {} is not available",
                executable.display()
            )
        })?;
        if !metadata.is_file() {
            bail!("{label} executable {} is not a file", executable.display());
        }
        return Ok(());
    }

    let Some(path) = std::env::var_os("PATH") else {
        bail!(
            "PATH is not set, so {label} executable `{}` cannot be resolved",
            executable.display()
        );
    };
    for directory in std::env::split_paths(&path) {
        if directory.join(executable).is_file() {
            return Ok(());
        }
    }
    bail!(
        "{label} executable `{}` was not found on PATH",
        executable.display()
    );
}

fn maybe_prepare_output_dir(
    repo_root: &Path,
    output_dir: &Path,
    dry_run: bool,
    resume: bool,
) -> Result<Option<PathBuf>> {
    if dry_run {
        return Ok(None);
    }
    if resume {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        return Ok(None);
    }
    prepare_output_dir(repo_root, output_dir)
}

fn annotate_output_recovery(
    error: anyhow::Error,
    output_dir: &Path,
    previous_snapshot: Option<&Path>,
    context: &str,
) -> anyhow::Error {
    let mut message = format!("{context} for {}", output_dir.display());
    if let Some(snapshot) = previous_snapshot {
        message.push_str(&format!(
            ". Previous outputs were archived at {}; restore from that snapshot after fixing \
             the backend failure.",
            snapshot.display()
        ));
    }
    error.context(message)
}

fn prepare_output_dir(repo_root: &Path, output_dir: &Path) -> Result<Option<PathBuf>> {
    if !output_dir.exists() {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        return Ok(None);
    }
    if !output_dir.is_dir() {
        bail!(
            "Nemesis output path {} is not a directory",
            output_dir.display()
        );
    }

    let has_contents = fs::read_dir(output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
        .next()
        .transpose()?
        .is_some();
    let archived = if has_contents {
        let snapshot_root = repo_root.join(".auto").join("fresh-input").join(format!(
            "{}-previous-{}",
            output_dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("nemesis"),
            timestamp_slug()
        ));
        copy_tree(output_dir, &snapshot_root).with_context(|| {
            format!(
                "failed to archive existing Nemesis output from {} into {}",
                output_dir.display(),
                snapshot_root.display()
            )
        })?;
        Some(snapshot_root)
    } else {
        None
    };

    fs::remove_dir_all(output_dir)
        .with_context(|| format!("failed to clear {}", output_dir.display()))?;
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to recreate {}", output_dir.display()))?;
    Ok(archived)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        annotate_output_recovery, ensure_nemesis_finalizer_config, ensure_nemesis_fixer_config,
        ensure_nemesis_phase_config, maybe_prepare_output_dir, prepare_output_dir,
        resolve_auditor_model, validate_nemesis_execution_contract, PhaseConfig,
        DEFAULT_NEMESIS_AUDIT_MODEL,
    };
    use crate::nemesis::backend::select_backend;
    use crate::NemesisArgs;

    fn temp_repo_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "autodev-nemesis-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn run_git_in<'a>(repo: &Path, args: impl IntoIterator<Item = &'a str>) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to launch git");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git stdout should be utf-8")
    }

    fn init_repo(name: &str) -> PathBuf {
        let repo = temp_repo_path(name);
        fs::create_dir_all(&repo).expect("failed to create temp repo");
        run_git_in(&repo, ["init"]);
        run_git_in(&repo, ["config", "user.name", "autodev tests"]);
        run_git_in(&repo, ["config", "user.email", "autodev@example.com"]);
        fs::write(repo.join("README.md"), "# temp\n").expect("failed to write README");
        run_git_in(&repo, ["add", "README.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);
        repo
    }

    fn sample_args(model: &str) -> NemesisArgs {
        NemesisArgs {
            prompt_file: None,
            output_dir: None,
            resume: false,
            profile: crate::HardeningProfile::Balanced,
            model: model.to_string(),
            reasoning_effort: "high".to_string(),
            reviewer_model: "kimi".to_string(),
            reviewer_effort: "high".to_string(),
            kimi: false,
            minimax: false,
            report_only: false,
            branch: None,
            dry_run: true,
            fixer_model: "gpt-5.5".to_string(),
            fixer_effort: "high".to_string(),
            finalizer_model: "gpt-5.5".to_string(),
            finalizer_effort: "high".to_string(),
            audit_passes: 1,
            codex_bin: PathBuf::from("codex"),
            pi_bin: PathBuf::from("pi"),
            kimi_bin: PathBuf::from("kimi-cli"),
            use_kimi_cli: false,
        }
    }

    #[test]
    fn select_backend_treats_minimax_model_alias_as_pi() {
        let args = sample_args("minimax");
        let backend = select_backend(
            &args.model,
            &args.reasoning_effort,
            Path::new("codex"),
            Path::new("pi"),
            Path::new("kimi-cli"),
            false,
        );
        assert_eq!(backend.label(), "pi-minimax");
        assert_eq!(backend.model(), "minimax/MiniMax-M2.7-highspeed");
        assert_eq!(backend.variant(), "high");
    }

    #[test]
    fn select_backend_routes_kimi_through_kimi_cli_when_flag_is_on() {
        let args = sample_args("k2.6");
        let backend = select_backend(
            &args.model,
            &args.reasoning_effort,
            Path::new("codex"),
            Path::new("pi"),
            Path::new("kimi-cli"),
            true,
        );
        assert_eq!(backend.label(), "kimi-cli");
        // `k2.6` is the short id; it must be resolved to the provider-qualified
        // name kimi-cli actually reads from ~/.kimi/config.toml.
        assert_eq!(backend.model(), "kimi-code/kimi-for-coding");
        assert_eq!(backend.variant(), "high");
    }

    #[test]
    fn select_backend_treats_kimi_model_alias_as_pi_when_kimi_cli_off() {
        let args = sample_args("kimi");
        let backend = select_backend(
            &args.model,
            &args.reasoning_effort,
            Path::new("codex"),
            Path::new("pi"),
            Path::new("kimi-cli"),
            false,
        );
        assert_eq!(backend.label(), "pi-kimi");
        assert_eq!(backend.model(), "kimi-coding/k2p6");
        assert_eq!(backend.variant(), "high");
    }

    #[test]
    fn select_backend_normalizes_explicit_minimax_model_override() {
        let args = sample_args("minimax-m2.7-highspeed");
        let backend = select_backend(
            &args.model,
            &args.reasoning_effort,
            Path::new("codex"),
            Path::new("pi"),
            Path::new("kimi-cli"),
            false,
        );
        assert_eq!(backend.label(), "pi-minimax");
        assert_eq!(backend.model(), "minimax/MiniMax-M2.7-highspeed");
    }

    #[test]
    fn explicit_model_takes_precedence_over_minimax_flag() {
        let mut args = sample_args("kimi-coding/k2p5");
        args.minimax = true;
        assert_eq!(resolve_auditor_model(&args), "kimi-coding/k2p5");
    }

    #[test]
    fn explicit_model_takes_precedence_over_kimi_flag() {
        let mut args = sample_args("minimax/MiniMax-M2.7-highspeed");
        args.kimi = true;
        assert_eq!(
            resolve_auditor_model(&args),
            "minimax/MiniMax-M2.7-highspeed"
        );
    }

    #[test]
    fn minimax_flag_selects_minimax_when_model_is_default() {
        let mut args = sample_args(DEFAULT_NEMESIS_AUDIT_MODEL);
        args.minimax = true;
        assert_eq!(resolve_auditor_model(&args), "minimax");
    }

    #[test]
    fn kimi_flag_selects_k2p6_when_model_is_default() {
        let mut args = sample_args(DEFAULT_NEMESIS_AUDIT_MODEL);
        args.kimi = true;
        assert_eq!(resolve_auditor_model(&args), "k2.6");
    }

    #[test]
    fn no_flags_and_default_model_resolves_to_new_default() {
        let args = sample_args(DEFAULT_NEMESIS_AUDIT_MODEL);
        assert_eq!(resolve_auditor_model(&args), DEFAULT_NEMESIS_AUDIT_MODEL);
    }

    #[test]
    fn nemesis_phase_accepts_codex_default_models() {
        let config = PhaseConfig {
            model: "gpt-5.5".to_string(),
            effort: "high".to_string(),
        };
        assert!(ensure_nemesis_phase_config("nemesis", &config).is_ok());
    }

    #[test]
    fn nemesis_phase_rejects_empty_models() {
        let config = PhaseConfig {
            model: "   ".to_string(),
            effort: "high".to_string(),
        };
        assert!(ensure_nemesis_phase_config("nemesis", &config).is_err());
    }

    #[test]
    fn nemesis_report_only_contract_matches_help() {
        let args = sample_args("gpt-5.5");
        validate_nemesis_execution_contract(&args).expect("normal execution accepted");

        let mut report_only = args.clone();
        report_only.report_only = true;
        validate_nemesis_execution_contract(&report_only)
            .expect("single-pass report-only is truthful");
    }

    #[test]
    fn nemesis_audit_passes_gt_one_is_truthful() {
        let mut args = sample_args("gpt-5.5");
        args.audit_passes = 2;
        validate_nemesis_execution_contract(&args).expect("multi-pass execution accepted");

        args.report_only = true;
        let err = validate_nemesis_execution_contract(&args)
            .expect_err("report-only multi-pass claim rejected");
        assert!(err.to_string().contains("--report-only"));
    }

    #[test]
    fn nemesis_fixer_accepts_kimi_model_now_that_it_drives_remediation() {
        let config = PhaseConfig {
            model: "k2.6".to_string(),
            effort: "high".to_string(),
        };
        assert!(
            ensure_nemesis_fixer_config(&config).is_ok(),
            "explicit Kimi fixer opt-ins should continue to work"
        );
    }

    #[test]
    fn nemesis_fixer_rejects_empty_model() {
        let config = PhaseConfig {
            model: "   ".to_string(),
            effort: "high".to_string(),
        };
        assert!(ensure_nemesis_fixer_config(&config).is_err());
    }

    #[test]
    fn nemesis_finalizer_rejects_kimi_model() {
        let config = PhaseConfig {
            model: "k2.6".to_string(),
            effort: "high".to_string(),
        };
        let error = ensure_nemesis_finalizer_config(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("must use a Codex model"));
    }

    #[test]
    fn prepare_output_dir_failure_points_to_archived_snapshot() {
        let repo = init_repo("output-dir-recovery");
        let output_dir = repo.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        fs::write(output_dir.join("nemesis-audit.md"), "# old\n")
            .expect("failed to seed old output");

        let archived = prepare_output_dir(&repo, &output_dir).expect("prepare should archive");
        let annotated = annotate_output_recovery(
            anyhow::anyhow!("simulated model failure"),
            &output_dir,
            archived.as_deref(),
            "Nemesis audit pass failed",
        );
        let message = format!("{annotated:#}");
        assert!(message.contains("simulated model failure"));
        assert!(message.contains("Previous outputs were archived at"));
        assert!(message.contains(
            archived
                .as_ref()
                .expect("snapshot should exist")
                .display()
                .to_string()
                .as_str()
        ));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn dry_run_output_dir_prep_is_non_destructive() {
        let repo = init_repo("dry-run-output-dir");
        let output_dir = repo.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        let original = output_dir.join("nemesis-audit.md");
        fs::write(&original, "# keep me\n").expect("failed to seed old output");

        let archived = maybe_prepare_output_dir(&repo, &output_dir, true, false)
            .expect("dry-run should succeed");
        assert!(archived.is_none());
        assert!(
            original.exists(),
            "dry-run should not delete existing outputs"
        );
        assert!(
            !repo.join(".auto").join("fresh-input").exists(),
            "dry-run should not archive output snapshots"
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }
}
