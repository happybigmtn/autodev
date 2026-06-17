use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use clap::Args;

use crate::corpus::load_planning_corpus;
use crate::state::load_state;
use crate::task_parser::{parse_tasks, TaskStatus};
use crate::util::git_repo_root;

const AUTODEV_REQUIRED_LAYOUT: &[&str] = &["Cargo.toml", "src/main.rs", "README.md", "AGENTS.md"];
const PROJECT_AGENT_INSTRUCTION_FILES: &[&str] =
    &["AGENTS.md", "CLAUDE.md", ".github/copilot-instructions.md"];
const HELP_SURFACES: &[&[&str]] = &[
    &["--help"],
    &["corpus", "--help"],
    &["gen", "--help"],
    &["design", "--help"],
    &["super", "--help"],
    &["parallel", "--help"],
    &["doctor", "--help"],
    &["quota", "--help"],
    &["audit-harvest", "--help"],
    &["symphony", "--help"],
];
const OPTIONAL_TOOLS: &[OptionalTool] = &[
    OptionalTool {
        name: "codex",
        workflows: "model-backed health, qa, review, generation, loop, and parallel flows",
    },
    OptionalTool {
        name: "claude",
        workflows: "Claude-backed corpus and generation flows",
    },
    OptionalTool {
        name: "pi",
        workflows: "quota-aware account multiplexing and legacy PI-selected flows",
    },
    OptionalTool {
        name: "gh",
        workflows: "GitHub-facing ship and review flows",
    },
];

#[derive(Args, Clone)]
pub(crate) struct DoctorArgs {
    /// Include primary-dev-machine orchestration checks: landing path, Hermes,
    /// gbrain, GitHub auth, installed auto/source consistency, and disk budget.
    #[arg(long)]
    pub(crate) orchestrator: bool,

    /// Local autodev source checkout that should match the installed auto binary.
    #[arg(long, default_value = "/srv/dev/repos/autodev")]
    pub(crate) autodev_source: PathBuf,

    /// Permit repositories without origin to pass the landing-path check as local-only pilots.
    #[arg(long)]
    pub(crate) allow_local_only: bool,

    /// Minimum available disk, in KiB, required for orchestrator runs.
    #[arg(long, default_value_t = 10_000_000)]
    pub(crate) min_disk_kb: u64,
}

pub(crate) async fn run_doctor(args: DoctorArgs) -> Result<()> {
    let current_exe = env::current_exe().context("failed to resolve current auto executable")?;
    let report = build_doctor_report(&current_exe, &args);
    print_doctor_report(&report);
    if report.required_failed(args.orchestrator) {
        return Err(anyhow!("doctor failed"));
    }
    Ok(())
}

#[derive(Debug)]
struct DoctorReport {
    baseline: Vec<RequiredCheck>,
    execution: Vec<RequiredCheck>,
    capabilities: Vec<CapabilityCheck>,
    orchestrator: Vec<RequiredCheck>,
}

impl DoctorReport {
    fn required_failed(&self, include_orchestrator: bool) -> bool {
        self.baseline.iter().any(|check| !check.passed)
            || (include_orchestrator && self.orchestrator.iter().any(|check| !check.passed))
    }
}

#[derive(Debug)]
struct RequiredCheck {
    name: String,
    passed: bool,
    detail: String,
    action: Option<String>,
}

#[derive(Debug)]
struct CapabilityCheck {
    tool: &'static str,
    found: Option<PathBuf>,
    workflows: &'static str,
}

#[derive(Clone, Debug)]
struct OptionalTool {
    name: &'static str,
    workflows: &'static str,
}

#[derive(Debug)]
struct CommandProbe {
    success: bool,
    stdout: String,
    stderr: String,
    launch_error: Option<String>,
}

fn build_doctor_report(current_exe: &Path, args: &DoctorArgs) -> DoctorReport {
    let mut report = DoctorReport {
        baseline: Vec::new(),
        execution: Vec::new(),
        capabilities: build_optional_tool_checks(find_on_path),
        orchestrator: Vec::new(),
    };

    let repo_root = git_repo_root();
    match &repo_root {
        Ok(repo_root) => {
            report.baseline.push(RequiredCheck {
                name: "repo root".to_string(),
                passed: true,
                detail: format!("found {}", repo_root.display()),
                action: None,
            });
            report.baseline.extend(check_repo_checkout(repo_root));
            report.execution.extend(check_planning_health(repo_root));
        }
        Err(err) => report.baseline.push(RequiredCheck {
            name: "repo root".to_string(),
            passed: false,
            detail: err.to_string(),
            action: Some("rerun from inside the repository checkout".to_string()),
        }),
    }

    if args.orchestrator {
        report.orchestrator = build_orchestrator_checks(
            repo_root.as_deref().ok(),
            &args.autodev_source,
            args.allow_local_only,
            args.min_disk_kb,
        );
    }

    report.baseline.push(check_version_probe(&run_auto_probe(
        current_exe,
        &["--version"],
    )));
    report.baseline.extend(check_help_surfaces_with(|args| {
        run_auto_probe(current_exe, args)
    }));

    report
}

