//! `auto symphony sync`: reconcile the implementation plan against Linear issues.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;

use crate::completion_artifacts::{
    default_review_doc, inspect_task_completion_evidence, review_contains_task,
};
use crate::symphony_command::linear::{LinearGraphqlClient, LinearIssue};
use crate::symphony_command::planner::{determine_sync_plan, DeterminedSyncPlan};
use crate::symphony_command::render_issue_description;
use crate::symphony_command::task::{
    issue_task_id, parse_task_header, parse_tasks, render_issue_title, SymphonyTask, TaskStatus,
};
use crate::symphony_command::workflow::{resolve_project_slug, resolve_repo_root};
use crate::util::atomic_write;
use crate::SymphonySyncArgs;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CompletionArtifactSync {
    plan_text: String,
    marked_done: Vec<String>,
    local_gap_tasks: Vec<String>,
    review_backfilled: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompletedPlanIssueUpdate {
    issue_id: String,
    task_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CompletedPlanIssueSync {
    task_ids: Vec<String>,
}

pub(crate) async fn run_sync(args: SymphonySyncArgs) -> Result<()> {
    let repo_root = resolve_repo_root(args.repo_root)?;
    let project_slug = resolve_project_slug(&repo_root, args.project_slug.as_deref())?;
    let client = LinearGraphqlClient::from_env()?;
    let project = client.fetch_project(&project_slug).await?;
    let todo_state_id = project.state_id(&args.todo_state).ok_or_else(|| {
        anyhow!(
            "project `{}` does not expose state `{}`",
            project.slug,
            args.todo_state
        )
    })?;
    let terminal_state_names = project.terminal_state_names();
    let mut existing_issues = client.fetch_project_issues(&project.slug).await?;
    let plan_text = load_plan_text(&repo_root)?;
    let all_tasks = parse_tasks(&plan_text);
    let completed_plan_sync =
        reconcile_completed_plan_issues(&client, &all_tasks, &mut existing_issues).await?;
    let completion_sync = reconcile_completion_artifacts(
        &repo_root,
        &plan_text,
        &all_tasks,
        &existing_issues,
        &terminal_state_names,
    )?;
    let tasks = parse_tasks(&completion_sync.plan_text)
        .into_iter()
        .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Partial))
        .collect::<Vec<_>>();
    let planning = if args.no_ai_planner {
        DeterminedSyncPlan::fallback(&tasks)
    } else {
        match determine_sync_plan(
            &repo_root,
            &completion_sync.plan_text,
            &tasks,
            &args.codex_bin,
            &args.planner_model,
            &args.planner_reasoning_effort,
        )
        .await
        {
            Ok(plan) => plan,
            Err(err) => {
                eprintln!(
                    "warning: Codex sync planner failed; falling back to deterministic scheduling: {err:#}"
                );
                DeterminedSyncPlan::fallback(&tasks)
            }
        }
    };
    let mut issues_by_task_id = existing_issues
        .into_iter()
        .filter_map(|issue| issue_task_id(&issue).map(|task_id| (task_id, issue)))
        .collect::<HashMap<_, _>>();
    let mut synced_issue_ids = HashMap::new();
    let mut created = 0usize;
    let mut updated = 0usize;
    let mut deleted_relations = 0usize;
    let mut created_relations = 0usize;

    for task in &tasks {
        let title = render_issue_title(task);
        let description = render_issue_description(&repo_root, task);
        let schedule = planning
            .task_plans
            .get(&task.id)
            .with_context(|| format!("missing planner schedule for task `{}`", task.id))?;

        let issue = match issues_by_task_id.remove(&task.id) {
            Some(mut existing) => {
                let should_reactivate =
                    issue_requires_reactivation(&existing, &terminal_state_names);
                if existing.archived_at.is_some() {
                    client.unarchive_issue(&existing.id).await?;
                    existing.archived_at = None;
                }
                let state_id = should_reactivate.then_some(todo_state_id.as_str());
                if existing.title != title
                    || existing.description != description
                    || existing.priority != Some(schedule.priority)
                    || state_id.is_some()
                {
                    updated += 1;
                    client
                        .update_issue(
                            &existing.id,
                            &title,
                            &description,
                            schedule.priority,
                            state_id,
                        )
                        .await?
                } else {
                    existing
                }
            }
            None => {
                created += 1;
                client
                    .create_issue(
                        &project.team.id,
                        &project.id,
                        &todo_state_id,
                        &title,
                        &description,
                        schedule.priority,
                    )
                    .await?
            }
        };

        synced_issue_ids.insert(task.id.clone(), issue.id.clone());
        issues_by_task_id.insert(task.id.clone(), issue);
    }

    for task in &tasks {
        let schedule = planning
            .task_plans
            .get(&task.id)
            .with_context(|| format!("missing planner schedule for task `{}`", task.id))?;
        let Some(blocked_issue_id) = synced_issue_ids.get(&task.id) else {
            continue;
        };
        let existing_issue = issues_by_task_id
            .get(&task.id)
            .with_context(|| format!("missing synced issue for task `{}`", task.id))?;
        let desired_blockers = schedule
            .dependencies
            .iter()
            .filter_map(|dependency| synced_issue_ids.get(dependency).cloned())
            .collect::<HashSet<_>>();
        let existing_blockers = existing_issue
            .blocked_by
            .iter()
            .map(|blocker| blocker.id.clone())
            .collect::<HashSet<_>>();

        for blocker in &existing_issue.blocked_by {
            if desired_blockers.contains(&blocker.id) {
                continue;
            }
            client
                .delete_relation(&blocker.relation_id)
                .await
                .with_context(|| {
                    format!(
                        "failed to remove stale blocker relation `{}` -> `{}` in Linear",
                        blocker.identifier.as_deref().unwrap_or(&blocker.id),
                        task.id
                    )
                })?;
            deleted_relations += 1;
        }

        for dependency in &schedule.dependencies {
            let Some(blocker_issue_id) = synced_issue_ids.get(dependency) else {
                continue;
            };
            if existing_blockers.contains(blocker_issue_id) {
                continue;
            }
            client
                .create_blocks_relation(blocker_issue_id, blocked_issue_id)
                .await
                .with_context(|| {
                    format!(
                        "failed to relate blocker `{}` -> `{}` in Linear",
                        dependency, task.id
                    )
                })?;
            created_relations += 1;
        }
    }

    println!(
        "synced {} tasks into Linear project `{}` (created {}, updated {}, relations +{}, relations -{})",
        tasks.len(),
        project.slug,
        created,
        updated,
        created_relations,
        deleted_relations
    );
    if !planning.strategy_summary.trim().is_empty() {
        println!("planner: {}", planning.strategy_summary.trim());
    }
    if !completed_plan_sync.task_ids.is_empty() {
        println!(
            "plan reconciliation: archived {} completed plan issue(s) in Linear ({})",
            completed_plan_sync.task_ids.len(),
            completed_plan_sync.task_ids.join(", ")
        );
    }
    if !completion_sync.marked_done.is_empty() {
        println!(
            "plan reconciliation: marked {} completed task(s) done in IMPLEMENTATION_PLAN.md ({})",
            completion_sync.marked_done.len(),
            completion_sync.marked_done.join(", ")
        );
    }
    if !completion_sync.local_gap_tasks.is_empty() {
        println!(
            "plan reconciliation: left {} Linear-complete task(s) unfinished because repo-local completion evidence is incomplete ({})",
            completion_sync.local_gap_tasks.len(),
            completion_sync.local_gap_tasks.join(", ")
        );
    }
    if !completion_sync.review_backfilled.is_empty() {
        println!(
            "review reconciliation: backfilled {} REVIEW.md handoff(s) ({})",
            completion_sync.review_backfilled.len(),
            completion_sync.review_backfilled.join(", ")
        );
    }
    Ok(())
}

