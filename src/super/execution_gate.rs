// Execution-gate stage: the LLM "Verdict: GO/NO-GO" pass, plus the
// deterministic plan-readiness verifier (`verify_parallel_ready_plan` and
// the task-block schema checks the orchestrator runs against the root
// `IMPLEMENTATION_PLAN.md`). Spliced into `super_command.rs` via `include!`.

async fn run_super_execution_gate(
    args: &SuperArgs,
    repo_root: &Path,
    planning_root: &Path,
    output_dir: Option<&Path>,
    super_root: &Path,
) -> Result<()> {
    let prompt =
        build_super_execution_gate_prompt(repo_root, planning_root, output_dir, super_root);
    run_super_codex_phase(
        repo_root,
        super_root,
        "super-execution-gate",
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
    )
    .await?;
    let gate_path = super_root.join(EXECUTION_GATE_FILE);
    require_nonempty_file(&gate_path)?;
    let gate = fs::read_to_string(&gate_path)
        .with_context(|| format!("failed to read {}", gate_path.display()))?;
    if !gate.lines().any(|line| line.trim() == "Verdict: GO") {
        bail!(
            "super execution gate did not approve parallel execution; expected `Verdict: GO` in {}",
            gate_path.display()
        );
    }
    Ok(())
}

fn build_super_execution_gate_prompt(
    repo_root: &Path,
    planning_root: &Path,
    output_dir: Option<&Path>,
    super_root: &Path,
) -> String {
    let output_clause = output_dir
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the latest gen output recorded in .auto/state.json".to_string());
    let findings_path = super_root.join(SUPER_FINDINGS_FILE);

    let role = format!(
        "You are the final `auto super` execution gate before `auto parallel` launches.\n\
\n\
The repository is `{repo_root}`. The planning corpus is `{planning_root}`. The generated output is `{output_clause}`. The super artifacts are under `{super_root}`.",
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        super_root = super_root.display(),
    );

    let edit_boundary = format!(
        "- You may read the repository, `{planning_root}`, generated output, root `specs/`, and root `IMPLEMENTATION_PLAN.md`.\n\
- You may read `{super_root}/design`; design/runtime UI contract risks are execution-gate inputs, not decoration.\n\
- You MUST read `{findings_path}` and treat it as the canonical CEO functional-review output. The retired seven-file markdown bundle is not present and must not be re-read.\n\
- You may edit only root `IMPLEMENTATION_PLAN.md`, root `specs/*.md`, and `{super_root}/{gate}`.\n\
- Do not edit source code, `genesis/`, `gen-*`, skill definition directories, or worker artifacts.",
        planning_root = planning_root.display(),
        super_root = super_root.display(),
        findings_path = findings_path.display(),
        gate = EXECUTION_GATE_FILE,
    );

    let inputs = format!(
        "Canonical CEO functional review JSON: `{findings_path}`. Schema: top-level `readiness`, `blockers[]`, `risks[]`, `gates[]`, and `campaign_plan` (with `horizon_days` and `milestones[]`). Cross-reference blockers, gates, and milestones by `id` when amending the queue.",
        findings_path = findings_path.display(),
    );

    let gate_criteria = "- The queue must implement the campaign plan in `super-findings.json` (`campaign_plan.milestones`), not a generic cleanup backlog or capacity-trimmed wishlist.\n\
- Every blocker with severity `high` in `super-findings.json` must be either resolved in-tree or have a priority task in `IMPLEMENTATION_PLAN.md` that cites its `BLK-` id.\n\
- UI/design tasks must be tied to runtime/API source of truth, generated bindings, existing frontend helpers, and cross-surface readback proof. Reject fake mockups, manual frontend bindings, and fixture-data fallbacks as acceptance evidence.\n\
- Security, reliability, QA, data/contracts, operations, release, DX, and performance lanes must receive the same severity and proof standard as design.\n\
- Priority tasks must be dependency-ordered and small enough for one focused worker session.\n\
- Every unfinished task must have concrete ownership, acceptance criteria, verification, required tests, completion artifacts, dependencies, estimated scope, and completion signal.\n\
- Verification must be narrow and meaningful. Reject broad package-wide test commands, malformed shell snippets, zero-test filters, and directory greps as sole proof.\n\
- Security, credentials, generated executable workflow text, destructive operations, and external-service tasks must carry explicit scope boundaries and proof expectations.\n\
- Research or decision tasks must produce concrete artifacts and must not silently authorize implementation before the decision is made.\n\
- If the plan is not ready for parallel execution, amend it until it is ready or write a NO-GO verdict explaining the blocker.";

    let output_spec = format!(
        "Write `{super_root}/{gate}` with:\n\
- `# SUPER EXECUTION GATE`\n\
- A line exactly `Verdict: GO` or `Verdict: NO-GO`\n\
- Queue summary (cite blocker / gate ids from `super-findings.json`)\n\
- Changes made\n\
- Remaining risks (cite `RSK-` ids)\n\
- Parallel launch notes\n\
\n\
Only write `Verdict: GO` if it is safe and useful for `auto parallel` to begin immediately after this gate.",
        super_root = super_root.display(),
        gate = EXECUTION_GATE_FILE,
    );

    PromptSpec::new(role)
        .ethos(EthosPosture::EthosOnly)
        .edit_boundary(edit_boundary)
        .input("Canonical findings", inputs)
        .input("Gate criteria", gate_criteria)
        .output("Execution gate verdict", output_spec)
        .verdicts(["Verdict: GO", "Verdict: NO-GO"])
        .render()
}

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq)]
struct DeterministicGateSummary {
    unchecked_tasks: usize,
    priority_tasks: usize,
    follow_on_tasks: usize,
}

