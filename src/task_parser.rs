#![allow(dead_code)]

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskStatus {
    Pending,
    Partial,
    Blocked,
    Done,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlanTask {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) status: TaskStatus,
    pub(crate) markdown: String,
    pub(crate) body: String,
    pub(crate) dependencies: Vec<String>,
    pub(crate) verification_text: Option<String>,
    pub(crate) completion_artifacts: Vec<String>,
    pub(crate) completion_path_target: Option<String>,
}

pub(crate) fn parse_tasks(plan: &str) -> Vec<PlanTask> {
    let mut tasks = Vec::new();
    let mut current_lines = Vec::<String>::new();

    for line in plan.lines() {
        if parse_task_header(line).is_some() {
            if let Some(task) = finalize_task(&current_lines) {
                tasks.push(task);
            }
            current_lines = vec![line.trim_end().to_string()];
            continue;
        }

        if !current_lines.is_empty() {
            current_lines.push(line.trim_end().to_string());
        }
    }

    if let Some(task) = finalize_task(&current_lines) {
        tasks.push(task);
    }

    tasks
}

fn finalize_task(lines: &[String]) -> Option<PlanTask> {
    let header = lines.first()?;
    let (status, id, title) = parse_task_header(header)?;
    let markdown = lines.join("\n").trim_end().to_string();
    let body = lines
        .iter()
        .skip(1)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string();

    Some(PlanTask {
        id,
        title,
        status,
        dependencies: parse_task_dependencies(&markdown),
        verification_text: task_field_body_until_any(
            &markdown,
            "Verification:",
            TASK_FIELD_BOUNDARIES,
        )
        .map(|value| value.trim_end().to_string())
        .filter(|value| !value.trim().is_empty()),
        completion_artifacts: parse_completion_artifacts(&markdown),
        completion_path_target: parse_task_completion_path(&markdown),
        markdown,
        body,
    })
}

fn parse_task_header(line: &str) -> Option<(TaskStatus, String, String)> {
    let trimmed = line.trim_start();
    let (status, rest) = if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
        (TaskStatus::Pending, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [~] ") {
        (TaskStatus::Partial, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [!] ") {
        (TaskStatus::Blocked, rest)
    } else if let Some(rest) = trimmed
        .strip_prefix("- [x] ")
        .or_else(|| trimmed.strip_prefix("- [X] "))
    {
        (TaskStatus::Done, rest)
    } else {
        return None;
    };

    let rest = rest.strip_prefix('`')?;
    let tick = rest.find('`')?;
    let id = rest[..tick].trim().to_string();
    if id.is_empty() {
        return None;
    }
    let title = rest[tick + 1..].trim().to_string();
    Some((status, id, title))
}

fn parse_task_dependencies(markdown: &str) -> Vec<String> {
    let Some(body) = task_field_body_until_any(markdown, "Dependencies:", TASK_FIELD_BOUNDARIES)
    else {
        return Vec::new();
    };

    let first_meaningful = body
        .lines()
        .map(strip_list_bullet)
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_ascii_lowercase);
    if first_meaningful
        .as_deref()
        .is_some_and(|line| line.starts_with("none"))
    {
        return Vec::new();
    }

    dedup_task_refs(
        body.lines()
            .flat_map(task_dependency_refs_from_line)
            .collect::<Vec<_>>(),
    )
}

fn task_dependency_refs_from_line(line: &str) -> Vec<String> {
    let trimmed = strip_list_bullet(line).trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("parallelism note:")
        || lower.starts_with("parallel with")
        || lower.starts_with("can run in parallel")
        || lower.starts_with("runs in parallel")
    {
        return Vec::new();
    }

    let without_parens = strip_parenthetical_groups(trimmed);
    let narrative_cut = without_parens.split(['.', ';']).next().unwrap_or("").trim();
    collect_task_refs(narrative_cut)
}