async fn reconcile_completed_plan_issues(
    client: &LinearGraphqlClient,
    tasks: &[SymphonyTask],
    issues: &mut [LinearIssue],
) -> Result<CompletedPlanIssueSync> {
    let updates = completed_plan_issue_updates(tasks, issues);
    if updates.is_empty() {
        return Ok(CompletedPlanIssueSync::default());
    }

    let mut updated_task_ids = Vec::new();

    for update in updates {
        client.archive_issue(&update.issue_id).await?;
        if let Some(issue) = issues.iter_mut().find(|issue| issue.id == update.issue_id) {
            issue.archived_at = Some("archived".to_string());
        }
        updated_task_ids.push(update.task_id);
    }

    Ok(CompletedPlanIssueSync {
        task_ids: updated_task_ids,
    })
}

fn completed_plan_issue_updates(
    tasks: &[SymphonyTask],
    issues: &[LinearIssue],
) -> Vec<CompletedPlanIssueUpdate> {
    let completed_task_ids = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Done))
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    if completed_task_ids.is_empty() {
        return Vec::new();
    }

    issues
        .iter()
        .filter_map(|issue| {
            let task_id = issue_task_id(issue)?;
            if !completed_task_ids.contains(task_id.as_str()) {
                return None;
            }
            if issue.archived_at.is_some() {
                return None;
            }
            Some(CompletedPlanIssueUpdate {
                issue_id: issue.id.clone(),
                task_id,
            })
        })
        .collect()
}