fn build_orchestrator_checks(
    repo_root: Option<&Path>,
    autodev_source: &Path,
    allow_local_only: bool,
    min_disk_kb: u64,
) -> Vec<RequiredCheck> {
    let mut checks = vec![
        check_installed_auto_matches_source(autodev_source),
        check_required_command("codex", &["--version"]),
        check_required_command("claude", &["--version"]),
        check_required_command("gbrain", &["list", "-n", "1"]),
        check_required_command("gh", &["auth", "status"]),
        check_hermes_gateway(),
    ];

    if let Some(repo_root) = repo_root {
        checks.push(check_git_landing_path(repo_root, allow_local_only));
        checks.push(check_github_repo_permission(repo_root, allow_local_only));
        checks.push(check_disk_budget(repo_root, min_disk_kb));
    } else {
        checks.push(RequiredCheck {
            name: "git landing path".to_string(),
            passed: false,
            detail: "repo root unavailable".to_string(),
            action: Some("rerun from inside the repository checkout".to_string()),
        });
        checks.push(RequiredCheck {
            name: "disk budget".to_string(),
            passed: false,
            detail: "repo root unavailable".to_string(),
            action: Some("rerun from inside the repository checkout".to_string()),
        });
    }

    checks
}

fn check_repo_checkout(repo_root: &Path) -> Vec<RequiredCheck> {
    match read_cargo_manifest(&repo_root.join("Cargo.toml")) {
        Ok(Some(manifest)) if manifest_is_autodev_source(&manifest) => {
            let mut checks = check_autodev_required_layout(repo_root);
            checks.push(check_autodev_cargo_manifest(&manifest));
            checks
        }
        Ok(Some(_)) => vec![check_project_checkout_layout(repo_root)],
        Ok(None) => vec![check_project_checkout_layout(repo_root)],
        Err(check) => vec![check],
    }
}

fn check_installed_auto_matches_source(autodev_source: &Path) -> RequiredCheck {
    if !autodev_source.join(".git").exists() {
        return RequiredCheck {
            name: "autodev source match".to_string(),
            passed: false,
            detail: format!("missing git checkout at {}", autodev_source.display()),
            action: Some(
                "clone or mount the canonical autodev source before orchestrator runs".to_string(),
            ),
        };
    }

    let source_head =
        match command_stdout_in(autodev_source, "git", &["rev-parse", "--short", "HEAD"]) {
            Ok(source_head) => source_head,
            Err(err) => {
                return RequiredCheck {
                    name: "autodev source match".to_string(),
                    passed: false,
                    detail: err,
                    action: Some("repair the autodev source checkout".to_string()),
                };
            }
        };
    let installed = env!("AUTODEV_GIT_SHA");
    if source_head.trim() == installed {
        RequiredCheck {
            name: "autodev source match".to_string(),
            passed: true,
            detail: format!(
                "installed auto commit {installed} matches {}",
                autodev_source.display()
            ),
            action: None,
        }
    } else {
        RequiredCheck {
            name: "autodev source match".to_string(),
            passed: false,
            detail: format!(
                "installed auto commit {installed} does not match source commit {} at {}",
                source_head.trim(),
                autodev_source.display()
            ),
            action: Some(format!(
                "run cargo install --path {} --locked --root $HOME/.local",
                autodev_source.display()
            )),
        }
    }
}

fn check_required_command(command: &'static str, args: &[&str]) -> RequiredCheck {
    let output = Command::new(command).args(args).output();
    match output {
        Ok(output) if output.status.success() => RequiredCheck {
            name: format!("{command} availability"),
            passed: true,
            detail: format!("{command} {}", args.join(" ")),
            action: None,
        },
        Ok(output) => RequiredCheck {
            name: format!("{command} availability"),
            passed: false,
            detail: compact_command_output(&output),
            action: Some(format!(
                "install or authenticate `{command}` before orchestrator runs"
            )),
        },
        Err(err) => RequiredCheck {
            name: format!("{command} availability"),
            passed: false,
            detail: err.to_string(),
            action: Some(format!(
                "install `{command}` on PATH before orchestrator runs"
            )),
        },
    }
}

fn check_hermes_gateway() -> RequiredCheck {
    let hermes_help = Command::new("hermes").arg("--help").output();
    match hermes_help {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            return RequiredCheck {
                name: "Hermes gateway".to_string(),
                passed: false,
                detail: format!("hermes CLI failed: {}", compact_command_output(&output)),
                action: Some("repair the hermes CLI before orchestrator runs".to_string()),
            };
        }
        Err(err) => {
            return RequiredCheck {
                name: "Hermes gateway".to_string(),
                passed: false,
                detail: err.to_string(),
                action: Some("install `hermes` on PATH before orchestrator runs".to_string()),
            };
        }
    }

    let status = Command::new("systemctl")
        .args(["--user", "is-active", "hermes-gateway.service"])
        .output();
    match status {
        Ok(output) if output.status.success() => RequiredCheck {
            name: "Hermes gateway".to_string(),
            passed: true,
            detail: "user service hermes-gateway.service is active".to_string(),
            action: None,
        },
        Ok(output) => RequiredCheck {
            name: "Hermes gateway".to_string(),
            passed: false,
            detail: compact_command_output(&output),
            action: Some("start with systemctl --user start hermes-gateway.service".to_string()),
        },
        Err(err) => RequiredCheck {
            name: "Hermes gateway".to_string(),
            passed: false,
            detail: err.to_string(),
            action: Some("install or repair user systemd for hermes-gateway.service".to_string()),
        },
    }
}

