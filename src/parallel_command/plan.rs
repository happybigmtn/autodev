use super::*;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LoopQueueSnapshot {
    pub(crate) pending_ids: Vec<String>,
    pub(crate) blocked_ids: Vec<String>,
}

pub(crate) fn build_iteration_prompt(prompt_template: &str, queue: &LoopQueueSnapshot) -> String {
    let blocked_clause = if queue.blocked_ids.is_empty() {
        "Blocked tasks marked `- [!]`: none".to_string()
    } else {
        format!(
            "Blocked tasks marked `- [!]` to skip this iteration: {}",
            queue.blocked_ids.join(", ")
        )
    };
    format!(
        "{prompt_template}\n\nCurrent queue state for this iteration:\n- First actionable unfinished task: `{}`\n- Unfinished task count: {}\n- {}\n\nExecute the instructions above.",
        queue.pending_ids[0],
        queue.pending_ids.len(),
        blocked_clause
    )
}

pub(crate) fn read_loop_plan(repo_root: &Path) -> Result<String> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoopTaskStatus {
    Pending,
    Blocked,
    Partial,
    Done,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoopTask {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) status: LoopTaskStatus,
    pub(crate) dependencies: Vec<String>,
    pub(crate) estimated_scope: Option<String>,
    pub(crate) completion_path_target: Option<String>,
    pub(crate) lane_kind: LaneKind,
    pub(crate) markdown: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LoopPlanSnapshot {
    pub(crate) tasks: Vec<LoopTask>,
}

impl LoopPlanSnapshot {
    pub(crate) fn task(&self, task_id: &str) -> Option<&LoopTask> {
        self.tasks.iter().find(|task| task.id == task_id)
    }

    pub(crate) fn queue_snapshot(&self) -> LoopQueueSnapshot {
        let mut queue = LoopQueueSnapshot::default();
        for task in &self.tasks {
            match task.status {
                LoopTaskStatus::Pending => queue.pending_ids.push(task.id.clone()),
                LoopTaskStatus::Partial => {
                    if !self.is_completion_path_placeholder(task) {
                        queue.pending_ids.push(task.id.clone());
                    }
                }
                LoopTaskStatus::Blocked => queue.blocked_ids.push(task.id.clone()),
                LoopTaskStatus::Done => {}
            }
        }
        queue
    }

    pub(crate) fn ready_tasks(&self, inflight: &BTreeSet<String>) -> Vec<LoopTask> {
        let unresolved = self.unresolved_dependency_ids(inflight);

        self.tasks
            .iter()
            .filter(|task| self.is_actionable_unfinished(task))
            .filter(|task| !inflight.contains(&task.id))
            .filter(|task| {
                task.dependencies
                    .iter()
                    .all(|dep| !unresolved.contains(dep))
            })
            .cloned()
            .collect()
    }

    pub(crate) fn is_actionable_unfinished(&self, task: &LoopTask) -> bool {
        matches!(
            task.status,
            LoopTaskStatus::Pending | LoopTaskStatus::Partial
        ) && !self.is_completion_path_placeholder(task)
    }

    pub(crate) fn unresolved_dependency_ids(
        &self,
        inflight: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let mut unresolved = self
            .tasks
            .iter()
            .filter(|task| {
                // A `Partial` (`[~]`) upstream has already landed its code to
                // canonical main; it is `Partial` only because its completion
                // receipts/evidence are not yet fully recorded. Dependents are
                // created from canonical main and build on that committed code,
                // so a missing receipt must not block them. Treating `Partial`
                // as unresolved self-blocks every downstream task in a queue
                // where work routinely lands `[~]` before receipts close out.
                // Only genuinely-unlanded upstreams gate dependents.
                matches!(
                    task.status,
                    LoopTaskStatus::Pending | LoopTaskStatus::Blocked
                )
            })
            .filter(|task| !self.is_completion_path_placeholder(task))
            .map(|task| task.id.clone())
            .chain(inflight.iter().cloned())
            .collect::<BTreeSet<_>>();

        for task in &self.tasks {
            let Some(target_id) = self.completion_path_target(task) else {
                continue;
            };
            if unresolved.contains(target_id) {
                unresolved.insert(task.id.clone());
            }
        }

        unresolved
    }

    pub(crate) fn completion_path_target<'a>(&'a self, task: &'a LoopTask) -> Option<&'a str> {
        if task.status != LoopTaskStatus::Partial {
            return None;
        }
        let target = task.completion_path_target.as_deref()?;
        if target == task.id {
            return None;
        }
        self.tasks
            .iter()
            .any(|candidate| candidate.id == target)
            .then_some(target)
    }

    pub(crate) fn is_completion_path_placeholder(&self, task: &LoopTask) -> bool {
        self.completion_path_target(task).is_some()
    }

    pub(crate) fn direct_unfinished_dependents(&self, task_id: &str) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|task| self.is_actionable_unfinished(task))
            .filter(|task| task.id != task_id)
            .filter(|task| task.dependencies.iter().any(|dep| dep == task_id))
            .map(|task| task.id.clone())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParallelBlockerKind {
    Pending,
    Blocked,
    Shelved,
    DeferredPartial,
    InFlight,
}

impl ParallelBlockerKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Shelved => "shelved",
            Self::DeferredPartial => "deferred-partial",
            Self::InFlight => "in-flight",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParallelBlockerDetail {
    pub(crate) task_id: String,
    pub(crate) kind: ParallelBlockerKind,
    pub(crate) downstream: Vec<String>,
}

pub(crate) fn parse_loop_plan(plan: &str) -> LoopPlanSnapshot {
    LoopPlanSnapshot {
        tasks: parse_shared_tasks(plan)
            .into_iter()
            .map(finalize_task)
            .collect(),
    }
}

pub(crate) fn finalize_task(task: SharedPlanTask) -> LoopTask {
    let SharedPlanTask {
        id,
        title,
        status,
        dependencies,
        completion_path_target,
        lane_kind,
        markdown,
        ..
    } = task;
    let mut status = loop_task_status(status);
    if matches!(
        status,
        LoopTaskStatus::Pending | LoopTaskStatus::Blocked | LoopTaskStatus::Partial
    ) {
        if task_is_deferred_not_shipped_placeholder(&title, &markdown) {
            status = LoopTaskStatus::Blocked;
        } else if matches!(status, LoopTaskStatus::Pending | LoopTaskStatus::Blocked)
            && task_is_non_actionable_placeholder(&title, &markdown)
        {
            status = LoopTaskStatus::Done;
        }
    }
    let inferred_lane_kind = lane_kind.unwrap_or_else(|| infer_lane_kind(&title, &markdown));
    LoopTask {
        id,
        title,
        status,
        dependencies,
        estimated_scope: task_field_line_value(&markdown, "Estimated scope:"),
        completion_path_target,
        lane_kind: inferred_lane_kind,
        markdown,
    }
}

pub(crate) fn infer_lane_kind(title: &str, markdown: &str) -> LaneKind {
    let text = format!("{title}\n{markdown}").to_ascii_lowercase();
    if text.contains("evidence only")
        || text.contains("evidence-only")
        || text.contains("verification only")
        || text.contains("receipt refresh")
        || text.contains("review handoff")
        || text.contains("proof-only")
    {
        LaneKind::Evidence
    } else {
        LaneKind::Code
    }
}

pub(crate) fn loop_task_status(status: SharedTaskStatus) -> LoopTaskStatus {
    match status {
        SharedTaskStatus::Pending => LoopTaskStatus::Pending,
        SharedTaskStatus::Blocked => LoopTaskStatus::Blocked,
        SharedTaskStatus::Partial => LoopTaskStatus::Partial,
        SharedTaskStatus::Done => LoopTaskStatus::Done,
    }
}

pub(crate) fn task_is_non_actionable_placeholder(title: &str, markdown: &str) -> bool {
    if title
        .trim()
        .to_ascii_lowercase()
        .starts_with("merged into ")
    {
        return true;
    }

    markdown.lines().any(|line| {
        let Some(rest) = line.trim_start().strip_prefix("Status:") else {
            return false;
        };
        let rest = rest.to_ascii_lowercase();
        rest.contains("placeholder") || rest.contains("merged into")
    })
}

pub(crate) fn task_is_deferred_not_shipped_placeholder(title: &str, markdown: &str) -> bool {
    std::iter::once(title).chain(markdown.lines()).any(|line| {
        let normalized = line
            .chars()
            .map(|ch| if ch.is_ascii_punctuation() { ' ' } else { ch })
            .collect::<String>()
            .to_ascii_lowercase();
        normalized.contains("deferred") && normalized.contains("not shipped")
    })
}

pub(crate) fn parse_task_header(line: &str) -> Option<(LoopTaskStatus, String, String)> {
    let (status, id, title) = parse_shared_task_header(line)?;
    Some((loop_task_status(status), id, title))
}

pub(crate) fn strip_list_bullet(line: &str) -> &str {
    let trimmed = line.trim_start();
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return rest;
        }
    }
    trimmed
}