fn issue_requires_reactivation(
    issue: &LinearIssue,
    terminal_state_names: &HashSet<String>,
) -> bool {
    issue.archived_at.is_some()
        || issue
            .state
            .as_deref()
            .is_some_and(|state| terminal_state_names.contains(state))
}

fn load_plan_text(repo_root: &Path) -> Result<String> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))
}

fn reconcile_completion_artifacts(
    repo_root: &Path,
    plan_text: &str,
    tasks: &[SymphonyTask],
    issues: &[LinearIssue],
    terminal_state_names: &HashSet<String>,
) -> Result<CompletionArtifactSync> {
    let completed_issue_by_task = issues
        .iter()
        .filter(|issue| {
            issue
                .state
                .as_deref()
                .is_some_and(|state| terminal_state_names.contains(state))
        })
        .filter_map(|issue| issue_task_id(issue).map(|task_id| (task_id, issue)))
        .collect::<HashMap<_, _>>();
    let mut locally_evidenced_task_ids = HashSet::new();
    let mut local_gap_tasks = Vec::new();
    for task in tasks {
        if !completed_issue_by_task.contains_key(task.id.as_str()) {
            continue;
        }
        let evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
        if evidence.is_fully_evidenced() {
            locally_evidenced_task_ids.insert(task.id.clone());
        } else {
            local_gap_tasks.push(task.id.clone());
        }
    }
    let (updated_plan_text, marked_done) =
        mark_tasks_done_in_plan(plan_text, &locally_evidenced_task_ids);
    if updated_plan_text != plan_text {
        let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
        atomic_write(&plan_path, updated_plan_text.as_bytes())
            .with_context(|| format!("failed to write {}", plan_path.display()))?;
    }

    let review_backfilled = backfill_review_entries(repo_root, tasks, &completed_issue_by_task)?;

    Ok(CompletionArtifactSync {
        plan_text: updated_plan_text,
        marked_done,
        local_gap_tasks,
        review_backfilled,
    })
}

fn mark_tasks_done_in_plan(
    plan_text: &str,
    completed_task_ids: &HashSet<String>,
) -> (String, Vec<String>) {
    if completed_task_ids.is_empty() {
        return (plan_text.to_string(), Vec::new());
    }

    let ends_with_newline = plan_text.ends_with('\n');
    let mut marked_done = Vec::new();
    let updated_lines = plan_text
        .lines()
        .map(|line| {
            let Some((status, task_id, _)) = parse_task_header(line) else {
                return line.to_string();
            };
            if matches!(status, TaskStatus::Done) || !completed_task_ids.contains(&task_id) {
                return line.to_string();
            }
            marked_done.push(task_id);
            mark_task_header_done(line)
        })
        .collect::<Vec<_>>();
    let mut updated = updated_lines.join("\n");
    if ends_with_newline {
        updated.push('\n');
    }
    (updated, marked_done)
}

fn mark_task_header_done(line: &str) -> String {
    let indent_len = line.len() - line.trim_start().len();
    let indent = &line[..indent_len];
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed.strip_prefix("- [!] "))
        .unwrap_or(trimmed);
    format!("{indent}- [x] {rest}")
}

fn backfill_review_entries(
    repo_root: &Path,
    tasks: &[SymphonyTask],
    completed_issue_by_task: &HashMap<String, &LinearIssue>,
) -> Result<Vec<String>> {
    if completed_issue_by_task.is_empty() {
        return Ok(Vec::new());
    }

    let review_path = repo_root.join("REVIEW.md");
    let mut review_text = if review_path.exists() {
        fs::read_to_string(&review_path)
            .with_context(|| format!("failed to read {}", review_path.display()))?
    } else {
        default_review_doc()
    };
    let original_review_text = review_text.clone();
    let mut added = Vec::new();

    for task in tasks {
        let Some(issue) = completed_issue_by_task.get(&task.id) else {
            continue;
        };
        if review_contains_task(&review_text, &task.id) {
            continue;
        }
        review_text.push_str(&render_review_backfill_entry(task, issue));
        added.push(task.id.clone());
    }

    if review_text != original_review_text {
        atomic_write(&review_path, review_text.as_bytes())
            .with_context(|| format!("failed to write {}", review_path.display()))?;
    }

    Ok(added)
}

