use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use dirs::cache_dir;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command as TokioCommand;

use crate::codex_stream::capture_codex_output_with_heartbeat;
use crate::completion_artifacts::{
    default_review_doc, inspect_task_completion_evidence, review_contains_task,
};
use crate::quota_config::Provider;
use crate::quota_exec;
use crate::task_parser::{
    parse_task_header as parse_shared_task_header, parse_tasks as parse_shared_tasks,
    TaskStatus as SharedTaskStatus,
};
use crate::util::{atomic_write, git_repo_root, git_stdout, repo_name};
use crate::{
    SymphonyArgs, SymphonyRunArgs, SymphonySubcommand, SymphonySyncArgs, SymphonyWorkflowArgs,
};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";
const RELATION_BLOCKS: &str = "blocks";
const SYNC_PLANNER_MAX_PRIORITY: i64 = 4;
const SYMPHONY_ROOT_ENV: &str = "AUTODEV_SYMPHONY_ROOT";

const FETCH_PROJECT_QUERY: &str = r#"
query AutoSymphonyProject($slug: String!) {
  projects(filter: {slugId: {eq: $slug}}, first: 1) {
    nodes {
      id
      name
      slugId
      teams(first: 10) {
        nodes {
          id
          key
          name
          states(first: 100) {
            nodes {
              id
              name
              type
            }
          }
        }
      }
    }
  }
}
"#;