fn verify_parallel_ready_plan(plan_path: &Path) -> Result<DeterministicGateSummary> {
    let markdown = fs::read_to_string(plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    if !markdown.trim_start().starts_with("# IMPLEMENTATION_PLAN") {
        bail!(
            "{} must start with `# IMPLEMENTATION_PLAN`",
            plan_path.display()
        );
    }
    for section in [
        "## Priority Work",
        "## Follow-On Work",
        "## Completed / Already Satisfied",
    ] {
        if !markdown.contains(section) {
            bail!("{} is missing `{section}`", plan_path.display());
        }
    }

    let tasks = extract_super_task_blocks(&markdown);
    let unchecked = tasks
        .iter()
        .filter(|task| !task.checked && task.section != SuperPlanSection::Completed)
        .collect::<Vec<_>>();
    if unchecked.is_empty() {
        bail!("{} has no unchecked executable tasks", plan_path.display());
    }
    let shared_tasks = parse_tasks(&markdown);
    let all_task_ids = shared_tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for task in &unchecked {
        verify_super_task(task, &all_task_ids)?;
    }

    Ok(DeterministicGateSummary {
        unchecked_tasks: unchecked.len(),
        priority_tasks: unchecked
            .iter()
            .filter(|task| task.section == SuperPlanSection::Priority)
            .count(),
        follow_on_tasks: unchecked
            .iter()
            .filter(|task| task.section == SuperPlanSection::FollowOn)
            .count(),
    })
}

fn verify_super_task(
    task: &SuperTaskBlock,
    all_task_ids: &std::collections::BTreeSet<&str>,
) -> Result<()> {
    let parsed_task = parse_tasks(&task.markdown)
        .into_iter()
        .find(|candidate| candidate.id == task.task_id)
        .with_context(|| {
            format!(
                "task `{}` is not parseable by shared task parser",
                task.task_id
            )
        })?;
    validate_execution_row(&parsed_task, all_task_ids)
        .with_context(|| format!("task `{}` failed execution-row validation", task.task_id))?;
    let verification = first_super_task_field_line(task, "Verification:").unwrap_or("");
    if verification_looks_broad_or_malformed(verification) {
        bail!(
            "task `{}` uses package-wide cargo test verification; include a concrete test-name filter",
            task.task_id
        );
    }

    for forbidden in [
        "TBD",
        "TODO",
        "decomposition required",
        "split before implementation",
    ] {
        if task.markdown.contains(forbidden) {
            bail!(
                "task `{}` contains forbidden placeholder `{forbidden}`",
                task.task_id
            );
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn verify_super_task_process_fields(task: &SuperTaskBlock) -> Result<()> {
    for &field in PLAN_TASK_PROCESS_FIELDS {
        let value = first_super_task_field_line(task, field)
            .with_context(|| format!("task `{}` is missing `{field}`", task.task_id))?;
        let lowercase = value.to_ascii_lowercase();
        for forbidden in ["tbd", "todo", "unspecified", "unknown"] {
            if lowercase.contains(forbidden) {
                bail!(
                    "task `{}` has vague `{field}` content `{forbidden}`",
                    task.task_id
                );
            }
        }
    }

    let ui_consumers = first_super_task_field_line(task, "UI consumers:").unwrap_or("none");
    let has_ui = !field_value_is_none(ui_consumers);
    let cross_surface = first_super_task_field_line(task, "Cross-surface tests:").unwrap_or("none");
    if has_ui && field_value_is_none(cross_surface) {
        bail!(
            "task `{}` names UI consumers but has no `Cross-surface tests:` proof",
            task.task_id
        );
    }

    let generated_artifacts =
        first_super_task_field_line(task, "Generated artifacts:").unwrap_or("none");
    let contract_generation =
        first_super_task_field_line(task, "Contract generation:").unwrap_or("none");
    if !field_value_is_none(generated_artifacts) && field_value_is_none(contract_generation) {
        bail!(
            "task `{}` names generated artifacts but has no `Contract generation:` command",
            task.task_id
        );
    }

    let review_closeout = first_super_task_field_line(task, "Review/closeout:").unwrap_or("");
    let review_lower = review_closeout.to_ascii_lowercase();
    if review_lower == "cargo check" || review_lower.contains("cargo check only") {
        bail!(
            "task `{}` cannot use only cargo check for `Review/closeout:`",
            task.task_id
        );
    }

    Ok(())
}

#[allow(dead_code)]
fn field_value_is_none(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower == "none" || lower.starts_with("none ") || lower.starts_with("none --")
}

fn verification_looks_broad_or_malformed(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("cargo test --all")
        || lower.contains("cargo test --workspace")
        || lower.lines().any(cargo_test_line_is_package_wide)
        || lower.lines().any(|line| line.trim() == "cargo --lib")
}

#[allow(dead_code)]
fn cargo_test_line_is_package_wide(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("cargo test") else {
        return false;
    };
    let tokens = rest.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return true;
    }
    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index];
        if token == "--" || token == "&&" || token == ";" || token == "||" {
            break;
        }
        if matches!(
            token,
            "-p" | "--package"
                | "--manifest-path"
                | "--target"
                | "--features"
                | "-F"
                | "--test"
                | "--bin"
                | "--example"
                | "--bench"
        ) {
            index += 2;
            continue;
        }
        if token.starts_with('-') || token.starts_with("--package=") || token.starts_with("-p") {
            index += 1;
            continue;
        }
        return false;
    }
    true
}

#[allow(dead_code)]
fn contains_path_like_token(body: &str) -> bool {
    body.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | '(' | ')'))
        .map(|token| token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | ':' | '.')))
        .any(|token| {
            token.contains('/')
                || token.starts_with("refs/")
                || [
                    "src",
                    "docs",
                    "specs",
                    "tests",
                    "scripts",
                    "README.md",
                    "IMPLEMENTATION_PLAN.md",
                ]
                .contains(&token)
                || [
                    ".rs", ".md", ".toml", ".json", ".yaml", ".yml", ".sh", ".ts", ".tsx", ".js",
                ]
                .iter()
                .any(|extension| token.ends_with(extension))
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SuperPlanSection {
    Priority,
    FollowOn,
    Completed,
}

struct SuperTaskBlock {
    section: SuperPlanSection,
    task_id: String,
    checked: bool,
    markdown: String,
}

fn extract_super_task_blocks(markdown: &str) -> Vec<SuperTaskBlock> {
    let mut section = SuperPlanSection::Priority;
    let mut blocks = Vec::new();
    let mut current = Vec::<String>::new();
    for line in markdown.lines() {
        match line.trim() {
            "## Priority Work" => {
                finish_super_task(section, &mut current, &mut blocks);
                section = SuperPlanSection::Priority;
                continue;
            }
            "## Follow-On Work" => {
                finish_super_task(section, &mut current, &mut blocks);
                section = SuperPlanSection::FollowOn;
                continue;
            }
            "## Completed / Already Satisfied" => {
                finish_super_task(section, &mut current, &mut blocks);
                section = SuperPlanSection::Completed;
                continue;
            }
            _ => {}
        }
        if parse_super_task_header(line).is_some() {
            finish_super_task(section, &mut current, &mut blocks);
            current.push(line.to_string());
        } else if !current.is_empty() {
            current.push(line.to_string());
        }
    }
    finish_super_task(section, &mut current, &mut blocks);
    blocks
}

fn finish_super_task(
    section: SuperPlanSection,
    current: &mut Vec<String>,
    blocks: &mut Vec<SuperTaskBlock>,
) {
    if current.is_empty() {
        return;
    }
    if let Some((checked, task_id)) = parse_super_task_header(&current[0]) {
        blocks.push(SuperTaskBlock {
            section,
            task_id,
            checked,
            markdown: current.join("\n"),
        });
    }
    current.clear();
}

fn parse_super_task_header(line: &str) -> Option<(bool, String)> {
    let trimmed = line.trim_start();
    let checked = if trimmed.starts_with("- [ ] ") || trimmed.starts_with("- [~] ") {
        false
    } else if trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ") {
        true
    } else {
        return None;
    };
    let rest = trimmed[6..].trim_start().strip_prefix('`')?;
    let tick = rest.find('`')?;
    Some((checked, rest[..tick].trim().to_string()))
}

#[allow(dead_code)]
fn task_field_value<'a>(task: &'a SuperTaskBlock, field: &str) -> Option<&'a str> {
    task.markdown
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(field).map(str::trim))
        .filter(|value| !value.is_empty())
}

fn first_super_task_field_line<'a>(task: &'a SuperTaskBlock, field: &str) -> Option<&'a str> {
    task.markdown
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(field).map(str::trim))
        .filter(|value| !value.is_empty())
}
