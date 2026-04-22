# Symphony GraphQL Surface

Date: 2026-04-22

Status: Accepted

Task: `TASK-013`

## Context

`specs/220426-symphony-linear-orchestration.md` claimed that GraphQL queries in
`src/linear_tracker.rs` are the only Linear egress points. The live code does not
match that: `src/symphony_command.rs` has its own Linear GraphQL client and
operation set for `auto symphony sync`, plus GraphQL snippets embedded in the
rendered Symphony workflow prompt.

The two Rust clients both post to `https://api.linear.app/graphql` with
`LINEAR_API_KEY`, and neither module uses Linear REST calls.

## Operation Inventory

`src/linear_tracker.rs` owns the host-side `auto parallel` Linear tracker used
for dispatch, done, drift, and auto-sync triggering:

- `FETCH_PROJECT_QUERY` -> `query AutoParallelProject`
- `FETCH_PROJECT_ISSUES_QUERY` -> `query AutoParallelProjectIssues`
- `UPDATE_ISSUE_STATE_MUTATION` -> `mutation AutoParallelUpdateIssueState`
- `ARCHIVE_ISSUE_MUTATION` -> `mutation AutoParallelArchiveIssue`

`src/symphony_command.rs` owns plan-to-Linear synchronization for
`auto symphony sync`:

- `FETCH_PROJECT_QUERY` -> `query AutoSymphonyProject`
- `FETCH_PROJECT_ISSUES_QUERY` -> `query AutoSymphonyProjectIssues`
- `CREATE_ISSUE_MUTATION` -> `mutation AutoSymphonyCreateIssue`
- `UPDATE_ISSUE_MUTATION` -> `mutation AutoSymphonyUpdateIssue`
- `UPDATE_ISSUE_AND_STATE_MUTATION` -> `mutation AutoSymphonyUpdateIssueAndState`
- `ARCHIVE_ISSUE_MUTATION` -> `mutation AutoSymphonyArchiveIssue`
- `UNARCHIVE_ISSUE_MUTATION` -> `mutation AutoSymphonyUnarchiveIssue`
- `DELETE_RELATION_MUTATION` -> `mutation AutoSymphonyDeleteRelation`
- `CREATE_RELATION_MUTATION` -> `mutation AutoSymphonyCreateRelation`

`src/symphony_command.rs` also renders these GraphQL examples into
`.auto/symphony/WORKFLOW.md` for external Symphony workers using
`linear_graphql`; these are prompt contract, not direct Rust network egress:

- `query IssueContext`
- `mutation UpdateIssueState`
- `mutation AddComment`

## Decision

Do not consolidate `symphony_command.rs` into `linear_tracker.rs` in the next
implementation increment. Widen the symphony spec to document the two current
Rust egress surfaces instead.

The split is intentional enough to keep for now:

- `linear_tracker.rs` is a narrow host adapter for `auto parallel`. It tracks
  existing issues by task id, classifies coverage drift, moves a dispatched task
  to the in-progress state, and archives a task after landing.
- `symphony_command.rs` is the authoritative sync implementation. It creates
  issues, updates title/body/priority/state, reopens archived or terminal issues,
  archives completed plan issues, and manages Linear blocker relations.
- The similarly named project, issue-list, and archive operations are not exact
  duplicates. `symphony_command.rs` requests archived issues, priorities, and
  inverse relations; `linear_tracker.rs` requests a smaller shape for runtime
  drift and status updates.

## Consequences

The spec should say Linear API egress is GraphQL-only and currently split
between `linear_tracker.rs` and `symphony_command.rs`; it should not say
`linear_tracker.rs` is the sole egress point.

No code consolidation task is queued from this decision. If future maintenance
pressure justifies dedupe, the bounded direction should be a new shared
`linear_graphql` transport/model module used by both callers, not moving the
sync-specific operations into `linear_tracker.rs` under its current name.