fn render_review_backfill_entry(task: &SymphonyTask, issue: &LinearIssue) -> String {
    let synced_at = Utc::now().format("%Y-%m-%d %H:%M UTC");
    let issue_ref = issue.identifier.as_deref().unwrap_or(issue.id.as_str());
    let state = issue.state.as_deref().unwrap_or("terminal");
    format!(
        "\n- `{task_id}`: Symphony/Linear completion backfill recorded at {synced_at} from issue `{issue_ref}` ({state}); no repo-local Symphony handoff was present, so auto review should reconstruct changed surfaces and exact validation from the landed history while using `IMPLEMENTATION_PLAN.md` as the behavioral contract. Title: {title}; status `awaiting_auto_review`.\n",
        task_id = task.id,
        synced_at = synced_at,
        issue_ref = issue_ref,
        state = state,
        title = task.title,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::completion_artifacts::review_contains_task;
    use crate::symphony_command::linear::LinearIssue;
    use crate::symphony_command::task::parse_tasks;

    use super::{
        completed_plan_issue_updates, issue_requires_reactivation, mark_tasks_done_in_plan,
    };

    #[test]
    fn mark_tasks_done_in_plan_preserves_task_record() {
        let plan = r#"
- [ ] `P-018` Loan widget
  Dependencies: `P-017B`

- [!] `P-019` Blocked follow-up
  Dependencies: `P-018`
"#;
        let completed = HashSet::from(["P-018".to_string()]);
        let (updated, marked) = mark_tasks_done_in_plan(plan, &completed);
        assert!(updated.contains("- [x] `P-018` Loan widget"));
        assert!(updated.contains("- [!] `P-019` Blocked follow-up"));
        assert_eq!(marked, vec!["P-018".to_string()]);
    }

    #[test]
    fn completed_plan_issue_updates_selects_active_checked_tasks() {
        let plan = r#"
- [x] `P-018` Loan widget
- [ ] `P-019` Pending widget
- [x] `P-020` Already archived
"#;
        let tasks = parse_tasks(plan);
        let issues = vec![
            LinearIssue {
                id: "issue-active".to_string(),
                identifier: Some("RSO-1".to_string()),
                title: "[P-018] Loan widget".to_string(),
                description: String::new(),
                archived_at: None,
                priority: None,
                state: Some("In Progress".to_string()),
                blocked_by: Vec::new(),
            },
            LinearIssue {
                id: "issue-pending".to_string(),
                identifier: Some("RSO-2".to_string()),
                title: "[P-019] Pending widget".to_string(),
                description: String::new(),
                archived_at: None,
                priority: None,
                state: Some("Todo".to_string()),
                blocked_by: Vec::new(),
            },
            LinearIssue {
                id: "issue-done".to_string(),
                identifier: Some("RSO-3".to_string()),
                title: "[P-020] Already archived".to_string(),
                description: String::new(),
                archived_at: Some("2026-04-18T00:00:00.000Z".to_string()),
                priority: None,
                state: Some("Done".to_string()),
                blocked_by: Vec::new(),
            },
        ];
        let updates = completed_plan_issue_updates(&tasks, &issues);

        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].issue_id, "issue-active");
        assert_eq!(updates[0].task_id, "P-018");
    }

    #[test]
    fn issue_requires_reactivation_for_archived_or_terminal_issues() {
        let terminal_state_names = HashSet::from(["Done".to_string()]);
        let archived_issue = LinearIssue {
            id: "issue-archived".to_string(),
            identifier: Some("RSO-1".to_string()),
            title: "archived".to_string(),
            description: String::new(),
            archived_at: Some("2026-04-18T00:00:00.000Z".to_string()),
            priority: None,
            state: Some("Done".to_string()),
            blocked_by: Vec::new(),
        };
        let terminal_issue = LinearIssue {
            id: "issue-done".to_string(),
            identifier: Some("RSO-2".to_string()),
            title: "done".to_string(),
            description: String::new(),
            archived_at: None,
            priority: None,
            state: Some("Done".to_string()),
            blocked_by: Vec::new(),
        };
        let active_issue = LinearIssue {
            id: "issue-active".to_string(),
            identifier: Some("RSO-3".to_string()),
            title: "active".to_string(),
            description: String::new(),
            archived_at: None,
            priority: None,
            state: Some("In Progress".to_string()),
            blocked_by: Vec::new(),
        };

        assert!(issue_requires_reactivation(
            &archived_issue,
            &terminal_state_names
        ));
        assert!(issue_requires_reactivation(
            &terminal_issue,
            &terminal_state_names
        ));
        assert!(!issue_requires_reactivation(
            &active_issue,
            &terminal_state_names
        ));
    }

    #[test]
    fn review_contains_task_matches_existing_handoff_shapes() {
        let review = r#"# REVIEW

Awaiting auto review:

- `P-018`: completed via Symphony

## `P-019` Parallel Implementation Handoff
"#;
        assert!(review_contains_task(review, "P-018"));
        assert!(review_contains_task(review, "P-019"));
        assert!(!review_contains_task(review, "P-020"));
    }
}
