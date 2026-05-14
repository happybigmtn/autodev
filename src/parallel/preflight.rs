#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ParallelPreflightReport {
    checks: Vec<ParallelPreflightCheck>,
}

impl ParallelPreflightReport {
    fn add(&mut self, status: PreflightStatus, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(ParallelPreflightCheck {
            status,
            name: name.into(),
            detail: detail.into(),
        });
    }

    fn prompt_clause(&self) -> String {
        self.checks
            .iter()
            .map(|check| {
                format!(
                    "- {} {}: {}",
                    check.status.label(),
                    check.name,
                    check.detail
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn summary(&self) -> String {
        let warnings = self
            .checks
            .iter()
            .filter(|check| check.status == PreflightStatus::Warn)
            .count();
        if warnings == 0 {
            format!("{} checks ok", self.checks.len())
        } else {
            format!("{} checks, {} warning(s)", self.checks.len(), warnings)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParallelPreflightCheck {
    status: PreflightStatus,
    name: String,
    detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreflightStatus {
    Ok,
    Warn,
}

impl PreflightStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
        }
    }
}

fn run_parallel_preflight(
    repo_root: &Path,
    plan: &LoopPlanSnapshot,
    run_root: &Path,
    parallel_logger: &ParallelEventLogger,
) -> Result<ParallelPreflightReport> {
    let mut report = ParallelPreflightReport::default();
    let task_text = plan
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                LoopTaskStatus::Pending | LoopTaskStatus::Partial
            )
        })
        .map(|task| format!("{} {}\n{}", task.id, task.title, task.markdown))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();

    let preflight_needs = classify_parallel_preflight_needs(&task_text, repo_root);

    if repo_uses_cargo(repo_root) {
        report.add(
            PreflightStatus::Ok,
            "cargo",
            "Rust workspace detected; worker Cargo target policy is included in every lane prompt",
        );
    }

    if preflight_needs.browser {
        if command_exists("agent-browser") {
            let socket = default_agent_browser_socket();
            if socket.exists() || try_warm_agent_browser_daemon(repo_root, &socket) {
                report.add(
                    PreflightStatus::Ok,
                    "agent-browser",
                    format!(
                        "CLI present and daemon socket is ready at {}",
                        socket.display()
                    ),
                );
            } else {
                report.add(
                    PreflightStatus::Warn,
                    "agent-browser",
                    format!(
                        "CLI present but daemon socket is still missing at {} after warm-up; browser lanes should report AUTO_ENV_BLOCKER if they cannot repair it",
                        socket.display()
                    ),
                );
            }
        } else {
            report.add(
                PreflightStatus::Warn,
                "agent-browser",
                "`agent-browser` is not on PATH; browser/e2e lanes may block",
            );
        }
    }

    if preflight_needs.docker {
        if !command_exists("docker") {
            report.add(
                PreflightStatus::Warn,
                "docker",
                "`docker` is not on PATH; Docker-backed smoke tests may block",
            );
        } else if repo_root.join("docker-compose.yml").exists()
            || repo_root.join("compose.yml").exists()
            || repo_root.join("compose.yaml").exists()
        {
            match command_stdout(repo_root, ["docker", "compose", "config", "--quiet"]) {
                Ok(_) => match command_stdout(
                    repo_root,
                    [
                        "docker",
                        "compose",
                        "ps",
                        "--services",
                        "--status",
                        "running",
                    ],
                ) {
                    Ok(services) if !services.trim().is_empty() => report.add(
                        PreflightStatus::Ok,
                        "docker compose",
                        format!(
                            "running services: {}",
                            services.lines().collect::<Vec<_>>().join(", ")
                        ),
                    ),
                    Ok(_) => report.add(
                        PreflightStatus::Warn,
                        "docker compose",
                        "compose config is valid but no services are currently running",
                    ),
                    Err(err) => report.add(
                        PreflightStatus::Warn,
                        "docker compose",
                        format!("could not inspect running services: {err}"),
                    ),
                },
                Err(err) => report.add(
                    PreflightStatus::Warn,
                    "docker compose",
                    format!("compose config check failed: {err}"),
                ),
            }
        } else {
            report.add(
                PreflightStatus::Warn,
                "docker compose",
                "tasks mention Docker or explicit regtest infrastructure but no compose file was found",
            );
        }
    }

    if preflight_needs.regtest {
        if command_exists("curl") {
            match command_stdout(
                repo_root,
                [
                    "curl",
                    "-sf",
                    "--max-time",
                    "2",
                    "http://127.0.0.1:18443/",
                    "-u",
                    "bitino:bitino",
                    "-H",
                    "content-type: application/json",
                    "--data",
                    "{\"jsonrpc\":\"1.0\",\"id\":\"auto-preflight\",\"method\":\"getblockchaininfo\",\"params\":[]}",
                ],
            ) {
                Ok(_) => report.add(
                    PreflightStatus::Ok,
                    "regtest rpc",
                    "127.0.0.1:18443 answered getblockchaininfo",
                ),
                Err(err) => report.add(
                    PreflightStatus::Warn,
                    "regtest rpc",
                    format!("127.0.0.1:18443 did not answer getblockchaininfo: {err}"),
                ),
            }
        } else {
            report.add(
                PreflightStatus::Warn,
                "regtest rpc",
                "`curl` is not on PATH; cannot probe local regtest RPC",
            );
        }
    }

    if report.checks.is_empty() {
        report.add(
            PreflightStatus::Ok,
            "general",
            "no browser, Docker, explicit regtest, or Cargo preflight checks were triggered by pending tasks",
        );
    }

    let rendered = report.prompt_clause();
    atomic_write(&run_root.join("preflight.txt"), rendered.as_bytes()).with_context(|| {
        format!(
            "failed to write {}",
            run_root.join("preflight.txt").display()
        )
    })?;
    parallel_logger.info(format!("preflight:   {}", report.summary()));
    for check in &report.checks {
        if check.status == PreflightStatus::Warn {
            parallel_logger.warn(format!(
                "preflight:   warn {}: {}",
                check.name, check.detail
            ));
        }
    }
    Ok(report)
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn contains_term(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, _)| {
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_term_char(ch));
        let end = start + needle.len();
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|ch| !is_term_char(ch));
        before_ok && after_ok
    })
}

