//! Linear GraphQL client, data model, and response parsing.

use std::collections::HashSet;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::symphony_command::queries::{
    ARCHIVE_ISSUE_MUTATION, CREATE_ISSUE_MUTATION, CREATE_RELATION_MUTATION,
    DELETE_RELATION_MUTATION, FETCH_PROJECT_ISSUES_QUERY, FETCH_PROJECT_QUERY,
    UNARCHIVE_ISSUE_MUTATION, UPDATE_ISSUE_AND_STATE_MUTATION, UPDATE_ISSUE_MUTATION,
};

pub(crate) const LINEAR_API_URL: &str = "https://api.linear.app/graphql";
pub(crate) const RELATION_BLOCKS: &str = "blocks";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinearBlocker {
    pub(crate) relation_id: String,
    pub(crate) id: String,
    pub(crate) identifier: Option<String>,
    pub(crate) state: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinearIssue {
    pub(crate) id: String,
    pub(crate) identifier: Option<String>,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) archived_at: Option<String>,
    pub(crate) priority: Option<i64>,
    pub(crate) state: Option<String>,
    pub(crate) blocked_by: Vec<LinearBlocker>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinearState {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) state_type: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinearTeam {
    pub(crate) id: String,
    pub(crate) key: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) states: Vec<LinearState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinearProject {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) slug: String,
    pub(crate) team: LinearTeam,
}

#[derive(Clone)]
pub(crate) struct LinearGraphqlClient {
    http: Client,
    api_key: String,
}

impl LinearProject {
    pub(crate) fn state_id(&self, state_name: &str) -> Option<String> {
        self.team
            .states
            .iter()
            .find(|state| normalize_name(&state.name) == normalize_name(state_name))
            .map(|state| state.id.clone())
    }

    pub(crate) fn terminal_state_names(&self) -> HashSet<String> {
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
    pub(crate) fn from_env() -> Result<Self> {
        let api_key = std::env::var("LINEAR_API_KEY")
            .context("LINEAR_API_KEY is not set in the current environment")?;
        Ok(Self {
            http: Client::new(),
            api_key,
        })
    }

    pub(crate) async fn fetch_project(&self, project_slug: &str) -> Result<LinearProject> {
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

    pub(crate) async fn fetch_project_issues(
        &self,
        project_slug: &str,
    ) -> Result<Vec<LinearIssue>> {
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

    pub(crate) async fn create_issue(
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

    pub(crate) async fn update_issue(
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

    pub(crate) async fn archive_issue(&self, issue_id: &str) -> Result<()> {
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

    pub(crate) async fn unarchive_issue(&self, issue_id: &str) -> Result<()> {
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

    pub(crate) async fn create_blocks_relation(
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

    pub(crate) async fn delete_relation(&self, relation_id: &str) -> Result<()> {
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
