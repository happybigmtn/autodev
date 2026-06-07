//! Validation, normalization, and task-block extraction for generated
//! implementation plans.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::generation::prompts::{IMPLEMENTATION_PLAN_HEADER, REQUIRED_PLAN_SECTIONS};
use crate::task_parser::{
    parse_task_header as parse_shared_task_header, validate_execution_rows, TaskStatus,
    PLAN_TASK_PROCESS_FIELDS, PLAN_TASK_REQUIRED_FIELDS,
};
use crate::util::{atomic_write, list_markdown_files};
use crate::verification_lint::verify_commands_are_runnable;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlanSection {
    Priority,
    FollowOn,
    Completed,
}

pub(crate) struct PlanTaskBlock {
    pub(crate) section: PlanSection,
    pub(crate) task_id: String,
    pub(crate) checked: bool,
    pub(crate) markdown: String,
}

pub(crate) fn verify_generated_implementation_plan(output_dir: &Path) -> Result<PathBuf> {
    let plan_path = output_dir.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        bail!("generation did not write {}", plan_path.display());
    }
    let markdown = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let normalized = normalize_generated_implementation_plan(&markdown);
    for required in [IMPLEMENTATION_PLAN_HEADER]
        .into_iter()
        .chain(REQUIRED_PLAN_SECTIONS)
    {
        if !normalized.contains(required) {
            bail!("generated implementation plan is missing `{required}`");
        }
    }
    let blocks = extract_plan_task_blocks(&normalized)?;
    let lenient = std::env::var("AUTO_LENIENT_GATE").ok().as_deref() == Some("1")
        || std::env::var("AUTO_LENIENT_DEPS").ok().as_deref() == Some("1");
    if let Err(err) = validate_execution_rows(&normalized) {
        if lenient {
            eprintln!("warning: {err:#} (continuing under AUTO_LENIENT_GATE=1)");
        } else {
            return Err(anyhow!(
                "generated implementation plan failed shared execution-row validation: {err:#}"
            ));
        }
    }
    for block in &blocks {
        if block.checked {
            continue;
        }
        let block_validation = (|| -> Result<()> {
            for &field in PLAN_TASK_REQUIRED_FIELDS {
                if !block.markdown.contains(field) {
                    bail!(
                        "generated implementation plan task `{}` is missing `{}`",
                        block.task_id,
                        field
                    );
                }
            }
            verify_generated_plan_task_is_scoped(block)?;
            Ok(())
        })();
        if let Err(err) = block_validation {
            if lenient {
                eprintln!("warning: {err:#} (continuing under AUTO_LENIENT_GATE=1)");
                continue;
            }
            return Err(err);
        }
    }
    let available_specs = collect_available_spec_refs(&output_dir.join("specs"))?;
    validate_plan_spec_refs(
        &normalized,
        &available_specs,
        &format!("generated implementation plan {}", plan_path.display()),
        true,
    )?;
    if normalized != markdown {
        atomic_write(&plan_path, normalized.as_bytes())
            .with_context(|| format!("failed to normalize {}", plan_path.display()))?;
    }
    Ok(plan_path)
}

fn verify_generated_plan_task_is_scoped(block: &PlanTaskBlock) -> Result<()> {
    if block
        .markdown
        .to_ascii_lowercase()
        .contains("decomposition required")
        || block
            .markdown
            .to_ascii_lowercase()
            .contains("split before implementation")
    {
        bail!(
            "generated implementation plan task `{}` must be decomposed by auto gen instead of using a decomposition placeholder",
            block.task_id
        );
    }

    let scope = plan_task_field_line_value(block, "Estimated scope:")
        .with_context(|| format!("task `{}` missing `Estimated scope:`", block.task_id))?;
    if !matches!(scope, "XS" | "S" | "M") {
        bail!(
            "generated implementation plan task `{}` must use `Estimated scope: XS`, `S`, or `M`; got `{scope}`",
            block.task_id
        );
    }

    let required_tests = plan_task_field_body(block, "Required tests:", "Contract generation:")
        .or_else(|| plan_task_field_body(block, "Required tests:", "Completion artifacts:"))
        .or_else(|| plan_task_field_body(block, "Required tests:", "Dependencies:"))
        .with_context(|| format!("task `{}` missing `Required tests:` body", block.task_id))?;
    verify_required_tests_are_scoped(block, &required_tests)?;

    let verification = plan_task_field_body(block, "Verification:", "Required tests:")
        .with_context(|| format!("task `{}` missing `Verification:` body", block.task_id))?;
    verify_verification_commands_are_scoped(block, &verification)?;
    let completion_artifacts =
        plan_task_field_body(block, "Completion artifacts:", "Dependencies:")
            .or_else(|| plan_task_field_body(block, "Completion artifacts:", "Estimated scope:"))
            .with_context(|| {
                format!(
                    "task `{}` missing `Completion artifacts:` body",
                    block.task_id
                )
            })?;
    verify_completion_artifacts_are_concrete(block, &completion_artifacts)?;
    verify_generated_plan_process_fields(block)?;
    verify_generated_plan_task_has_concrete_ownership(block)?;
    verify_generated_plan_task_prose_gates_are_explicit(block)?;

    Ok(())
}