fn check_git_landing_path(repo_root: &Path, allow_local_only: bool) -> RequiredCheck {
    let branch = command_stdout_in(repo_root, "git", &["branch", "--show-current"])
        .unwrap_or_else(|_| "detached".to_string());
    let branch = branch.trim();
    let origin = command_stdout_in(repo_root, "git", &["remote", "get-url", "origin"]);
    let Ok(origin) = origin else {
        return RequiredCheck {
            name: "git landing path".to_string(),
            passed: allow_local_only,
            detail: "no origin remote configured".to_string(),
            action: if allow_local_only {
                None
            } else {
                Some(
                    "configure origin or rerun with --allow-local-only for a local pilot"
                        .to_string(),
                )
            },
        };
    };

    if branch.is_empty() || branch == "detached" {
        return RequiredCheck {
            name: "git landing path".to_string(),
            passed: false,
            detail: format!("detached HEAD with origin {}", origin.trim()),
            action: Some("checkout a named campaign branch before orchestration".to_string()),
        };
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["push", "--dry-run", "origin"])
        .arg(format!("HEAD:{branch}"))
        .output();
    match output {
        Ok(output) if output.status.success() => RequiredCheck {
            name: "git landing path".to_string(),
            passed: true,
            detail: format!("dry-run push to origin/{branch} succeeded"),
            action: None,
        },
        Ok(output) => RequiredCheck {
            name: "git landing path".to_string(),
            passed: false,
            detail: compact_command_output(&output),
            action: Some(
                "fix push credentials, set a fork PR path, or mark the run local-only".to_string(),
            ),
        },
        Err(err) => RequiredCheck {
            name: "git landing path".to_string(),
            passed: false,
            detail: err.to_string(),
            action: Some("repair git before orchestrator runs".to_string()),
        },
    }
}

fn check_github_repo_permission(repo_root: &Path, allow_local_only: bool) -> RequiredCheck {
    let origin = command_stdout_in(repo_root, "git", &["remote", "get-url", "origin"]);
    let Ok(origin) = origin else {
        return RequiredCheck {
            name: "GitHub repo permission".to_string(),
            passed: allow_local_only,
            detail: "no origin remote configured".to_string(),
            action: if allow_local_only {
                None
            } else {
                Some("configure a GitHub origin or mark the run local-only".to_string())
            },
        };
    };
    let Some(repo) = parse_github_repo_from_remote_url(origin.trim()) else {
        return RequiredCheck {
            name: "GitHub repo permission".to_string(),
            passed: false,
            detail: format!("origin is not a parseable GitHub repo: {}", origin.trim()),
            action: Some(
                "configure a GitHub origin or document the alternate landing path".to_string(),
            ),
        };
    };

    let output = Command::new("gh")
        .args(["repo", "view", &repo, "--json", "viewerPermission"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let permission = extract_json_string_field(&text, "viewerPermission")
                .unwrap_or_else(|| "unknown".to_string());
            let passed = matches!(permission.as_str(), "WRITE" | "MAINTAIN" | "ADMIN");
            RequiredCheck {
                name: "GitHub repo permission".to_string(),
                passed,
                detail: format!("{repo} viewerPermission={permission}"),
                action: (!passed).then(|| {
                    "use an owned fork plus PR, or run with a repo where the orchestrator can write"
                        .to_string()
                }),
            }
        }
        Ok(output) => RequiredCheck {
            name: "GitHub repo permission".to_string(),
            passed: false,
            detail: compact_command_output(&output),
            action: Some(
                "authenticate gh or verify repo visibility before orchestration".to_string(),
            ),
        },
        Err(err) => RequiredCheck {
            name: "GitHub repo permission".to_string(),
            passed: false,
            detail: err.to_string(),
            action: Some("install `gh` and authenticate before orchestration".to_string()),
        },
    }
}

fn check_disk_budget(path: &Path, min_disk_kb: u64) -> RequiredCheck {
    let output = Command::new("df").arg("-Pk").arg(path).output();
    let Ok(output) = output else {
        return RequiredCheck {
            name: "disk budget".to_string(),
            passed: false,
            detail: "failed to run df".to_string(),
            action: Some("repair coreutils/df before orchestrator runs".to_string()),
        };
    };
    if !output.status.success() {
        return RequiredCheck {
            name: "disk budget".to_string(),
            passed: false,
            detail: compact_command_output(&output),
            action: Some("repair filesystem visibility before orchestrator runs".to_string()),
        };
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let available_kb = stdout
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|value| value.parse::<u64>().ok());
    let Some(available_kb) = available_kb else {
        return RequiredCheck {
            name: "disk budget".to_string(),
            passed: false,
            detail: format!("could not parse df output: {}", stdout.trim()),
            action: Some("inspect disk capacity manually before orchestrator runs".to_string()),
        };
    };
    RequiredCheck {
        name: "disk budget".to_string(),
        passed: available_kb >= min_disk_kb,
        detail: format!("{available_kb} KiB available; required {min_disk_kb} KiB"),
        action: (available_kb < min_disk_kb)
            .then(|| "prune .auto/artifacts or expand disk before orchestrator runs".to_string()),
    }
}

