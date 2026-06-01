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
}

pub(crate) async fn run_pilot(args: PilotArgs) -> Result<()> {
    let intent = args.intent.join(" ");
    let paths = PilotPaths::resolve(&args)?;
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
        command_selection_from_surface, render_command_selection_markdown, safe_artifact_slug,
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
}