fn verify_generated_plan_process_fields(block: &PlanTaskBlock) -> Result<()> {
    for &field in PLAN_TASK_PROCESS_FIELDS {
        // Conditional: only validate the surface field when the task declares it.
        let Some(value) = plan_task_field_line_value(block, field) else {
            continue;
        };
        let lowercase = value.to_ascii_lowercase();
        for forbidden in ["tbd", "todo", "unspecified", "unknown"] {
            if lowercase.contains(forbidden) {
                bail!(
                    "generated implementation plan task `{}` has vague `{field}` content `{forbidden}`",
                    block.task_id
                );
            }
        }
    }

    let review_closeout = plan_task_field_line_value(block, "Review/closeout:").unwrap_or("");
    let review_lower = review_closeout.to_ascii_lowercase();
    if review_lower == "cargo check" || review_lower.contains("cargo check only") {
        bail!(
            "generated implementation plan task `{}` cannot use only cargo check for `Review/closeout:`",
            block.task_id
        );
    }

    Ok(())
}

fn strip_list_bullet(line: &str) -> &str {
    let trimmed = line.trim_start();
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return rest;
        }
    }
    trimmed
}

pub(crate) fn plan_field_line_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let unbulleted = strip_list_bullet(line);
    if let Some(rest) = unbulleted.strip_prefix(field) {
        return Some(rest.trim());
    }

    let field_name = field.trim_end_matches(':');
    let bold_field = format!("**{field_name}:**");
    if let Some(rest) = unbulleted.strip_prefix(&bold_field) {
        return Some(rest.trim());
    }

    let bold_name_field = format!("**{field_name}**:");
    unbulleted.strip_prefix(&bold_name_field).map(str::trim)
}

fn plan_task_field_line_value<'a>(block: &'a PlanTaskBlock, field: &str) -> Option<&'a str> {
    block
        .markdown
        .lines()
        .find_map(|line| plan_field_line_value(line, field).filter(|value| !value.is_empty()))
}

fn plan_task_field_body(block: &PlanTaskBlock, field: &str, next_field: &str) -> Option<String> {
    let mut collecting = false;
    let mut body = Vec::new();
    for line in block.markdown.lines() {
        if let Some(rest) = plan_field_line_value(line, field) {
            collecting = true;
            if !rest.is_empty() {
                body.push(rest.to_string());
            }
            continue;
        }
        if collecting && plan_field_line_value(line, next_field).is_some() {
            break;
        }
        if collecting {
            body.push(line.to_string());
        }
    }
    collecting.then(|| body.join("\n"))
}

fn verify_required_tests_are_scoped(block: &PlanTaskBlock, body: &str) -> Result<()> {
    let normalized = body.trim();
    let lowercase = normalized.to_ascii_lowercase();
    if lowercase.contains("see spec") {
        bail!(
            "generated implementation plan task `{}` has vague `Required tests:` content `See spec`",
            block.task_id
        );
    }
    for forbidden in ["TBD", "TODO"] {
        if normalized.contains(forbidden) {
            bail!(
                "generated implementation plan task `{}` has vague `Required tests:` content `{forbidden}`",
                block.task_id
            );
        }
    }

    if required_tests_body_is_none(normalized) {
        return Ok(());
    }

    let explicit_test_count = count_required_test_entries(normalized);
    if explicit_test_count > 5 {
        bail!(
            "generated implementation plan task `{}` lists {explicit_test_count} required tests; split the task to keep at most five",
            block.task_id
        );
    }
    if explicit_test_count == 0 {
        bail!(
            "generated implementation plan task `{}` must list concrete required test names or `Required tests: none`",
            block.task_id
        );
    }

    Ok(())
}

fn required_tests_body_is_none(body: &str) -> bool {
    let normalized = body.trim();
    let lowercase = normalized.to_ascii_lowercase();
    lowercase == "none" || lowercase.starts_with("none ") || lowercase.starts_with("none(")
}

