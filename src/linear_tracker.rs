use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::symphony_command::{parse_tasks, render_issue_title, TaskStatus};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";
const TASK_SENTINEL_PREFIX: &str = "<!-- auto-symphony:";
const DEFAULT_IN_PROGRESS_STATE: &str = "In Progress";
const DEFAULT_DONE_STATE: &str = "Done";
const GENERIC_TERMINAL_STATES: [&str; 4] = ["closed", "cancelled", "canceled", "duplicate"];

const FETCH_PROJECT_QUERY: &str = r#"
query AutoParallelProject($slug: String!) {
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
query AutoParallelProjectIssues($slug: String!, $first: Int!, $after: String) {
  issues(filter: {project: {slugId: {eq: $slug}}}, first: $first, after: $after) {
    nodes {
      id
      identifier
      title
      description
      state {
        name
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

const UPDATE_ISSUE_STATE_MUTATION: &str = r#"
mutation AutoParallelUpdateIssueState($id: String!, $stateId: String!) {
  issueUpdate(id: $id, input: {stateId: $stateId}) {
    success
    issue {
      id
      state {
        name
      }
    }
  }
}
"#;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct WorkflowConfig {
    project_slug: Option<String>,
    active_states: Vec<String>,
    terminal_states: Vec<String>,
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
    states: Vec<LinearState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinearProject {
    slug: String,
    team: LinearTeam,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinearIssue {
    id: String,
    title: String,
    description: String,
    state: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrackedIssue {
    id: String,
    title: String,
    description: String,
    state: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LinearCoverageDrift {
    pub(crate) missing_task_ids: Vec<String>,
    pub(crate) stale_task_ids: Vec<String>,
    pub(crate) terminal_task_ids: Vec<String>,
}

impl LinearCoverageDrift {
    pub(crate) fn is_empty(&self) -> bool {
        self.missing_task_ids.is_empty()
            && self.stale_task_ids.is_empty()
            && self.terminal_task_ids.is_empty()
    }
}

#[derive(Clone)]
struct LinearGraphqlClient {
    http: Client,
    api_key: String,
}

pub(crate) struct LinearTracker {
    client: LinearGraphqlClient,
    project: LinearProject,
    in_progress_state_id: String,
    in_progress_state_name: String,
    done_state_id: String,
    done_state_name: String,
    terminal_state_names: HashSet<String>,
    issues_by_task_id: HashMap<String, TrackedIssue>,
    last_plan_fingerprint: Option<u64>,
    last_auto_sync_attempt_fingerprint: Option<u64>,
}

impl LinearTracker {
    pub(crate) async fn maybe_from_repo(repo_root: &Path) -> Result<Option<Self>> {
        if std::env::var("LINEAR_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .is_none()
        {
            return Ok(None);
        }

        let config = read_workflow_config(repo_root)?;
        let Some(project_slug) = config.project_slug.clone() else {
            return Ok(None);
        };

        let client = LinearGraphqlClient::from_env()?;
        let project = client.fetch_project(&project_slug).await?;
        let in_progress_state_name = derive_in_progress_state_name(&config);
        let done_state_name = derive_done_state_name(&config);
        let in_progress_state_id = project.state_id(&in_progress_state_name).ok_or_else(|| {
            anyhow!(
                "Linear project `{}` does not expose state `{}`",
                project.slug,
                in_progress_state_name
            )
        })?;
        let done_state_id = project.state_id(&done_state_name).ok_or_else(|| {
            anyhow!(
                "Linear project `{}` does not expose state `{}`",
                project.slug,
                done_state_name
            )
        })?;
        let mut tracker = Self {
            client,
            terminal_state_names: project.terminal_state_names(),
            project,
            in_progress_state_id,
            in_progress_state_name,
            done_state_id,
            done_state_name,
            issues_by_task_id: HashMap::new(),
            last_plan_fingerprint: None,
            last_auto_sync_attempt_fingerprint: None,
        };
        tracker.refresh_issues().await?;
        Ok(Some(tracker))
    }

    pub(crate) fn summary(&self) -> String {
        format!(
            "host sync -> Linear project `{}` (dispatch `{}`, done `{}`)",
            self.project.slug, self.in_progress_state_name, self.done_state_name
        )
    }

    pub(crate) async fn refresh_if_plan_changed(&mut self, plan_text: &str) -> Result<()> {
        let fingerprint = plan_fingerprint(plan_text);
        if self.last_plan_fingerprint == Some(fingerprint) {
            return Ok(());
        }
        self.refresh_issues().await?;
        self.last_plan_fingerprint = Some(fingerprint);
        Ok(())
    }

    pub(crate) async fn refresh_after_sync(&mut self, plan_text: &str) -> Result<()> {
        self.refresh_issues().await?;
        let fingerprint = plan_fingerprint(plan_text);
        self.last_plan_fingerprint = Some(fingerprint);
        self.last_auto_sync_attempt_fingerprint = Some(fingerprint);
        Ok(())
    }

    pub(crate) fn coverage_drift(
        &self,
        plan_text: &str,
    ) -> LinearCoverageDrift {
        let mut drift = LinearCoverageDrift::default();
        for task in parse_tasks(plan_text)
            .into_iter()
            .filter(|task| task.status == TaskStatus::Pending)
        {
            let Some(issue) = self.issues_by_task_id.get(&task.id) else {
                drift.missing_task_ids.push(task.id);
                continue;
            };
            if issue
                .state
                .as_deref()
                .is_some_and(|state| self.terminal_state_names.contains(state))
            {
                drift.terminal_task_ids.push(task.id);
                continue;
            }
            let expected_title = render_issue_title(&task);
            let expected_markdown_block = format!("\n---\n\n{}\n", task.markdown);
            if issue.title != expected_title
                || !issue.description.contains(&expected_markdown_block)
            {
                drift.stale_task_ids.push(task.id);
            }
        }
        drift
    }

    pub(crate) fn should_attempt_auto_sync(&self, plan_text: &str) -> bool {
        self.last_auto_sync_attempt_fingerprint != Some(plan_fingerprint(plan_text))
    }

    pub(crate) fn mark_auto_sync_attempt(&mut self, plan_text: &str) {
        self.last_auto_sync_attempt_fingerprint = Some(plan_fingerprint(plan_text));
    }

    pub(crate) async fn note_dispatch(&mut self, task_id: &str) -> Result<()> {
        let Some(issue) = self.issues_by_task_id.get_mut(task_id) else {
            return Ok(());
        };
        if issue.state.as_deref().is_some_and(|state| {
            normalize_name(state) == normalize_name(&self.in_progress_state_name)
        }) {
            return Ok(());
        }
        if issue
            .state
            .as_deref()
            .is_some_and(|state| self.terminal_state_names.contains(state))
        {
            return Ok(());
        }
        let updated_state = self
            .client
            .update_issue_state(&issue.id, &self.in_progress_state_id)
            .await?;
        issue.state = Some(updated_state.unwrap_or_else(|| self.in_progress_state_name.clone()));
        Ok(())
    }

    pub(crate) async fn note_done(&mut self, task_id: &str) -> Result<()> {
        let Some(issue) = self.issues_by_task_id.get_mut(task_id) else {
            return Ok(());
        };
        let updated_state = self
            .client
            .update_issue_state(&issue.id, &self.done_state_id)
            .await?;
        issue.state = Some(updated_state.unwrap_or_else(|| self.done_state_name.clone()));
        Ok(())
    }

    async fn refresh_issues(&mut self) -> Result<()> {
        let issues = self.client.fetch_project_issues(&self.project.slug).await?;
        self.issues_by_task_id = issues
            .into_iter()
            .filter_map(|issue| {
                issue_task_id(&issue).map(|task_id| {
                    (
                        task_id,
                        TrackedIssue {
                            id: issue.id,
                            title: issue.title,
                            description: issue.description,
                            state: issue.state,
                        },
                    )
                })
            })
            .collect();
        Ok(())
    }
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

    async fn update_issue_state(&self, issue_id: &str, state_id: &str) -> Result<Option<String>> {
        let payload = self
            .graphql(
                UPDATE_ISSUE_STATE_MUTATION,
                json!({
                    "id": issue_id,
                    "stateId": state_id,
                }),
            )
            .await?;
        let update = payload
            .get("issueUpdate")
            .ok_or_else(|| anyhow!("Linear issueUpdate response missing payload"))?;
        let success = update
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !success {
            bail!("Linear issueUpdate returned success=false");
        }
        Ok(update
            .get("issue")
            .and_then(|issue| issue.get("state"))
            .and_then(|state| optional_string(state, "name")))
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

fn read_workflow_config(repo_root: &Path) -> Result<WorkflowConfig> {
    let workflow_path = repo_root.join(".auto").join("symphony").join("WORKFLOW.md");
    if !workflow_path.is_file() {
        return Ok(WorkflowConfig::default());
    }
    let text = fs::read_to_string(&workflow_path)
        .with_context(|| format!("failed to read {}", workflow_path.display()))?;
    let Some(front_matter) = markdown_front_matter(&text) else {
        return Ok(WorkflowConfig::default());
    };
    Ok(WorkflowConfig {
        project_slug: front_matter_scalar(front_matter, "project_slug"),
        active_states: front_matter_list(front_matter, "active_states"),
        terminal_states: front_matter_list(front_matter, "terminal_states"),
    })
}

fn derive_in_progress_state_name(config: &WorkflowConfig) -> String {
    config
        .active_states
        .get(1)
        .cloned()
        .or_else(|| config.active_states.last().cloned())
        .unwrap_or_else(|| DEFAULT_IN_PROGRESS_STATE.to_string())
}

fn derive_done_state_name(config: &WorkflowConfig) -> String {
    config
        .terminal_states
        .iter()
        .find(|state| !GENERIC_TERMINAL_STATES.contains(&normalize_name(state).as_str()))
        .cloned()
        .unwrap_or_else(|| DEFAULT_DONE_STATE.to_string())
}

fn markdown_front_matter(markdown: &str) -> Option<&str> {
    let stripped = markdown.strip_prefix("---\n")?;
    let end = stripped.find("\n---\n")?;
    Some(&stripped[..end])
}

fn front_matter_scalar(front_matter: &str, field: &str) -> Option<String> {
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

fn front_matter_list(front_matter: &str, field: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut collecting = false;
    let field_prefix = format!("{field}:");

    for line in front_matter.lines() {
        let trimmed = line.trim_start();
        if collecting {
            if let Some(value) = trimmed.strip_prefix('-') {
                let value = value.trim();
                if !value.is_empty() {
                    values.push(unquote_yamlish_scalar(value));
                }
                continue;
            }
            if trimmed.is_empty() {
                continue;
            }
            if !line.starts_with(' ') && !line.starts_with('\t') {
                break;
            }
            if !trimmed.starts_with('-') {
                break;
            }
        }
        if trimmed.starts_with(&field_prefix) {
            collecting = true;
        }
    }

    values
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

fn parse_project(value: &Value) -> Result<LinearProject> {
    let slug = required_string(value, "slugId")?;
    let team_value = value
        .get("teams")
        .and_then(|value| value.get("nodes"))
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
        .ok_or_else(|| anyhow!("Linear project missing teams payload"))?;
    let team_id = required_string(team_value, "id")?;
    let states = team_value
        .get("states")
        .and_then(|value| value.get("nodes"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Linear team states payload malformed"))?
        .iter()
        .map(parse_state)
        .collect::<Result<Vec<_>>>()?;
    Ok(LinearProject {
        slug,
        team: LinearTeam {
            id: team_id,
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
    Ok(LinearIssue {
        id: required_string(value, "id")?,
        title: required_string(value, "title")?,
        description: required_string(value, "description").unwrap_or_default(),
        state: value
            .get("state")
            .and_then(|state| optional_string(state, "name")),
    })
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

fn plan_fingerprint(plan_text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    plan_text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::{
        derive_done_state_name, derive_in_progress_state_name, front_matter_list,
        front_matter_scalar, issue_task_id_from_description, plan_fingerprint, WorkflowConfig,
    };

    #[test]
    fn parses_workflow_scalar_and_lists() {
        let front_matter = "tracker:\n  kind: linear\n  project_slug: \"alpha\"\n  active_states:\n    - Todo\n    - In Progress\n  terminal_states:\n    - Closed\n    - Done\n";
        assert_eq!(
            front_matter_scalar(front_matter, "project_slug"),
            Some("alpha".to_string())
        );
        assert_eq!(
            front_matter_list(front_matter, "active_states"),
            vec!["Todo".to_string(), "In Progress".to_string()]
        );
        assert_eq!(
            front_matter_list(front_matter, "terminal_states"),
            vec!["Closed".to_string(), "Done".to_string()]
        );
    }

    #[test]
    fn derives_state_names_from_workflow_config() {
        let config = WorkflowConfig {
            project_slug: Some("alpha".to_string()),
            active_states: vec!["Todo".to_string(), "Building".to_string()],
            terminal_states: vec![
                "Closed".to_string(),
                "Cancelled".to_string(),
                "Shipped".to_string(),
            ],
        };
        assert_eq!(derive_in_progress_state_name(&config), "Building");
        assert_eq!(derive_done_state_name(&config), "Shipped");
    }

    #[test]
    fn description_task_id_parser_reads_symphony_sentinel() {
        let description =
            "<!-- auto-symphony: repo=autonomy task_id=P-021 base_branch=trunk -->\n\nBody";
        assert_eq!(
            issue_task_id_from_description(description),
            Some("P-021".to_string())
        );
    }

    #[test]
    fn plan_fingerprint_changes_with_content() {
        assert_ne!(plan_fingerprint("a"), plan_fingerprint("b"));
        assert_eq!(plan_fingerprint("same"), plan_fingerprint("same"));
    }
}