fn parse_completion_artifacts(markdown: &str) -> Vec<String> {
    let Some(body) =
        task_field_body_until_any(markdown, "Completion artifacts:", TASK_FIELD_BOUNDARIES)
    else {
        return Vec::new();
    };

    let first_meaningful = body
        .lines()
        .map(strip_list_bullet)
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_ascii_lowercase);
    if first_meaningful
        .as_deref()
        .is_some_and(|line| line.starts_with("none"))
    {
        return Vec::new();
    }

    body.lines().flat_map(artifact_paths_from_line).collect()
}

fn parse_task_completion_path(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if let Some(start) = lower.find("completion path:") {
            return collect_task_refs(line[start + "completion path:".len()..].trim())
                .into_iter()
                .next();
        }

        if let Some(start) = lower.find("completion path is") {
            return collect_task_refs(line[start + "completion path is".len()..].trim())
                .into_iter()
                .next();
        }

        if let Some(end) = lower.find("for the completion path") {
            return collect_task_refs(line[..end].trim()).into_iter().last();
        }

        None
    })
}

const TASK_FIELD_BOUNDARIES: &[&str] = &[
    "Spec:",
    "Why now:",
    "Codebase evidence:",
    "Owns:",
    "Integration touchpoints:",
    "Scope boundary:",
    "Acceptance criteria:",
    "Verification:",
    "Required tests:",
    "Completion artifacts:",
    "Dependencies:",
    "Estimated scope:",
    "Completion signal:",
    "Status:",
];

fn task_field_body_until_any(markdown: &str, field: &str, next_fields: &[&str]) -> Option<String> {
    let mut collecting = false;
    let mut body = Vec::new();
    for line in markdown.lines() {
        let unbulleted = strip_list_bullet(line);
        if let Some(rest) = unbulleted.strip_prefix(field) {
            collecting = true;
            if !rest.trim().is_empty() {
                body.push(rest.trim().to_string());
            }
            continue;
        }
        if collecting
            && next_fields
                .iter()
                .filter(|next_field| **next_field != field)
                .any(|next_field| unbulleted.starts_with(next_field))
        {
            break;
        }
        if collecting {
            body.push(line.to_string());
        }
    }
    collecting.then(|| body.join("\n"))
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

fn strip_parenthetical_groups(text: &str) -> String {
    let mut depth = 0usize;
    let mut rendered = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => rendered.push(ch),
            _ => {}
        }
    }
    rendered
}

fn collect_task_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else {
            break;
        };
        let candidate = &rest[..end];
        if candidate
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            refs.push(candidate.to_string());
        }
        rest = &rest[end + 1..];
    }
    dedup_task_refs(refs)
}

fn dedup_task_refs(refs: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for task_id in refs {
        if seen.insert(task_id.clone()) {
            ordered.push(task_id);
        }
    }
    ordered
}

fn artifact_paths_from_line(line: &str) -> Vec<String> {
    let trimmed = strip_list_bullet(line).trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut paths = Vec::new();
    let mut rest = trimmed;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else {
            break;
        };
        let candidate = rest[..end].trim();
        if looks_like_repo_relative_path(candidate) {
            paths.push(candidate.to_string());
        }
        rest = &rest[end + 1..];
    }
    if !paths.is_empty() {
        return paths;
    }

    let candidate = trimmed
        .split(" -- ")
        .next()
        .unwrap_or(trimmed)
        .split(" — ")
        .next()
        .unwrap_or(trimmed)
        .trim();
    if looks_like_repo_relative_path(candidate) {
        paths.push(candidate.to_string());
    }
    paths
}

fn looks_like_repo_relative_path(candidate: &str) -> bool {
    !candidate.is_empty()
        && !candidate.starts_with('/')
        && (candidate.contains('/')
            || candidate.starts_with('.')
            || candidate.ends_with(".md")
            || candidate.ends_with(".json")
            || candidate.ends_with(".txt"))
}

#[cfg(test)]
mod tests {
    use super::{parse_tasks, TaskStatus};