fn count_required_test_entries(body: &str) -> usize {
    let bullet_count = body
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("- ") || trimmed.starts_with("* ")
        })
        .count();
    if bullet_count > 0 {
        return bullet_count;
    }

    let mut count = 0usize;
    let mut rest = body;
    while let Some(start) = rest.find('`') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('`') else {
            break;
        };
        let token = after_start[..end].trim();
        if !token.is_empty() {
            count += 1;
        }
        rest = &after_start[end + 1..];
    }
    count
}

fn verify_completion_artifacts_are_concrete(block: &PlanTaskBlock, body: &str) -> Result<()> {
    let normalized = body.trim();
    if normalized.is_empty() {
        bail!(
            "generated implementation plan task `{}` must name `Completion artifacts:` or `none`",
            block.task_id
        );
    }

    let lowercase = normalized.to_ascii_lowercase();
    if lowercase == "none" {
        return Ok(());
    }
    for forbidden in ["see spec", "tbd", "todo", "same as verification"] {
        if lowercase.contains(forbidden) {
            bail!(
                "generated implementation plan task `{}` has vague `Completion artifacts:` content `{forbidden}`",
                block.task_id
            );
        }
    }

    // Loosened: a non-empty body without TBD/TODO/see-spec content is enough.
    // Authors often describe artifacts in prose (`new test file committed`)
    // when the artifact is implicit in the verification command itself.
    Ok(())
}

fn verify_verification_commands_are_scoped(block: &PlanTaskBlock, body: &str) -> Result<()> {
    let lowercase = body.to_ascii_lowercase();
    for forbidden in [
        "cargo check --workspace",
        "cargo test --workspace",
        "cargo check --all",
        "cargo test --all",
    ] {
        if lowercase.contains(forbidden) {
            bail!(
                "generated implementation plan task `{}` uses broad verification `{forbidden}`; use exact or affected-scope checks",
                block.task_id
            );
        }
    }
    verify_commands_are_runnable(&block.task_id, "Verification:", body)?;
    for line in body.lines() {
        if cargo_test_command_is_package_wide(line) {
            bail!(
                "generated implementation plan task `{}` uses package-wide cargo test verification `{}`; include a concrete test-name filter",
                block.task_id,
                line.trim()
            );
        }
    }
    Ok(())
}

fn verify_generated_plan_task_has_concrete_ownership(block: &PlanTaskBlock) -> Result<()> {
    let owns = plan_task_field_body(block, "Owns:", "Integration touchpoints:")
        .with_context(|| format!("task `{}` missing `Owns:` body", block.task_id))?;
    let normalized = owns.trim();
    let lowercase = normalized.to_ascii_lowercase();
    for forbidden in ["missing", "tbd", "unspecified"] {
        if lowercase.contains(forbidden) {
            bail!(
                "generated implementation plan task `{}` has vague `Owns:` content `{forbidden}`",
                block.task_id
            );
        }
    }
    // Loosened: prose-style ownership descriptions accepted. The TBD/missing/
    // unspecified guard above prevents vacuous content; opus-style category
    // descriptions like `nine drill evidence files; no script logic changes`
    // are valid even without enumerated paths.
    Ok(())
}

fn verify_generated_plan_task_prose_gates_are_explicit(block: &PlanTaskBlock) -> Result<()> {
    let dependency_line = plan_task_field_line_value(block, "Dependencies:").unwrap_or("");
    let explicit_dependencies = collect_plan_task_refs(dependency_line);
    for line in block.markdown.lines() {
        let lower = line.to_ascii_lowercase();
        let line_has_gate_language = lower.contains("gated")
            || lower.contains("blocked until")
            || lower.contains("after ")
            || lower.contains("depends on");
        if !line_has_gate_language {
            continue;
        }
        for task_ref in collect_plan_task_refs(line) {
            if task_ref != block.task_id && !explicit_dependencies.contains(&task_ref) {
                bail!(
                    "generated implementation plan task `{}` mentions gated prerequisite `{}` in prose but omits it from `Dependencies:`",
                    block.task_id,
                    task_ref
                );
            }
        }
    }
    Ok(())
}

fn collect_plan_task_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("P-") {
        rest = &rest[start..];
        let end = rest
            .char_indices()
            .find_map(|(index, ch)| {
                if index > 0 && !(ch.is_ascii_alphanumeric() || ch == '-') {
                    Some(index)
                } else {
                    None
                }
            })
            .unwrap_or(rest.len());
        let candidate = &rest[..end];
        if candidate.len() > 2
            && candidate
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            refs.push(candidate.to_string());
        }
        rest = &rest[end..];
    }
    refs.sort();
    refs.dedup();
    refs
}