fn check_planning_health(repo_root: &Path) -> Vec<RequiredCheck> {
    let mut checks = Vec::new();
    let state = load_state(repo_root).unwrap_or_default();
    let planning_root = state
        .planning_root
        .clone()
        .unwrap_or_else(|| repo_root.join("genesis"));
    let planning_source = if state.planning_root.is_some() {
        "saved state"
    } else {
        "default genesis"
    };
    match load_planning_corpus(&planning_root) {
        Ok(corpus) => checks.push(RequiredCheck {
            name: "planning root".to_string(),
            passed: true,
            detail: format!(
                "{} from {planning_source}; {} primary plan(s)",
                planning_root.display(),
                corpus.primary_plans.len()
            ),
            action: None,
        }),
        Err(err) => checks.push(RequiredCheck {
            name: "planning root".to_string(),
            passed: false,
            detail: format!("{} from {planning_source}: {err}", planning_root.display()),
            action: Some(
                "run auto corpus or pass --planning-root to model-backed commands".to_string(),
            ),
        }),
    }

    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    match std::fs::read_to_string(&plan_path) {
        Ok(plan) => {
            let tasks = parse_tasks(&plan);
            let pending = tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Pending)
                .count();
            let partial = tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Partial)
                .count();
            let blocked = tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Blocked)
                .count();
            let done = tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Done)
                .count();
            checks.push(RequiredCheck {
                name: "queue health".to_string(),
                passed: !tasks.is_empty(),
                detail: format!(
                    "{} task(s): {pending} pending, {partial} partial, {blocked} blocked, {done} done",
                    tasks.len()
                ),
                action: tasks
                    .is_empty()
                    .then(|| "restore IMPLEMENTATION_PLAN.md task rows before running auto parallel".to_string()),
            });
        }
        Err(err) => checks.push(RequiredCheck {
            name: "queue health".to_string(),
            passed: false,
            detail: format!("failed to read {}: {err}", plan_path.display()),
            action: Some("restore IMPLEMENTATION_PLAN.md before running auto parallel".to_string()),
        }),
    }

    let snapshot = state
        .latest_output_dir
        .as_ref()
        .map(|path| {
            if path.exists() {
                format!("latest generated snapshot exists at {}", path.display())
            } else {
                format!("latest generated snapshot is missing at {}", path.display())
            }
        })
        .unwrap_or_else(|| "no generated snapshot recorded".to_string());
    checks.push(RequiredCheck {
        name: "generated snapshot".to_string(),
        passed: true,
        detail: snapshot,
        action: None,
    });

    checks
}

fn check_autodev_required_layout(repo_root: &Path) -> Vec<RequiredCheck> {
    let missing: Vec<&str> = AUTODEV_REQUIRED_LAYOUT
        .iter()
        .copied()
        .filter(|relative| !repo_root.join(relative).is_file())
        .collect();

    if missing.is_empty() {
        vec![RequiredCheck {
            name: "repo layout".to_string(),
            passed: true,
            detail: format!("found {}", AUTODEV_REQUIRED_LAYOUT.join(", ")),
            action: None,
        }]
    } else {
        vec![RequiredCheck {
            name: "repo layout".to_string(),
            passed: false,
            detail: format!("missing {}", missing.join(", ")),
            action: Some("restore the checkout or rerun from the repository root".to_string()),
        }]
    }
}

fn check_project_checkout_layout(repo_root: &Path) -> RequiredCheck {
    let found_instructions: Vec<&str> = PROJECT_AGENT_INSTRUCTION_FILES
        .iter()
        .copied()
        .filter(|relative| repo_root.join(relative).is_file())
        .collect();

    if found_instructions.is_empty() {
        return RequiredCheck {
            name: "project checkout".to_string(),
            passed: false,
            detail: format!(
                "missing agent instructions; expected one of {}",
                PROJECT_AGENT_INSTRUCTION_FILES.join(", ")
            ),
            action: Some(
                "add AGENTS.md or equivalent repo-local agent instructions before model-backed work"
                    .to_string(),
            ),
        };
    }

    RequiredCheck {
        name: "project checkout".to_string(),
        passed: true,
        detail: format!(
            "non-autodev repo with agent instructions at {}",
            found_instructions.join(", ")
        ),
        action: None,
    }
}

fn read_cargo_manifest(path: &Path) -> std::result::Result<Option<toml::Value>, RequiredCheck> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(RequiredCheck {
                name: "Cargo.toml manifest".to_string(),
                passed: false,
                detail: format!("failed to read {}: {err}", path.display()),
                action: Some("restore Cargo.toml before rerunning doctor".to_string()),
            });
        }
    };

    match toml::from_str(&text) {
        Ok(manifest) => Ok(Some(manifest)),
        Err(err) => Err(RequiredCheck {
            name: "Cargo.toml manifest".to_string(),
            passed: false,
            detail: format!("failed to parse {}: {err}", path.display()),
            action: Some("fix Cargo.toml before rerunning doctor".to_string()),
        }),
    }
}

