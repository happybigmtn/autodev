use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Args;
use serde_json::{json, Value};

use crate::util::atomic_write;

const DEFAULT_FOCUS: &str =
    "prove Hermes/gbrain/autodev/Codex/Claude orchestration with supervised Codex workers";
const GBRAIN_CONTEXT_SLUGS: &[&str] = &[
    "manuals/canonical-development-operating-manual",
    "manuals/hermes-codex-supervisor-workflow",
    "manuals/hermes-orchestrator-development-memory",
    "infrastructure/dev-orchestrator-machine-registry-2026-06-01",
    "infrastructure/dev-orchestrator-security-baseline-2026-06-01",
];

#[derive(Args, Clone)]
pub(crate) struct PilotArgs {
    /// Repository slug under --base-dir, for example autonomy-bitino.
    pub(crate) repo_slug: String,

    /// Operator intent for the run.
    #[arg(required = true)]
    pub(crate) intent: Vec<String>,

    /// Base directory containing repository checkouts.
    #[arg(long, default_value = "/srv/dev/repos")]
    pub(crate) base_dir: PathBuf,

    /// Override the generated run id.
    #[arg(long)]
    pub(crate) run_id: Option<String>,

    /// Override the run artifact root.
    #[arg(long)]
    pub(crate) run_root: Option<PathBuf>,

    /// Autodev source checkout expected to match the installed auto binary.
    #[arg(long, default_value = "/srv/dev/repos/autodev")]
    pub(crate) autodev_source: PathBuf,

    /// Permit a no-origin repository to pass readiness as a local-only pilot.
    #[arg(long)]
    pub(crate) allow_local_only: bool,

    /// Minimum available disk, in KiB, required by orchestrator readiness.
    #[arg(long, default_value_t = 10_000_000)]
    pub(crate) min_disk_kb: u64,

    /// Focus string passed through the run environment and planning spine.
    #[arg(long, default_value = DEFAULT_FOCUS)]
    pub(crate) focus: String,

    /// Model hint for Codex/autodev execution.
    #[arg(long, default_value = "gpt-5.5")]
    pub(crate) model: String,

    /// Execution reasoning-effort hint.
    #[arg(long, default_value = "high")]
    pub(crate) effort: String,

    /// Planning reasoning-effort hint.
    #[arg(long, default_value = "xhigh")]
    pub(crate) plan_effort: String,

    /// Worker lane hint.
    #[arg(long, default_value_t = 3)]
    pub(crate) threads: usize,

    /// Planning mode selected by the wrapper.
    #[arg(long, default_value = "auto")]
    pub(crate) planning_mode: String,

    /// Require the normal corpus/gen planning spine.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub(crate) require_planning_spine: bool,

    /// Optional authoritative plan file copied into the run root.
    #[arg(long)]
    pub(crate) plan: Option<PathBuf>,

    /// Optional sibling source repositories used as planning references.
    #[arg(long = "reference-repo")]
    pub(crate) reference_repos: Vec<PathBuf>,

    /// Only run typed preflight/setup. Execution remains owned by the caller.
    #[arg(long)]
    pub(crate) preflight_only: bool,

    /// Run typed preflight plus the autodev planning spine, then stop before execution.
    #[arg(long)]
    pub(crate) planning_only: bool,

    /// Validate a completed pilot run's closeout artifacts, then stop.
    #[arg(long)]
    pub(crate) closeout_only: bool,
}

pub(crate) async fn run_pilot(args: PilotArgs) -> Result<()> {
    let selected_modes = [args.preflight_only, args.planning_only, args.closeout_only]
        .iter()
        .filter(|selected| **selected)
        .count();
    if selected_modes > 1 {
        bail!("--preflight-only, --planning-only, and --closeout-only are mutually exclusive");
    }
    let intent = args.intent.join(" ");
    let paths = PilotPaths::resolve(&args)?;
    if args.closeout_only {
        validate_closeout(&args, &paths)?;
        println!("repo: {}", args.repo_slug);
        println!("workdir: {}", paths.workdir.display());
        println!("run: {}", paths.run_id);
        println!("run_root: {}", paths.run_root.display());
        println!(
            "closeout: {}",
            paths.run_root.join("pilot-closeout.json").display()
        );
        println!("pilot closeout ok");
        return Ok(());
    }
    fs::create_dir_all(paths.run_root.join("gbrain"))?;
    fs::create_dir_all(paths.run_root.join("codex"))?;
    fs::create_dir_all(paths.run_root.join("logs"))?;

    let remote_sync_mode = remote_sync_mode(&paths.repo);
    let current_branch = current_branch(&paths.workdir);

    write_run_env(&args, &paths, &intent, &remote_sync_mode, &current_branch)?;
    capture_auto_surface(&paths, &args.repo_slug)?;
    run_doctor_preflights(&args, &paths, &remote_sync_mode)?;
    capture_gbrain_context(&args, &paths)?;
    copy_plan_input(&args, &paths)?;

    println!("repo: {}", args.repo_slug);
    println!("workdir: {}", paths.workdir.display());
    println!("run: {}", paths.run_id);
    println!("run_root: {}", paths.run_root.display());
    println!(
        "preflight: {}",
        paths.run_root.join("pilot-preflight.json").display()
    );
    if args.preflight_only {
        println!("pilot preflight ok");
        return Ok(());
    }
    if args.planning_only {
        run_planning_phase(&args, &paths, &intent)?;
        println!(
            "planning: {}",
            paths.run_root.join("pilot-planning.json").display()
        );
        println!("pilot planning ok");
    }
    Ok(())
}