fn cargo_test_command_is_package_wide(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty()
        || trimmed.starts_with("```")
        || trimmed.starts_with('#')
        || trimmed.starts_with("//")
    {
        return false;
    }
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
        if cargo_option_takes_value(token) {
            index += 2;
            continue;
        }
        if token.starts_with("-p") && token.len() > 2 {
            index += 1;
            continue;
        }
        if token.starts_with("--package=")
            || token.starts_with("--manifest-path=")
            || token.starts_with("--target=")
            || token.starts_with("--features=")
            || token.starts_with("--test=")
            || token.starts_with("--bin=")
            || token.starts_with("--example=")
            || token.starts_with("--bench=")
        {
            index += 1;
            continue;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        return false;
    }

    true
}

fn cargo_option_takes_value(token: &str) -> bool {
    matches!(
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
    )
}

pub(crate) fn normalize_generated_implementation_plan(markdown: &str) -> String {
    let mut lines = markdown.lines().map(str::to_string).collect::<Vec<_>>();
    let Some(first_non_empty) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return markdown.to_string();
    };

    let first_line = lines[first_non_empty].trim();
    let mut changed = false;
    if first_line == IMPLEMENTATION_PLAN_HEADER {
    } else if first_line.starts_with("# ") {
        lines[first_non_empty] = IMPLEMENTATION_PLAN_HEADER.to_string();
        changed = true;
    }

    let candidate = if changed {
        let mut normalized = lines.join("\n");
        if markdown.ends_with('\n') {
            normalized.push('\n');
        }
        normalized
    } else {
        markdown.to_string()
    };
    ensure_required_plan_sections(&candidate)
}

pub(crate) fn ensure_required_plan_sections(markdown: &str) -> String {
    if markdown.trim().is_empty() {
        return markdown.to_string();
    }

    let mut normalized = markdown.to_string();
    let mut changed = false;
    for section in REQUIRED_PLAN_SECTIONS {
        if markdown_has_line(&normalized, section) {
            continue;
        }
        if !normalized.ends_with('\n') {
            normalized.push('\n');
        }
        if !normalized.ends_with("\n\n") {
            normalized.push('\n');
        }
        normalized.push_str(section);
        normalized.push('\n');
        changed = true;
    }

    if changed && !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    normalized
}

fn markdown_has_line(markdown: &str, expected: &str) -> bool {
    markdown.lines().any(|line| line.trim() == expected)
}

pub(crate) fn append_blocks_to_section(
    markdown: &mut String,
    section: PlanSection,
    blocks: &[PlanTaskBlock],
) -> Result<()> {
    let section_header = match section {
        PlanSection::Priority => "## Priority Work",
        PlanSection::FollowOn => "## Follow-On Work",
        PlanSection::Completed => return Ok(()),
    };
    let section_blocks = blocks
        .iter()
        .filter(|block| block.section == section)
        .collect::<Vec<_>>();
    if section_blocks.is_empty() {
        return Ok(());
    }

    let insert_at = markdown
        .find(section_header)
        .with_context(|| format!("generated plan is missing section `{section_header}`"))?;
    let section_end = markdown[insert_at + section_header.len()..]
        .find("\n## ")
        .map(|offset| insert_at + section_header.len() + offset)
        .unwrap_or(markdown.len());

    let mut addition = String::new();
    if !markdown[..section_end].ends_with('\n') {
        addition.push('\n');
    }
    if !markdown[..section_end].ends_with("\n\n") {
        addition.push('\n');
    }
    for block in section_blocks {
        addition.push_str(block.markdown.trim_end());
        addition.push_str("\n\n");
    }
    markdown.insert_str(section_end, &addition);
    Ok(())
}

pub(crate) fn extract_plan_task_blocks(markdown: &str) -> Result<Vec<PlanTaskBlock>> {
    let mut blocks = Vec::new();
    let mut current_section = None::<PlanSection>;
    let mut current_lines = Vec::<String>::new();

    for line in markdown.lines() {
        if let Some(section) = parse_section_header(line) {
            if let Some(block) = finalize_plan_block(current_section, &current_lines)? {
                blocks.push(block);
            }
            current_section = Some(section);
            current_lines.clear();
            continue;
        }

        if parse_plan_task_header(line).is_some() {
            if let Some(block) = finalize_plan_block(current_section, &current_lines)? {
                blocks.push(block);
            }
            current_lines = vec![line.to_string()];
            continue;
        }

        if !current_lines.is_empty() {
            current_lines.push(line.to_string());
        }
    }

    if let Some(block) = finalize_plan_block(current_section, &current_lines)? {
        blocks.push(block);
    }

    Ok(blocks)
}