pub(crate) fn task_field_line_value(markdown: &str, field: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        strip_list_bullet(line)
            .strip_prefix(field)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
    })
}

pub(crate) fn task_field_body(markdown: &str, field: &str, next_field: &str) -> Option<String> {
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

pub(crate) fn inspect_loop_plan(repo_root: &Path) -> Result<LoopPlanSnapshot> {
    let plan = read_loop_plan(repo_root)?;
    Ok(parse_loop_plan(&plan))
}

pub(crate) fn update_task_completion_in_plan(
    repo_root: &Path,
    task_id: &str,
    status: LoopTaskStatus,
) -> Result<bool> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(false);
    }

    let plan = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let updated = update_task_completion_in_plan_text(&plan, task_id, status);
    if updated == plan {
        return Ok(false);
    }

    atomic_write(&plan_path, updated.as_bytes())
        .with_context(|| format!("failed to write {}", plan_path.display()))?;
    Ok(true)
}

pub(crate) fn update_task_completion_in_plan_text(
    plan: &str,
    task_id: &str,
    status: LoopTaskStatus,
) -> String {
    let mut updated = String::new();

    for chunk in plan.split_inclusive('\n') {
        let line = chunk.trim_end_matches('\n').trim_end_matches('\r');
        if let Some((_, current_task_id, _)) = parse_task_header(line) {
            if current_task_id == task_id {
                updated.push_str(&mark_task_header_status(chunk, status));
                continue;
            }
        }
        updated.push_str(chunk);
    }

    updated
}

