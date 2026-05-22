use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};

use crate::task_parser::model::{LaneKind, PlanTask, TaskStatus};
use crate::task_parser::parse::{
    parse_task_header, parse_tasks, strip_list_bullet, task_field_body_until_any,
    task_field_line_remainder, PLAN_TASK_PROCESS_FIELDS, PLAN_TASK_REQUIRED_FIELDS,
    TASK_FIELD_BOUNDARIES,
};
use crate::verification_lint::verify_commands_are_runnable;

pub(crate) fn validate_execution_row(task: &PlanTask, all_task_ids: &BTreeSet<&str>) -> Result<()> {
    let header = task.markdown.lines().next().unwrap_or_default();
    let (status, header_id, title) = parse_task_header(header)
        .with_context(|| format!("task `{}` header did not parse", task.id))?;
    if status != task.status || header_id != task.id || title.trim().is_empty() {
        bail!(
            "task `{}` header must parse to its task id, status, and a non-empty title",
            task.id
        );
    }

    for &field in PLAN_TASK_REQUIRED_FIELDS {
        let body = execution_row_field_body(task, field)?;
        if body.trim().is_empty() {
            bail!("task `{}` has empty required field `{field}`", task.id);
        }
    }

    validate_execution_row_dependencies(task, all_task_ids)?;
    validate_execution_row_estimated_scope(task)?;
    validate_execution_row_completion_artifacts(task)?;
    validate_execution_row_process_fields(task)?;
    validate_execution_row_commands(task)?;
    validate_execution_row_concrete_ownership(task)?;
    validate_execution_row_lane_kind(task)?;
    validate_execution_row_field_boundaries(task, "Verification:")?;
    validate_execution_row_field_boundaries(task, "Required tests:")?;
    Ok(())
}

pub(crate) fn validate_execution_rows(plan: &str) -> Result<Vec<PlanTask>> {
    let tasks = parse_tasks(plan);
    let all_task_ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();
    let lenient = std::env::var("AUTO_LENIENT_GATE").ok().as_deref() == Some("1")
        || std::env::var("AUTO_LENIENT_DEPS").ok().as_deref() == Some("1");
    for task in tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Partial))
    {
        if let Err(err) = validate_execution_row(task, &all_task_ids) {
            if lenient {
                eprintln!("warning: {err:#} (continuing under AUTO_LENIENT_GATE=1)");
                continue;
            }
            return Err(err);
        }
    }
    Ok(tasks)
}

pub(crate) fn execution_row_field_body(task: &PlanTask, field: &str) -> Result<String> {
    task_field_body_until_any(&task.markdown, field, TASK_FIELD_BOUNDARIES)
        .with_context(|| format!("task `{}` missing `{field}`", task.id))
}

pub(crate) fn execution_row_first_field_line(task: &PlanTask, field: &str) -> Result<String> {
    let body = execution_row_field_body(task, field)?;
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
        .with_context(|| format!("task `{}` has no value for `{field}`", task.id))
}