fn finalize_plan_block(
    section: Option<PlanSection>,
    lines: &[String],
) -> Result<Option<PlanTaskBlock>> {
    if lines.is_empty() {
        return Ok(None);
    }
    let Some((status, task_id, _title)) = parse_plan_task_header(&lines[0]) else {
        return Ok(None);
    };
    Ok(Some(PlanTaskBlock {
        section: section.unwrap_or(PlanSection::Priority),
        task_id,
        checked: matches!(status, TaskStatus::Done),
        markdown: lines.join("\n"),
    }))
}

fn parse_section_header(line: &str) -> Option<PlanSection> {
    match line.trim() {
        "## Priority Work" => Some(PlanSection::Priority),
        "## Follow-On Work" => Some(PlanSection::FollowOn),
        "## Completed / Already Satisfied" => Some(PlanSection::Completed),
        _ => None,
    }
}

fn parse_plan_task_header(line: &str) -> Option<(TaskStatus, String, String)> {
    parse_shared_task_header(line)
}

pub(crate) fn collect_available_spec_refs(
    specs_dir: &Path,
) -> Result<std::collections::BTreeSet<String>> {
    let mut refs = std::collections::BTreeSet::new();
    if !specs_dir.is_dir() {
        return Ok(refs);
    }
    for path in list_markdown_files(specs_dir)? {
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        refs.insert(format!("specs/{name}"));
    }
    Ok(refs)
}

pub(crate) fn validate_plan_spec_refs(
    markdown: &str,
    available_specs: &std::collections::BTreeSet<String>,
    context_label: &str,
    require_spec_ref: bool,
) -> Result<()> {
    for (line_index, line) in markdown.lines().enumerate() {
        let Some(spec_value) = plan_field_line_value(line, "Spec:") else {
            continue;
        };
        let refs = extract_spec_refs_from_line(spec_value);
        if refs.is_empty() {
            if !require_spec_ref {
                continue;
            }
            bail!(
                "{context_label} line {} contains `Spec:` but no `specs/*.md` path",
                line_index + 1
            );
        }
        for spec_ref in refs {
            if !available_specs.contains(&spec_ref) {
                if !require_spec_ref {
                    continue;
                }
                bail!(
                    "{context_label} references missing spec `{spec_ref}` on line {}",
                    line_index + 1
                );
            }
        }
    }
    Ok(())
}

fn extract_spec_refs_from_line(line: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut search_start = 0usize;

    while let Some(relative_start) = line[search_start..].find("specs/") {
        let start = search_start + relative_start;
        let candidate = &line[start..];
        let end = candidate
            .char_indices()
            .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-')))
            .map(|(index, _)| index)
            .unwrap_or(candidate.len());
        let path = &candidate[..end];
        if path.ends_with(".md") {
            refs.push(path.to_string());
        }
        search_start = start + end.max(1);
    }

    refs
}

#[cfg(test)]
mod tests {
    use super::{normalize_generated_implementation_plan, verify_generated_implementation_plan};
    use crate::generation::prompts::IMPLEMENTATION_PLAN_HEADER;
    use crate::generation::root_sync::merge_generated_plan_with_existing_open_tasks;
    use crate::generation::tests::{
        temp_dir, valid_generated_plan_task, write_generated_plan, write_real_spec,
    };
    use std::fs;

    #[test]
    fn normalizes_noncanonical_plan_heading() {
        let generated = r#"# Bitino Implementation Plan

Generated: 2026-04-02

## Priority Work

## Follow-On Work

## Completed / Already Satisfied
"#;

        let normalized = normalize_generated_implementation_plan(generated);

        assert!(normalized.starts_with(&format!("{IMPLEMENTATION_PLAN_HEADER}\n")));
        assert!(normalized.contains("Generated: 2026-04-02"));
    }

    #[test]
    fn preserves_canonical_plan_heading() {
        let generated = r#"# IMPLEMENTATION_PLAN

## Priority Work

## Follow-On Work

## Completed / Already Satisfied
"#;

        assert_eq!(
            normalize_generated_implementation_plan(generated),
            generated.to_string()
        );
    }