struct PilotPaths {
    repo: PathBuf,
    workdir: PathBuf,
    run_id: String,
    run_root: PathBuf,
}

impl PilotPaths {
    fn resolve(args: &PilotArgs) -> Result<Self> {
        let repo = args.base_dir.join(&args.repo_slug);
        let run_id = args.run_id.clone().unwrap_or_else(|| {
            format!("{}-{}", Utc::now().format("%Y%m%dT%H%M%SZ"), args.repo_slug)
        });
        let (workdir, default_run_root) = if repo.is_dir() {
            (repo.clone(), repo.join(".auto/orchestrator").join(&run_id))
        } else {
            (
                args.base_dir.clone(),
                PathBuf::from("/home/dev/orchestrator-runs").join(&run_id),
            )
        };
        let run_root = args.run_root.clone().unwrap_or(default_run_root);
        Ok(Self {
            repo,
            workdir,
            run_id,
            run_root,
        })
    }
}

fn write_run_env(
    args: &PilotArgs,
    paths: &PilotPaths,
    intent: &str,
    remote_sync_mode: &str,
    current_branch: &str,
) -> Result<()> {
    let reference_repos = if args.reference_repos.is_empty() {
        "none".to_string()
    } else {
        args.reference_repos
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let plan_path = args
        .plan
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".to_string());
    let auto_skip_remote_sync = if remote_sync_mode == "local-no-origin" {
        "1"
    } else {
        ""
    };
    let body = format!(
        "\
repo_slug={repo_slug}
repo={repo}
workdir={workdir}
run_id={run_id}
run_root={run_root}
prompt={intent}
focus={focus}
model={model}
effort={effort}
plan_effort={plan_effort}
threads={threads}
plan_path={plan_path}
planning_mode={planning_mode}
require_planning_spine={require_planning_spine}
preflight_only={preflight_only}
planning_only={planning_only}
closeout_only={closeout_only}
reference_repos={reference_repos}
autodev_source={autodev_source}
remote_sync_mode={remote_sync_mode}
current_branch={current_branch}
AUTO_SKIP_REMOTE_SYNC={auto_skip_remote_sync}
created={created}
",
        repo_slug = args.repo_slug,
        repo = paths.repo.display(),
        workdir = paths.workdir.display(),
        run_id = paths.run_id,
        run_root = paths.run_root.display(),
        focus = args.focus,
        model = args.model,
        effort = args.effort,
        plan_effort = args.plan_effort,
        threads = args.threads,
        planning_mode = args.planning_mode,
        require_planning_spine = if args.require_planning_spine { 1 } else { 0 },
        preflight_only = if args.preflight_only { 1 } else { 0 },
        planning_only = if args.planning_only { 1 } else { 0 },
        closeout_only = if args.closeout_only { 1 } else { 0 },
        autodev_source = args.autodev_source.display(),
        created = Utc::now().format("%FT%TZ"),
    );
    atomic_write(&paths.run_root.join("run.env"), body.as_bytes())
}

fn capture_auto_surface(paths: &PilotPaths, repo_slug: &str) -> Result<()> {
    let current_exe =
        std::env::current_exe().context("failed to resolve current auto executable")?;
    let logs = paths.run_root.join("logs");
    write_command_output(
        &paths.workdir,
        &current_exe,
        &["--version"],
        &logs.join("auto-version.log"),
    )?;
    write_command_output(
        &paths.workdir,
        &current_exe,
        &["--help"],
        &logs.join("auto-help.log"),
    )?;
    let command_surface_output =
        run_command(&paths.workdir, &current_exe, &["command-surface", "--json"])?;
    if !command_surface_output.status.success() {
        bail!(
            "auto command-surface --json failed: {}",
            compact_output(&command_surface_output)
        );
    }
    let command_surface = String::from_utf8_lossy(&command_surface_output.stdout).to_string();
    let command_surface_path = logs.join("auto-command-surface.json");
    atomic_write(&command_surface_path, command_surface.as_bytes())?;
    let surface: Value = serde_json::from_str(&command_surface)
        .with_context(|| format!("failed to parse {}", command_surface_path.display()))?;
    let command_names = surface
        .get("commands")
        .and_then(Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(|command| command.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    atomic_write(
        &logs.join("auto-commands.txt"),
        command_names.join("\n").as_bytes(),
    )?;
    for command_name in &command_names {
        let log_path = logs.join(format!("auto-help-{command_name}.log"));
        let _ = write_command_output(
            &paths.workdir,
            &current_exe,
            &[command_name.as_str(), "--help"],
            &log_path,
        );
    }

    let selection = command_selection_from_surface(&surface, &paths.run_id, repo_slug);
    atomic_write(
        &paths.run_root.join("autodev-command-selection.json"),
        serde_json::to_string_pretty(&selection)?.as_bytes(),
    )?;
    atomic_write(
        &paths.run_root.join("autodev-command-selection.md"),
        render_command_selection_markdown(&selection, &paths.run_id).as_bytes(),
    )?;
    Ok(())
}

fn run_planning_phase(args: &PilotArgs, paths: &PilotPaths, intent: &str) -> Result<()> {
    let current_exe =
        std::env::current_exe().context("failed to resolve current auto executable")?;
    let effective_mode = effective_planning_mode(&args.planning_mode, &paths.workdir)?;
    let mut records = Vec::new();

    if args.require_planning_spine && effective_mode != "none" {
        let mut corpus_args = vec![
            "corpus".to_string(),
            "--idea".to_string(),
            intent.to_string(),
            "--focus".to_string(),
            args.focus.clone(),
            "--model".to_string(),
            args.model.clone(),
            "--reasoning-effort".to_string(),
            args.plan_effort.clone(),
            "--review-model".to_string(),
            args.model.clone(),
            "--review-effort".to_string(),
            args.plan_effort.clone(),
        ];
        append_reference_repo_args(&mut corpus_args, &args.reference_repos);
        let record = run_logged_auto(
            &paths.workdir,
            &current_exe,
            corpus_args,
            &paths.run_root.join("corpus.log"),
            "corpus",
        )?;
        let success = command_record_success(&record);
        records.push(record);
        if !success {
            write_planning_manifest(args, paths, &effective_mode, &records)?;
            bail!(
                "auto corpus failed; see {}",
                paths.run_root.join("corpus.log").display()
            );
        }

        let gen_args = vec![
            "gen".to_string(),
            "--snapshot-only".to_string(),
            "--model".to_string(),
            args.model.clone(),
            "--reasoning-effort".to_string(),
            args.plan_effort.clone(),
            "--review-model".to_string(),
            args.model.clone(),
            "--review-effort".to_string(),
            args.plan_effort.clone(),
        ];
        let record = run_logged_auto(
            &paths.workdir,
            &current_exe,
            gen_args,
            &paths.run_root.join("gen.log"),
            "gen",
        )?;
        let success = command_record_success(&record);
        records.push(record);
        if !success {
            write_planning_manifest(args, paths, &effective_mode, &records)?;
            bail!(
                "auto gen failed; see {}",
                paths.run_root.join("gen.log").display()
            );
        }
    }

    if matches!(effective_mode.as_str(), "steward" | "full") {
        let mut steward_args = vec![
            "steward".to_string(),
            "--report-only".to_string(),
            "--output-dir".to_string(),
            paths
                .run_root
                .join("steward-preflight")
                .display()
                .to_string(),
            "--model".to_string(),
            args.model.clone(),
            "--reasoning-effort".to_string(),
            args.effort.clone(),
            "--finalizer-model".to_string(),
            args.model.clone(),
            "--finalizer-effort".to_string(),
            args.effort.clone(),
        ];
        append_reference_repo_args(&mut steward_args, &args.reference_repos);
        let record = run_logged_auto(
            &paths.workdir,
            &current_exe,
            steward_args,
            &paths.run_root.join("steward-preflight.log"),
            "steward",
        )?;
        let success = command_record_success(&record);
        records.push(record);
        if !success {
            write_planning_manifest(args, paths, &effective_mode, &records)?;
            bail!(
                "auto steward failed; see {}",
                paths.run_root.join("steward-preflight.log").display()
            );
        }
    }

    write_planning_manifest(args, paths, &effective_mode, &records)
}

fn effective_planning_mode(requested: &str, workdir: &Path) -> Result<String> {
    match requested {
        "auto" => {
            if workdir.join("IMPLEMENTATION_PLAN.md").exists()
                || workdir.join("WORKLIST.md").exists()
            {
                Ok("full".to_string())
            } else {
                Ok("greenfield".to_string())
            }
        }
        "greenfield" | "steward" | "full" | "none" => Ok(requested.to_string()),
        other => bail!("unsupported planning mode: {other}"),
    }
}

fn append_reference_repo_args(args: &mut Vec<String>, reference_repos: &[PathBuf]) {
    for repo in reference_repos {
        args.push("--reference-repo".to_string());
        args.push(repo.display().to_string());
    }
}

fn run_logged_auto(
    cwd: &Path,
    command: &Path,
    args: Vec<String>,
    log_path: &Path,
    phase: &str,
) -> Result<Value> {
    let output = run_command_strings(cwd, command, &args)?;
    atomic_write(log_path, render_output(&output).as_bytes())?;
    Ok(json!({
        "phase": phase,
        "argv": args,
        "log": log_path,
        "status": {
            "success": output.status.success(),
            "code": output.status.code()
        }
    }))
}

fn command_record_success(record: &Value) -> bool {
    record
        .get("status")
        .and_then(|status| status.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn write_planning_manifest(
    args: &PilotArgs,
    paths: &PilotPaths,
    effective_mode: &str,
    records: &[Value],
) -> Result<()> {
    let manifest = json!({
        "schema_version": 1,
        "repo_slug": args.repo_slug,
        "repo": paths.repo,
        "workdir": paths.workdir,
        "run_id": paths.run_id,
        "run_root": paths.run_root,
        "created": Utc::now().format("%FT%TZ").to_string(),
        "requested_planning_mode": args.planning_mode,
        "effective_planning_mode": effective_mode,
        "require_planning_spine": args.require_planning_spine,
        "commands": records,
        "artifacts": {
            "preflight_manifest": paths.run_root.join("pilot-preflight.json"),
            "planning_manifest": paths.run_root.join("pilot-planning.json"),
            "corpus_log": paths.run_root.join("corpus.log"),
            "gen_log": paths.run_root.join("gen.log"),
            "steward_preflight_log": paths.run_root.join("steward-preflight.log"),
            "steward_preflight_dir": paths.run_root.join("steward-preflight")
        }
    });
    atomic_write(
        &paths.run_root.join("pilot-planning.json"),
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
    )
}

fn run_doctor_preflights(
    args: &PilotArgs,
    paths: &PilotPaths,
    remote_sync_mode: &str,
) -> Result<()> {
    let current_exe =
        std::env::current_exe().context("failed to resolve current auto executable")?;
    let mut orchestrator_args = vec![
        "doctor".to_string(),
        "--orchestrator".to_string(),
        "--autodev-source".to_string(),
        args.autodev_source.display().to_string(),
        "--min-disk-kb".to_string(),
        args.min_disk_kb.to_string(),
    ];
    if args.allow_local_only || remote_sync_mode == "local-no-origin" {
        orchestrator_args.push("--allow-local-only".to_string());
    }
    let orchestrator_refs = orchestrator_args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let orchestrator = run_command(&paths.workdir, &current_exe, &orchestrator_refs)?;
    atomic_write(
        &paths.run_root.join("orchestrator-doctor.log"),
        render_output(&orchestrator).as_bytes(),
    )?;
    if !orchestrator.status.success() {
        bail!(
            "auto doctor --orchestrator failed: {}",
            compact_output(&orchestrator)
        );
    }

    let doctor = run_command(&paths.workdir, &current_exe, &["doctor"])?;
    atomic_write(
        &paths.run_root.join("doctor.log"),
        render_output(&doctor).as_bytes(),
    )?;
    if !doctor.status.success() {
        bail!("auto doctor failed: {}", compact_output(&doctor));
    }
    Ok(())
}

fn capture_gbrain_context(args: &PilotArgs, paths: &PilotPaths) -> Result<()> {
    let mut slugs = GBRAIN_CONTEXT_SLUGS
        .iter()
        .copied()
        .map(str::to_string)
        .collect::<Vec<_>>();
    slugs.push(format!("projects/{}", args.repo_slug));
    for slug in slugs {
        let safe_slug = safe_artifact_slug(&slug);
        let output = Command::new("gbrain")
            .arg("get")
            .arg(&slug)
            .current_dir(&paths.workdir)
            .output();
        match output {
            Ok(output) => {
                atomic_write(
                    &paths
                        .run_root
                        .join("gbrain")
                        .join(format!("{safe_slug}.md")),
                    &output.stdout,
                )?;
                atomic_write(
                    &paths
                        .run_root
                        .join("gbrain")
                        .join(format!("{safe_slug}.err")),
                    &output.stderr,
                )?;
            }
            Err(err) => {
                atomic_write(
                    &paths
                        .run_root
                        .join("gbrain")
                        .join(format!("{safe_slug}.err")),
                    err.to_string().as_bytes(),
                )?;
            }
        }
    }
    Ok(())
}

fn copy_plan_input(args: &PilotArgs, paths: &PilotPaths) -> Result<()> {
    let Some(plan) = &args.plan else {
        write_preflight_manifest(args, paths, None)?;
        return Ok(());
    };
    let bytes = fs::read(plan).with_context(|| format!("failed to read {}", plan.display()))?;
    atomic_write(&paths.run_root.join("plan-input.md"), &bytes)?;
    write_preflight_manifest(args, paths, Some(plan))
}

fn write_preflight_manifest(
    args: &PilotArgs,
    paths: &PilotPaths,
    plan: Option<&Path>,
) -> Result<()> {
    let manifest = json!({
        "schema_version": 1,
        "repo_slug": args.repo_slug,
        "repo": paths.repo,
        "workdir": paths.workdir,
        "run_id": paths.run_id,
        "run_root": paths.run_root,
        "plan_input": plan,
        "created": Utc::now().format("%FT%TZ").to_string(),
        "artifacts": {
            "run_env": paths.run_root.join("run.env"),
            "orchestrator_doctor": paths.run_root.join("orchestrator-doctor.log"),
            "doctor": paths.run_root.join("doctor.log"),
            "command_surface": paths.run_root.join("logs/auto-command-surface.json"),
            "command_selection_json": paths.run_root.join("autodev-command-selection.json"),
            "command_selection_markdown": paths.run_root.join("autodev-command-selection.md"),
            "gbrain_dir": paths.run_root.join("gbrain")
        }
    });
    atomic_write(
        &paths.run_root.join("pilot-preflight.json"),
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
    )
}

fn validate_closeout(args: &PilotArgs, paths: &PilotPaths) -> Result<()> {
    let mut errors = Vec::new();
    require_nonempty_artifact(
        &paths.run_root.join("pilot-preflight.json"),
        "preflight manifest",
        &mut errors,
    );
    let planning_manifest_path = paths.run_root.join("pilot-planning.json");
    require_nonempty_artifact(&planning_manifest_path, "planning manifest", &mut errors);
    require_nonempty_artifact(
        &paths.run_root.join("autodev-command-selection.json"),
        "command-selection json",
        &mut errors,
    );
    require_nonempty_artifact(
        &paths.run_root.join("autodev-command-selection.md"),
        "command-selection markdown",
        &mut errors,
    );
    require_nonempty_artifact(&paths.run_root.join("receipt.md"), "receipt", &mut errors);

    validate_command_selection(&paths.run_root, &mut errors);
    validate_receipt(&paths.run_root.join("receipt.md"), &mut errors);
    validate_planning_manifest(&planning_manifest_path, &mut errors);
    validate_steward_promotion_decisions(&paths.run_root, &mut errors);

    let success = errors.is_empty();
    let manifest = json!({
        "schema_version": 1,
        "repo_slug": args.repo_slug,
        "repo": paths.repo,
        "workdir": paths.workdir,
        "run_id": paths.run_id,
        "run_root": paths.run_root,
        "created": Utc::now().format("%FT%TZ").to_string(),
        "status": if success { "ok" } else { "failed" },
        "errors": errors,
        "artifacts": {
            "preflight_manifest": paths.run_root.join("pilot-preflight.json"),
            "planning_manifest": planning_manifest_path,
            "closeout_manifest": paths.run_root.join("pilot-closeout.json"),
            "command_selection_json": paths.run_root.join("autodev-command-selection.json"),
            "command_selection_markdown": paths.run_root.join("autodev-command-selection.md"),
            "receipt": paths.run_root.join("receipt.md"),
            "orchestration_failure": paths.run_root.join("orchestration-failure.md")
        }
    });
    atomic_write(
        &paths.run_root.join("pilot-closeout.json"),
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
    )?;
    if !success {
        let rendered = manifest["errors"]
            .as_array()
            .map_or_else(String::new, |errors| {
                errors
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|error| format!("- {error}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        atomic_write(
            &paths.run_root.join("orchestration-failure.md"),
            format!("# Orchestration Failure\n\n{rendered}\n").as_bytes(),
        )?;
        bail!(
            "pilot closeout validation failed; see {}",
            paths.run_root.join("pilot-closeout.json").display()
        );
    }
    Ok(())
}

fn require_nonempty_artifact(path: &Path, label: &str, errors: &mut Vec<String>) {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => {}
        _ => errors.push(format!("missing required {label}: {}", path.display())),
    }
}

fn validate_command_selection(run_root: &Path, errors: &mut Vec<String>) {
    let json_path = run_root.join("autodev-command-selection.json");
    let Ok(text) = fs::read_to_string(&json_path) else {
        return;
    };
    let Ok(selection) = serde_json::from_str::<Value>(&text) else {
        errors.push(format!(
            "invalid command-selection json: {}",
            json_path.display()
        ));
        return;
    };
    let Some(commands) = selection.get("commands").and_then(Value::as_array) else {
        errors.push("command-selection json has no commands array".to_string());
        return;
    };
    if commands.is_empty() {
        errors.push("command-selection json has an empty commands array".to_string());
    }
    for command in commands {
        validate_decision_node(command, "command", errors);
    }

    let markdown_path = run_root.join("autodev-command-selection.md");
    if let Ok(markdown) = fs::read_to_string(&markdown_path) {
        if markdown.contains("UNDECIDED") {
            errors.push(format!(
                "command-selection markdown still contains UNDECIDED: {}",
                markdown_path.display()
            ));
        }
    }
}

fn validate_decision_node(node: &Value, path: &str, errors: &mut Vec<String>) {
    let command = node.get("command").and_then(Value::as_str).unwrap_or(path);
    let decision = node.get("decision").and_then(Value::as_str).unwrap_or("");
    let reason = node.get("reason").and_then(Value::as_str).unwrap_or("");
    if decision.is_empty() || decision == "UNDECIDED" {
        errors.push(format!("missing decision for {command}"));
    }
    if reason.trim().is_empty() {
        errors.push(format!("missing reason for {command}"));
    }
    for key in ["actions", "subcommands"] {
        if let Some(children) = node.get(key).and_then(Value::as_array) {
            for child in children {
                validate_decision_node(child, command, errors);
            }
        }
    }
}

fn validate_receipt(path: &Path, errors: &mut Vec<String>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let lower = text.to_ascii_lowercase();
    for required in [
        "inputs",
        "commands",
        "artifacts",
        "tests",
        "commit",
        "risk",
        "next",
    ] {
        if !lower.contains(required) {
            errors.push(format!(
                "receipt missing required closeout term `{required}`: {}",
                path.display()
            ));
        }
    }
}

fn validate_planning_manifest(path: &Path, errors: &mut Vec<String>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(manifest) = serde_json::from_str::<Value>(&text) else {
        errors.push(format!(
            "invalid planning manifest json: {}",
            path.display()
        ));
        return;
    };
    let require_spine = manifest
        .get("require_planning_spine")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let effective_mode = manifest
        .get("effective_planning_mode")
        .and_then(Value::as_str)
        .unwrap_or("");
    let commands = manifest
        .get("commands")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if require_spine && effective_mode != "none" {
        require_successful_phase(&commands, "corpus", errors);
        require_successful_phase(&commands, "gen", errors);
    }
    if matches!(effective_mode, "steward" | "full") {
        require_successful_phase(&commands, "steward", errors);
    }
}

fn require_successful_phase(commands: &[Value], phase: &str, errors: &mut Vec<String>) {
    let ok = commands.iter().any(|command| {
        command.get("phase").and_then(Value::as_str) == Some(phase)
            && command
                .get("status")
                .and_then(|status| status.get("success"))
                .and_then(Value::as_bool)
                == Some(true)
    });
    if !ok {
        errors.push(format!(
            "planning manifest missing successful `{phase}` phase"
        ));
    }
}

fn validate_steward_promotion_decisions(run_root: &Path, errors: &mut Vec<String>) {
    let promotions = run_root.join("steward-preflight/PROMOTIONS.md");
    if !promotions.exists() {
        return;
    }
    let decisions = run_root.join("steward-promotion-decisions.md");
    require_nonempty_artifact(&decisions, "steward promotion decisions", errors);
    if let Ok(text) = fs::read_to_string(&decisions) {
        if text.contains("UNDECIDED") {
            errors.push(format!(
                "steward promotion decisions still contain UNDECIDED: {}",
                decisions.display()
            ));
        }
    }
}

fn command_selection_from_surface(surface: &Value, run_id: &str, repo: &str) -> Value {
    let commands = surface
        .get("commands")
        .and_then(Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .map(selection_command_from_surface_command)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "schema_version": 1,
        "run_id": run_id,
        "repo": repo,
        "source": "auto command-surface --json",
        "commands": commands
    })
}

fn selection_command_from_surface_command(command: &Value) -> Value {
    let command_name = command.get("name").and_then(Value::as_str).unwrap_or("");
    let subcommands = command
        .get("subcommands")
        .and_then(Value::as_array)
        .map(|subcommands| {
            subcommands
                .iter()
                .map(|subcommand| {
                    selection_subcommand_from_surface_command(
                        subcommand,
                        &format!("auto {command_name}"),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let actions = command
        .get("arguments")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter(|arg| arg.get("id").and_then(Value::as_str) == Some("action"))
                .flat_map(|arg| {
                    arg.get("possible_values")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .map(|value| {
                    let action_name = value.get("name").and_then(Value::as_str).unwrap_or("");
                    json!({
                        "name": action_name,
                        "help": value.get("help").and_then(Value::as_str).unwrap_or(""),
                        "command": format!("auto {command_name} {action_name}"),
                        "decision": "UNDECIDED",
                        "reason": ""
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "name": command_name,
        "about": command.get("about").and_then(Value::as_str).unwrap_or(""),
        "usage": command.get("usage").and_then(Value::as_str).unwrap_or(""),
        "command": format!("auto {command_name}"),
        "decision": "UNDECIDED",
        "reason": "",
        "subcommands": subcommands,
        "actions": actions
    })
}

fn selection_subcommand_from_surface_command(command: &Value, parent_command: &str) -> Value {
    let command_name = command.get("name").and_then(Value::as_str).unwrap_or("");
    let full_command = format!("{parent_command} {command_name}");
    json!({
        "name": command_name,
        "about": command.get("about").and_then(Value::as_str).unwrap_or(""),
        "usage": command.get("usage").and_then(Value::as_str).unwrap_or(""),
        "command": full_command.clone(),
        "decision": "UNDECIDED",
        "reason": "",
        "subcommands": command.get("subcommands")
            .and_then(Value::as_array)
            .map(|subcommands| {
                subcommands
                    .iter()
                    .map(|subcommand| selection_subcommand_from_surface_command(subcommand, &full_command))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    })
}

fn render_command_selection_markdown(selection: &Value, run_id: &str) -> String {
    let mut out = String::new();
    out.push_str("# Autodev Command Selection\n\n");
    out.push_str(&format!("Run: `{run_id}`\n\n"));
    out.push_str("Structured ledger: `autodev-command-selection.json`\n\n");
    out.push_str("Every command, action, and subcommand discovered from the live `auto` surface must be marked `selected`, `deferred`, or `skipped` before implementation.\n\n");
    out.push_str("| Surface | Decision | Reason and Evidence |\n");
    out.push_str("|---|---|---|\n");
    if let Some(commands) = selection.get("commands").and_then(Value::as_array) {
        for command in commands {
            append_selection_markdown_rows(&mut out, command, "command");
        }
    }
    out
}

fn append_selection_markdown_rows(out: &mut String, command: &Value, kind: &str) {
    let surface = command.get("command").and_then(Value::as_str).unwrap_or("");
    let about = command
        .get("about")
        .or_else(|| command.get("help"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .replace('|', "/");
    out.push_str(&format!("| `{surface}` | UNDECIDED | {kind}: {about} |\n"));
    if let Some(actions) = command.get("actions").and_then(Value::as_array) {
        for action in actions {
            append_selection_markdown_rows(out, action, "action");
        }
    }
    if let Some(subcommands) = command.get("subcommands").and_then(Value::as_array) {
        for subcommand in subcommands {
            append_selection_markdown_rows(out, subcommand, "subcommand");
        }
    }
}

fn safe_artifact_slug(slug: &str) -> String {
    slug.replace('/', "__")
}

fn remote_sync_mode(repo: &Path) -> String {
    if repo.join(".git").exists()
        && Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["remote", "get-url", "origin"])
            .output()
            .is_ok_and(|output| !output.status.success())
    {
        "local-no-origin".to_string()
    } else {
        "normal".to_string()
    }
}

fn current_branch(workdir: &Path) -> String {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["branch", "--show-current"])
        .output()
    else {
        return "unknown".to_string();
    };
    if !output.status.success() {
        return "unknown".to_string();
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        "detached".to_string()
    } else {
        branch
    }
}

fn write_command_output(cwd: &Path, command: &Path, args: &[&str], path: &Path) -> Result<()> {
    let output = run_command(cwd, command, args)?;
    atomic_write(path, render_output(&output).as_bytes())
}

fn run_command(cwd: &Path, command: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new(command)
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run {} {}", command.display(), args.join(" ")))
}

fn run_command_strings(
    cwd: &Path,
    command: &Path,
    args: &[String],
) -> Result<std::process::Output> {
    Command::new(command)
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run {} {}", command.display(), args.join(" ")))
}

fn render_output(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn compact_output(output: &std::process::Output) -> String {
    let mut text = render_output(output)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.len() > 240 {
        text.truncate(240);
        text.push_str("...");
    }
    if text.is_empty() {
        format!("exit status {}", output.status)
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;
    use serde_json::json;

    use crate::cli::{Cli, Command};

    use super::{
        command_selection_from_surface, effective_planning_mode, render_command_selection_markdown,
        safe_artifact_slug, validate_closeout, PilotPaths,
    };

    #[test]
    fn pilot_args_allow_options_after_quoted_intent() {
        let cli = Cli::try_parse_from([
            "auto",
            "pilot",
            "autonomy-bitino",
            "typed pilot preflight self-test",
            "--run-id",
            "run-1",
            "--run-root",
            "/tmp/pilot-run",
            "--preflight-only",
        ])
        .expect("pilot args parse");

        let Command::Pilot(args) = cli.command else {
            panic!("expected pilot command");
        };
        assert_eq!(args.repo_slug, "autonomy-bitino");
        assert_eq!(args.intent, vec!["typed pilot preflight self-test"]);
        assert_eq!(args.run_id.as_deref(), Some("run-1"));
        assert_eq!(args.run_root.as_deref(), Some(Path::new("/tmp/pilot-run")));
        assert!(args.preflight_only);
    }

    #[test]
    fn pilot_args_reject_preflight_and_planning_only_together() {
        let err = Cli::try_parse_from([
            "auto",
            "pilot",
            "autonomy-bitino",
            "intent",
            "--preflight-only",
            "--planning-only",
        ])
        .expect("clap allows both flags; runtime rejects this");

        let Command::Pilot(args) = err.command else {
            panic!("expected pilot command");
        };
        assert!(args.preflight_only);
        assert!(args.planning_only);
    }

    #[test]
    fn pilot_args_parse_closeout_only() {
        let cli = Cli::try_parse_from([
            "auto",
            "pilot",
            "autonomy-bitino",
            "intent",
            "--run-root",
            "/tmp/pilot-run",
            "--closeout-only",
        ])
        .expect("pilot args parse");

        let Command::Pilot(args) = cli.command else {
            panic!("expected pilot command");
        };
        assert!(args.closeout_only);
        assert_eq!(args.run_root.as_deref(), Some(Path::new("/tmp/pilot-run")));
    }

    #[test]
    fn effective_planning_mode_auto_detects_active_plan() {
        let temp = std::env::temp_dir().join(format!(
            "autodev-pilot-mode-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        assert_eq!(
            effective_planning_mode("auto", &temp).expect("mode"),
            "greenfield"
        );
        std::fs::write(temp.join("IMPLEMENTATION_PLAN.md"), "# Plan\n").expect("write plan");
        assert_eq!(
            effective_planning_mode("auto", &temp).expect("mode"),
            "full"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn effective_planning_mode_rejects_unknown_mode() {
        assert!(effective_planning_mode("surprise", Path::new("/tmp")).is_err());
    }

    #[test]
    fn command_selection_includes_commands_actions_and_subcommands() {
        let surface = json!({
            "commands": [
                {
                    "name": "parallel",
                    "about": "Run lanes",
                    "usage": "auto parallel [ACTION]",
                    "arguments": [
                        {
                            "id": "action",
                            "possible_values": [
                                {"name": "status", "help": "Show status"},
                                {"name": "receipt-backfill", "help": "Backfill receipts"}
                            ]
                        }
                    ],
                    "subcommands": [
                        {"name": "nested", "about": "Nested help", "usage": "auto nested", "arguments": [], "subcommands": []}
                    ]
                }
            ]
        });

        let selection = command_selection_from_surface(&surface, "run-1", "repo");
        let commands = selection["commands"].as_array().expect("commands array");
        assert_eq!(commands[0]["command"], "auto parallel");
        assert_eq!(commands[0]["decision"], "UNDECIDED");
        assert_eq!(commands[0]["actions"][0]["command"], "auto parallel status");
        assert_eq!(
            commands[0]["actions"][1]["command"],
            "auto parallel receipt-backfill"
        );
        assert_eq!(
            commands[0]["subcommands"][0]["command"],
            "auto parallel nested"
        );
    }

    #[test]
    fn command_selection_markdown_lists_surfaces() {
        let selection = json!({
            "commands": [
                {
                    "command": "auto doctor",
                    "about": "Check | readiness",
                    "actions": [],
                    "subcommands": [
                        {"command": "auto doctor nested", "about": "Nested", "actions": [], "subcommands": []}
                    ]
                }
            ]
        });

        let markdown = render_command_selection_markdown(&selection, "run-1");

        assert!(markdown.contains("Run: `run-1`"));
        assert!(markdown.contains("| `auto doctor` | UNDECIDED | command: Check / readiness |"));
        assert!(markdown.contains("| `auto doctor nested` | UNDECIDED | subcommand: Nested |"));
    }

    #[test]
    fn safe_artifact_slug_replaces_slashes() {
        assert_eq!(
            safe_artifact_slug("manuals/hermes-orchestrator-development-memory"),
            "manuals__hermes-orchestrator-development-memory"
        );
    }

    #[test]
    fn closeout_validation_accepts_complete_artifacts() {
        let root = unique_temp_dir("closeout-ok");
        write_complete_closeout_artifacts(&root, false);
        let args = test_pilot_args(root.clone());
        let paths = test_pilot_paths(root.clone());

        validate_closeout(&args, &paths).expect("valid closeout");

        assert!(root.join("pilot-closeout.json").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn closeout_validation_rejects_undecided_selection() {
        let root = unique_temp_dir("closeout-undecided");
        write_complete_closeout_artifacts(&root, true);
        let args = test_pilot_args(root.clone());
        let paths = test_pilot_paths(root.clone());

        let err = validate_closeout(&args, &paths).expect_err("invalid closeout");

        assert!(err.to_string().contains("pilot closeout validation failed"));
        let failure = std::fs::read_to_string(root.join("orchestration-failure.md"))
            .expect("failure artifact");
        assert!(failure.contains("missing decision"));
        let _ = std::fs::remove_dir_all(root);
    }

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-pilot-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn test_pilot_args(run_root: std::path::PathBuf) -> super::PilotArgs {
        super::PilotArgs {
            repo_slug: "repo".to_string(),
            intent: vec!["intent".to_string()],
            base_dir: run_root.parent().unwrap_or(Path::new("/tmp")).to_path_buf(),
            run_id: Some("run".to_string()),
            run_root: Some(run_root),
            autodev_source: Path::new("/srv/dev/repos/autodev").to_path_buf(),
            allow_local_only: true,
            min_disk_kb: 1,
            focus: "focus".to_string(),
            model: "model".to_string(),
            effort: "high".to_string(),
            plan_effort: "xhigh".to_string(),
            threads: 1,
            planning_mode: "none".to_string(),
            require_planning_spine: false,
            plan: None,
            reference_repos: Vec::new(),
            preflight_only: false,
            planning_only: false,
            closeout_only: true,
        }
    }

    fn test_pilot_paths(run_root: std::path::PathBuf) -> PilotPaths {
        PilotPaths {
            repo: run_root.clone(),
            workdir: run_root.clone(),
            run_id: "run".to_string(),
            run_root,
        }
    }

    fn write_complete_closeout_artifacts(root: &Path, undecided: bool) {
        std::fs::write(root.join("pilot-preflight.json"), "{}\n").expect("preflight");
        std::fs::write(
            root.join("pilot-planning.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "effective_planning_mode": "none",
                "require_planning_spine": false,
                "commands": []
            }))
            .expect("planning json"),
        )
        .expect("planning");
        let decision = if undecided { "UNDECIDED" } else { "selected" };
        let reason = if undecided {
            ""
        } else {
            "operator chose the planning path"
        };
        std::fs::write(
            root.join("autodev-command-selection.json"),
            serde_json::to_string_pretty(&json!({
                "commands": [
                    {
                        "command": "auto pilot",
                        "decision": decision,
                        "reason": reason,
                        "actions": [],
                        "subcommands": []
                    }
                ]
            }))
            .expect("selection json"),
        )
        .expect("selection");
        let markdown_decision = if undecided { "UNDECIDED" } else { "selected" };
        std::fs::write(
            root.join("autodev-command-selection.md"),
            format!("| `auto pilot` | {markdown_decision} | {reason} |\n"),
        )
        .expect("selection md");
        std::fs::write(
            root.join("receipt.md"),
            "\
# Receipt

## Inputs
gbrain context and planning manifest

## Commands
auto pilot --planning-only

## Artifacts
pilot-planning.json

## Tests
cargo test pilot_command

## Commit
not required for fixture

## Risks
none

## Next Command
auto pilot --closeout-only
",
        )
        .expect("receipt");
    }
}
