//! Deterministic execution gate: parse the snapshot plan and validate task rows.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::super_command::manifest::super_snapshot_promotion_command;
use crate::super_command::{
    IMPLEMENTATION_PLAN, SUPER_GENERATION_MODE_SNAPSHOT_ONLY, SUPER_PLAN_SOURCE_GENERATED_SNAPSHOT,
    SUPER_PLAN_SOURCE_ROOT_LEDGER, SUPER_ROOT_PLAN_STATUS_UNCHANGED,
};
use crate::task_parser::{parse_tasks, validate_execution_row, PLAN_TASK_PROCESS_FIELDS};

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq)]
pub(crate) struct DeterministicGateSummary {
    pub(crate) unchecked_tasks: usize,
    pub(crate) priority_tasks: usize,
    pub(crate) follow_on_tasks: usize,
    #[serde(default)]
    pub(crate) plan_path: Option<String>,
    #[serde(default)]
    pub(crate) plan_source: String,
    #[serde(default)]
    pub(crate) generation_mode: String,
    #[serde(default)]
    pub(crate) root_plan_status: String,
    #[serde(default)]
    pub(crate) promotion_required: bool,
    #[serde(default)]
    pub(crate) promotion_command: Option<String>,
}

pub(crate) fn verify_parallel_ready_plan(plan_path: &Path) -> Result<DeterministicGateSummary> {
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
    let lenient = std::env::var("AUTO_LENIENT_GATE").ok().as_deref() == Some("1")
        || std::env::var("AUTO_LENIENT_DEPS").ok().as_deref() == Some("1");
    for task in &unchecked {
        if let Err(err) = verify_super_task(task, &all_task_ids) {
            if lenient {
                eprintln!("warning: {err:#} (continuing under AUTO_LENIENT_GATE=1)");
                continue;
            }
            return Err(err);
        }
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
        plan_path: Some(plan_path.display().to_string()),
        plan_source: SUPER_PLAN_SOURCE_ROOT_LEDGER.to_string(),
        generation_mode: "root".to_string(),
        root_plan_status: "inspected".to_string(),
        promotion_required: false,
        promotion_command: None,
    })
}

pub(crate) fn verify_super_snapshot_ready_plan(
    output_dir: Option<&Path>,
) -> Result<DeterministicGateSummary> {
    let output_dir = output_dir.context(
        "auto super generated snapshot path is unavailable; cannot run deterministic gate",
    )?;
    let plan_path = output_dir.join(IMPLEMENTATION_PLAN);
    let mut gate = verify_parallel_ready_plan(&plan_path)?;
    gate.plan_source = SUPER_PLAN_SOURCE_GENERATED_SNAPSHOT.to_string();
    gate.generation_mode = SUPER_GENERATION_MODE_SNAPSHOT_ONLY.to_string();
    gate.root_plan_status = SUPER_ROOT_PLAN_STATUS_UNCHANGED.to_string();
    gate.promotion_required = true;
    gate.promotion_command = Some(super_snapshot_promotion_command(output_dir));
    Ok(gate)
}

