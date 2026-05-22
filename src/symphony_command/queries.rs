//! GraphQL operation strings for the Linear API.

pub(crate) const FETCH_PROJECT_QUERY: &str = r#"
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

pub(crate) const FETCH_PROJECT_ISSUES_QUERY: &str = r#"
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

pub(crate) const CREATE_ISSUE_MUTATION: &str = r#"
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

pub(crate) const UPDATE_ISSUE_MUTATION: &str = r#"
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

pub(crate) const UPDATE_ISSUE_AND_STATE_MUTATION: &str = r#"
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

pub(crate) const ARCHIVE_ISSUE_MUTATION: &str = r#"
mutation AutoSymphonyArchiveIssue($id: String!) {
  issueArchive(id: $id) {
    success
  }
}
"#;

pub(crate) const UNARCHIVE_ISSUE_MUTATION: &str = r#"
mutation AutoSymphonyUnarchiveIssue($id: String!) {
  issueUnarchive(id: $id) {
    success
  }
}
"#;

pub(crate) const DELETE_RELATION_MUTATION: &str = r#"
mutation AutoSymphonyDeleteRelation($id: String!) {
  issueRelationDelete(id: $id) {
    success
  }
}
"#;

pub(crate) const CREATE_RELATION_MUTATION: &str = r#"
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