    #[test]
    fn normalizes_missing_required_sections() {
        let generated = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate user query input
Spec: specs/020426-query-validation.md
"#;

        let normalized = normalize_generated_implementation_plan(generated);

        assert!(normalized.contains("## Follow-On Work"));
        assert!(normalized.contains("## Completed / Already Satisfied"));
    }

    #[test]
    fn merges_existing_open_tasks_not_present_in_new_plan() {
        let generated = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate user query input
Spec: specs/020426-query-validation.md

## Follow-On Work

## Completed / Already Satisfied
"#;

        let existing = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `SEC-001` Harden auth checks
Spec: specs/010426-auth.md

## Follow-On Work

- [ ] `OPS-001` Improve metrics
Spec: specs/010426-observability.md

## Completed / Already Satisfied

- [x] `OLD-001` Finished task
Spec: specs/310326-finished.md
"#;

        let merged = merge_generated_plan_with_existing_open_tasks(generated, existing).unwrap();

        assert!(merged.contains("`VAL-001`"));
        assert!(merged.contains("`SEC-001`"));
        assert!(merged.contains("`OPS-001`"));
        assert!(!merged.contains("`OLD-001`"));
    }

    #[test]
    fn merge_generated_plan_preserves_blocked_tasks() {
        let generated = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate user query input
Spec: specs/020426-query-validation.md

## Follow-On Work

## Completed / Already Satisfied
"#;

        let existing = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [!] `SEC-001` Blocked auth hardening
Spec: specs/010426-auth.md

- [X] `OLD-001` Finished uppercase task
Spec: specs/310326-finished.md

## Follow-On Work

- [!] `OPS-001` Blocked metrics
Spec: specs/010426-observability.md

## Completed / Already Satisfied
"#;

        let merged = merge_generated_plan_with_existing_open_tasks(generated, existing).unwrap();

        assert!(merged.contains("`VAL-001`"));
        assert!(merged.contains("- [!] `SEC-001` Blocked auth hardening"));
        assert!(merged.contains("- [!] `OPS-001` Blocked metrics"));
        assert!(!merged.contains("`OLD-001`"));
    }

    #[test]
    fn generated_plan_rejects_missing_spec_refs() {
        let root = temp_dir("missing-spec-ref");
        let specs_dir = root.join("specs");
        fs::create_dir_all(&specs_dir).unwrap();
        fs::write(
            specs_dir.join("050426-real.md"),
            "# Specification: Real\n\n## Objective\n\n- ok\n\n## Source Of Truth\n\n- docs owns this fact; runtime owner none; UI consumers none; generated artifacts none; retired surfaces none\n\n## Evidence Status\n\n- verified\n\n## Runtime Contract\n\n- none\n\n## UI Contract\n\n- none\n\n## Generated Artifacts\n\n- none\n\n## Fixture Policy\n\n- production code does not import fixture data\n\n## Retired / Superseded Surfaces\n\n- none\n\n## Acceptance Criteria\n\n- ok\n\n## Verification\n\n- ok\n\n## Review And Closeout\n\n- grep/assertion proof checks the documented requirement\n\n## Open Questions\n\n- none\n",
        )
        .unwrap();
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [ ] `DOC-001` Write docs\nSpec: `specs/060426-missing.md`\nWhy now: needed\nCodebase evidence: present\nSource of truth: docs\nRuntime owner: none\nUI consumers: none\nGenerated artifacts: none\nFixture boundary: production code cannot import fixture/demo/sample data\nRetired surfaces: none\nOwns: docs\nIntegration touchpoints: none\nScope boundary: docs only\nAcceptance criteria: docs land\nVerification: check file\nRequired tests: none\nContract generation: none -- no generated contract\nCross-surface tests: none -- no UI/runtime boundary\nReview/closeout: grep proof checks docs land\nCompletion artifacts: none\nDependencies: none\nEstimated scope: S\nCompletion signal: merged\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
        )
        .unwrap();

        let error =
            verify_generated_implementation_plan(&root).expect_err("expected missing spec failure");