fn check_autodev_cargo_manifest(manifest: &toml::Value) -> RequiredCheck {
    let package_name = manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str);
    let has_auto_bin = manifest_declares_auto_bin(manifest);

    if package_name == Some("autodev") && has_auto_bin {
        RequiredCheck {
            name: "Cargo.toml manifest".to_string(),
            passed: true,
            detail: "package autodev declares binary auto at src/main.rs".to_string(),
            action: None,
        }
    } else {
        RequiredCheck {
            name: "Cargo.toml manifest".to_string(),
            passed: false,
            detail: "expected package autodev and [[bin]] auto -> src/main.rs".to_string(),
            action: Some("restore the autodev package and auto binary declarations".to_string()),
        }
    }
}

fn manifest_is_autodev_source(manifest: &toml::Value) -> bool {
    manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        == Some("autodev")
        || manifest_declares_auto_bin(manifest)
}

fn manifest_declares_auto_bin(manifest: &toml::Value) -> bool {
    manifest
        .get("bin")
        .and_then(toml::Value::as_array)
        .is_some_and(|bins| {
            bins.iter().any(|bin| {
                bin.get("name").and_then(toml::Value::as_str) == Some("auto")
                    && bin.get("path").and_then(toml::Value::as_str) == Some("src/main.rs")
            })
        })
}

fn check_version_probe(probe: &CommandProbe) -> RequiredCheck {
    if let Some(error) = &probe.launch_error {
        return RequiredCheck {
            name: "binary provenance".to_string(),
            passed: false,
            detail: format!("failed to run auto --version: {error}"),
            action: Some("run cargo build or cargo install --path . --root ~/.local".to_string()),
        };
    }

    let output = format!("{}\n{}", probe.stdout, probe.stderr);
    let has_package_version = output.contains(env!("CARGO_PKG_VERSION"));
    let has_metadata =
        output.contains("commit:") && output.contains("dirty:") && output.contains("profile:");

    if probe.success && has_package_version && has_metadata {
        RequiredCheck {
            name: "binary provenance".to_string(),
            passed: true,
            detail: first_nonempty_line(&probe.stdout)
                .unwrap_or("auto --version ok")
                .to_string(),
            action: None,
        }
    } else {
        RequiredCheck {
            name: "binary provenance".to_string(),
            passed: false,
            detail: format!(
                "auto --version did not expose package version plus commit/dirty/profile metadata: {}",
                compact_probe_output(probe)
            ),
            action: Some("rebuild with cargo build or reinstall with cargo install --path . --root ~/.local".to_string()),
        }
    }
}

fn check_help_surfaces_with(mut run: impl FnMut(&[&str]) -> CommandProbe) -> Vec<RequiredCheck> {
    HELP_SURFACES
        .iter()
        .map(|args| {
            let probe = run(args);
            let display = format_auto_args(args);
            if let Some(error) = &probe.launch_error {
                return RequiredCheck {
                    name: format!("help surface `{display}`"),
                    passed: false,
                    detail: format!("failed to run {display}: {error}"),
                    action: Some("run cargo build or reinstall the auto binary".to_string()),
                };
            }

            if probe.success && probe.stdout.contains("Usage:") {
                RequiredCheck {
                    name: format!("help surface `{display}`"),
                    passed: true,
                    detail: "help parsed".to_string(),
                    action: None,
                }
            } else {
                RequiredCheck {
                    name: format!("help surface `{display}`"),
                    passed: false,
                    detail: format!(
                        "help output was not parseable: {}",
                        compact_probe_output(&probe)
                    ),
                    action: Some("run cargo test doctor_command_is_parseable".to_string()),
                }
            }
        })
        .collect()
}

fn build_optional_tool_checks(
    mut find: impl FnMut(&str) -> Option<PathBuf>,
) -> Vec<CapabilityCheck> {
    OPTIONAL_TOOLS
        .iter()
        .map(|tool| CapabilityCheck {
            tool: tool.name,
            found: find(tool.name),
            workflows: tool.workflows,
        })
        .collect()
}

fn run_auto_probe(current_exe: &Path, args: &[&str]) -> CommandProbe {
    match Command::new(current_exe).args(args).output() {
        Ok(output) => CommandProbe {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            launch_error: None,
        },
        Err(err) => CommandProbe {
            success: false,
            stdout: String::new(),
            stderr: String::new(),
            launch_error: Some(err.to_string()),
        },
    }
}

fn find_on_path(tool: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
}

fn command_stdout_in(
    cwd: &Path,
    command: &str,
    args: &[&str],
) -> std::result::Result<String, String> {
    let output = Command::new(command)
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(compact_command_output(&output))
    }
}