pub(crate) fn mark_task_header_status(line: &str, status: LoopTaskStatus) -> String {
    let newline = if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let stripped = line.trim_end_matches('\n').trim_end_matches('\r');
    let indent_len = stripped.len() - stripped.trim_start().len();
    let indent = &stripped[..indent_len];
    let trimmed = stripped.trim_start();
    let (existing_done, rest) = if let Some(rest) = trimmed.strip_prefix("- [x] ") {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [X] ") {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
        (false, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [!] ") {
        (false, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [~] ") {
        (false, rest)
    } else {
        (false, trimmed)
    };
    // Completion is monotonic forward: once a task header is marked [x], an
    // automated reconcile pass must not demote it. This guards against
    // duplicate-ID rows in IMPLEMENTATION_PLAN.md where landing one row would
    // otherwise rewrite a sibling [x] row that shares the same task ID.
    if existing_done && status != LoopTaskStatus::Done {
        return line.to_string();
    }
    let marker = match status {
        LoopTaskStatus::Pending => "- [ ]",
        LoopTaskStatus::Blocked => "- [!]",
        LoopTaskStatus::Partial => "- [~]",
        LoopTaskStatus::Done => "- [x]",
    };
    format!("{indent}{marker} {rest}{newline}")
}

#[cfg(test)]
mod tests {
    use crate::parallel_command::*;

    #[test]
    fn lane_kind_routes_operator_and_evidence_tasks() {
        let plan = parse_loop_plan(
            r#"
- [ ] `OPS-001` Loom key ceremony
  Lane kind: operator
  Verification: `ssh root@loom true`
  Dependencies: none

- [ ] `EVID-001` Refresh receipt
  Lane kind: evidence
  Scope boundary: evidence only.
  Verification: `cargo test receipt_refresh`
  Dependencies: none

- [ ] `CODE-001` Normal code
  Verification: `cargo test code`
  Dependencies: none
"#,
        );
        assert_eq!(plan.task("OPS-001").unwrap().lane_kind, LaneKind::Operator);
        assert_eq!(plan.task("EVID-001").unwrap().lane_kind, LaneKind::Evidence);
        assert_eq!(plan.task("CODE-001").unwrap().lane_kind, LaneKind::Code);

        let verdict = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            &[],
            &[],
        );
        assert_eq!(
            verdict,
            "GO: safe to launch or resume; code lanes ready: CODE-001; evidence queue: EVID-001; operator queue: OPS-001"
        );
        assert!(!verdict.contains("code lanes ready: OPS-001"));
        assert!(verdict.contains("evidence queue: EVID-001"));
    }

    #[test]
    fn inferred_mainnet_autonomous_gate_remains_dispatchable_code() {
        let plan = parse_loop_plan(
            r#"
- [ ] `LIVE-001` Autonomous loom mainnet canary
  Verification: `LAUNCH_GATE_AUTHORIZE_REAL_RBTC=1 bash scripts/e2e/canary.sh`
  Scope boundary: fail-closed live mainnet proof; emits AUTO_ENV_BLOCKER when credentials or authorization are absent.
  Review/closeout: requires operator approval before any live run.
  Dependencies: none

- [ ] `OPS-001` Human signoff ceremony
  Lane kind: operator
  Verification: `ssh root@loom true`
  Review/closeout: requires operator approval before any live run.
  Dependencies: none
"#,
        );
        assert_eq!(plan.task("LIVE-001").unwrap().lane_kind, LaneKind::Code);
        assert_eq!(plan.task("OPS-001").unwrap().lane_kind, LaneKind::Operator);

        let verdict = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            &[],
            &[],
        );
        assert_eq!(
            verdict,
            "GO: safe to launch or resume; code lanes ready: LIVE-001; operator queue: OPS-001"
        );
        assert!(!verdict.contains("operator queue: LIVE-001"));
    }

    #[test]
    fn parse_loop_plan_tracks_ready_and_blocked_dependencies() {
        let plan = r#"
- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
- [ ] `TASK-002` Second task
  Dependencies:
  - `TASK-001`
  Estimated scope: medium
- [!] `TASK-003` Blocked task
  Dependencies:
  - `TASK-999`
  Estimated scope: large
- [x] `TASK-004` Completed task
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-001", "TASK-002"]);
        assert_eq!(queue.blocked_ids, vec!["TASK-003"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-001"]
        );
    }

    #[test]
    fn parse_loop_plan_skips_merged_placeholder_tasks() {
        let plan = r#"
- [ ] `WEB-CODEGEN-A` Real tranche head
  Dependencies:
  - None
- [ ] `WEB-PAYOUT-TRUTH` Merged into WEB-CODEGEN-A
  Status: This standalone item is kept as a checkbox placeholder for traceability but its work is now folded into WEB-CODEGEN-A above.
  Dependencies:
  - `WEB-CODEGEN-A`
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["WEB-CODEGEN-A"]);
        assert!(queue.blocked_ids.is_empty());
        assert_eq!(snapshot.tasks.len(), 2);
        assert_eq!(snapshot.tasks[1].status, LoopTaskStatus::Done);
    }

    #[test]
    fn parse_loop_plan_blocks_deferred_not_shipped_rows() {
        let plan = r#"
- [ ] `TASK-A` Implement deferred queue handling
  Dependencies:
  - None
- [ ] `TASK-D` Future feature — **DEFERRED, not shipped**
  Dependencies:
  - None
- [ ] `TASK-E` Depends on deferred feature
  Dependencies:
  - `TASK-D`
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-A", "TASK-E"]);
        assert_eq!(queue.blocked_ids, vec!["TASK-D"]);
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == "TASK-D")
                .map(|task| task.status),
            Some(LoopTaskStatus::Blocked)
        );
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-A"]
        );
    }

    #[test]
    fn parse_loop_plan_treats_none_dependencies_as_empty() {
        let plan = r#"
- [ ] `WEB-HOUSE-AUDIT` Audit
  Dependencies: none (parallel with `WEB-CODEGEN-A`)
  Estimated scope: S
- [ ] `WEB-CODEGEN-A` Real tranche head
  Dependencies: `WEB-HOUSE-AUDIT`
  Estimated scope: L
"#;

        let snapshot = parse_loop_plan(plan);
        assert!(snapshot.tasks[0].dependencies.is_empty());
        assert_eq!(snapshot.tasks[1].dependencies, vec!["WEB-HOUSE-AUDIT"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["WEB-HOUSE-AUDIT"]
        );
    }

    #[test]
    fn parse_loop_plan_ignores_parallelism_notes_in_dependency_lines() {
        let plan = r#"
- [x] `WEB-HOUSE-AUDIT` Audit
  Dependencies: none
  Estimated scope: S
- [x] `WEB-CHANNEL-COVERAGE` Coverage
  Dependencies: none
  Estimated scope: S
- [ ] `WEB-CODEGEN-A` Codegen
  Dependencies: `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE`
  Estimated scope: L
- [ ] `WEB-CLIENT-BUILD` Build
  Dependencies: `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE` (Wave 0 gate — finding #3; parallel with `WEB-CODEGEN-A` + `WEB-DESIGN-SYSTEM`)
  Estimated scope: M
- [ ] `WEB-DESIGN-SYSTEM` Design
  Dependencies: `WEB-CLIENT-BUILD` (need bundle for shell exports), `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE` (Wave 0 gate — finding #3). Parallel with `WEB-CODEGEN-A`.
  Estimated scope: L
"#;

        let snapshot = parse_loop_plan(plan);
        let codegen = snapshot
            .tasks
            .iter()
            .find(|task| task.id == "WEB-CODEGEN-A")
            .expect("WEB-CODEGEN-A present");
        let build = snapshot
            .tasks
            .iter()
            .find(|task| task.id == "WEB-CLIENT-BUILD")
            .expect("WEB-CLIENT-BUILD present");
        let design = snapshot
            .tasks
            .iter()
            .find(|task| task.id == "WEB-DESIGN-SYSTEM")
            .expect("WEB-DESIGN-SYSTEM present");

        assert_eq!(
            codegen.dependencies,
            vec!["WEB-HOUSE-AUDIT", "WEB-CHANNEL-COVERAGE"]
        );
        assert_eq!(
            build.dependencies,
            vec!["WEB-HOUSE-AUDIT", "WEB-CHANNEL-COVERAGE"]
        );
        assert_eq!(
            design.dependencies,
            vec![
                "WEB-CLIENT-BUILD",
                "WEB-HOUSE-AUDIT",
                "WEB-CHANNEL-COVERAGE"
            ]
        );
    }

    #[test]
    fn partial_dependency_is_satisfied_so_dependent_is_ready() {
        // A `[~]` upstream has already landed its code to canonical main; it is
        // `Partial` only because its completion receipts are still outstanding.
        // Both the upstream (still actionable for closeout) and its dependent
        // are pending, but the dependent must be READY — its code dependency is
        // satisfied. Regression: treating `Partial` as an unresolved dependency
        // self-blocked every downstream task in a queue where work routinely
        // lands `[~]` before receipts close out.
        let plan = r#"
- [~] `TASK-001` Evidence gap
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-002` Depends on partial
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-001", "TASK-002"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-001", "TASK-002"]
        );
    }

    #[test]
    fn parse_loop_plan_skips_partial_completion_path_placeholders() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Completion path: `TASK-010`.
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
- [ ] `TASK-020` Depends on placeholder alias
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-010", "TASK-020"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-010"]
        );
    }

    #[test]
    fn parse_loop_plan_skips_partial_prose_completion_path_placeholders() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Reconciled via `TASK-099` (see `TASK-010` for the completion path).
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
- [ ] `TASK-020` Depends on placeholder alias
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-010", "TASK-020"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-010"]
        );
    }

    #[test]
    fn completion_path_alias_resolves_once_follow_on_is_done() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Completion path: `TASK-010`.
  Dependencies: none
  Estimated scope: S
- [x] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
- [ ] `TASK-020` Depends on placeholder alias
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-020"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-020"]
        );
    }

    #[test]
    fn update_task_completion_in_plan_text_marks_partial_instead_of_dropping_block() {
        let plan = r#"- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
- [ ] `TASK-002` Second task
  Dependencies:
  - `TASK-001`
  Estimated scope: medium
"#;

        let updated =
            update_task_completion_in_plan_text(plan, "TASK-001", LoopTaskStatus::Partial);

        assert!(updated.contains("- [~] `TASK-001` First task"));
        assert!(updated.contains("TASK-002"));
        assert!(updated.starts_with("- [~] `TASK-001`"));
    }

    #[test]
    fn update_task_completion_in_plan_text_does_not_demote_existing_done_rows() {
        // Two rows share the same task ID (duplicate-ID harvest residue). When
        // a lane lands the still-pending row and reconcile writes Partial, the
        // already-completed sibling must remain `[x]`.
        let plan = r#"- [x] `AUDIT-94` Already completed sibling
  Dependencies: none
  Estimated scope: small
- [ ] `AUDIT-94` Newly assigned duplicate-id row
  Dependencies: none
  Estimated scope: small
"#;

        let updated =
            update_task_completion_in_plan_text(plan, "AUDIT-94", LoopTaskStatus::Partial);

        assert!(
            updated.contains("- [x] `AUDIT-94` Already completed sibling"),
            "completed sibling must not be demoted: {updated}"
        );
        assert!(
            updated.contains("- [~] `AUDIT-94` Newly assigned duplicate-id row"),
            "still-pending duplicate must be marked partial: {updated}"
        );
    }

    #[test]
    fn iteration_prompt_injects_actionable_and_blocked_tasks() {
        let queue = LoopQueueSnapshot {
            pending_ids: vec!["META-001".to_string(), "GATE-P4".to_string()],
            blocked_ids: vec!["DEC-001".to_string()],
        };
        let prompt = build_iteration_prompt("base prompt", &queue);

        assert!(prompt.contains("First actionable unfinished task: `META-001`"));
        assert!(prompt.contains("Unfinished task count: 2"));
        assert!(prompt.contains("Blocked tasks marked `- [!]` to skip this iteration: DEC-001"));
    }
}
