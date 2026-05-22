use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::completion_artifacts::inspect_task_completion_evidence;
use crate::task_parser::{
    parse_task_header, parse_tasks, validate_execution_rows, PlanTask, TaskStatus,
};
use crate::util::atomic_write;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LoopQueueSnapshot {
    pub(crate) pending_ids: Vec<String>,
    pub(crate) blocked_ids: Vec<String>,
}

pub(crate) fn inspect_loop_queue(repo_root: &Path) -> Result<LoopQueueSnapshot> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(LoopQueueSnapshot::default());
    }
    let plan = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    validate_execution_rows(&plan)
        .with_context(|| format!("{} contains an invalid execution row", plan_path.display()))?;
    Ok(parse_loop_queue(&plan))
}

pub(crate) fn parse_loop_queue(plan: &str) -> LoopQueueSnapshot {
    let mut queue = LoopQueueSnapshot::default();
    let tasks = parse_tasks(plan);
    for task in &tasks {
        match task.status {
            TaskStatus::Pending => queue.pending_ids.push(task.id.clone()),
            TaskStatus::Partial => {
                if !is_completion_path_placeholder(task, &tasks) {
                    queue.pending_ids.push(task.id.clone());
                }
            }
            TaskStatus::Blocked => queue.blocked_ids.push(task.id.clone()),
            TaskStatus::Done => {}
        }
    }
    queue
}

fn is_completion_path_placeholder(task: &PlanTask, tasks: &[PlanTask]) -> bool {
    if task.status != TaskStatus::Partial {
        return false;
    }
    let Some(target) = task.completion_path_target.as_deref() else {
        return false;
    };
    target != task.id && tasks.iter().any(|candidate| candidate.id == target)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoopCompletionReconciliation {
    pub(crate) task_id: String,
    pub(crate) missing_reasons: Vec<String>,
    pub(crate) plan_updated: bool,
}

pub(crate) fn reconcile_loop_task_completion_evidence(
    repo_root: &Path,
    task_id: &str,
) -> Result<Option<LoopCompletionReconciliation>> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(None);
    }

    let plan = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let Some(task) = parse_tasks(&plan)
        .into_iter()
        .find(|task| task.id == task_id)
    else {
        return Ok(None);
    };
    if task.status != TaskStatus::Done {
        return Ok(None);
    }

    let evidence = inspect_task_completion_evidence(repo_root, &task.id, &task.markdown);
    if evidence.is_fully_evidenced() {
        return Ok(Some(LoopCompletionReconciliation {
            task_id: task.id,
            missing_reasons: Vec::new(),
            plan_updated: false,
        }));
    }

    let missing_reasons = evidence.missing_reasons();
    let updated = update_loop_task_status_in_plan_text(&plan, &task.id, TaskStatus::Partial);
    let plan_updated = updated != plan;
    if plan_updated {
        atomic_write(&plan_path, updated.as_bytes())
            .with_context(|| format!("failed to write {}", plan_path.display()))?;
    }

    Ok(Some(LoopCompletionReconciliation {
        task_id: task.id,
        missing_reasons,
        plan_updated,
    }))
}

fn update_loop_task_status_in_plan_text(plan: &str, task_id: &str, status: TaskStatus) -> String {
    let mut updated = String::new();

    for chunk in plan.split_inclusive('\n') {
        let line = chunk.trim_end_matches('\n').trim_end_matches('\r');
        if let Some((_, current_task_id, _)) = parse_task_header(line) {
            if current_task_id == task_id {
                updated.push_str(&mark_loop_task_header_status(chunk, status));
                continue;
            }
        }
        updated.push_str(chunk);
    }

    updated
}