const FETCH_PROJECT_ISSUES_QUERY: &str = r#"
query AutoSymphonyProjectIssues($slug: String!, $first: Int!, $after: String) {
  issues(
    filter: {project: {slugId: {eq: $slug}}}
    first: $first
    after: $after
    includeArchived: true
  ) {
    nodes {
      id
      identifier
      title
      description
      archivedAt
      priority
      state {
        name
      }
      inverseRelations(first: 100) {
        nodes {
          id
          type
          issue {
            id
            identifier
            state {
              name
            }
          }
        }
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

const CREATE_ISSUE_MUTATION: &str = r#"
mutation AutoSymphonyCreateIssue(
  $teamId: String!
  $projectId: String!
  $stateId: String!
  $title: String!
  $description: String!
  $priority: Int
) {
  issueCreate(
    input: {
      teamId: $teamId
      projectId: $projectId
      stateId: $stateId
      title: $title
      description: $description
      priority: $priority
    }
  ) {
    success
    issue {
      id
      identifier
      title
      description
      priority
      state {
        name
      }
      inverseRelations(first: 100) {
        nodes {
          id
          type
          issue {
            id
            identifier
            state {
              name
            }
          }
        }
      }
    }
  }
}
"#;

const UPDATE_ISSUE_MUTATION: &str = r#"
mutation AutoSymphonyUpdateIssue(
  $id: String!
  $title: String!
  $description: String!
  $priority: Int
) {
  issueUpdate(
    id: $id
    input: {
      title: $title
      description: $description
      priority: $priority
    }
  ) {
    success
    issue {
      id
      identifier
      title
      description
      priority
      state {
        name
      }
      inverseRelations(first: 100) {
        nodes {
          id
          type
          issue {
            id
            identifier
            state {
              name
            }
          }
        }
      }
    }
  }
}
"#;

const UPDATE_ISSUE_AND_STATE_MUTATION: &str = r#"
mutation AutoSymphonyUpdateIssueAndState(
  $id: String!
  $title: String!
  $description: String!
  $stateId: String!
  $priority: Int
) {
  issueUpdate(
    id: $id
    input: {
      title: $title
      description: $description
      stateId: $stateId
      priority: $priority
    }
  ) {
    success
    issue {
      id
      identifier
      title
      description
      priority
      state {
        name
      }
      inverseRelations(first: 100) {
        nodes {
          id
          type
          issue {
            id
            identifier
            state {
              name
            }
          }
        }
      }
    }
  }
}
"#;

const ARCHIVE_ISSUE_MUTATION: &str = r#"
mutation AutoSymphonyArchiveIssue($id: String!) {
  issueArchive(id: $id) {
    success
  }
}
"#;

const UNARCHIVE_ISSUE_MUTATION: &str = r#"
mutation AutoSymphonyUnarchiveIssue($id: String!) {
  issueUnarchive(id: $id) {
    success
  }
}
"#;

const DELETE_RELATION_MUTATION: &str = r#"
mutation AutoSymphonyDeleteRelation($id: String!) {
  issueRelationDelete(id: $id) {
    success
  }
}
"#;

const CREATE_RELATION_MUTATION: &str = r#"
mutation AutoSymphonyCreateRelation(
  $issueId: String!
  $relatedIssueId: String!
  $type: IssueRelationType!
) {
  issueRelationCreate(
    input: {
      issueId: $issueId
      relatedIssueId: $relatedIssueId
      type: $type
    }
  ) {
    success
    issueRelation {
      id
    }
  }
}
"#;

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

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinearBlocker {
    relation_id: String,
    id: String,
    identifier: Option<String>,
    state: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinearIssue {
    id: String,
    identifier: Option<String>,
    title: String,
    description: String,
    archived_at: Option<String>,
    priority: Option<i64>,
    state: Option<String>,
    blocked_by: Vec<LinearBlocker>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinearState {
    id: String,
    name: String,
    state_type: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinearTeam {
    id: String,
    key: Option<String>,
    name: Option<String>,
    states: Vec<LinearState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinearProject {
    id: String,
    name: String,
    slug: String,
    team: LinearTeam,
}

#[derive(Clone)]
struct LinearGraphqlClient {
    http: Client,
    api_key: String,
}

struct RenderedWorkflow {
    output_path: PathBuf,
    base_branch: String,
    workspace_root: PathBuf,
    logs_root: PathBuf,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct WorkflowBootstrapConfig {
    project_slug: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct EffectiveTaskSchedule {
    dependencies: Vec<String>,
    external_dependencies: Vec<String>,
    priority: i64,
    rationale: String,
}

#[derive(Debug, Deserialize)]
struct PlannerResponse {
    #[serde(default)]
    strategy_summary: String,
    tasks: Vec<PlannerTask>,
}

#[derive(Debug, Deserialize)]
struct PlannerTask {
    task_id: String,
    priority: i64,
    #[allow(dead_code)]
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    external_dependencies: Vec<String>,
    #[serde(default)]
    rationale: String,
}

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

pub(crate) async fn run_symphony(args: SymphonyArgs) -> Result<()> {
    match args.command {
        SymphonySubcommand::Sync(args) => run_sync(args).await,
        SymphonySubcommand::Workflow(args) => {
            let rendered = render_workflow(args).await?;
            println!("workflow: {}", rendered.output_path.display());
            println!("base_branch: {}", rendered.base_branch);
            println!("workspace_root: {}", rendered.workspace_root.display());
            Ok(())
        }
        SymphonySubcommand::Run(args) => run_foreground(args).await,
    }
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

#[derive(Clone, Debug, Default)]
struct DeterminedSyncPlan {
    strategy_summary: String,
    task_plans: HashMap<String, EffectiveTaskSchedule>,
}

impl DeterminedSyncPlan {
    fn fallback(tasks: &[SymphonyTask]) -> Self {
        let priorities = fallback_task_priorities(tasks);
        let mut task_plans = HashMap::new();
        for task in tasks {
            let priority = priorities
                .get(&task.id)
                .copied()
                .unwrap_or(SYNC_PLANNER_MAX_PRIORITY);
            task_plans.insert(
                task.id.clone(),
                EffectiveTaskSchedule {
                    dependencies: dedup_task_refs(task.dependencies.clone()),
                    external_dependencies: Vec::new(),
                    priority,
                    rationale: "deterministic fallback from explicit Dependencies lines"
                        .to_string(),
                },
            );
        }
        Self {
            strategy_summary: "deterministic fallback from explicit Dependencies lines".to_string(),
            task_plans,
        }
    }
}

async fn determine_sync_plan(
    repo_root: &Path,
    plan_text: &str,
    tasks: &[SymphonyTask],
    codex_bin: &Path,
    model: &str,
    reasoning_effort: &str,
) -> Result<DeterminedSyncPlan> {
    let planner_dir = repo_root.join(".auto").join("symphony");
    fs::create_dir_all(&planner_dir)
        .with_context(|| format!("failed to create {}", planner_dir.display()))?;
    let prompt = build_sync_planner_prompt(repo_root, plan_text, tasks);
    let prompt_path = planner_dir.join("sync-planner-prompt.md");
    let raw_response_path = planner_dir.join("sync-planner-response.jsonl");
    let stderr_path = planner_dir.join("sync-planner-stderr.log");
    let parsed_response_path = planner_dir.join("sync-planner-result.json");
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!(
        "planner: analyzing {} pending task(s) in `{}` with {} / {}",
        tasks.len(),
        repo_name(repo_root),
        model,
        reasoning_effort
    );
    println!("planner prompt: {}", prompt_path.display());
    println!("planner raw output: {}", raw_response_path.display());
    println!("planner stderr: {}", stderr_path.display());

    let (stdout_raw, stderr_text) =
        run_codex_planner(repo_root, &prompt, model, reasoning_effort, codex_bin).await?;
    atomic_write(&raw_response_path, stdout_raw.as_bytes())
        .with_context(|| format!("failed to write {}", raw_response_path.display()))?;
    atomic_write(&stderr_path, stderr_text.as_bytes())
        .with_context(|| format!("failed to write {}", stderr_path.display()))?;

    let planner_message = extract_agent_message_from_codex_stream(&stdout_raw)
        .ok_or_else(|| anyhow!("Codex planner did not emit a final agent_message"))?;
    let planner_json = extract_planner_json(&planner_message)
        .ok_or_else(|| anyhow!("Codex planner response did not contain valid JSON"))?;
    atomic_write(&parsed_response_path, planner_json.as_bytes())
        .with_context(|| format!("failed to write {}", parsed_response_path.display()))?;
    let parsed: PlannerResponse = serde_json::from_str(&planner_json)
        .with_context(|| "failed to parse Codex planner JSON response")?;
    normalize_planner_response(tasks, parsed)
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

fn build_sync_planner_prompt(repo_root: &Path, plan_text: &str, tasks: &[SymphonyTask]) -> String {
    let task_ids = tasks
        .iter()
        .map(|task| format!("`{}`", task.id))
        .collect::<Vec<_>>()
        .join(", ");
    let preamble = plan_preamble(plan_text);
    let task_digests = tasks
        .iter()
        .map(render_sync_task_digest)
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        r#"You are planning issue dispatch for `auto symphony sync`.

Repository: `{repo}`
Repo root: `{repo_root}`
Goal: produce a dependency DAG and Linear priorities that maximize safe throughput for 5 concurrent Symphony lanes.

This is a concrete planning deliverable, not a quick heuristic pass. Treat it like a Codex work item:
- inspect the live repository when needed
- use tools to verify queue facts
- leave scratch notes or drafts under `.auto/symphony/` if that helps you reason clearly before the final JSON

Constraints:
- `IMPLEMENTATION_PLAN.md` is the primary source of truth, but you may inspect the live repo to resolve ambiguous shared surfaces or blocker language.
- Preserve every explicit prerequisite from the plan.
- Treat each task's `Dependencies:` block as the authoritative machine blocker set for repo-local scheduling.
- Do not invent new repo-local `dependencies` from critical-path prose, parenthetical notes, "parallel with" commentary, merge-conflict caution, or broad shared-surface anxiety. Use `priority` and `rationale` to shape waves instead.
- If prose gating looks real but is not encoded in the task contract, reflect it in `priority`/`rationale` rather than smuggling in a hidden blocker. That kind of fix belongs in the plan itself.
- Be conservative about merge-conflict risk, but do not serialize unrelated work unnecessarily.
- Use `priority` values `1` through `4`, where `1` is the first work Symphony should prefer.
- Treat `priority: 1` as the immediate first-wave launch set for a 5-lane run, not a broad bucket for every early task.
- Prefer roughly 3-7 tasks at `priority: 1`. If more tasks are technically runnable, push the less urgent ones to `priority: 2` or add blockers so the top wave stays intentional.
- Use `priority: 2` for the immediate next wave after the first launch set, `priority: 3` for post-foundation or expansion-gated work, and `priority: 4` for late, conditional, or externally blocked work.
- When two tasks are both early but one is clearly more central to shared foundations, MVP gating, or unblock sequencing, do not leave them tied at `priority: 1` just because both are runnable.
- `dependencies` must list task IDs already present in that task's explicit dependency contract after normalizing obvious narrative wrappers.
- Put cross-repo or otherwise unsynced blockers in `external_dependencies`.
- Return every pending task exactly once. Do not omit any task and do not invent new task IDs.
- Before finalizing, do at least one concrete verification pass with tools so the run stays observable and grounded.
- If the queue is large, create a compact scratch summary such as `.auto/symphony/sync-planner-working.md` or `.auto/symphony/sync-planner-working.json` before you emit the final answer.
- Before finalizing, check the size of the `priority: 1` set and tighten it if it is too broad for a 5-lane start.
- Respond with JSON only. No prose outside the JSON object. No code fences.

Pending task IDs:
{task_ids}

Return this exact schema:
{{
  "strategy_summary": "short explanation",
  "tasks": [
    {{
      "task_id": "P-000",
      "priority": 1,
      "dependencies": ["P-001"],
      "external_dependencies": ["OTHER-123"],
      "rationale": "short scheduling reason"
    }}
  ]
}}

Queue preamble:

```md
{preamble}
```

Pending task digests:

```md
{task_digests}
```
"#,
        repo = repo_name(repo_root),
        repo_root = repo_root.display(),
        task_ids = task_ids,
        preamble = preamble,
        task_digests = task_digests
    )
}

async fn run_codex_planner(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<(String, String)> {
    let mut command = planner_command(repo_root, model, reasoning_effort, codex_bin)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch sync planner from {}", repo_root.display()))?;

    let mut stdin = child
        .stdin
        .take()
        .context("Codex planner stdin should be piped")?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .context("failed to write sync planner prompt to Codex")?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .context("Codex planner stdout should be piped")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex planner stderr should be piped")?;

    let stdout_task = tokio::spawn(async move {
        capture_codex_output_with_heartbeat(stdout, "sync planner", 15).await
    });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child
        .wait()
        .await
        .context("failed waiting for Codex planner")?;
    let stdout_raw = stdout_task
        .await
        .context("Codex planner stdout capture task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Codex planner stderr capture task panicked")??;

    if !status.success() {
        bail!(
            "Codex planner failed: {}",
            if stderr_text.trim().is_empty() {
                stdout_raw.trim()
            } else {
                stderr_text.trim()
            }
        );
    }
    Ok((stdout_raw, stderr_text))
}

fn planner_command(
    repo_root: &Path,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<TokioCommand> {
    let mut command = if quota_exec::is_quota_available(Provider::Codex) {
        let auto_bin = std::env::current_exe().context("failed to resolve current auto binary")?;
        let mut command = TokioCommand::new(auto_bin);
        command.arg("quota").arg("open").arg("codex").arg("exec");
        command
    } else {
        TokioCommand::new(codex_bin)
    };
    command
        .arg("--json")
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .arg("--skip-git-repo-check")
        .arg("--cd")
        .arg(repo_root)
        .arg("-m")
        .arg(model)
        .arg("-c")
        .arg(format!("model_reasoning_effort=\"{reasoning_effort}\""));
    Ok(command)
}

fn normalize_planner_response(
    tasks: &[SymphonyTask],
    response: PlannerResponse,
) -> Result<DeterminedSyncPlan> {
    let known_ids = tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let mut by_id = HashMap::<String, PlannerTask>::new();
    for task in response.tasks {
        if !known_ids.contains(&task.task_id) {
            bail!("Codex planner returned unknown task `{}`", task.task_id);
        }
        if by_id.insert(task.task_id.clone(), task).is_some() {
            bail!("Codex planner returned duplicate task entry");
        }
    }
    for task in tasks {
        if !by_id.contains_key(&task.id) {
            bail!("Codex planner omitted task `{}`", task.id);
        }
    }

    let mut task_plans = HashMap::new();
    for task in tasks {
        let planned = by_id
            .remove(&task.id)
            .with_context(|| format!("Codex planner omitted task `{}`", task.id))?;
        let mut dependencies = task.dependencies.clone();
        dependencies.retain(|dependency| dependency != &task.id);
        dependencies = dedup_task_refs(dependencies);

        let mut external_dependencies = planned.external_dependencies;
        external_dependencies.extend(
            dependencies
                .iter()
                .filter(|dependency| !known_ids.contains((*dependency).as_str()))
                .cloned(),
        );
        external_dependencies = dedup_task_refs(external_dependencies);

        task_plans.insert(
            task.id.clone(),
            EffectiveTaskSchedule {
                dependencies,
                external_dependencies,
                priority: planned.priority.clamp(1, SYNC_PLANNER_MAX_PRIORITY),
                rationale: planned.rationale.trim().to_string(),
            },
        );
    }

    validate_schedule_dag(tasks, &task_plans)?;

    Ok(DeterminedSyncPlan {
        strategy_summary: response.strategy_summary.trim().to_string(),
        task_plans,
    })
}

fn validate_schedule_dag(
    tasks: &[SymphonyTask],
    task_plans: &HashMap<String, EffectiveTaskSchedule>,
) -> Result<()> {
    let task_ids = tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let mut indegree = HashMap::<String, usize>::new();
    let mut dependents = HashMap::<String, Vec<String>>::new();
    for task in tasks {
        let internal_deps = task_plans
            .get(&task.id)
            .map(|schedule| {
                schedule
                    .dependencies
                    .iter()
                    .filter(|dependency| task_ids.contains((*dependency).as_str()))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        indegree.insert(task.id.clone(), internal_deps.len());
        for dependency in internal_deps {
            dependents
                .entry(dependency)
                .or_default()
                .push(task.id.clone());
        }
    }

    let order = task_order_map(tasks);
    let mut queue = tasks
        .iter()
        .filter(|task| indegree.get(&task.id).copied().unwrap_or(0) == 0)
        .map(|task| task.id.clone())
        .collect::<VecDeque<_>>();
    let mut visited = 0usize;

    while let Some(task_id) = queue.pop_front() {
        visited += 1;
        let mut children = dependents.remove(&task_id).unwrap_or_default();
        children.sort_by_key(|task| order.get(task).copied().unwrap_or(usize::MAX));
        for child in children {
            let entry = indegree
                .get_mut(&child)
                .with_context(|| format!("missing indegree for task `{child}`"))?;
            *entry = entry.saturating_sub(1);
            if *entry == 0 {
                queue.push_back(child);
            }
        }
    }

    if visited != tasks.len() {
        bail!("planner dependency graph contains a cycle");
    }
    Ok(())
}

fn fallback_task_priorities(tasks: &[SymphonyTask]) -> HashMap<String, i64> {
    let order = task_order_map(tasks);
    let task_ids = tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let mut indegree = HashMap::<String, usize>::new();
    let mut dependents = HashMap::<String, Vec<String>>::new();
    let mut max_parent_wave = HashMap::<String, usize>::new();
    let mut waves = HashMap::<String, usize>::new();

    for task in tasks {
        let internal_deps = task
            .dependencies
            .iter()
            .filter(|dependency| task_ids.contains((*dependency).as_str()))
            .cloned()
            .collect::<Vec<_>>();
        indegree.insert(task.id.clone(), internal_deps.len());
        for dependency in internal_deps {
            dependents
                .entry(dependency)
                .or_default()
                .push(task.id.clone());
        }
    }

    let mut queue = tasks
        .iter()
        .filter(|task| indegree.get(&task.id).copied().unwrap_or(0) == 0)
        .map(|task| task.id.clone())
        .collect::<VecDeque<_>>();

    while let Some(task_id) = queue.pop_front() {
        let current_wave = max_parent_wave.get(&task_id).copied().unwrap_or(0);
        waves.insert(task_id.clone(), current_wave);
        let mut children = dependents.remove(&task_id).unwrap_or_default();
        children.sort_by_key(|task| order.get(task).copied().unwrap_or(usize::MAX));
        for child in children {
            let child_wave = max_parent_wave.entry(child.clone()).or_insert(0);
            *child_wave = (*child_wave).max(current_wave + 1);
            let entry = indegree.get_mut(&child).expect("child indegree must exist");
            *entry = entry.saturating_sub(1);
            if *entry == 0 {
                queue.push_back(child);
            }
        }
    }

    let mut fallback_wave = waves.values().copied().max().unwrap_or(0) + 1;
    for task in tasks {
        if waves.contains_key(&task.id) {
            continue;
        }
        waves.insert(task.id.clone(), fallback_wave);
        fallback_wave += 1;
    }

    tasks
        .iter()
        .map(|task| {
            let wave = waves.get(&task.id).copied().unwrap_or(3);
            (
                task.id.clone(),
                (wave as i64 + 1).clamp(1, SYNC_PLANNER_MAX_PRIORITY),
            )
        })
        .collect()
}

fn task_order_map(tasks: &[SymphonyTask]) -> HashMap<String, usize> {
    tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.clone(), index))
        .collect()
}

fn dedup_task_refs(refs: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for reference in refs {
        let normalized = reference.trim();
        if normalized.is_empty() || !seen.insert(normalized.to_string()) {
            continue;
        }
        deduped.push(normalized.to_string());
    }
    deduped
}

fn plan_preamble(plan_text: &str) -> String {
    let mut lines = Vec::new();
    for line in plan_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- [ ] ")
            || trimmed.starts_with("- [!] ")
            || trimmed.starts_with("- [x] ")
            || trimmed.starts_with("- [X] ")
        {
            break;
        }
        lines.push(line.to_string());
    }
    lines.join("\n")
}

fn render_sync_task_digest(task: &SymphonyTask) -> String {
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

fn extract_agent_message_from_codex_stream(raw: &str) -> Option<String> {
    let mut last_message = None;
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(message) = value
            .get("item")
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            .filter(|item_type| *item_type == "agent_message")
            .and_then(|_| value.get("item"))
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
        {
            last_message = Some(message.to_string());
            continue;
        }
        if let Some(message) = value.get("last_agent_message").and_then(Value::as_str) {
            last_message = Some(message.to_string());
        }
    }
    last_message
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

fn extract_planner_json(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }
    if let Some(fenced) = extract_fenced_json_block(trimmed) {
        if serde_json::from_str::<Value>(&fenced).is_ok() {
            return Some(fenced);
        }
    }
    let prefix = extract_complete_json_value_prefix(trimmed)?;
    serde_json::from_str::<Value>(&prefix).ok()?;
    Some(prefix)
}

fn extract_complete_json_value_prefix(content: &str) -> Option<String> {
    let content = content.trim_start();
    let mut stream = serde_json::Deserializer::from_str(content).into_iter::<Value>();
    stream.next()?.ok()?;
    let end = stream.byte_offset();
    Some(content[..end].trim_end().to_string())
}

fn extract_fenced_json_block(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if !trimmed.starts_with("```") {
        return None;
    }
    let mut lines = trimmed.lines();
    let opening = lines.next()?.trim();
    if !opening.starts_with("```") {
        return None;
    }

    let mut body = Vec::new();
    for line in lines {
        if line.trim_start().starts_with("```") {
            return Some(body.join("\n").trim().to_string());
        }
        body.push(line.to_string());
    }
    None
}

async fn read_stream<R>(stream: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = tokio::io::BufReader::new(stream);
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .await
        .context("failed to read stream")?;
    Ok(text)
}

async fn render_workflow(args: SymphonyWorkflowArgs) -> Result<RenderedWorkflow> {
    let repo_root = resolve_repo_root(args.repo_root)?;
    let project_slug = resolve_project_slug(&repo_root, args.project_slug.as_deref())?;
    let base_branch = resolve_base_branch(&repo_root, args.base_branch)?;
    let workflow_path = resolve_workflow_path(&repo_root, args.output);
    let workspace_root = resolve_workspace_root(&repo_root, args.workspace_root)?;
    let logs_root = default_logs_root(&repo_root);
    let remote_url = git_stdout(&repo_root, ["remote", "get-url", "origin"])?
        .trim()
        .to_string();
    let repo_label = repo_name(&repo_root);
    let output = render_workflow_markdown(WorkflowRenderSpec {
        repo_root: &repo_root,
        repo_label: &repo_label,
        project_slug: &project_slug,
        remote_url: &remote_url,
        base_branch: &base_branch,
        workspace_root: &workspace_root,
        poll_interval_ms: args.poll_interval_ms,
        max_concurrent_agents: args.max_concurrent_agents,
        model: &args.model,
        reasoning_effort: &args.reasoning_effort,
        todo_state: "Todo",
        in_progress_state: &args.in_progress_state,
        done_state: &args.done_state,
        blocked_state: args.blocked_state.as_deref(),
    })?;

    if let Some(parent) = workflow_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    atomic_write(&workflow_path, output.as_bytes())?;

    Ok(RenderedWorkflow {
        output_path: workflow_path,
        base_branch,
        workspace_root,
        logs_root,
    })
}

async fn run_foreground(args: SymphonyRunArgs) -> Result<()> {
    let symphony_root = resolve_symphony_root(args.symphony_root.clone())?;

    if args.sync_first {
        run_sync(SymphonySyncArgs {
            repo_root: args.repo_root.clone(),
            project_slug: args.project_slug.clone(),
            todo_state: args.todo_state.clone(),
            planner_model: args.planner_model.clone(),
            planner_reasoning_effort: args.planner_reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            no_ai_planner: args.no_ai_planner,
        })
        .await?;
    }

    let rendered = render_workflow(SymphonyWorkflowArgs {
        repo_root: args.repo_root.clone(),
        project_slug: args.project_slug.clone(),
        output: args.output.clone(),
        workspace_root: args.workspace_root.clone(),
        base_branch: args.base_branch.clone(),
        max_concurrent_agents: args.max_concurrent_agents,
        poll_interval_ms: args.poll_interval_ms,
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        in_progress_state: args.in_progress_state.clone(),
        done_state: args.done_state.clone(),
        blocked_state: args.blocked_state.clone(),
    })
    .await?;

    let symphony_bin = symphony_root.join("bin").join("symphony");
    if !symphony_bin.is_file() {
        bail!(
            "Symphony binary not found at {}; build it first with `cd {} && mix build` or `mise exec -- mix build`",
            symphony_bin.display(),
            symphony_root.display()
        );
    }

    let logs_root = args.logs_root.unwrap_or(rendered.logs_root);
    fs::create_dir_all(&logs_root)
        .with_context(|| format!("failed to create {}", logs_root.display()))?;
    let live_log_path = logs_root.join("log").join("symphony.log");
    println!("workflow: {}", rendered.output_path.display());
    println!("logs root: {}", logs_root.display());
    println!("live log:  {}", live_log_path.display());
    if args.sync_first {
        println!("sync:      completed before launch");
    } else {
        println!("sync:      skipped (use --sync-first to refresh Linear issues first)");
    }

    let mut command = TokioCommand::new(&symphony_bin);
    command
        .current_dir(&symphony_root)
        .arg("--i-understand-that-this-will-be-running-without-the-usual-guardrails")
        .arg("--logs-root")
        .arg(&logs_root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(port) = args.port {
        command.arg("--port").arg(port.to_string());
    }
    command.arg(&rendered.output_path);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch Symphony from {}", symphony_bin.display()))?;
    let status = child.wait().await.with_context(|| {
        format!(
            "failed waiting for Symphony process from {}",
            symphony_bin.display()
        )
    })?;
    if !status.success() {
        bail!("Symphony exited with status {status}");
    }
    Ok(())
}

fn resolve_symphony_root(explicit_root: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = explicit_root {
        return Ok(root);
    }

    let Some(root) = std::env::var_os(SYMPHONY_ROOT_ENV).filter(|value| !value.is_empty()) else {
        bail!(
            "missing symphony root: pass --symphony-root <path> or set {SYMPHONY_ROOT_ENV}=<path>"
        );
    };

    Ok(PathBuf::from(root))
}

fn resolve_repo_root(repo_root: Option<PathBuf>) -> Result<PathBuf> {
    match repo_root {
        Some(path) => Ok(path),
        None => git_repo_root(),
    }
}

fn resolve_project_slug(repo_root: &Path, cli_slug: Option<&str>) -> Result<String> {
    if let Some(slug) = cli_slug.map(str::trim).filter(|slug| !slug.is_empty()) {
        return Ok(slug.to_string());
    }
    if let Some(slug) = read_existing_workflow_config(repo_root)?.project_slug {
        return Ok(slug);
    }
    bail!(
        "Linear project slug is required for the first Symphony setup; pass --project-slug once or generate .auto/symphony/WORKFLOW.md first"
    );
}

fn resolve_workflow_path(repo_root: &Path, output: Option<PathBuf>) -> PathBuf {
    output.unwrap_or_else(|| repo_root.join(".auto").join("symphony").join("WORKFLOW.md"))
}

fn read_existing_workflow_config(repo_root: &Path) -> Result<WorkflowBootstrapConfig> {
    let workflow_path = resolve_workflow_path(repo_root, None);
    if !workflow_path.is_file() {
        return Ok(WorkflowBootstrapConfig::default());
    }
    let text = fs::read_to_string(&workflow_path)
        .with_context(|| format!("failed to read {}", workflow_path.display()))?;
    let Some(front_matter) = markdown_front_matter(&text) else {
        return Ok(WorkflowBootstrapConfig::default());
    };
    Ok(WorkflowBootstrapConfig {
        project_slug: front_matter_line_value(front_matter, "project_slug"),
    })
}

fn markdown_front_matter(markdown: &str) -> Option<&str> {
    let stripped = markdown.strip_prefix("---\n")?;
    let end = stripped.find("\n---\n")?;
    Some(&stripped[..end])
}

fn front_matter_line_value(front_matter: &str, field: &str) -> Option<String> {
    front_matter.lines().find_map(|line| {
        let trimmed = line.trim_start();
        trimmed
            .strip_prefix(field)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(unquote_yamlish_scalar)
    })
}

fn unquote_yamlish_scalar(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|trimmed| trimmed.strip_suffix('"'))
        .map(|trimmed| trimmed.replace("\\\"", "\""))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|trimmed| trimmed.strip_suffix('\''))
                .map(|trimmed| trimmed.replace("''", "'"))
        })
        .unwrap_or_else(|| value.to_string())
        .trim()
        .to_string()
}

fn resolve_workspace_root(repo_root: &Path, workspace_root: Option<PathBuf>) -> Result<PathBuf> {
    match workspace_root {
        Some(path) => Ok(path),
        None => {
            let base = cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("symphony-workspaces");
            Ok(base.join(repo_name(repo_root)))
        }
    }
}

fn default_logs_root(repo_root: &Path) -> PathBuf {
    repo_root.join(".auto").join("symphony").join("logs")
}

fn resolve_base_branch(repo_root: &Path, override_branch: Option<String>) -> Result<String> {
    if let Some(branch) = override_branch {
        return Ok(branch);
    }
    if let Ok(remote_head) = git_stdout(
        repo_root,
        ["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        if let Some(branch) = remote_head.trim().strip_prefix("origin/") {
            if !branch.is_empty() {
                return Ok(branch.to_string());
            }
        }
    }
    let current = git_stdout(repo_root, ["branch", "--show-current"])?;
    let current = current.trim();
    if !current.is_empty() {
        return Ok(current.to_string());
    }
    Ok("main".to_string())
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

fn parse_task_header(line: &str) -> Option<(TaskStatus, String, String)> {
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

fn issue_task_id(issue: &LinearIssue) -> Option<String> {
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

struct WorkflowRenderSpec<'a> {
    repo_root: &'a Path,
    repo_label: &'a str,
    project_slug: &'a str,
    remote_url: &'a str,
    base_branch: &'a str,
    workspace_root: &'a Path,
    poll_interval_ms: u64,
    max_concurrent_agents: usize,
    model: &'a str,
    reasoning_effort: &'a str,
    todo_state: &'a str,
    in_progress_state: &'a str,
    done_state: &'a str,
    blocked_state: Option<&'a str>,
}

fn render_workflow_markdown(spec: WorkflowRenderSpec<'_>) -> Result<String> {
    validate_workflow_render_spec(&spec)?;
    let shared_cargo_target_dir = shared_cargo_target_dir(spec.workspace_root);
    let workspace_root_text = path_text("workspace root", spec.workspace_root)?;
    let repo_root_text = path_text("repo root", spec.repo_root)?;
    let shared_cargo_target_dir_text =
        path_text("shared Cargo target dir", &shared_cargo_target_dir)?;
    let workspace_root_yaml = yaml_double_quote(&workspace_root_text);
    let shared_cargo_target_dir_yaml = yaml_double_quote(&shared_cargo_target_dir_text);
    let base_branch_shell = shell_quote(spec.base_branch);
    let origin_base_branch_shell = shell_quote(&format!("origin/{}", spec.base_branch));
    let origin_base_range_shell = shell_quote(&format!("origin/{}..HEAD", spec.base_branch));
    let model_reasoning_effort = ["model_reasoning_effort=", spec.reasoning_effort].concat();
    let model_reasoning_effort_shell = shell_quote(&model_reasoning_effort);
    let model_shell = shell_quote(spec.model);
    let blocked_state_line = spec
        .blocked_state
        .map(|state| format!("- If you hit a true external blocker (missing auth/permissions/secrets), add one precise Linear comment and move the issue to `{state}` before stopping.\n"))
        .unwrap_or_else(|| "- If you hit a true external blocker (missing auth/permissions/secrets), add one precise Linear comment describing the blocker before stopping.\n".to_string());
    let before_run_hook = [
        "set -eu".to_string(),
        format!("mkdir -p {}", shell_quote(&shared_cargo_target_dir_text)),
        "if [ -f .git/info/exclude ]; then".to_string(),
        "  if ! grep -qxF '/.cargo-target' .git/info/exclude; then printf '/.cargo-target\\n' >> .git/info/exclude; fi".to_string(),
        "  if ! grep -qxF '/.cargo-target*' .git/info/exclude; then printf '/.cargo-target*\\n' >> .git/info/exclude; fi".to_string(),
        "fi".to_string(),
        "for stale_cargo_target in .cargo-target .cargo-target-*; do".to_string(),
        "  if [ -e \"$stale_cargo_target\" ] || [ -L \"$stale_cargo_target\" ]; then".to_string(),
        "    echo \"before_run: removing repo-local cargo target path $stale_cargo_target\"".to_string(),
        "    rm -rf \"$stale_cargo_target\"".to_string(),
        "  fi".to_string(),
        "done".to_string(),
        "ln -s ../.cargo-target .cargo-target".to_string(),
        ["git fetch origin ", &base_branch_shell].concat(),
        ["git checkout ", &base_branch_shell].concat(),
        [
            "ahead_commits=$(git rev-list --count ",
            &origin_base_range_shell,
            ")",
        ]
        .concat(),
        "should_rebase=1".to_string(),
        "if [ \"$ahead_commits\" -gt 0 ]; then".to_string(),
        [
            "  merge_base=$(git merge-base HEAD ",
            &origin_base_branch_shell,
            ")",
        ]
        .concat(),
        "  echo \"before_run: found $ahead_commits unpushed local commit(s), restoring them to workspace changes before continuing\"".to_string(),
        "  git reset --mixed \"$merge_base\"".to_string(),
        "  should_rebase=0".to_string(),
        "fi".to_string(),
        "if [ -d .git/rebase-merge ] || [ -d .git/rebase-apply ] || [ -f .git/MERGE_HEAD ] || [ -f .git/CHERRY_PICK_HEAD ]; then".to_string(),
        "  echo \"before_run: unfinished git operation detected, preserving workspace state and skipping rebase sync\"".to_string(),
        "  should_rebase=0".to_string(),
        "fi".to_string(),
        "if git ls-files --unmerged | grep -q .; then".to_string(),
        "  echo \"before_run: unmerged index entries detected, preserving workspace state for repair\"".to_string(),
        "  should_rebase=0".to_string(),
        "fi".to_string(),
        "if ! git diff --quiet || ! git diff --cached --quiet; then".to_string(),
        "  echo \"before_run: dirty worktree, skipping rebase sync to preserve local changes\""
            .to_string(),
        "  should_rebase=0".to_string(),
        "fi".to_string(),
        "if [ \"$should_rebase\" -eq 1 ]; then".to_string(),
        ["  git pull --rebase origin ", &base_branch_shell].concat(),
        "fi".to_string(),
    ]
    .into_iter()
    .map(|line| format!("    {line}"))
    .collect::<Vec<_>>()
    .join("\n");
    let codex_command = [
        "env CARGO_TARGET_DIR=",
        &shell_quote(&shared_cargo_target_dir_text),
        " auto quota open codex --config shell_environment_policy.inherit=all --config ",
        &model_reasoning_effort_shell,
        " --model ",
        &model_shell,
        " app-server",
    ]
    .concat();
    Ok(format!(
        "---\n\
tracker:\n  kind: linear\n  api_key: $LINEAR_API_KEY\n  project_slug: {project_slug_yaml}\n  active_states:\n    - {todo_state_yaml}\n    - {in_progress_state_yaml}\n  terminal_states:\n    - Closed\n    - Cancelled\n    - Canceled\n    - Duplicate\n    - {done_state_yaml}\n\
polling:\n  interval_ms: {poll_interval_ms}\n\
workspace:\n  root: {workspace_root_yaml}\n\
hooks:\n  after_create: |\n    git clone --depth 1 {remote_url} .\n  before_run: |\n{before_run_hook}\n  timeout_ms: 300000\n\
agent:\n  max_concurrent_agents: {max_concurrent_agents}\n  max_turns: 20\n\
codex:\n  command: >-\n    {codex_command}\n  approval_policy: never\n  thread_sandbox: workspace-write\n  turn_sandbox_policy:\n    type: workspaceWrite\n    writableRoots:\n      - {workspace_root_yaml}\n      - {shared_cargo_target_dir_yaml}\n  read_timeout_ms: 60000\n  max_turn_wall_clock_ms: 1800000\n  max_turn_total_tokens: 12000000\n---\n\n\
You are running an unattended implementation-plan execution session for repository `{repo_label}`.\n\n\
Repository root inside the workspace clone: `{repo_root_text}`\n\
Integration branch: `{base_branch}`\n\
Linear project: `{project_slug}`\n\n\
{{% if attempt %}}\n\
Continuation context:\n\n\
- This is retry attempt #{{{{ attempt }}}} because the issue remained active.\n\
- Resume from the current workspace state instead of restarting from scratch.\n\
- Do not repeat already-finished investigation or validation unless your code changes require it.\n\
{{% if resume_reason %}}- Failure context from the previous attempt: {{{{ resume_reason }}}}\n\
{{% endif %}}{{% if resume_guidance %}}- Recovery guidance: {{{{ resume_guidance }}}}\n\
{{% endif %}}{{% endif %}}\n\n\
Issue context:\n\
Identifier: {{{{ issue.identifier }}}}\n\
Title: {{{{ issue.title }}}}\n\
Current status: {{{{ issue.state }}}}\n\
URL: {{{{ issue.url }}}}\n\n\
Description:\n\
{{% if issue.description %}}\n\
{{{{ issue.description }}}}\n\
{{% else %}}\n\
No description provided.\n\
{{% endif %}}\n\n\
You must execute the task body from the issue description as the source of truth. The description came from `IMPLEMENTATION_PLAN.md` and includes the task id, acceptance criteria, verification commands, and scope boundary.\n\n\
Operating rules:\n\n\
- Read and follow the repository's `AGENTS.md` plus any directly referenced repo docs before editing code.\n\
- Work only inside the provided repository clone.\n\
- Use targeted validation only; do not widen scope with broad workspace tests.\n\
- Before making changes, search the codebase, tests, and planning artifacts. Do not assume a surface is missing until you verify it.\n\
- Build a short task brief for yourself before editing: task id, spec refs, owned surfaces, integration touchpoints, scope boundary, acceptance criteria, verification, and any assumptions you are relying on.\n\
- Restate the task's assumptions and success conditions from repo evidence before editing. If the task contract is ambiguous, resolve the ambiguity from repo evidence or leave a precise blocker instead of guessing.\n\
- Keep changes scoped to the issue's task body. Do not silently take on unrelated cleanup.\n\
- One issue = one task = one landing attempt. Never mark more than one plan task done, never append `REVIEW.md` handoff text for a second task, and never treat adjacent cleanup as free work.\n\
- Do not mark adjacent tasks done just because the current diff incidentally helps them. Leave those tasks untouched for their own issue unless the plan contract explicitly says this issue owns them.\n\
- Never ask a human to perform follow-up work during normal execution.\n\
{blocked_state_line}\
- Before editing, fetch the current issue via `linear_graphql`, inspect the team states, and if the issue is in `{todo_state}`, move it to `{in_progress_state}`.\n\
- Work directly on `{base_branch}` in this clone. Fresh workspaces are synced from `origin/{base_branch}` before the first turn.\n\
- If you are resuming a dirty workspace after a retry or stall, preserve that local state instead of trying to rebase it before continuing.\n\
- Never run `git fetch`, `git pull`, `git rebase`, `git push`, or branch-switching commands yourself in this workspace. Use `git status`, `git diff`, `git log`, and `git show` for inspection only; Symphony performs sync and landing host-side.\n\
- Do not run the final `git add` or `git commit` flow yourself; Symphony performs landing host-side.\n\
- Never request interactive user input or MCP elicitation. This is a non-interactive unattended run, so make the narrowest reasonable assumption from the issue, repo, and current workspace instead.\n\
- Do not keep multiple long-running shell sessions alive at once. Finish or abandon one long-running `exec_command` session before starting another.\n\
- For `cargo test`, `cargo check`, `cargo build`, `xtask`, and other compile-heavy commands, set the initial `yield_time_ms` high enough to cover the expected runtime instead of polling every few seconds or every minute.\n\
- Do not babysit background compiles with repeated `write_stdin` polls when a single longer wait would do. Prefer one generous wait over many short polls.\n\
- Do not start a second Cargo compile/test/check command while another Cargo command is still running in the same lane unless the issue explicitly requires it.\n\
- If the workspace contains conflict markers, unmerged files, or other repair debt from a prior attempt, fix that workspace integrity problem first before resuming feature work.\n\
- If `apply_patch` verification fails repeatedly, stop repeating the same patch shape. Re-read the file on disk and switch to smaller exact-context edits or a targeted full-file rewrite.\n\
- Before changing task or issue completion state, run a targeted grep or equivalent acceptance check against each acceptance criterion so shipping status cannot outrun actual delivery.\n\
- Never rewrite `IMPLEMENTATION_PLAN.md` prose. The only allowed plan edit is changing the matching task line from `- [ ]` or `- [!]` to `- [x]` when that task is actually complete. Do not edit repo-level rules, acceptance criteria, verification blocks, dependencies, scope boundaries, or unrelated task statuses.\n\
- If you touch `IMPLEMENTATION_PLAN.md`, run `scripts/check-plan-integrity.sh` before landing and fix any reported drift.\n\
- Use the inherited shared `CARGO_TARGET_DIR` from Symphony for Cargo commands. Do not override it with workspace-local or ad hoc temp paths, and do not create `/.cargo-target/` inside the repo clone. If that directory appears, delete it before landing.\n\
- If repo docs mention a fresh isolated Cargo target dir for local development, that guidance is overridden in Symphony sessions. Never prefix Cargo with a different `CARGO_TARGET_DIR`, never invent `/.cargo-target*` variants such as `/.cargo-target-rso29/`, and if `/.cargo-target` is present in the repo clone it must remain the shared `../.cargo-target` symlink.\n\
- If the repo contains `scripts/run-task-verification.sh`, run the concrete executable verification commands through that wrapper instead of invoking them bare. Do not treat narrative `Verification:` prose as literal shell input; if the task only gives prose, derive the narrowest truthful executable proof yourself and record blockers honestly instead of patching the wrapper.\n\
- Never hand-edit verification receipt files. They are execution evidence, not notes.\n\
- If the repo contains `scripts/check-task-scope.py`, run `python3 scripts/check-task-scope.py --staged` before landing. If adjacent integration edits outside the owned or touchpoint surfaces are genuinely required, keep them minimal and record them under `Scope exceptions:` in the task's `REVIEW.md` handoff with a one-line reason per path.\n\
- A task is only ready for `- [x]` or a terminal issue state when local review handoff, verification evidence, and declared completion artifacts are all present. If any of that evidence is still missing, leave the task as `- [~]` or unfinished instead of bluffing it done.\n\
- When the task is complete, mark the matching task in `IMPLEMENTATION_PLAN.md` as `- [x]` instead of deleting it so downstream dependency truth remains visible.\n\
- Append a `REVIEW.md` handoff entry before landing. Preserve the existing file style when present; if `REVIEW.md` is missing, create it with a simple awaiting-review section. Include the task id, changed files or surfaces, `Scope exceptions: none` or the explicit exception list, the exact validation commands you actually ran, and any remaining blockers or `none`.\n\
- When the task is complete, run the verification required by the issue description, then call `symphony_land_issue` with `{{\"baseBranch\":\"{base_branch}\",\"doneState\":\"{done_state}\"}}`. That host-side tool commits the implementation plus the `IMPLEMENTATION_PLAN.md` and `REVIEW.md` artifact updates, rebases onto `origin/{base_branch}`, pushes, and only then moves the issue to `{done_state}`.\n\
- If `symphony_land_issue` reports a rebase conflict, stop retrying the same land immediately. Inspect the conflicting files against `origin/{base_branch}`, integrate the latest base-branch changes into your workspace, rerun targeted validation, and only then try landing again.\n\
- Before starting another exploration turn, inspect the current diff and outstanding acceptance criteria. If the same blocker persists across two consecutive turns or a turn ends without new diff or verification progress, stop looping, leave one precise Linear comment, and move the issue to blocked if such a state exists.\n\
- If validation fails, fix the issue instead of leaving partial work behind.\n\
- Final response should contain only: changed files, validation run, and any remaining blockers.\n\n\
Use these exact GraphQL operations when you need to inspect states or update the issue state:\n\n\
```graphql\n\
query IssueContext($id: String!) {{\n\
  issue(id: $id) {{\n\
    id\n\
    identifier\n\
    state {{\n\
      name\n\
    }}\n\
    team {{\n\
      states(first: 50) {{\n\
        nodes {{\n\
          id\n\
          name\n\
          type\n\
        }}\n\
      }}\n\
    }}\n\
  }}\n\
}}\n\
```\n\n\
```graphql\n\
mutation UpdateIssueState($id: String!, $stateId: String!) {{\n\
  issueUpdate(id: $id, input: {{stateId: $stateId}}) {{\n\
    success\n\
  }}\n\
}}\n\
```\n\n\
```graphql\n\
mutation AddComment($issueId: String!, $body: String!) {{\n\
  commentCreate(input: {{issueId: $issueId, body: $body}}) {{\n\
    success\n\
  }}\n\
}}\n\
```\n",
        project_slug = spec.project_slug,
        project_slug_yaml = yaml_double_quote(spec.project_slug),
        todo_state = spec.todo_state,
        todo_state_yaml = yaml_double_quote(spec.todo_state),
        in_progress_state = spec.in_progress_state,
        in_progress_state_yaml = yaml_double_quote(spec.in_progress_state),
        done_state = spec.done_state,
        done_state_yaml = yaml_double_quote(spec.done_state),
        poll_interval_ms = spec.poll_interval_ms,
        remote_url = shell_quote(spec.remote_url),
        base_branch = spec.base_branch,
        before_run_hook = before_run_hook,
        max_concurrent_agents = spec.max_concurrent_agents,
        codex_command = codex_command,
        repo_label = spec.repo_label,
        repo_root_text = repo_root_text,
        workspace_root_yaml = workspace_root_yaml,
        shared_cargo_target_dir_yaml = shared_cargo_target_dir_yaml,
        blocked_state_line = blocked_state_line,
    ))
}

fn shared_cargo_target_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".cargo-target")
}

fn validate_workflow_render_spec(spec: &WorkflowRenderSpec<'_>) -> Result<()> {
    validate_single_line_scalar("repo label", spec.repo_label)?;
    validate_single_line_scalar("project slug", spec.project_slug)?;
    validate_single_line_scalar("remote URL", spec.remote_url)?;
    validate_branch_name(spec.base_branch)?;
    validate_token_scalar("model", spec.model)?;
    validate_token_scalar("reasoning effort", spec.reasoning_effort)?;
    validate_single_line_scalar("todo state", spec.todo_state)?;
    validate_single_line_scalar("in-progress state", spec.in_progress_state)?;
    validate_single_line_scalar("done state", spec.done_state)?;
    if let Some(blocked_state) = spec.blocked_state {
        validate_single_line_scalar("blocked state", blocked_state)?;
    }
    path_text("repo root", spec.repo_root)?;
    path_text("workspace root", spec.workspace_root)?;
    Ok(())
}

fn validate_branch_name(branch: &str) -> Result<()> {
    validate_single_line_scalar("base branch", branch)?;
    if branch.starts_with('-') || !branch.chars().all(is_safe_branch_char) {
        bail!("invalid base branch `{branch}`; use only letters, digits, '.', '-', '_', or '/'");
    }
    if branch.starts_with('/')
        || branch.ends_with('/')
        || branch.contains("..")
        || branch.contains("//")
        || branch.contains("@{")
        || branch.contains('\\')
    {
        bail!(
            "invalid base branch `{branch}`; use a plain branch name without shell metacharacters or git ref punctuation"
        );
    }
    Ok(())
}

fn validate_token_scalar(label: &str, value: &str) -> Result<()> {
    validate_single_line_scalar(label, value)?;
    if value.starts_with('-') || !value.chars().all(is_safe_token_char) {
        bail!(
            "invalid {label} `{value}`; use only letters, digits, '.', '-', '_', '/', ':', or '+'"
        );
    }
    Ok(())
}

fn validate_single_line_scalar(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("invalid {label}; value must not be empty");
    }
    if value
        .chars()
        .any(|ch| ch == '\n' || ch == '\r' || ch.is_control())
    {
        bail!("invalid {label}; value must be a single line without control characters");
    }
    Ok(())
}

fn path_text(label: &str, path: &Path) -> Result<String> {
    let text = path.display().to_string();
    validate_single_line_scalar(label, &text)?;
    Ok(text)
}

fn is_safe_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '/' | ':' | '+')
}

fn is_safe_branch_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '/')
}

fn yaml_double_quote(raw: &str) -> String {
    format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
}

fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\"'\"'"))
}

impl LinearProject {
    fn state_id(&self, state_name: &str) -> Option<String> {
        self.team
            .states
            .iter()
            .find(|state| normalize_name(&state.name) == normalize_name(state_name))
            .map(|state| state.id.clone())
    }

    fn terminal_state_names(&self) -> HashSet<String> {
        self.team
            .states
            .iter()
            .filter(|state| {
                state.state_type.as_deref().is_some_and(|kind| {
                    matches!(
                        normalize_name(kind).as_str(),
                        "completed" | "canceled" | "cancelled"
                    )
                })
            })
            .map(|state| state.name.clone())
            .collect()
    }
}

impl LinearGraphqlClient {
    fn from_env() -> Result<Self> {
        let api_key = std::env::var("LINEAR_API_KEY")
            .context("LINEAR_API_KEY is not set in the current environment")?;
        Ok(Self {
            http: Client::new(),
            api_key,
        })
    }

    async fn fetch_project(&self, project_slug: &str) -> Result<LinearProject> {
        let payload = self
            .graphql(FETCH_PROJECT_QUERY, json!({ "slug": project_slug }))
            .await?;
        let project = payload
            .get("projects")
            .and_then(|value| value.get("nodes"))
            .and_then(Value::as_array)
            .and_then(|nodes| nodes.first())
            .ok_or_else(|| anyhow!("Linear project `{project_slug}` not found"))?;
        parse_project(project)
    }

    async fn fetch_project_issues(&self, project_slug: &str) -> Result<Vec<LinearIssue>> {
        let mut issues = Vec::new();
        let mut after = None::<String>;

        loop {
            let payload = self
                .graphql(
                    FETCH_PROJECT_ISSUES_QUERY,
                    json!({
                        "slug": project_slug,
                        "first": 100,
                        "after": after,
                    }),
                )
                .await?;
            let connection = payload.get("issues").ok_or_else(|| {
                anyhow!("Linear issues payload missing for project `{project_slug}`")
            })?;
            let nodes = connection
                .get("nodes")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    anyhow!("Linear issues nodes payload malformed for project `{project_slug}`")
                })?;
            for node in nodes {
                issues.push(parse_issue(node)?);
            }
            let page_info = connection
                .get("pageInfo")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    anyhow!("Linear pageInfo payload malformed for project `{project_slug}`")
                })?;
            let has_next = page_info
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            after = page_info
                .get("endCursor")
                .and_then(Value::as_str)
                .map(|value| value.to_string());
            if !has_next {
                break;
            }
        }

        Ok(issues)
    }

    async fn create_issue(
        &self,
        team_id: &str,
        project_id: &str,
        state_id: &str,
        title: &str,
        description: &str,
        priority: i64,
    ) -> Result<LinearIssue> {
        let payload = self
            .graphql(
                CREATE_ISSUE_MUTATION,
                json!({
                    "teamId": team_id,
                    "projectId": project_id,
                    "stateId": state_id,
                    "title": title,
                    "description": description,
                    "priority": priority,
                }),
            )
            .await?;
        let issue = payload
            .get("issueCreate")
            .and_then(|value| value.get("issue"))
            .ok_or_else(|| anyhow!("Linear issueCreate response missing issue payload"))?;
        parse_issue(issue)
    }

    async fn update_issue(
        &self,
        issue_id: &str,
        title: &str,
        description: &str,
        priority: i64,
        state_id: Option<&str>,
    ) -> Result<LinearIssue> {
        let payload = self
            .graphql(
                if state_id.is_some() {
                    UPDATE_ISSUE_AND_STATE_MUTATION
                } else {
                    UPDATE_ISSUE_MUTATION
                },
                match state_id {
                    Some(state_id) => json!({
                        "id": issue_id,
                        "title": title,
                        "description": description,
                        "priority": priority,
                        "stateId": state_id,
                    }),
                    None => json!({
                        "id": issue_id,
                        "title": title,
                        "description": description,
                        "priority": priority,
                    }),
                },
            )
            .await?;
        let issue = payload
            .get("issueUpdate")
            .and_then(|value| value.get("issue"))
            .ok_or_else(|| anyhow!("Linear issueUpdate response missing issue payload"))?;
        parse_issue(issue)
    }

    async fn archive_issue(&self, issue_id: &str) -> Result<()> {
        let payload = self
            .graphql(ARCHIVE_ISSUE_MUTATION, json!({ "id": issue_id }))
            .await?;
        let archive = payload
            .get("issueArchive")
            .ok_or_else(|| anyhow!("Linear issueArchive response missing payload"))?;
        if !archive
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            bail!("Linear issueArchive returned success=false");
        }
        Ok(())
    }

    async fn unarchive_issue(&self, issue_id: &str) -> Result<()> {
        let payload = self
            .graphql(UNARCHIVE_ISSUE_MUTATION, json!({ "id": issue_id }))
            .await?;
        let unarchive = payload
            .get("issueUnarchive")
            .ok_or_else(|| anyhow!("Linear issueUnarchive response missing payload"))?;
        if !unarchive
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            bail!("Linear issueUnarchive returned success=false");
        }
        Ok(())
    }

    async fn create_blocks_relation(
        &self,
        blocker_issue_id: &str,
        blocked_issue_id: &str,
    ) -> Result<()> {
        let payload = self
            .graphql(
                CREATE_RELATION_MUTATION,
                json!({
                    "issueId": blocker_issue_id,
                    "relatedIssueId": blocked_issue_id,
                    "type": RELATION_BLOCKS,
                }),
            )
            .await?;
        let success = payload
            .get("issueRelationCreate")
            .and_then(|value| value.get("success"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if success {
            Ok(())
        } else {
            bail!("Linear issueRelationCreate returned success=false")
        }
    }

    async fn delete_relation(&self, relation_id: &str) -> Result<()> {
        let payload = self
            .graphql(DELETE_RELATION_MUTATION, json!({ "id": relation_id }))
            .await?;
        let success = payload
            .get("issueRelationDelete")
            .and_then(|value| value.get("success"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if success {
            Ok(())
        } else {
            bail!("Linear issueRelationDelete returned success=false")
        }
    }

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let resp = self
            .http
            .post(LINEAR_API_URL)
            .header("Authorization", &self.api_key)
            .json(&json!({
                "query": query,
                "variables": variables,
            }))
            .send()
            .await
            .context("failed to send Linear GraphQL request")?;

        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .context("failed to decode Linear GraphQL response body")?;

        if !status.is_success() {
            bail!("Linear GraphQL request failed with status {status}: {body}");
        }
        if let Some(errors) = body.get("errors") {
            bail!("Linear GraphQL returned errors: {errors}");
        }
        body.get("data")
            .cloned()
            .ok_or_else(|| anyhow!("Linear GraphQL response missing data payload"))
    }
}

fn parse_project(value: &Value) -> Result<LinearProject> {
    let id = required_string(value, "id")?;
    let name = required_string(value, "name")?;
    let slug = required_string(value, "slugId")?;
    let team_value = value
        .get("teams")
        .and_then(|value| value.get("nodes"))
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
        .ok_or_else(|| anyhow!("Linear project missing teams payload"))?;
    let team_id = required_string(team_value, "id")?;
    let team_key = optional_string(team_value, "key");
    let team_name = optional_string(team_value, "name");
    let states = team_value
        .get("states")
        .and_then(|value| value.get("nodes"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Linear team states payload malformed"))?
        .iter()
        .map(parse_state)
        .collect::<Result<Vec<_>>>()?;

    Ok(LinearProject {
        id,
        name,
        slug,
        team: LinearTeam {
            id: team_id,
            key: team_key,
            name: team_name,
            states,
        },
    })
}

fn parse_state(value: &Value) -> Result<LinearState> {
    Ok(LinearState {
        id: required_string(value, "id")?,
        name: required_string(value, "name")?,
        state_type: optional_string(value, "type"),
    })
}

fn parse_issue(value: &Value) -> Result<LinearIssue> {
    let blocked_by = value
        .get("inverseRelations")
        .and_then(|value| value.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| nodes.iter().filter_map(parse_blocker).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(LinearIssue {
        id: required_string(value, "id")?,
        identifier: optional_string(value, "identifier"),
        title: required_string(value, "title")?,
        description: required_string(value, "description").unwrap_or_default(),
        archived_at: optional_string(value, "archivedAt"),
        priority: value.get("priority").and_then(Value::as_i64),
        state: value
            .get("state")
            .and_then(|state| optional_string(state, "name")),
        blocked_by,
    })
}

fn parse_blocker(value: &Value) -> Option<LinearBlocker> {
    let relation_type = value.get("type")?.as_str()?;
    if normalize_name(relation_type) != RELATION_BLOCKS {
        return None;
    }
    let issue = value.get("issue")?;
    Some(LinearBlocker {
        relation_id: required_string(value, "id").ok()?,
        id: required_string(issue, "id").ok()?,
        identifier: optional_string(issue, "identifier"),
        state: issue
            .get("state")
            .and_then(|state| optional_string(state, "name")),
    })
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(|text| text.to_string())
        .ok_or_else(|| anyhow!("missing string field `{field}`"))
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(|text| text.to_string())
}

fn normalize_name(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{
        completed_plan_issue_updates, extract_agent_message_from_codex_stream,
        fallback_task_priorities, issue_requires_reactivation, issue_task_id_from_description,
        mark_tasks_done_in_plan, markdown_front_matter, normalize_planner_response, parse_tasks,
        render_issue_description, render_workflow_markdown, resolve_symphony_root,
        review_contains_task, shell_quote, single_line_excerpt, LinearIssue, PlannerResponse,
        PlannerTask, SymphonyTask, TaskStatus, WorkflowRenderSpec, SYMPHONY_ROOT_ENV,
    };
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    struct EnvRestore {
        previous: Option<OsString>,
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(SYMPHONY_ROOT_ENV, previous);
            } else {
                std::env::remove_var(SYMPHONY_ROOT_ENV);
            }
        }
    }

    fn symphony_root_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn replace_symphony_root_env(value: Option<&str>) -> EnvRestore {
        let previous = std::env::var_os(SYMPHONY_ROOT_ENV);
        if let Some(value) = value {
            std::env::set_var(SYMPHONY_ROOT_ENV, value);
        } else {
            std::env::remove_var(SYMPHONY_ROOT_ENV);
        }
        EnvRestore { previous }
    }

    #[test]
    fn run_requires_symphony_root_when_unset() {
        let _guard = symphony_root_env_lock().lock().expect("env lock");
        let _restore = replace_symphony_root_env(None);

        let error = resolve_symphony_root(None).expect_err("missing root should fail");
        let message = error.to_string();

        assert!(message.contains("missing symphony root"));
        assert!(message.contains("--symphony-root <path>"));
        assert!(message.contains("AUTODEV_SYMPHONY_ROOT=<path>"));
    }

    #[test]
    fn run_uses_symphony_root_env_when_arg_missing() {
        let _guard = symphony_root_env_lock().lock().expect("env lock");
        let _restore = replace_symphony_root_env(Some("/tmp/autodev-symphony"));

        let root = resolve_symphony_root(None).expect("env root should resolve");

        assert_eq!(root, PathBuf::from("/tmp/autodev-symphony"));
    }

    #[test]
    fn run_symphony_root_arg_overrides_env() {
        let _guard = symphony_root_env_lock().lock().expect("env lock");
        let _restore = replace_symphony_root_env(Some("/tmp/autodev-env-symphony"));

        let root = resolve_symphony_root(Some(PathBuf::from("/tmp/autodev-cli-symphony")))
            .expect("explicit root should resolve");

        assert_eq!(root, PathBuf::from("/tmp/autodev-cli-symphony"));
    }

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

    #[test]
    fn fallback_priorities_follow_dependency_waves() {
        let tasks = vec![
            SymphonyTask {
                id: "P-001".to_string(),
                title: "foundation".to_string(),
                status: TaskStatus::Pending,
                dependencies: Vec::new(),
                markdown: String::new(),
            },
            SymphonyTask {
                id: "P-002".to_string(),
                title: "depends on foundation".to_string(),
                status: TaskStatus::Pending,
                dependencies: vec!["P-001".to_string()],
                markdown: String::new(),
            },
            SymphonyTask {
                id: "P-003".to_string(),
                title: "deep dependency".to_string(),
                status: TaskStatus::Pending,
                dependencies: vec!["P-002".to_string()],
                markdown: String::new(),
            },
        ];
        let priorities = fallback_task_priorities(&tasks);
        assert_eq!(priorities.get("P-001"), Some(&1));
        assert_eq!(priorities.get("P-002"), Some(&2));
        assert_eq!(priorities.get("P-003"), Some(&3));
    }

    #[test]
    fn normalize_planner_response_keeps_explicit_machine_dependencies() {
        let tasks = vec![
            SymphonyTask {
                id: "P-001".to_string(),
                title: "foundation".to_string(),
                status: TaskStatus::Pending,
                dependencies: Vec::new(),
                markdown: String::new(),
            },
            SymphonyTask {
                id: "P-002".to_string(),
                title: "feature".to_string(),
                status: TaskStatus::Pending,
                dependencies: vec!["P-001".to_string()],
                markdown: String::new(),
            },
        ];
        let response = PlannerResponse {
            strategy_summary: "test".to_string(),
            tasks: vec![
                PlannerTask {
                    task_id: "P-001".to_string(),
                    priority: 1,
                    dependencies: Vec::new(),
                    external_dependencies: Vec::new(),
                    rationale: "foundation".to_string(),
                },
                PlannerTask {
                    task_id: "P-002".to_string(),
                    priority: 2,
                    dependencies: vec!["P-003".to_string()],
                    external_dependencies: vec!["EXT-1".to_string()],
                    rationale: "feature".to_string(),
                },
            ],
        };

        let normalized = normalize_planner_response(&tasks, response).expect("planner response");
        assert_eq!(
            normalized.task_plans["P-002"].dependencies,
            vec!["P-001".to_string()]
        );
        assert_eq!(
            normalized.task_plans["P-002"].external_dependencies,
            vec!["EXT-1".to_string()]
        );
    }

    #[test]
    fn codex_agent_message_extraction_skips_banner_lines() {
        let raw = r#"Reading prompt from stdin...
{"type":"thread.started","thread_id":"abc"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"ok\":true}"}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":1}}
"#;
        assert_eq!(
            extract_agent_message_from_codex_stream(raw),
            Some("{\"ok\":true}".to_string())
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
    fn workflow_render_is_repo_specific() {
        let repo_root = PathBuf::from("/home/r/Coding/autonomy");
        let workspace_root = PathBuf::from("/tmp/symphony-workspaces/autonomy");
        let markdown = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5",
            "high",
        ))
        .expect("workflow should render");
        assert!(markdown.contains("project_slug: \"autonomy-symphony\""));
        assert!(markdown.contains("git clone --depth 1 'git@github.com:example/autonomy.git' ."));
        assert!(markdown.contains("mkdir -p '/tmp/symphony-workspaces/autonomy/.cargo-target'"));
        assert!(markdown.contains("printf '/.cargo-target\\n' >> .git/info/exclude"));
        assert!(markdown.contains("printf '/.cargo-target*\\n' >> .git/info/exclude"));
        assert!(markdown.contains("removing repo-local cargo target path $stale_cargo_target"));
        assert!(markdown.contains("ln -s ../.cargo-target .cargo-target"));
        assert!(markdown.contains("git fetch origin 'trunk'"));
        assert!(markdown.contains("git rev-list --count 'origin/trunk..HEAD'"));
        assert!(markdown.contains("should_rebase=1"));
        assert!(markdown.contains("git reset --mixed \"$merge_base\""));
        assert!(markdown.contains("unfinished git operation detected"));
        assert!(markdown.contains("unmerged index entries detected"));
        assert!(markdown.contains("if ! git diff --quiet || ! git diff --cached --quiet; then"));
        assert!(markdown.contains("restoring them to workspace changes before continuing"));
        assert!(markdown.contains("skipping rebase sync to preserve local changes"));
        assert!(markdown.contains("root: \"/tmp/symphony-workspaces/autonomy\""));
        assert!(markdown.contains("Failure context from the previous attempt"));
        assert!(markdown.contains("Recovery guidance"));
        assert!(markdown.contains("mark the matching task in `IMPLEMENTATION_PLAN.md` as `- [x]`"));
        assert!(markdown
            .contains("Fresh workspaces are synced from `origin/trunk` before the first turn."));
        assert!(markdown.contains("If you are resuming a dirty workspace after a retry or stall"));
        assert!(markdown.contains("Never run `git fetch`, `git pull`, `git rebase`, `git push`, or branch-switching commands yourself"));
        assert!(markdown.contains("Do not run the final `git add` or `git commit` flow yourself"));
        assert!(markdown.contains("Never request interactive user input or MCP elicitation"));
        assert!(markdown.contains("Do not keep multiple long-running shell sessions alive at once"));
        assert!(markdown
            .contains("Do not babysit background compiles with repeated `write_stdin` polls"));
        assert!(markdown.contains("Do not start a second Cargo compile/test/check command"));
        assert!(markdown.contains("Build a short task brief for yourself before editing"));
        assert!(markdown.contains("One issue = one task = one landing attempt"));
        assert!(markdown.contains("Do not mark adjacent tasks done"));
        assert!(markdown.contains("If `apply_patch` verification fails repeatedly"));
        assert!(markdown.contains("Never rewrite `IMPLEMENTATION_PLAN.md` prose"));
        assert!(markdown.contains("run `scripts/check-plan-integrity.sh` before landing"));
        assert!(markdown.contains("Use the inherited shared `CARGO_TARGET_DIR` from Symphony"));
        assert!(markdown.contains("do not create `/.cargo-target/` inside the repo clone"));
        assert!(markdown.contains("If repo docs mention a fresh isolated Cargo target dir"));
        assert!(markdown.contains("never invent `/.cargo-target*` variants"));
        assert!(markdown.contains("If the repo contains `scripts/run-task-verification.sh`"));
        assert!(markdown.contains("Never hand-edit verification receipt files"));
        assert!(markdown.contains("If the repo contains `scripts/check-task-scope.py`"));
        assert!(markdown.contains("Scope exceptions: none"));
        assert!(markdown.contains("Append a `REVIEW.md` handoff entry before landing."));
        assert!(markdown.contains("If the same blocker persists across two consecutive turns"));
        assert!(markdown.contains("max_turn_wall_clock_ms: 1800000"));
        assert!(markdown.contains("max_turn_total_tokens: 12000000"));
        assert!(markdown.contains("read_timeout_ms: 60000"));
        assert!(markdown.contains("command: >-"));
        assert!(markdown.contains("turn_sandbox_policy:"));
        assert!(markdown.contains("writableRoots:"));
        assert!(markdown.contains("      - \"/tmp/symphony-workspaces/autonomy\""));
        assert!(markdown.contains("      - \"/tmp/symphony-workspaces/autonomy/.cargo-target\""));
        assert!(markdown.contains("env CARGO_TARGET_DIR="));
        assert!(markdown.contains("'/tmp/symphony-workspaces/autonomy/.cargo-target'"));
        assert!(markdown.contains(
            "call `symphony_land_issue` with `{\"baseBranch\":\"trunk\",\"doneState\":\"Done\"}`"
        ));
        assert!(markdown.contains("If `symphony_land_issue` reports a rebase conflict"));
    }

    #[test]
    fn workflow_render_rejects_hostile_branch() {
        let repo_root = PathBuf::from("/home/r/Coding/autonomy");
        let workspace_root = PathBuf::from("/tmp/symphony-workspaces/autonomy");
        let error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "main; touch /tmp/pwned",
            "gpt-5.5",
            "high",
        ))
        .expect_err("hostile branch should be rejected");

        assert!(error.to_string().contains("base branch"));
    }

    #[test]
    fn workflow_render_rejects_hostile_model_and_effort() {
        let repo_root = PathBuf::from("/home/r/Coding/autonomy");
        let workspace_root = PathBuf::from("/tmp/symphony-workspaces/autonomy");

        let model_error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5 --dangerously-bypass-approvals-and-sandbox",
            "high",
        ))
        .expect_err("hostile model should be rejected");
        assert!(model_error.to_string().contains("model"));

        let effort_error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5",
            "high\nwritableRoots:",
        ))
        .expect_err("hostile effort should be rejected");
        assert!(effort_error.to_string().contains("reasoning effort"));

        let remote_error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git\n  timeout_ms: 1",
            "trunk",
            "gpt-5.5",
            "high",
        ))
        .expect_err("hostile remote URL should be rejected");
        assert!(remote_error.to_string().contains("remote URL"));

        let hostile_workspace_root = PathBuf::from("/tmp/symphony\nhooks:");
        let path_error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            hostile_workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5",
            "high",
        ))
        .expect_err("hostile path should be rejected");
        assert!(path_error.to_string().contains("workspace root"));

        let quoted_workspace_root = PathBuf::from("/tmp/symphony workspaces/auto'quote");
        let markdown = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            quoted_workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5",
            "high",
        ))
        .expect("paths with spaces and quotes should render safely");
        assert!(
            markdown.contains("mkdir -p '/tmp/symphony workspaces/auto'\"'\"'quote/.cargo-target'")
        );
        assert!(markdown.contains("root: \"/tmp/symphony workspaces/auto'quote\""));
    }

    fn test_workflow_spec<'a>(
        repo_root: &'a std::path::Path,
        workspace_root: &'a std::path::Path,
        remote_url: &'a str,
        base_branch: &'a str,
        model: &'a str,
        reasoning_effort: &'a str,
    ) -> WorkflowRenderSpec<'a> {
        WorkflowRenderSpec {
            repo_root,
            repo_label: "autonomy",
            project_slug: "autonomy-symphony",
            remote_url,
            base_branch,
            workspace_root,
            poll_interval_ms: 5000,
            max_concurrent_agents: 1,
            model,
            reasoning_effort,
            todo_state: "Todo",
            in_progress_state: "In Progress",
            done_state: "Done",
            blocked_state: Some("Backlog"),
        }
    }

    #[test]
    fn markdown_front_matter_extracts_project_slug() {
        let workflow = r#"---
tracker:
  kind: linear
  project_slug: "autonomy-symphony"
---

body
"#;
        let front_matter = markdown_front_matter(workflow).expect("front matter");
        assert!(front_matter.contains("project_slug: \"autonomy-symphony\""));
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
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