fn contains_any_term(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| contains_term(haystack, needle))
}

fn is_term_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParallelPreflightNeeds {
    browser: bool,
    docker: bool,
    regtest: bool,
}

fn classify_parallel_preflight_needs(task_text: &str, repo_root: &Path) -> ParallelPreflightNeeds {
    let browser = contains_any(
        task_text,
        &["agent-browser", "playwright", "browser", "e2e", "web"],
    );
    let regtest = contains_any(task_text, &["regtest", "rbtc-regtest", "bitcoin-regtest"]);
    let docker = contains_any_term(task_text, &["docker", "podman"])
        || contains_any(
            task_text,
            &[
                "docker compose",
                "docker-compose",
                "compose.yml",
                "compose.yaml",
            ],
        )
        || regtest
        || repo_root.join("docker-compose.yml").exists()
        || repo_root.join("compose.yml").exists()
        || repo_root.join("compose.yaml").exists();

    ParallelPreflightNeeds {
        browser,
        docker,
        regtest,
    }
}

fn command_exists(command: &str) -> bool {
    Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {}", shell_quote(command)))
        .output()
        .is_ok_and(|output| output.status.success())
}

fn command_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String> {
    let Some((program, rest)) = args.split_first() else {
        bail!("empty command");
    };
    let output = Command::new(program)
        .args(rest)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run `{}` in {}", args.join(" "), cwd.display()))?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    String::from_utf8(output.stdout).context("command stdout was not valid UTF-8")
}

fn default_agent_browser_socket() -> PathBuf {
    if let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir)
            .join("agent-browser")
            .join("default.sock");
    }
    let uid = command_stdout(Path::new("."), ["id", "-u"]).unwrap_or_else(|_| "1000".to_string());
    PathBuf::from("/run/user")
        .join(uid.trim())
        .join("agent-browser")
        .join("default.sock")
}

fn try_warm_agent_browser_daemon(repo_root: &Path, socket: &Path) -> bool {
    let open = Command::new("agent-browser")
        .args(["open", "about:blank"])
        .current_dir(repo_root)
        .output();
    if !open.as_ref().is_ok_and(|output| output.status.success()) {
        return false;
    }

    let _ = Command::new("agent-browser")
        .arg("close")
        .current_dir(repo_root)
        .output();
    socket.exists()
}