fn compact_command_output(output: &std::process::Output) -> String {
    let mut text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
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

fn parse_github_repo_from_remote_url(remote_url: &str) -> Option<String> {
    let remote_url = remote_url.trim().trim_end_matches(".git");
    if let Some(path) = remote_url.strip_prefix("git@github.com:") {
        return normalize_github_repo_path(path);
    }
    if let Some(path) = remote_url.strip_prefix("ssh://git@github.com/") {
        return normalize_github_repo_path(path);
    }
    if let Some(path) = remote_url.strip_prefix("https://github.com/") {
        return normalize_github_repo_path(path);
    }
    if let Some(path) = remote_url.strip_prefix("http://github.com/") {
        return normalize_github_repo_path(path);
    }
    None
}

fn normalize_github_repo_path(path: &str) -> Option<String> {
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn extract_json_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn print_doctor_report(report: &DoctorReport) {
    print!("{}", render_doctor_report(report));
}

fn render_doctor_report(report: &DoctorReport) -> String {
    let mut output = String::new();

    output.push_str("baseline readiness:\n");
    for check in &report.baseline {
        output.push_str(&render_required_check(check));
    }

    output.push('\n');
    output.push_str("execution readiness:\n");
    if report.execution.is_empty() {
        output.push_str("- [warn] planning and queue state were not checked because the repo root was unavailable\n");
    }
    for check in &report.execution {
        output.push_str(&render_required_check(check));
    }

    output.push('\n');
    output.push_str("model/tool capabilities:\n");
    for check in &report.capabilities {
        match &check.found {
            Some(path) => output.push_str(&format!(
                "- [ok] {}: found at {}; enables {}\n",
                check.tool,
                path.display(),
                check.workflows
            )),
            None => output.push_str(&format!(
                "- [warn] {}: not found on PATH; unavailable until installed/authenticated: {}\n",
                check.tool, check.workflows
            )),
        }
    }

    output.push('\n');
    output.push_str("model/network:\n");
    if report.orchestrator.is_empty() {
        output.push_str("- [ok] no model providers, network APIs, Linear, GitHub, Symphony, Docker, browser automation, or tmux sessions were invoked\n");
    } else {
        output.push_str("- [ok] no model providers, Linear, Symphony, Docker, browser automation, or tmux sessions were invoked\n");
        output.push_str("- [warn] orchestrator readiness may invoke GitHub, gbrain, Hermes, systemd, git dry-run push, and disk probes\n");
    }

    if !report.orchestrator.is_empty() {
        output.push('\n');
        output.push_str("orchestrator readiness:\n");
        for check in &report.orchestrator {
            output.push_str(&render_required_check(check));
        }
    }

    output.push('\n');
    output.push_str("next steps:\n");
    if report.baseline.iter().any(|check| !check.passed) {
        output
            .push_str("- fix the failed baseline readiness checks above, then rerun auto doctor\n");
        output.push_str("doctor failed\n");
    } else if report.orchestrator.iter().any(|check| !check.passed) {
        output.push_str("- fix failed orchestrator readiness checks before running pilot-dev or auto parallel campaigns\n");
        output.push_str("doctor failed\n");
    } else if report.execution.iter().any(|check| !check.passed) {
        output.push_str("- baseline is ready for no-model commands\n");
        output.push_str(
            "- fix failed execution readiness checks before running planning or model-backed workflows\n",
        );
        output.push_str(
            "- install or authenticate model tools only for workflows that need those capabilities\n",
        );
        output.push_str("doctor ok\n");
    } else {
        output.push_str("- baseline is ready for no-model commands\n");
        output.push_str("- run cargo test for local regression proof\n");
        output.push_str(
            "- run model-backed commands such as auto health only after credentials are configured\n",
        );
        output.push_str("doctor ok\n");
    }

    output
}

fn render_required_check(check: &RequiredCheck) -> String {
    let mut output = String::new();
    let status = if check.passed { "ok" } else { "fail" };
    output.push_str(&format!("- [{status}] {}: {}\n", check.name, check.detail));
    if let Some(action) = &check.action {
        output.push_str(&format!("  next: {action}\n"));
    }
    output
}

fn compact_probe_output(probe: &CommandProbe) -> String {
    let mut output = format!("{} {}", probe.stdout.trim(), probe.stderr.trim())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if output.len() > 240 {
        output.truncate(240);
        output.push_str("...");
    }
    if output.is_empty() {
        format!("exit success={}", probe.success)
    } else {
        output
    }
}

fn first_nonempty_line(text: &str) -> Option<&str> {
    text.lines().find(|line| !line.trim().is_empty())
}

fn format_auto_args(args: &[&str]) -> String {
    if args == ["--help"].as_slice() {
        "auto --help".to_string()
    } else {
        format!("auto {}", args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        build_optional_tool_checks, check_help_surfaces_with, check_planning_health,
        check_repo_checkout, check_version_probe, extract_json_string_field, format_auto_args,
        parse_github_repo_from_remote_url, render_doctor_report, CapabilityCheck, CommandProbe,
        DoctorReport, RequiredCheck, HELP_SURFACES,
    };

    #[test]
    fn doctor_reports_missing_optional_tools_without_panicking() {
        let checks = build_optional_tool_checks(|_| None);

        assert_eq!(checks.len(), 4);
        assert!(checks.iter().all(|check| check.found.is_none()));
        assert!(checks.iter().any(|check| check.tool == "codex"));
        assert!(checks.iter().any(|check| check.tool == "claude"));
        assert!(checks.iter().any(|check| check.tool == "pi"));
        assert!(checks.iter().any(|check| check.tool == "gh"));
    }

    #[test]
    fn doctor_reports_found_optional_tools_as_capabilities() {
        let checks = build_optional_tool_checks(|tool| {
            (tool == "codex").then(|| PathBuf::from("/usr/local/bin/codex"))
        });

        let codex = checks
            .iter()
            .find(|check| check.tool == "codex")
            .expect("codex check");
        assert_eq!(codex.found, Some(PathBuf::from("/usr/local/bin/codex")));
        assert!(checks
            .iter()
            .filter(|check| check.tool != "codex")
            .all(|check| check.found.is_none()));
    }

    #[test]
    fn doctor_checks_expected_help_surfaces() {
        let mut observed = Vec::new();
        let checks = check_help_surfaces_with(|args| {
            observed.push(format_auto_args(args));
            CommandProbe {
                success: true,
                stdout: "Usage: auto <COMMAND>\n".to_string(),
                stderr: String::new(),
                launch_error: None,
            }
        });

        assert_eq!(
            observed,
            vec![
                "auto --help",
                "auto corpus --help",
                "auto gen --help",
                "auto design --help",
                "auto super --help",
                "auto parallel --help",
                "auto doctor --help",
                "auto quota --help",
                "auto audit-harvest --help",
                "auto symphony --help",
            ]
        );
        assert_eq!(checks.len(), HELP_SURFACES.len());
        assert!(checks.iter().all(|check| check.passed));
    }

    #[test]
    fn doctor_reports_active_planning_and_queue_health() {
        let repo = temp_repo("planning-health");
        fs::create_dir_all(repo.join("genesis/plans")).expect("failed to create corpus");
        fs::write(repo.join("genesis/plans/001-build.md"), "# Build\n")
            .expect("failed to write plan");
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n- [ ] `TASK-1` Pending\nDependencies: none\n\n- [~] `TASK-2` Partial\nDependencies: none\n\n- [!] `TASK-3` Blocked\nDependencies: `TASK-1`\n\n- [x] `TASK-4` Done\nDependencies: none\n",
        )
        .expect("failed to write queue");

        let checks = check_planning_health(&repo);
        let planning = checks
            .iter()
            .find(|check| check.name == "planning root")
            .expect("planning check should exist");
        let queue = checks
            .iter()
            .find(|check| check.name == "queue health")
            .expect("queue check should exist");

        assert!(planning.passed);
        assert!(planning.detail.contains("1 primary plan"));
        assert!(queue.passed);
        assert!(queue
            .detail
            .contains("1 pending, 1 partial, 1 blocked, 1 done"));
    }

    #[test]
    fn doctor_renders_no_model_first_run_contract() {
        let version = check_version_probe(&CommandProbe {
            success: true,
            stdout: format!(
                "auto {}\ncommit: abc123\ndirty: false\nprofile: debug\n",
                env!("CARGO_PKG_VERSION")
            ),
            stderr: String::new(),
            launch_error: None,
        });
        let report = DoctorReport {
            baseline: vec![
                RequiredCheck {
                    name: "repo layout".to_string(),
                    passed: true,
                    detail: "found Cargo.toml, src/main.rs, README.md, AGENTS.md".to_string(),
                    action: None,
                },
                version,
            ],
            execution: vec![RequiredCheck {
                name: "queue health".to_string(),
                passed: true,
                detail: "1 task(s): 1 pending, 0 partial, 0 blocked, 0 done".to_string(),
                action: None,
            }],
            capabilities: vec![CapabilityCheck {
                tool: "codex",
                found: None,
                workflows: "model-backed flows",
            }],
            orchestrator: Vec::new(),
        };

        let rendered = render_doctor_report(&report);

        assert!(rendered.contains("- [ok] repo layout: found Cargo.toml"));
        assert!(rendered.contains("- [ok] binary provenance: auto"));
        assert!(rendered.contains("- [warn] codex: not found on PATH"));
        assert!(rendered.contains("no model providers, network APIs, Linear, GitHub"));
        assert!(rendered.contains("Docker, browser automation, or tmux sessions were invoked"));
        assert!(rendered.contains("doctor ok"));
    }

    #[test]
    fn doctor_distinguishes_baseline_from_execution_readiness() {
        let report = DoctorReport {
            baseline: vec![RequiredCheck {
                name: "repo layout".to_string(),
                passed: true,
                detail: "found Cargo.toml, src/main.rs, README.md, AGENTS.md".to_string(),
                action: None,
            }],
            execution: vec![RequiredCheck {
                name: "queue health".to_string(),
                passed: true,
                detail: "4 task(s): 1 pending, 1 partial, 1 blocked, 1 done".to_string(),
                action: None,
            }],
            capabilities: vec![CapabilityCheck {
                tool: "codex",
                found: None,
                workflows: "model-backed flows",
            }],
            orchestrator: Vec::new(),
        };

        let rendered = render_doctor_report(&report);

        assert!(rendered.contains("baseline readiness:\n- [ok] repo layout:"));
        assert!(rendered.contains("execution readiness:\n- [ok] queue health:"));
        assert!(rendered.contains("model/tool capabilities:\n- [warn] codex: not found on PATH"));
        assert!(!rendered.contains("required:\n"));
        assert!(!rendered.lines().any(|line| line == "capabilities:"));

        let execution_blocked = DoctorReport {
            baseline: vec![RequiredCheck {
                name: "repo layout".to_string(),
                passed: true,
                detail: "found Cargo.toml, src/main.rs, README.md, AGENTS.md".to_string(),
                action: None,
            }],
            execution: vec![RequiredCheck {
                name: "planning root".to_string(),
                passed: false,
                detail: "missing genesis".to_string(),
                action: Some(
                    "run auto corpus or pass --planning-root to model-backed commands".to_string(),
                ),
            }],
            capabilities: vec![CapabilityCheck {
                tool: "claude",
                found: None,
                workflows: "Claude-backed corpus and generation flows",
            }],
            orchestrator: Vec::new(),
        };
        let rendered = render_doctor_report(&execution_blocked);

        assert!(!execution_blocked.required_failed(false));
        assert!(rendered.contains("- [fail] planning root: missing genesis"));
        assert!(rendered.contains("baseline is ready for no-model commands"));
        assert!(rendered.contains("doctor ok"));
    }

    #[test]
    fn doctor_renders_orchestrator_readiness_as_hard_gate() {
        let report = DoctorReport {
            baseline: vec![RequiredCheck {
                name: "repo layout".to_string(),
                passed: true,
                detail: "found AGENTS.md".to_string(),
                action: None,
            }],
            execution: Vec::new(),
            capabilities: Vec::new(),
            orchestrator: vec![RequiredCheck {
                name: "git landing path".to_string(),
                passed: false,
                detail: "no origin remote configured".to_string(),
                action: Some("configure origin".to_string()),
            }],
        };

        let rendered = render_doctor_report(&report);

        assert!(report.required_failed(true));
        assert!(!report.required_failed(false));
        assert!(rendered.contains("orchestrator readiness:\n- [fail] git landing path:"));
        assert!(rendered.contains("fix failed orchestrator readiness checks"));
        assert!(rendered.contains("doctor failed"));
    }

    #[test]
    fn doctor_parses_common_github_remote_urls() {
        assert_eq!(
            parse_github_repo_from_remote_url("git@github.com:happybigmtn/autodev.git"),
            Some("happybigmtn/autodev".to_string())
        );
        assert_eq!(
            parse_github_repo_from_remote_url("https://github.com/NousResearch/hermes-agent.git"),
            Some("NousResearch/hermes-agent".to_string())
        );
        assert_eq!(
            parse_github_repo_from_remote_url("ssh://git@github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            parse_github_repo_from_remote_url("git@example.com:owner/repo.git"),
            None
        );
    }

    #[test]
    fn doctor_extracts_viewer_permission_from_gh_json() {
        assert_eq!(
            extract_json_string_field(r#"{"viewerPermission":"WRITE"}"#, "viewerPermission"),
            Some("WRITE".to_string())
        );
        assert_eq!(
            extract_json_string_field(r#"{"name":"repo"}"#, "viewerPermission"),
            None
        );
    }

    #[test]
    fn doctor_accepts_non_autodev_project_with_agent_instructions() {
        let repo = temp_repo("project-checkout");
        fs::write(repo.join("AGENTS.md"), "build here\n").expect("write AGENTS.md");
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"agent-product\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write Cargo.toml");

        let checks = check_repo_checkout(&repo);

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "project checkout");
        assert!(checks[0].passed, "{checks:?}");
        fs::remove_dir_all(repo).expect("cleanup temp repo");
    }

    #[test]
    fn doctor_rejects_project_without_agent_instructions() {
        let repo = temp_repo("missing-agent-instructions");
        fs::write(repo.join("README.md"), "no instructions yet\n").expect("write README.md");

        let checks = check_repo_checkout(&repo);

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "project checkout");
        assert!(!checks[0].passed, "{checks:?}");
        fs::remove_dir_all(repo).expect("cleanup temp repo");
    }

    #[test]
    fn doctor_keeps_strict_autodev_manifest_check_for_autodev_source() {
        let repo = temp_repo("autodev-source");
        fs::create_dir_all(repo.join("src")).expect("create src");
        fs::write(repo.join("src/main.rs"), "fn main() {}\n").expect("write main");
        fs::write(repo.join("README.md"), "autodev\n").expect("write README");
        fs::write(repo.join("AGENTS.md"), "autodev agents\n").expect("write AGENTS");
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"autodev\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
        )
        .expect("write Cargo.toml");

        let checks = check_repo_checkout(&repo);

        assert_eq!(checks.len(), 2);
        assert!(checks
            .iter()
            .any(|check| check.name == "repo layout" && check.passed));
        assert!(checks
            .iter()
            .any(|check| check.name == "Cargo.toml manifest" && !check.passed));
        fs::remove_dir_all(repo).expect("cleanup temp repo");
    }

    fn temp_repo(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("autodev-doctor-{label}-{stamp}"));
        fs::create_dir_all(&path).expect("create temp repo");
        path
    }
}