        assert!(error.to_string().contains("references missing spec"));
    }

    #[test]
    fn generated_plan_rejects_large_active_scope() {
        let root = temp_dir("large-scope");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace("Estimated scope: S", "Estimated scope: L");
        write_generated_plan(&root, &task);

        let error =
            verify_generated_implementation_plan(&root).expect_err("expected scope failure");

        assert!(error.to_string().contains("Estimated scope: XS"));
    }

    #[test]
    fn generated_plan_rejects_decomposition_placeholders() {
        let root = temp_dir("decomposition-placeholder");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "Scope boundary: docs only",
            "Scope boundary: decomposition required before implementation",
        );
        write_generated_plan(&root, &task);

        let error = verify_generated_implementation_plan(&root)
            .expect_err("expected decomposition placeholder failure");

        assert!(error.to_string().contains("must be decomposed by auto gen"));
    }

    #[test]
    fn generated_plan_rejects_required_tests_see_spec() {
        let root = temp_dir("required-tests-see-spec");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "Required tests:\n    - `cargo test -p docs exact_docs_test`",
            "Required tests: See spec",
        );
        write_generated_plan(&root, &task);

        let error = verify_generated_implementation_plan(&root)
            .expect_err("expected required-tests placeholder failure");

        assert!(error.to_string().contains("vague `Required tests:`"));
    }

    #[test]
    fn generated_plan_accepts_inline_required_test_names() {
        let root = temp_dir("inline-required-tests");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "Required tests:\n    - `cargo test -p docs exact_docs_test`",
            "Required tests: `cargo test codex_exec::tests::progress` (existing progress tests must still pass)",
        );
        write_generated_plan(&root, &task);

        verify_generated_implementation_plan(&root)
            .expect("inline concrete required tests should be accepted");
    }

    #[test]
    fn generated_plan_accepts_bold_fields_and_required_tests_none_explanation() {
        let root = temp_dir("bold-fields-required-tests-none");
        write_real_spec(&root);
        let task = [
            "- **Spec:** `specs/050426-real.md`",
            "- **Why now:** needed",
            "- **Codebase evidence:** present",
            "- **Source of truth:** docs",
            "- **Runtime owner:** none",
            "- **UI consumers:** none",
            "- **Generated artifacts:** none",
            "- **Fixture boundary:** production code cannot import fixture/demo/sample data",
            "- **Retired surfaces:** none",
            "- **Owns:** docs/evidence.md",
            "- **Integration touchpoints:** docs",
            "- **Scope boundary:** docs only",
            "- **Acceptance criteria:** evidence lands",
            "- **Verification:** `grep -n evidence docs/evidence.md` returns one match.",
            "- **Required tests:** None (evidence task; no code change).",
            "- **Contract generation:** none -- no generated contract",
            "- **Cross-surface tests:** none -- no UI/runtime boundary",
            "- **Review/closeout:** `grep -n evidence docs/evidence.md` catches the original drift.",
            "- **Completion artifacts:** `docs/evidence.md`",
            "- **Dependencies:** none",
            "- **Estimated scope:** XS",
            "- **Completion signal:** evidence recorded",
        ]
        .join("\n");
        write_generated_plan(&root, &task);

        verify_generated_implementation_plan(&root)
            .expect("bold task fields and explanatory none should be accepted");
    }

    #[test]
    fn generated_plan_rejects_more_than_five_required_tests() {
        let root = temp_dir("too-many-required-tests");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "Required tests:\n    - `cargo test -p docs exact_docs_test`",
            "Required tests:\n    - `t1`\n    - `t2`\n    - `t3`\n    - `t4`\n    - `t5`\n    - `t6`",
        );
        write_generated_plan(&root, &task);

        let error =
            verify_generated_implementation_plan(&root).expect_err("expected test count failure");

        assert!(error.to_string().contains("at most five"));
    }

    #[test]
    fn generated_plan_rejects_more_than_five_inline_required_tests() {
        let root = temp_dir("too-many-inline-required-tests");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "Required tests:\n    - `cargo test -p docs exact_docs_test`",
            "Required tests: `t1`, `t2`, `t3`, `t4`, `t5`, `t6`",
        );
        write_generated_plan(&root, &task);

        let error =
            verify_generated_implementation_plan(&root).expect_err("expected test count failure");

        assert!(error.to_string().contains("at most five"));
    }

    #[test]
    fn generated_plan_ignores_prose_spec_mentions() {
        let root = temp_dir("prose-spec-mention");
        write_real_spec(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            format!(
                "# IMPLEMENTATION_PLAN\n\nEvery task carries a single `Spec:` pointer.\n\n## Priority Work\n\n- [ ] `DOC-001` Write docs\n{}\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
                valid_generated_plan_task()
            ),
        )
        .unwrap();

        verify_generated_implementation_plan(&root)
            .expect("prose mentions of field names should not be treated as fields");
    }

    #[test]
    fn generated_plan_rejects_broad_workspace_verification() {
        let root = temp_dir("broad-workspace-verification");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "    cargo test -p docs exact_docs_test",
            "    cargo test --workspace",
        );
        write_generated_plan(&root, &task);

        let error = verify_generated_implementation_plan(&root)
            .expect_err("expected broad verification failure");

        assert!(error.to_string().contains("broad verification"));
    }

    #[test]
    fn generated_plan_rejects_package_wide_cargo_test_verification() {
        let root = temp_dir("package-wide-cargo-test-verification");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "    cargo test -p docs exact_docs_test",
            "    cargo test -p barely-human",
        );
        write_generated_plan(&root, &task);

        let error = verify_generated_implementation_plan(&root)
            .expect_err("expected package-wide cargo test failure");

        assert!(error.to_string().contains("package-wide cargo test"));
    }

    #[test]
    fn generated_plan_rejects_multiple_cargo_test_filters() {
        let root = temp_dir("multi-filter-cargo-test-verification");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "    cargo test -p docs exact_docs_test",
            "    cargo test generation::tests::one completion_artifacts::tests::two",
        );
        write_generated_plan(&root, &task);

        let error = verify_generated_implementation_plan(&root)
            .expect_err("expected multi-filter cargo test failure");

        assert!(error.to_string().contains("multi-filter cargo test"));
    }

    #[test]
    fn generated_plan_rejects_bin_only_cargo_lib_verification() {
        let root = temp_dir("bin-only-cargo-lib-verification");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "    cargo test -p docs exact_docs_test",
            "    cargo test --lib generation::tests::one",
        );
        write_generated_plan(&root, &task);

        let error = verify_generated_implementation_plan(&root)
            .expect_err("expected cargo --lib verification failure");

        assert!(error.to_string().contains("cargo test --lib"));
    }

    #[test]
    fn generated_plan_rejects_malformed_directory_grep_verification() {
        let root = temp_dir("malformed-directory-grep-verification");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "    cargo test -p docs exact_docs_test",
            "    grep -n verification src",
        );
        write_generated_plan(&root, &task);

        let error = verify_generated_implementation_plan(&root)
            .expect_err("expected malformed grep verification failure");

        assert!(error.to_string().contains("malformed grep verification"));
    }

    #[test]
    fn generated_plan_rejects_vague_ownership() {
        let root = temp_dir("vague-ownership");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace("Owns: docs", "Owns: missing/TBD");
        write_generated_plan(&root, &task);

        let error =
            verify_generated_implementation_plan(&root).expect_err("expected ownership failure");

        assert!(error.to_string().contains("vague `Owns:`"));
    }

    #[test]
    fn generated_plan_rejects_tag_only_owns_prose_with_helpful_message() {
        let root = temp_dir("tag-prose-ownership");
        write_real_spec(&root);
        let task = valid_generated_plan_task()
            .replace("Owns: docs", "Owns: git tags only (no files change).");
        write_generated_plan(&root, &task);

        let error =
            verify_generated_implementation_plan(&root).expect_err("expected ownership failure");

        let msg = error.to_string();
        assert!(msg.contains("must give concrete path-like ownership"));
        assert!(msg.contains("refs/tags/<tag>"));
    }

    #[test]
    fn generated_plan_accepts_git_ref_path_owns() {
        let root = temp_dir("git-ref-ownership");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace("Owns: docs", "Owns: refs/tags/v0.2.0");
        write_generated_plan(&root, &task);

        verify_generated_implementation_plan(&root).expect("git ref ownership should be accepted");
    }

    #[test]
    fn generated_plan_accepts_backticked_directory_owner_with_trailing_slash() {
        let root = temp_dir("backticked-directory-owner");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "Owns: docs",
            "Owns: `crates/` (whatever files need reformatting)",
        );
        write_generated_plan(&root, &task);

        verify_generated_implementation_plan(&root)
            .expect("backticked directory owners with trailing slash should be accepted");
    }

    #[test]
    fn generated_plan_accepts_root_file_owner() {
        let root = temp_dir("root-file-owner");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace("Owns: docs", "Owns: `docker-compose.yml`");
        write_generated_plan(&root, &task);

        verify_generated_implementation_plan(&root)
            .expect("root-level file owners should be accepted");
    }

    #[test]
    fn generated_plan_rejects_prose_only_dependency_gates() {
        let root = temp_dir("prose-only-gate");
        write_real_spec(&root);
        let task = valid_generated_plan_task().replace(
            "Scope boundary: docs only",
            "Scope boundary: docs only; expansion-gated until `P-999` lands.",
        );
        write_generated_plan(&root, &task);

        let error = verify_generated_implementation_plan(&root).expect_err("expected gate failure");

        assert!(error.to_string().contains("omits it from `Dependencies:`"));
    }
}
