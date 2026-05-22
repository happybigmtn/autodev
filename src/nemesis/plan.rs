//! Markdown implementation-plan parsing and merging for `auto nemesis`.
//!
//! Parses `## Priority Work` / `## Follow-On Work` / `## Completed` sections
//! into task blocks, syncs the nemesis spec into root `specs/`, and appends new
//! unchecked `NEM-` tasks into the root `IMPLEMENTATION_PLAN.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};

use crate::util::atomic_write;

pub(crate) const EMPTY_PLAN: &str =
    "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n";
const REQUIRED_PLAN_SECTIONS: [&str; 3] = [
    "## Priority Work",
    "## Follow-On Work",
    "## Completed / Already Satisfied",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlanSection {
    Priority,
    FollowOn,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlanTaskBlock {
    section: PlanSection,
    task_id: String,
    checked: bool,
    markdown: String,
}

pub(crate) fn load_unchecked_nemesis_task_ids(plan_path: &Path) -> Result<BTreeSet<String>> {
    unchecked_nemesis_task_ids(
        &fs::read_to_string(plan_path)
            .with_context(|| format!("failed to read {}", plan_path.display()))?,
    )
}

pub(crate) fn unchecked_nemesis_task_ids(markdown: &str) -> Result<BTreeSet<String>> {
    Ok(extract_plan_task_blocks(markdown)?
        .into_iter()
        .filter(|block| !block.checked)
        .filter(|block| block.task_id.starts_with("NEM-"))
        .map(|block| block.task_id)
        .collect())
}

pub(crate) fn sync_nemesis_spec_to_root(repo_root: &Path, spec_path: &Path) -> Result<PathBuf> {
    let root_specs_dir = repo_root.join("specs");
    fs::create_dir_all(&root_specs_dir)
        .with_context(|| format!("failed to create {}", root_specs_dir.display()))?;
    let destination = next_nemesis_spec_destination(&root_specs_dir, spec_path, Local::now());

    fs::copy(spec_path, &destination).with_context(|| {
        format!(
            "failed to copy {} -> {}",
            spec_path.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

fn next_nemesis_spec_destination(
    root_specs_dir: &Path,
    spec_path: &Path,
    timestamp: DateTime<Local>,
) -> PathBuf {
    let date_prefix = timestamp.format("%d%m%y-%H%M%S").to_string();
    let slug = spec_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("nemesis-audit");
    let extension = spec_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("md");

    let mut counter = 1usize;
    loop {
        let candidate = if counter == 1 {
            root_specs_dir.join(format!("{date_prefix}-{slug}.{extension}"))
        } else {
            root_specs_dir.join(format!("{date_prefix}-{slug}-{counter}.{extension}"))
        };
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

pub(crate) fn append_nemesis_plan_to_root(
    repo_root: &Path,
    nemesis_plan_path: &Path,
) -> Result<usize> {
    let root_plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    let existing = if root_plan_path.exists() {
        fs::read_to_string(&root_plan_path)
            .with_context(|| format!("failed to read {}", root_plan_path.display()))?
    } else {
        EMPTY_PLAN.to_string()
    };
    let nemesis_plan = fs::read_to_string(nemesis_plan_path)
        .with_context(|| format!("failed to read {}", nemesis_plan_path.display()))?;

    let (merged, appended) = append_new_open_tasks(&existing, &nemesis_plan)?;
    atomic_write(&root_plan_path, merged.as_bytes())
        .with_context(|| format!("failed to write {}", root_plan_path.display()))?;
    Ok(appended)
}

fn append_new_open_tasks(existing: &str, nemesis_plan: &str) -> Result<(String, usize)> {
    let normalized_existing = normalize_root_plan(existing);
    let existing_blocks = extract_plan_task_blocks(&normalized_existing)?;
    let existing_ids = existing_blocks
        .iter()
        .map(|block| block.task_id.as_str())
        .collect::<BTreeSet<_>>();

    let new_blocks = extract_plan_task_blocks(nemesis_plan)?
        .into_iter()
        .filter(|block| !block.checked)
        .filter(|block| !existing_ids.contains(block.task_id.as_str()))
        .collect::<Vec<_>>();

    if new_blocks.is_empty() {
        return Ok((normalized_existing, 0));
    }

    let mut merged = normalized_existing;
    append_blocks_to_section(&mut merged, PlanSection::Priority, &new_blocks)?;
    append_blocks_to_section(&mut merged, PlanSection::FollowOn, &new_blocks)?;
    Ok((merged, new_blocks.len()))
}

fn normalize_root_plan(markdown: &str) -> String {
    if markdown.trim().is_empty() {
        return EMPTY_PLAN.to_string();
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

fn append_blocks_to_section(
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
        .with_context(|| format!("root plan is missing section `{section_header}`"))?;
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

fn extract_plan_task_blocks(markdown: &str) -> Result<Vec<PlanTaskBlock>> {
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
    let Some((checked, task_id, _title)) = parse_plan_task_header(&lines[0]) else {
        return Ok(None);
    };
    Ok(Some(PlanTaskBlock {
        section: section.unwrap_or(PlanSection::Priority),
        task_id,
        checked,
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

fn parse_plan_task_header(line: &str) -> Option<(bool, String, String)> {
    let trimmed = line.trim_start();
    let checked = if trimmed.starts_with("- [ ] ") {
        false
    } else if trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ") {
        true
    } else {
        return None;
    };
    let rest = trimmed[6..].trim_start();
    let rest = rest.strip_prefix('`')?;
    let tick = rest.find('`')?;
    let task_id = rest[..tick].trim().to_string();
    let title = rest[tick + 1..].trim().to_string();
    Some((checked, task_id, title))
}

#[cfg(test)]
mod tests {
    use super::{append_new_open_tasks, next_nemesis_spec_destination, unchecked_nemesis_task_ids};

    #[test]
    fn appends_only_new_unchecked_nemesis_tasks() {
        let existing = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate query
Spec: specs/020426-query.md

## Follow-On Work

## Completed / Already Satisfied
"#;

        let nemesis = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `NEM-001` Harden cross-surface invariant
Spec: specs/020426-nemesis-audit.md

- [ ] `VAL-001` Validate query
Spec: specs/020426-query.md

## Follow-On Work

- [ ] `NEM-002` Add state-sync regression coverage
Spec: specs/020426-nemesis-audit.md

## Completed / Already Satisfied

- [x] `NEM-003` Already satisfied
Spec: specs/020426-nemesis-audit.md
"#;

        let (merged, appended) = append_new_open_tasks(existing, nemesis).unwrap();
        assert_eq!(appended, 2);
        assert!(merged.contains("`NEM-001`"));
        assert!(merged.contains("`NEM-002`"));
        assert_eq!(merged.matches("`VAL-001`").count(), 1);
        assert!(!merged.contains("`NEM-003`"));
    }

    #[test]
    fn appends_nemesis_tasks_when_existing_plan_is_missing_sections() {
        let existing = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate query
Spec: specs/020426-query.md
"#;

        let nemesis = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `NEM-001` Harden cross-surface invariant
Spec: specs/020426-nemesis-audit.md

## Follow-On Work

- [ ] `NEM-002` Add state-sync regression coverage
Spec: specs/020426-nemesis-audit.md

## Completed / Already Satisfied
"#;

        let (merged, appended) = append_new_open_tasks(existing, nemesis).unwrap();
        assert_eq!(appended, 2);
        assert!(merged.contains("## Follow-On Work"));
        assert!(merged.contains("## Completed / Already Satisfied"));
        assert!(merged.contains("`NEM-001`"));
        assert!(merged.contains("`NEM-002`"));
    }

    #[test]
    fn unchecked_task_preflight_skips_satisfied_plans() {
        let unchecked = unchecked_nemesis_task_ids(
            r#"# IMPLEMENTATION_PLAN

## Priority Work

## Follow-On Work

## Completed / Already Satisfied

- [x] `NEM-001` Already done
Spec: nemesis/nemesis-audit.md
"#,
        )
        .expect("plan should parse");
        assert!(unchecked.is_empty());
    }

    #[test]
    fn spec_sync_destination_uses_time_and_collision_suffix() {
        use chrono::{Local, TimeZone};
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "autodev-nemesis-spec-destination-{}-{nonce}",
            std::process::id()
        ));
        let specs_dir = root.join("specs");
        fs::create_dir_all(&specs_dir).expect("failed to create specs dir");
        let spec_path = root.join("nemesis-audit.md");
        fs::write(&spec_path, "# Specification:\n").expect("failed to write spec");
        let timestamp = Local
            .with_ymd_and_hms(2026, 4, 5, 12, 34, 56)
            .single()
            .expect("timestamp should exist");

        let first = next_nemesis_spec_destination(&specs_dir, &spec_path, timestamp);
        assert!(first
            .file_name()
            .and_then(|value| value.to_str())
            .expect("file name should be utf-8")
            .starts_with("050426-123456-nemesis-audit"));
        fs::write(&first, "# existing\n").expect("failed to create existing collision file");
        let second = next_nemesis_spec_destination(&specs_dir, &spec_path, timestamp);
        assert_ne!(first, second);
        assert!(second
            .file_name()
            .and_then(|value| value.to_str())
            .expect("file name should be utf-8")
            .contains("-2."));

        fs::remove_dir_all(&root).expect("failed to remove temp dir");
    }
}