    #[test]
    fn parses_all_plan_statuses_and_fields() {
        let plan = r#"
# Queue

- [ ] `AD-001` Pending parser inventory

  Spec: `specs/230426-shared-task-parser-and-blocked-preservation.md`
  Verification: `cargo test task_parser::tests::parses_all_plan_statuses_and_fields`
  Completion artifacts: `docs/decisions/parser.md`
  Dependencies: none

- [~] `AD-002` Historical parser gap

  Completion path: `AD-006`
  Verification:
  - `cargo test task_parser::tests::dependencies_none_and_multiline_notes_are_stable`
  - manually inspect existing parser adapters
  Completion artifacts:
  - `src/task_parser.rs`
  Dependencies: `AD-001`

- [!] `AD-003` Blocked parser migration
  Dependencies: `AD-002`

- [x] `AD-004` Lowercase done
  Dependencies: `AD-002`

- [X] `AD-005` Uppercase done
  Dependencies: `AD-002`
"#;

        let tasks = parse_tasks(plan);
        assert_eq!(tasks.len(), 5);

        assert_eq!(tasks[0].id, "AD-001");
        assert_eq!(tasks[0].title, "Pending parser inventory");
        assert_eq!(tasks[0].status, TaskStatus::Pending);
        assert!(tasks[0]
            .markdown
            .starts_with("- [ ] `AD-001` Pending parser inventory"));
        assert!(tasks[0]
            .body
            .contains("Spec: `specs/230426-shared-task-parser-and-blocked-preservation.md`"));
        assert!(tasks[0].dependencies.is_empty());
        assert_eq!(
            tasks[0].verification_text.as_deref(),
            Some("`cargo test task_parser::tests::parses_all_plan_statuses_and_fields`")
        );
        assert_eq!(
            tasks[0].completion_artifacts,
            vec!["docs/decisions/parser.md"]
        );

        assert_eq!(tasks[1].status, TaskStatus::Partial);
        assert_eq!(tasks[1].dependencies, vec!["AD-001"]);
        assert_eq!(tasks[1].completion_path_target.as_deref(), Some("AD-006"));
        assert_eq!(
            tasks[1].verification_text.as_deref(),
            Some(
                "  - `cargo test task_parser::tests::dependencies_none_and_multiline_notes_are_stable`\n  - manually inspect existing parser adapters"
            )
        );
        assert_eq!(tasks[1].completion_artifacts, vec!["src/task_parser.rs"]);

        assert_eq!(tasks[2].status, TaskStatus::Blocked);
        assert_eq!(tasks[3].status, TaskStatus::Done);
        assert_eq!(tasks[4].status, TaskStatus::Done);
    }

    #[test]
    fn dependencies_none_and_multiline_notes_are_stable() {
        let plan = r#"
- [ ] `AD-001` No dependencies with narrative refs
  Dependencies: none (parallel with `AD-999`)
  Estimated scope: S

- [ ] `AD-002` Multiline dependencies
  Dependencies:
  - `AD-001` (parallel with `AD-999`)
  - `AD-003`; blocked by `AD-004` in an older plan.
  - Parallelism note: can run beside `AD-005`.
  Estimated scope: M
"#;

        let tasks = parse_tasks(plan);
        assert!(tasks[0].dependencies.is_empty());
        assert_eq!(tasks[1].dependencies, vec!["AD-001", "AD-003"]);
    }

    #[test]
    fn completion_path_placeholders_are_metadata_not_ready_work() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Completion path: `TASK-010`.
  Dependencies: none
  Estimated scope: S

- [ ] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
"#;

        let tasks = parse_tasks(plan);
        assert_eq!(tasks[0].status, TaskStatus::Partial);
        assert_eq!(tasks[0].completion_path_target.as_deref(), Some("TASK-010"));
        assert!(tasks[0].dependencies.is_empty());
        assert_eq!(tasks[1].status, TaskStatus::Pending);
        assert_eq!(tasks[1].completion_path_target, None);
    }
}