fn mark_loop_task_header_status(line: &str, status: TaskStatus) -> String {
    let newline = if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    let indent_len = line.len() - line.trim_start().len();
    let indent = &line[..indent_len];
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed.strip_prefix("- [!] "))
        .or_else(|| trimmed.strip_prefix("- [~] "))
        .or_else(|| trimmed.strip_prefix("- [x] "))
        .or_else(|| trimmed.strip_prefix("- [X] "))
        .unwrap_or(trimmed);
    let marker = match status {
        TaskStatus::Pending => "- [ ]",
        TaskStatus::Partial => "- [~]",
        TaskStatus::Blocked => "- [!]",
        TaskStatus::Done => "- [x]",
    };
    format!("{indent}{marker} {rest}{newline}")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{inspect_loop_queue, parse_loop_queue, reconcile_loop_task_completion_evidence};
    use crate::loop_command::queue::LoopQueueSnapshot;
    use crate::loop_command::testkit::unique_temp_dir;

    #[test]
    fn parse_loop_queue_separates_pending_and_blocked_tasks() {
        let queue = parse_loop_queue(
            r#"
- [!] `DEC-001` Choose project license
- [ ] `META-001` Add LICENSE file and Cargo license metadata
- [x] `DONE-001` Finished already
- [ ] `GATE-P4` Phase 4 checkpoint
"#,
        );

        assert_eq!(
            queue,
            LoopQueueSnapshot {
                pending_ids: vec!["META-001".to_string(), "GATE-P4".to_string()],
                blocked_ids: vec!["DEC-001".to_string()],
            }
        );
    }

    #[test]
    fn parse_loop_queue_treats_tilde_tasks_as_pending() {
        let queue = parse_loop_queue(
            r#"
- [~] `PARTIAL-001` Partially completed task
- [!] `BLOCKED-001` Blocked task
- [x] `DONE-001` Finished already
"#,
        );

        assert_eq!(
            queue,
            LoopQueueSnapshot {
                pending_ids: vec!["PARTIAL-001".to_string()],
                blocked_ids: vec!["BLOCKED-001".to_string()],
            }
        );
    }

    #[test]
    fn parse_loop_queue_skips_partial_completion_path_placeholders() {
        let queue = parse_loop_queue(
            r#"
- [~] `TASK-001` Historical evidence gap. Completion path: `TASK-010`.
  Dependencies: none
- [ ] `TASK-010` Real completion path
  Dependencies: none
- [ ] `TASK-020` Next task
  Dependencies: none
"#,
        );

        assert_eq!(
            queue,
            LoopQueueSnapshot {
                pending_ids: vec!["TASK-010".to_string(), "TASK-020".to_string()],
                blocked_ids: Vec::new(),
            }
        );
    }

    #[test]
    fn loop_rejects_invalid_execution_row() {
        let root = unique_temp_dir("loop-invalid-row");
        fs::create_dir_all(&root).expect("failed to create temp dir");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            r#"- [ ] `TASK-1` Invalid task
  Spec: `specs/task.md`
  Dependencies: after something happens
"#,
        )
        .expect("failed to write plan");

        let err = inspect_loop_queue(&root).expect_err("invalid row rejected");
        assert!(format!("{err:#}").contains("invalid execution row"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn loop_and_parallel_ready_sets_match_for_schema_fixture() {
        let queue = parse_loop_queue(
            r#"
- [ ] `TASK-1` Ready
  Dependencies: none
- [ ] `TASK-2` Blocked
  Dependencies: `TASK-1`
- [!] `TASK-3` Explicitly blocked
  Dependencies: none
"#,
        );
        assert_eq!(queue.pending_ids, vec!["TASK-1", "TASK-2"]);
        assert_eq!(queue.blocked_ids, vec!["TASK-3"]);
    }

    #[test]
    fn loop_marks_task_partial_when_completion_evidence_missing() {
        let root = unique_temp_dir("loop-completion-evidence");
        fs::create_dir_all(root.join("scripts")).expect("failed to create scripts dir");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("failed to write wrapper");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            r#"- [x] `TASK-1` Evidence-backed task

  Verification: `cargo test loop_command::queue::tests::loop_marks_task_partial_when_completion_evidence_missing`
  Completion artifacts: `docs/proof.md`
  Dependencies: none
"#,
        )
        .expect("failed to write plan");

        let reconciliation = reconcile_loop_task_completion_evidence(&root, "TASK-1")
            .expect("reconciliation should succeed")
            .expect("done task should be reconciled");

        let plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md"))
            .expect("failed to read updated plan");
        assert!(reconciliation.plan_updated);
        assert!(reconciliation
            .missing_reasons
            .iter()
            .any(|reason| reason.contains("missing REVIEW.md handoff")));
        assert!(reconciliation
            .missing_reasons
            .iter()
            .any(|reason| reason.contains("missing verification receipt")));
        assert!(reconciliation
            .missing_reasons
            .iter()
            .any(|reason| reason.contains("missing completion artifact")));
        assert!(plan.contains("- [~] `TASK-1` Evidence-backed task"));

        fs::remove_dir_all(&root).expect("failed to remove temp workspace");
    }
}
