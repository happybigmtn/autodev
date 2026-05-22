//! Symphony task model, plan parsing, and issue-body rendering.

use std::path::Path;

use crate::symphony_command::linear::LinearIssue;
use crate::symphony_command::workflow::resolve_base_branch;
use crate::task_parser::{
    parse_task_header as parse_shared_task_header, parse_tasks as parse_shared_tasks,
    TaskStatus as SharedTaskStatus,
};
use crate::util::repo_name;

const TASK_SENTINEL_PREFIX: &str = "<!-- auto-symphony:";
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TaskStatus {
    Pending,
    Blocked,
    Partial,
    Done,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SymphonyTask {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) status: TaskStatus,
    pub(crate) dependencies: Vec<String>,
    pub(crate) markdown: String,
}

pub(crate) fn render_sync_task_digest(task: &SymphonyTask) -> String {
    let why_now = single_line_excerpt(task_field_line_value(&task.markdown, "Why now:"), 220);
    let owns = single_line_excerpt(
        task_field_body(&task.markdown, "Owns:", "Integration touchpoints:"),
        220,
    );
    let touchpoints = single_line_excerpt(
        task_field_body(
            &task.markdown,
            "Integration touchpoints:",
            "Scope boundary:",
        ),
        220,
    );
    let scope_boundary = single_line_excerpt(
        task_field_body(&task.markdown, "Scope boundary:", "Acceptance criteria:"),
        220,
    );
    let dependencies = if task.dependencies.is_empty() {
        "none".to_string()
    } else {
        task.dependencies
            .iter()
            .map(|dependency| format!("`{dependency}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "- `{}` {}\n  Explicit dependencies: {}\n  Why now: {}\n  Owns: {}\n  Integration touchpoints: {}\n  Scope boundary: {}",
        task.id, task.title, dependencies, why_now, owns, touchpoints, scope_boundary
    )
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

fn task_field_line_value(markdown: &str, field: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        strip_list_bullet(line)
            .strip_prefix(field)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
    })
}

fn single_line_excerpt(value: Option<String>, max_chars: usize) -> String {
    let mut normalized = value
        .unwrap_or_else(|| "none".to_string())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let normalized_chars = normalized.chars().count();
    if normalized_chars > max_chars {
        let keep_chars = max_chars.saturating_sub(3);
        if keep_chars == 0 {
            return "...".chars().take(max_chars).collect();
        }
        let truncate_at = normalized
            .char_indices()
            .nth(keep_chars)
            .map(|(idx, _)| idx)
            .unwrap_or(normalized.len());
        normalized.truncate(truncate_at);
        normalized.push_str("...");
    }
    normalized
}

fn task_field_excerpt(markdown: &str, field: &str, next_field: &str, max_chars: usize) -> String {
    single_line_excerpt(task_field_body(markdown, field, next_field), max_chars)
}

fn render_issue_task_brief(task: &SymphonyTask) -> String {
    let dependencies = if task.dependencies.is_empty() {
        "none".to_string()
    } else {
        task.dependencies
            .iter()
            .map(|dependency| format!("`{dependency}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let why_now = single_line_excerpt(task_field_line_value(&task.markdown, "Why now:"), 260);
    let owns = task_field_excerpt(&task.markdown, "Owns:", "Integration touchpoints:", 260);
    let touchpoints = task_field_excerpt(
        &task.markdown,
        "Integration touchpoints:",
        "Scope boundary:",
        260,
    );
    let scope_boundary = task_field_excerpt(
        &task.markdown,
        "Scope boundary:",
        "Acceptance criteria:",
        260,
    );
    let acceptance =
        task_field_excerpt(&task.markdown, "Acceptance criteria:", "Verification:", 260);
    let verification = task_field_excerpt(&task.markdown, "Verification:", "Required tests:", 260);
    let completion_artifacts = task_field_excerpt(
        &task.markdown,
        "Completion artifacts:",
        "Dependencies:",
        260,
    );
    let completion_signal = single_line_excerpt(
        task_field_line_value(&task.markdown, "Completion signal:"),
        260,
    );
    format!(
        "## Task brief\n\
- Explicit dependencies: {dependencies}\n\
- Why now: {why_now}\n\
- Owns: {owns}\n\
- Integration touchpoints: {touchpoints}\n\
- Scope boundary: {scope_boundary}\n\
- Acceptance criteria: {acceptance}\n\
- Verification: {verification}\n\
- Completion artifacts: {completion_artifacts}\n\
- Completion signal: {completion_signal}\n\
- Landing contract: complete only `{task_id}` in this workspace. If a small adjacent integration edit is required, keep it minimal and record it under `Scope exceptions:` in `REVIEW.md`.\n",
        task_id = task.id
    )
}

pub(crate) fn parse_tasks(plan: &str) -> Vec<SymphonyTask> {
    parse_shared_tasks(plan)
        .into_iter()
        .map(|task| SymphonyTask {
            id: task.id,
            title: task.title,
            status: symphony_task_status(task.status),
            dependencies: task.dependencies,
            markdown: task.markdown,
        })
        .collect()
}

fn symphony_task_status(status: SharedTaskStatus) -> TaskStatus {
    match status {
        SharedTaskStatus::Pending => TaskStatus::Pending,
        SharedTaskStatus::Blocked => TaskStatus::Blocked,
        SharedTaskStatus::Partial => TaskStatus::Partial,
        SharedTaskStatus::Done => TaskStatus::Done,
    }
}

pub(crate) fn parse_task_header(line: &str) -> Option<(TaskStatus, String, String)> {
    let (status, id, title) = parse_shared_task_header(line)?;
    Some((symphony_task_status(status), id, title))
}

fn task_field_body(markdown: &str, field: &str, next_field: &str) -> Option<String> {
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
        if collecting && unbulleted.starts_with(next_field) {
            break;
        }
        if collecting {
            body.push(line.to_string());
        }
    }
    collecting.then(|| body.join("\n"))
}

pub(crate) fn render_issue_title(task: &SymphonyTask) -> String {
    format!("[{}] {}", task.id, task.title)
}

pub(crate) fn task_contract_fingerprint(task: &SymphonyTask) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    task.id.hash(&mut hasher);
    task.title.hash(&mut hasher);
    task.markdown.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn render_issue_description(repo_root: &Path, task: &SymphonyTask) -> String {
    let base_branch = resolve_base_branch(repo_root, None).unwrap_or_else(|_| "main".to_string());
    let task_brief = render_issue_task_brief(task);
    let fingerprint = task_contract_fingerprint(task);
    format!(
        "{TASK_SENTINEL_PREFIX} repo={repo} task_id={task_id} base_branch={base_branch} fingerprint={fingerprint:016x} -->\n\n\
Repository: `{repo}`\n\
Task ID: `{task_id}`\n\
Base branch: `{base_branch}`\n\
Synced from: `{plan_path}`\n\n\
{task_brief}\n\
This issue is auto-generated from the repository implementation plan. Re-run `auto symphony sync` to refresh the source-of-truth task body.\n\n\
---\n\n{markdown}\n",
        repo = repo_name(repo_root),
        task_id = task.id,
        base_branch = base_branch,
        fingerprint = fingerprint,
        plan_path = repo_root.join("IMPLEMENTATION_PLAN.md").display(),
        task_brief = task_brief,
        markdown = task.markdown
    )
}

pub(crate) fn issue_task_id(issue: &LinearIssue) -> Option<String> {
    issue_task_id_from_description(&issue.description)
        .or_else(|| issue_task_id_from_title(&issue.title))
}

fn issue_task_id_from_description(description: &str) -> Option<String> {
    description
        .lines()
        .find(|line| line.starts_with(TASK_SENTINEL_PREFIX))
        .and_then(|line| {
            line.split_whitespace().find_map(|segment| {
                segment
                    .strip_prefix("task_id=")
                    .map(|value| value.trim_end_matches("-->").to_string())
            })
        })
}

fn issue_task_id_from_title(title: &str) -> Option<String> {
    let rest = title.strip_prefix('[')?;
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        issue_task_id_from_description, parse_tasks, render_issue_description, single_line_excerpt,
        SymphonyTask, TaskStatus,
    };

    #[test]
    fn parse_tasks_extracts_pending_items_and_dependencies() {
        let plan = r#"
- [ ] `P-018` First task
  Dependencies: `P-017B`
  Acceptance criteria:
    - something

- [!] `P-019` Blocked task
  Dependencies: `P-018`

- [x] `P-020` Done task

- [X] `P-021` Uppercase done task
"#;
        let tasks = parse_tasks(plan);
        assert_eq!(tasks.len(), 4);
        assert_eq!(tasks[0].id, "P-018");
        assert_eq!(tasks[0].status, TaskStatus::Pending);
        assert_eq!(tasks[0].dependencies, vec!["P-017B"]);
        assert_eq!(tasks[1].status, TaskStatus::Blocked);
        assert_eq!(tasks[2].status, TaskStatus::Done);
        assert_eq!(tasks[3].status, TaskStatus::Done);
    }

    #[test]
    fn parse_tasks_recognizes_partial_items() {
        let plan = r#"
- [~] `P-021` Landed but missing evidence
  Dependencies: `P-020`
"#;
        let tasks = parse_tasks(plan);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Partial);
        assert_eq!(tasks[0].dependencies, vec!["P-020"]);
    }

    #[test]
    fn parse_tasks_collects_multiline_and_external_dependencies() {
        let plan = r#"
- [ ] `P-043` Watcher memory lane
  Dependencies: `P-016` (post-turn bridge), `P-015J` (minimal observer actor
  identity path).
  External dependency: sibling Bitino global-room tranche (`GCRAPS-003` through `GCRAPS-006`) is signed off.
  Estimated scope: M
"#;
        let tasks = parse_tasks(plan);
        assert_eq!(
            tasks[0].dependencies,
            vec![
                "P-016".to_string(),
                "P-015J".to_string(),
                "GCRAPS-003".to_string(),
                "GCRAPS-006".to_string(),
            ]
        );
    }

    #[test]
    fn parse_tasks_treats_none_dependencies_as_empty() {
        let plan = r#"
- [ ] `WEB-HOUSE-AUDIT` Audit
  Dependencies: none (Wave 0 foundation; parallel with `WEB-CODEGEN-A`)
  Estimated scope: M
"#;
        let tasks = parse_tasks(plan);
        assert!(tasks[0].dependencies.is_empty());
    }

    #[test]
    fn parse_tasks_ignores_parallelism_notes_in_dependency_lines() {
        let plan = r#"
- [ ] `WEB-HOUSE-AUDIT` Foundation
  Dependencies: none
  Estimated scope: S

- [ ] `WEB-CHANNEL-COVERAGE` Coverage
  Dependencies: none
  Estimated scope: S

- [ ] `WEB-CLIENT-BUILD` Bundle
  Dependencies: `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE` (Wave 0 gate — finding #3; parallel with `WEB-CODEGEN-A` + `WEB-DESIGN-SYSTEM`)
  Estimated scope: M
"#;
        let tasks = parse_tasks(plan);
        assert_eq!(
            tasks[2].dependencies,
            vec![
                "WEB-HOUSE-AUDIT".to_string(),
                "WEB-CHANNEL-COVERAGE".to_string(),
            ]
        );
    }

    #[test]
    fn rendered_issue_description_carries_sentinel() {
        let repo_root = PathBuf::from("/tmp/autonomy");
        let task = SymphonyTask {
            id: "P-018".to_string(),
            title: "Loan widget".to_string(),
            status: TaskStatus::Pending,
            dependencies: vec!["P-017B".to_string()],
            markdown: r#"- [ ] `P-018` Loan widget
  Why now: Keep the borrowing flow unblocked.
  Owns: `src/loan.rs`
  Integration touchpoints: `src/app.rs`
  Scope boundary: Does not change repayment rules.
  Acceptance criteria:
    - Loan widget renders the approved state.
  Verification:
    cargo test -p autonomy loan_widget
  Required tests:
    - `loan_widget`
  Completion signal: Widget proof is green."#
                .to_string(),
        };
        let description = render_issue_description(&repo_root, &task);
        assert!(description.contains("task_id=P-018"));
        assert!(description.contains("## Task brief"));
        assert!(description.contains("Owns: `src/loan.rs`"));
        assert!(description.contains("Landing contract: complete only `P-018`"));
        assert_eq!(
            issue_task_id_from_description(&description),
            Some("P-018".to_string())
        );
    }

    #[test]
    fn single_line_excerpt_truncates_on_utf8_boundaries() {
        assert_eq!(
            single_line_excerpt(Some("hello élan world".to_string()), 10),
            "hello é..."
        );
    }

    #[test]
    fn single_line_excerpt_handles_tiny_limits() {
        assert_eq!(single_line_excerpt(Some("abcdef".to_string()), 0), "");
        assert_eq!(single_line_excerpt(Some("abcdef".to_string()), 2), "..");
        assert_eq!(single_line_excerpt(Some("abcdef".to_string()), 3), "...");
    }
}