pub(crate) fn verify_super_task(
    task: &SuperTaskBlock,
    all_task_ids: &std::collections::BTreeSet<&str>,
) -> Result<()> {
    let verification = first_super_task_field_line(task, "Verification:").unwrap_or("");
    if verification_looks_broad_or_malformed(verification) {
        bail!(
            "task `{}` uses package-wide cargo test verification; include a concrete test-name filter",
            task.task_id
        );
    }

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
pub(crate) fn verify_super_task_process_fields(task: &SuperTaskBlock) -> Result<()> {
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
pub(crate) fn field_value_is_none(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower == "none" || lower.starts_with("none ") || lower.starts_with("none --")
}

pub(crate) fn verification_looks_broad_or_malformed(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("cargo test --all")
        || lower.contains("cargo test --workspace")
        || lower.lines().any(cargo_test_line_is_package_wide)
        || lower.lines().any(|line| line.trim() == "cargo --lib")
}

#[allow(dead_code)]
pub(crate) fn cargo_test_line_is_package_wide(line: &str) -> bool {
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
pub(crate) fn contains_path_like_token(body: &str) -> bool {
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
pub(crate) enum SuperPlanSection {
    Priority,
    FollowOn,
    Completed,
}

pub(crate) struct SuperTaskBlock {
    section: SuperPlanSection,
    task_id: String,
    checked: bool,
    markdown: String,
}

pub(crate) fn extract_super_task_blocks(markdown: &str) -> Vec<SuperTaskBlock> {
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

pub(crate) fn finish_super_task(
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

pub(crate) fn parse_super_task_header(line: &str) -> Option<(bool, String)> {
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
pub(crate) fn task_field_value<'a>(task: &'a SuperTaskBlock, field: &str) -> Option<&'a str> {
    task.markdown
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(field).map(str::trim))
        .filter(|value| !value.is_empty())
}

pub(crate) fn first_super_task_field_line<'a>(
    task: &'a SuperTaskBlock,
    field: &str,
) -> Option<&'a str> {
    task.markdown
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(field).map(str::trim))
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::super_command::manifest::super_snapshot_promotion_command;
    use crate::super_command::{
        IMPLEMENTATION_PLAN, SUPER_GENERATION_MODE_SNAPSHOT_ONLY,
        SUPER_PLAN_SOURCE_GENERATED_SNAPSHOT, SUPER_ROOT_PLAN_STATUS_UNCHANGED,
    };

    use super::{verify_parallel_ready_plan, verify_super_snapshot_ready_plan};

    #[test]
    fn deterministic_gate_accepts_scoped_unfinished_task() {
        let root = temp_dir("super-gate-ok");
        let plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(&plan, valid_plan("cargo test super_command::tests::deterministic_gate_accepts_scoped_unfinished_task")).unwrap();
        let summary = verify_parallel_ready_plan(&plan).unwrap();
        assert_eq!(summary.unchecked_tasks, 1);
        assert_eq!(summary.priority_tasks, 1);
    }

    #[test]
    fn deterministic_gate_rejects_package_wide_cargo_test() {
        let root = temp_dir("super-gate-broad");
        let plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(&plan, valid_plan("cargo test")).unwrap();
        let error = verify_parallel_ready_plan(&plan).expect_err("expected broad test rejection");
        assert!(error.to_string().contains("package-wide cargo test"));
    }

    #[test]
    fn super_rejects_task_missing_runtime_ui_fields() {
        let root = temp_dir("super-gate-missing-runtime-ui");
        let plan = root.join(IMPLEMENTATION_PLAN);
        let malformed = valid_plan(
            "cargo test super_command::tests::super_rejects_task_missing_runtime_ui_fields",
        )
        .replace("    Runtime owner: `src/super_command.rs`\n", "");
        fs::write(&plan, malformed).unwrap();

        let error = verify_parallel_ready_plan(&plan)
            .expect_err("expected rich runtime/UI task contract rejection");

        assert!(format!("{error:#}").contains("task `TASK-001` missing `Runtime owner:`"));
    }

    #[test]
    fn super_accepts_generated_rich_task_contract() {
        let root = temp_dir("super-gate-rich-contract");
        let plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(
            &plan,
            valid_plan(
                "cargo test super_command::tests::super_accepts_generated_rich_task_contract",
            ),
        )
        .unwrap();

        let summary = verify_parallel_ready_plan(&plan).unwrap();

        assert_eq!(summary.unchecked_tasks, 1);
        assert_eq!(summary.priority_tasks, 1);
        assert_eq!(summary.follow_on_tasks, 0);
    }

    #[test]
    fn super_deterministic_gate_reads_generated_snapshot_plan() {
        let root = temp_dir("super-snapshot-gate");
        let root_plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(
            &root_plan,
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
        )
        .unwrap();
        let gen_dir = root.join("gen-snapshot");
        fs::create_dir_all(&gen_dir).unwrap();
        let generated_plan = gen_dir.join(IMPLEMENTATION_PLAN);
        fs::write(
            &generated_plan,
            valid_plan(
                "cargo test super_command::tests::super_deterministic_gate_reads_generated_snapshot_plan",
            ),
        )
        .unwrap();

        let gate = verify_super_snapshot_ready_plan(Some(&gen_dir)).unwrap();

        assert_eq!(gate.unchecked_tasks, 1);
        assert_eq!(gate.plan_path, Some(generated_plan.display().to_string()));
        assert_eq!(gate.plan_source, SUPER_PLAN_SOURCE_GENERATED_SNAPSHOT);
        assert_eq!(gate.generation_mode, SUPER_GENERATION_MODE_SNAPSHOT_ONLY);
        assert_eq!(gate.root_plan_status, SUPER_ROOT_PLAN_STATUS_UNCHANGED);
        assert!(gate.promotion_required);
        assert_eq!(
            gate.promotion_command,
            Some(super_snapshot_promotion_command(&gen_dir))
        );
    }

    fn valid_plan(verification: &str) -> String {
        format!(
            r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `TASK-001` Harden super gate

    Spec: `specs/220426-super.md`
    Why now: proves the gate works.
    Codebase evidence: `src/super_command.rs`
    Source of truth: `src/super_command.rs`
    Runtime owner: `src/super_command.rs`
    UI consumers: terminal output
    Generated artifacts: `.auto/super/*/DETERMINISTIC-GATE.json`
    Fixture boundary: production code parses the live root plan, not fixture rows.
    Retired surfaces: legacy active task rows without runtime/UI contract fields.
    Owns: `src/super_command.rs`
    Integration touchpoints: `src/main.rs`
    Scope boundary: do not launch workers.
    Acceptance criteria: scoped plan passes.
    Verification: {verification}
    Required tests: `cargo test super_command::tests::super_accepts_generated_rich_task_contract`
    Contract generation: `cargo test super_command::tests::super_accepts_generated_rich_task_contract`
    Cross-surface tests: `cargo test super_command::tests::super_accepts_generated_rich_task_contract`
    Review/closeout: reviewer checks super and generation task contracts stay aligned.
    Completion artifacts: `src/super_command.rs`
    Lane kind: code
    Dependencies: none
    Estimated scope: S
    Completion signal: tests pass.

## Follow-On Work

## Completed / Already Satisfied
"#
        )
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