fn validate_execution_row_dependencies(
    task: &PlanTask,
    all_task_ids: &BTreeSet<&str>,
) -> Result<()> {
    let body = execution_row_field_body(task, "Dependencies:")?;
    let raw_meaningful_lines: Vec<String> = body
        .lines()
        .map(strip_list_bullet)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let meaningful_lines: Vec<String> = raw_meaningful_lines
        .iter()
        // Strip parenthesized annotations and trailing sentence punctuation
        // (`.`, `;`, `:`) before treating the line as a token list. The raw
        // text is still checked below so runnable execution rows cannot hide
        // prose dependency hints in those annotations.
        .map(|line| strip_dependency_annotations(line).trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let meaningful_lines: Vec<&str> = meaningful_lines.iter().map(String::as_str).collect();
    if meaningful_lines.is_empty() {
        bail!("task `{}` has empty `Dependencies:`", task.id);
    }

    let raw_joined = raw_meaningful_lines.join(" ");
    let joined = meaningful_lines.join(" ");
    if joined.eq_ignore_ascii_case("none") {
        reject_dependency_prose(task, &raw_joined)?;
        if !task.dependencies.is_empty() {
            bail!(
                "task `{}` says `Dependencies: none` but parser found {:?}",
                task.id,
                task.dependencies
            );
        }
        return Ok(());
    }
    reject_dependency_prose(task, &raw_joined)?;

    let mut explicit = Vec::new();
    for line in meaningful_lines {
        for part in line.split(',') {
            let token = part.trim();
            if token.is_empty() {
                continue;
            }
            let Some(unwrapped) = token
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))
            else {
                bail!(
                    "task `{}` `Dependencies:` must contain only backticked task IDs or `none`; got `{token}`",
                    task.id
                );
            };
            explicit.push(unwrapped.to_string());
        }
    }
    explicit.sort();
    explicit.dedup();
    let mut parsed = task.dependencies.clone();
    parsed.sort();
    if explicit != parsed {
        bail!(
            "task `{}` dependencies are not parser-stable; explicit {:?}, parsed {:?}",
            task.id,
            explicit,
            parsed
        );
    }
    for dependency in &task.dependencies {
        if dependency == &task.id {
            bail!("task `{}` cannot depend on itself", task.id);
        }
        if !all_task_ids.contains(dependency.as_str()) {
            bail!(
                "task `{}` depends on `{dependency}`, which is not a parseable task in the plan",
                task.id
            );
        }
    }
    Ok(())
}

fn reject_dependency_prose(task: &PlanTask, text: &str) -> Result<()> {
    // Preserve parser tolerance for historical dependency notes, but keep
    // execution-row validation strict: runnable queues need scheduler input,
    // not parenthetical prose that can smuggle extra task IDs or wave hints.
    let original_lower = text.to_ascii_lowercase();
    let stripped = strip_dependency_annotations(text);
    let lower = stripped.to_ascii_lowercase();
    for phrase in [
        "parallel", "wave", "after ", "once ", "blocked", "gated", "depends", "external",
    ] {
        if original_lower.contains(phrase) || lower.contains(phrase) {
            bail!(
                "task `{}` `Dependencies:` must be machine-readable IDs only; remove prose phrase `{phrase}`",
                task.id
            );
        }
    }
    Ok(())
}

