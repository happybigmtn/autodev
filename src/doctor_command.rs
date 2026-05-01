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
    &["quota", "--help"],
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
pub(crate) struct DoctorArgs {}

pub(crate) async fn run_doctor(_args: DoctorArgs) -> Result<()> {
    let current_exe = env::current_exe().context("failed to resolve current auto executable")?;
    let report = build_doctor_report(&current_exe);
    print_doctor_report(&report);
    if report.required_failed() {
        return Err(anyhow!("doctor failed"));
    }
    Ok(())
}

#[derive(Debug)]
struct DoctorReport {
    required: Vec<RequiredCheck>,
    capabilities: Vec<CapabilityCheck>,
}

impl DoctorReport {
    fn required_failed(&self) -> bool {
        self.required.iter().any(|check| !check.passed)
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

fn build_doctor_report(current_exe: &Path) -> DoctorReport {
    let mut report = DoctorReport {
        required: Vec::new(),
        capabilities: build_optional_tool_checks(find_on_path),
    };

    match git_repo_root() {
        Ok(repo_root) => {
            report.required.push(RequiredCheck {
                name: "repo root".to_string(),
                passed: true,
                detail: format!("found {}", repo_root.display()),
                action: None,
            });
            report.required.extend(check_repo_checkout(&repo_root));
            report.required.extend(check_planning_health(&repo_root));
        }
        Err(err) => report.required.push(RequiredCheck {
            name: "repo root".to_string(),
            passed: false,
            detail: err.to_string(),
            action: Some("rerun from inside the repository checkout".to_string()),
        }),
    }

    report.required.push(check_version_probe(&run_auto_probe(
        current_exe,
        &["--version"],
    )));
    report.required.extend(check_help_surfaces_with(|args| {
        run_auto_probe(current_exe, args)
    }));

    report
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

fn print_doctor_report(report: &DoctorReport) {
    print!("{}", render_doctor_report(report));
}

fn render_doctor_report(report: &DoctorReport) -> String {
    let mut output = String::new();

    output.push_str("required:\n");
    for check in &report.required {
        let status = if check.passed { "ok" } else { "fail" };
        output.push_str(&format!("- [{status}] {}: {}\n", check.name, check.detail));
        if let Some(action) = &check.action {
            output.push_str(&format!("  next: {action}\n"));
        }
    }

    output.push('\n');
    output.push_str("capabilities:\n");
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
    output.push_str("- [ok] no model providers, network APIs, Linear, GitHub, Symphony, Docker, browser automation, or tmux sessions were invoked\n");

    output.push('\n');
    output.push_str("next steps:\n");
    if report.required_failed() {
        output.push_str("- fix the failed required checks above, then rerun auto doctor\n");
        output.push_str("doctor failed\n");
    } else {
        output.push_str("- run cargo test for local regression proof\n");
        output.push_str(
            "- run model-backed commands such as auto health only after credentials are configured\n",
        );
        output.push_str("doctor ok\n");
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
        check_repo_checkout, check_version_probe, format_auto_args, render_doctor_report,
        CapabilityCheck, CommandProbe, DoctorReport, RequiredCheck, HELP_SURFACES,
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
                "auto quota --help",
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
            required: vec![
                RequiredCheck {
                    name: "repo layout".to_string(),
                    passed: true,
                    detail: "found Cargo.toml, src/main.rs, README.md, AGENTS.md".to_string(),
                    action: None,
                },
                version,
            ],
            capabilities: vec![CapabilityCheck {
                tool: "codex",
                found: None,
                workflows: "model-backed flows",
            }],
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