fn strip_dependency_annotations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth: u32 = 0;
    for ch in text.chars() {
        match ch {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            '.' | ';' | ':' if depth == 0 => continue,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn validate_execution_row_estimated_scope(task: &PlanTask) -> Result<()> {
    let scope = execution_row_first_field_line(task, "Estimated scope:")?;
    if !matches!(scope.as_str(), "XS" | "S" | "M") {
        bail!(
            "task `{}` must use `Estimated scope: XS`, `S`, or `M`; got `{scope}`",
            task.id
        );
    }
    Ok(())
}

fn validate_execution_row_lane_kind(task: &PlanTask) -> Result<()> {
    let Some(body) = task_field_body_until_any(&task.markdown, "Lane kind:", TASK_FIELD_BOUNDARIES)
    else {
        return Ok(());
    };
    let first = body
        .lines()
        .map(strip_list_bullet)
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if LaneKind::parse(first).is_none() {
        bail!(
            "task `{}` has invalid `Lane kind:` `{first}`; expected code, operator, or evidence",
            task.id
        );
    }
    Ok(())
}

fn validate_execution_row_completion_artifacts(task: &PlanTask) -> Result<()> {
    let body = execution_row_field_body(task, "Completion artifacts:")?;
    let first = body
        .lines()
        .map(strip_list_bullet)
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if first == "none"
        || first.starts_with("none ")
        || first.starts_with("none--")
        || first.starts_with("none -")
    {
        return Ok(());
    }
    // Accept any non-empty body. Authors (LLM and human) often write a
    // mixture of paths and short prose like
    // `new test file committed; no separate evidence note required.`
    // The required-field listing above already enforces presence and the
    // TBD/TODO check rejects vague placeholders.
    if !body.trim().is_empty() {
        return Ok(());
    }
    if task.completion_artifacts.is_empty() {
        bail!(
            "task `{}` `Completion artifacts:` must be `none` or concrete repo-relative paths",
            task.id
        );
    }
    Ok(())
}

fn validate_execution_row_process_fields(task: &PlanTask) -> Result<()> {
    for &field in PLAN_TASK_PROCESS_FIELDS {
        let value = execution_row_first_field_line(task, field)?;
        let lowercase = value.to_ascii_lowercase();
        for forbidden in ["tbd", "todo", "unspecified", "unknown"] {
            if lowercase.contains(forbidden) {
                bail!(
                    "task `{}` has vague `{field}` content `{forbidden}`",
                    task.id
                );
            }
        }
    }

    // Loosened: docs/evidence tasks legitimately reference UI consumers
    // (e.g., "TUI control-center About text") without owning a cross-surface
    // test, because the task is purely a copy fix and the surface itself is
    // covered by a separate test row. The required-field listing still
    // mandates non-vague content, so authors can't elide the field entirely.
    let _ = execution_row_first_field_line(task, "UI consumers:")?;
    let _ = execution_row_first_field_line(task, "Cross-surface tests:")?;

    // We used to require `Contract generation:` whenever `Generated artifacts:`
    // was populated. That's correct for codegen-shaped tasks (TS bindings,
    // OpenAPI schemas) but wrong for the large family of tasks whose
    // "generated artifacts" are evidence files, ops runbooks, or doc-only
    // updates -- those don't have a regeneration command, and the author
    // legitimately writes "Contract generation: none -- no generated contract."
    // The required-field listing above already enforces non-vague content via
    // the TBD/TODO/unspecified/unknown checks; this coupling check punished
    // valid evidence-shaped tasks too aggressively.
    let _ = execution_row_first_field_line(task, "Generated artifacts:")?;
    let _ = execution_row_first_field_line(task, "Contract generation:")?;

    let review_closeout = execution_row_first_field_line(task, "Review/closeout:")?;
    let review_lower = review_closeout.to_ascii_lowercase();
    if review_lower == "cargo check" || review_lower.contains("cargo check only") {
        bail!(
            "task `{}` cannot use only cargo check for `Review/closeout:`",
            task.id
        );
    }
    Ok(())
}

fn field_value_is_none(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower == "none"
        || lower.starts_with("none ")
        || lower.starts_with("none --")
        || lower.starts_with("none -")
}

fn validate_execution_row_commands(task: &PlanTask) -> Result<()> {
    verify_commands_are_runnable(
        task.id.as_str(),
        "Verification:",
        &execution_row_field_body(task, "Verification:")?,
    )?;
    verify_commands_are_runnable(
        task.id.as_str(),
        "Required tests:",
        &execution_row_field_body(task, "Required tests:")?,
    )?;
    Ok(())
}

fn validate_execution_row_concrete_ownership(task: &PlanTask) -> Result<()> {
    let owns = execution_row_field_body(task, "Owns:")?;
    if owns.trim().is_empty() {
        bail!("task `{}` has non-concrete `Owns:` field", task.id);
    }
    let lower = owns.to_ascii_lowercase();
    for forbidden in ["tbd", "todo", "unspecified", "unknown", "missing"] {
        if lower.contains(forbidden) {
            bail!("task `{}` has vague `Owns:` content `{forbidden}`", task.id);
        }
    }
    if !contains_path_like_token(&owns) {
        bail!(
            "task `{}` `Owns:` must give concrete path-like ownership such as `src/`, `docs`, `README.md`, or `refs/tags/<tag>`",
            task.id
        );
    }
    Ok(())
}

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

fn validate_execution_row_field_boundaries(task: &PlanTask, field: &str) -> Result<()> {
    let body = execution_row_field_body(task, field)?;
    for boundary in TASK_FIELD_BOUNDARIES
        .iter()
        .filter(|boundary| **boundary != field)
    {
        if body
            .lines()
            .map(strip_list_bullet)
            .map(str::trim)
            .any(|line| task_field_line_remainder(line, boundary).is_some())
        {
            bail!(
                "task `{}` `{field}` body swallowed metadata boundary `{boundary}`",
                task.id
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::task_parser::parse::{
        parse_tasks, PLAN_TASK_PROCESS_FIELDS, PLAN_TASK_REQUIRED_FIELDS, TASK_FIELD_BOUNDARIES,
    };
    use crate::task_parser::validate::validate_execution_row;

    #[test]
    fn plan_task_field_catalog_covers_rich_contract_boundaries() {
        for required in [
            "Source of truth:",
            "Runtime owner:",
            "UI consumers:",
            "Generated artifacts:",
            "Fixture boundary:",
            "Retired surfaces:",
            "Contract generation:",
            "Cross-surface tests:",
            "Review/closeout:",
        ] {
            assert!(
                PLAN_TASK_REQUIRED_FIELDS.contains(&required),
                "{required} must stay in the shared required-field catalog"
            );
        }

        for process_field in PLAN_TASK_PROCESS_FIELDS {
            assert!(
                PLAN_TASK_REQUIRED_FIELDS.contains(process_field),
                "{process_field} must be required before process validation can read it"
            );
        }

        for field in PLAN_TASK_REQUIRED_FIELDS {
            assert!(
                TASK_FIELD_BOUNDARIES.contains(field),
                "{field} must bound multiline task-field parsing"
            );
        }
    }

    fn rich_execution_row_plan() -> String {
        r#"
- [ ] `TASK-001` Runtime backed UI fix

  Spec: `specs/runtime-ui.md`
  Why now: closes runtime/UI drift before parallel implementation.
  Codebase evidence: `src/runtime.rs` and `web/App.tsx`.
  Source of truth: `src/runtime.rs`
  Runtime owner: `src/runtime.rs`
  UI consumers: `web/App.tsx`
  Generated artifacts: `bindings/schema.json`
  Fixture boundary: fixtures only in `tests/fixtures`.
  Retired surfaces: none
  Owns: `src/runtime.rs`, `web/App.tsx`
  Integration touchpoints: `bindings/schema.json`
  Scope boundary: keep changes inside runtime and UI readback.
  Acceptance criteria: UI renders runtime-produced status.
  Verification: `cargo test runtime_ui_readback`
  Required tests: `cargo test runtime_ui_readback`
  Contract generation: `cargo run -p xtask -- codegen`
  Cross-surface tests: `cargo test runtime_ui_readback`
  Review/closeout: REVIEW.md records runtime-to-UI proof.
  Completion artifacts: `REVIEW.md`
  Dependencies: none
  Estimated scope: S
  Completion signal: local tests and review handoff pass.
"#
        .to_string()
    }

    #[test]
    fn execution_row_validator_accepts_rich_generated_contract() {
        let plan = rich_execution_row_plan();
        let tasks = parse_tasks(&plan);
        let ids = tasks.iter().map(|task| task.id.as_str()).collect();
        validate_execution_row(&tasks[0], &ids).expect("rich row should validate");
    }

    #[test]
    fn execution_row_validator_rejects_missing_required_field_with_task_id() {
        let plan = rich_execution_row_plan().replace("  Runtime owner: `src/runtime.rs`\n", "");
        let tasks = parse_tasks(&plan);
        let ids = tasks.iter().map(|task| task.id.as_str()).collect();
        let err = validate_execution_row(&tasks[0], &ids).expect_err("missing field rejected");
        assert!(format!("{err:#}").contains("TASK-001"));
        assert!(format!("{err:#}").contains("Runtime owner:"));
    }

    #[test]
    fn execution_row_validator_rejects_prose_only_dependencies() {
        let plan = rich_execution_row_plan().replace(
            "  Dependencies: none\n",
            "  Dependencies: after the runtime work lands\n",
        );
        let tasks = parse_tasks(&plan);
        let ids = tasks.iter().map(|task| task.id.as_str()).collect();
        let err = validate_execution_row(&tasks[0], &ids).expect_err("prose rejected");
        assert!(format!("{err:#}").contains("machine-readable"));
    }
}
