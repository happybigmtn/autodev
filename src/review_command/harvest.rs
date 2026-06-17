//! Plan-completion harvest: move finished IMPLEMENTATION_PLAN.md rows into the review queue.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::completion_artifacts::review_contains_task;
use crate::review_command::queue::{
    ensure_trailing_blank_line, EMPTY_COMPLETED_DOC, REVIEW_HEADER,
};
use crate::task_parser::{
    parse_task_header as parse_shared_task_header, validate_execution_rows, TaskStatus,
};
use crate::util::{atomic_write, git_stdout};
use crate::verification_lint::verify_commands_are_runnable;

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct PlanReviewHarvestResult {
    pub(crate) removed_count: usize,
    pub(crate) appended_count: usize,
    pub(crate) skipped_count: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CompletedPlanItem {
    task_id: String,
    markdown: String,
}

pub(crate) fn harvest_completed_plan_items_for_review(
    repo_root: &Path,
    direct_review_queue: bool,
) -> Result<PlanReviewHarvestResult> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(PlanReviewHarvestResult::default());
    }

    let plan_text = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    validate_execution_rows(&plan_text)
        .with_context(|| "review queue write rejected invalid execution row")?;
    let (updated_plan, completed_items) = extract_completed_plan_items(&plan_text);
    if completed_items.is_empty() {
        return Ok(PlanReviewHarvestResult::default());
    }

    atomic_write(&plan_path, updated_plan.as_bytes())
        .with_context(|| format!("failed to write {}", plan_path.display()))?;

    let review_path = repo_root.join("REVIEW.md");
    let review_text = fs::read_to_string(&review_path).unwrap_or_default();
    let archived_text = if direct_review_queue {
        String::new()
    } else {
        fs::read_to_string(repo_root.join("ARCHIVED.md")).unwrap_or_default()
    };
    let completed_text = if direct_review_queue {
        String::new()
    } else {
        fs::read_to_string(repo_root.join("COMPLETED.md")).unwrap_or_default()
    };
    let review_history = if direct_review_queue {
        collect_historical_review_docs(repo_root)?
    } else {
        Vec::new()
    };

    let mut handoff_items = Vec::new();
    let mut skipped_count = 0usize;
    for item in &completed_items {
        let already_queued_or_reviewed = review_contains_task(&review_text, &item.task_id)
            || review_contains_task(&completed_text, &item.task_id)
            || review_contains_task(&archived_text, &item.task_id)
            || review_history
                .iter()
                .any(|historic| review_contains_task(historic, &item.task_id));
        if already_queued_or_reviewed {
            skipped_count += 1;
            continue;
        }
        handoff_items.push(render_completed_plan_review_item(item));
    }

    if !handoff_items.is_empty() {
        if direct_review_queue {
            append_review_items_preserving_doc(&review_path, REVIEW_HEADER, &handoff_items)?;
        } else {
            append_review_items_preserving_doc(
                &repo_root.join("COMPLETED.md"),
                EMPTY_COMPLETED_DOC.trim(),
                &handoff_items,
            )?;
        }
    }

    Ok(PlanReviewHarvestResult {
        removed_count: completed_items.len(),
        appended_count: handoff_items.len(),
        skipped_count,
    })
}

pub(crate) fn extract_completed_plan_items(plan_text: &str) -> (String, Vec<CompletedPlanItem>) {
    #[derive(Default)]
    struct PendingBlock {
        completed_task_id: Option<String>,
        lines: Vec<String>,
    }

    fn flush(
        pending: &mut Option<PendingBlock>,
        kept_lines: &mut Vec<String>,
        completed_items: &mut Vec<CompletedPlanItem>,
    ) {
        let Some(block) = pending.take() else {
            return;
        };
        if let Some(task_id) = block.completed_task_id {
            completed_items.push(CompletedPlanItem {
                task_id,
                markdown: block.lines.join("\n"),
            });
        } else {
            kept_lines.extend(block.lines);
        }
    }

    let mut kept_lines = Vec::new();
    let mut completed_items = Vec::new();
    let mut pending = None::<PendingBlock>;

    for line in plan_text.lines() {
        if is_top_level_plan_task_header(line) {
            flush(&mut pending, &mut kept_lines, &mut completed_items);
            pending = Some(PendingBlock {
                completed_task_id: completed_plan_task_id(line),
                lines: vec![line.to_string()],
            });
            continue;
        }

        if let Some(block) = &mut pending {
            block.lines.push(line.to_string());
        } else {
            kept_lines.push(line.to_string());
        }
    }
    flush(&mut pending, &mut kept_lines, &mut completed_items);

    let mut updated = kept_lines.join("\n");
    if plan_text.ends_with('\n') {
        updated.push('\n');
    }
    (updated, completed_items)
}

pub(crate) fn is_top_level_plan_task_header(line: &str) -> bool {
    line.starts_with("- [") && parse_shared_task_header(line).is_some()
}

pub(crate) fn completed_plan_task_id(line: &str) -> Option<String> {
    let (status, task_id, _) = parse_shared_task_header(line)?;
    (status == TaskStatus::Done).then_some(task_id)
}

pub(crate) fn render_completed_plan_review_item(item: &CompletedPlanItem) -> String {
    let mut rendered = format!(
        "- `{}`: Implementation plan completion handoff; status `awaiting_auto_review`.\n\
  - Source: `IMPLEMENTATION_PLAN.md`.\n\
  - Original IMPLEMENTATION_PLAN.md item:\n\
    ```md\n",
        item.task_id
    );
    for line in item.markdown.lines() {
        rendered.push_str("    ");
        rendered.push_str(line);
        rendered.push('\n');
    }
    rendered.push_str("    ```");
    if let Err(err) = verify_commands_are_runnable(&item.task_id, "Verification:", &item.markdown) {
        rendered.push_str(&format!(
            "\n  - ⚠ Verification command not directly runnable: {err:#}. Reviewer: derive a concrete proof (e.g. `cargo test <module>::tests::<name>` or `rg -n <pattern> <path>`) before signing off."
        ));
    }
    rendered
}

pub(crate) fn append_review_items_preserving_doc(
    path: &Path,
    default_header: &str,
    items: &[String],
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let mut content = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        let mut header = default_header.trim_end().to_string();
        header.push_str("\n\n");
        header
    };
    ensure_trailing_blank_line(&mut content);
    content.push_str(&items.join("\n\n"));
    content.push('\n');
    atomic_write(path, content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn collect_historical_review_docs(repo_root: &Path) -> Result<Vec<String>> {
    let hashes = git_stdout(
        repo_root,
        ["log", "--all", "--format=%H", "--", "REVIEW.md"],
    )?;
    let mut docs = Vec::new();
    for hash in hashes.lines() {
        let spec = format!("{hash}:REVIEW.md");
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["show", &spec])
            .output()
            .with_context(|| format!("failed to read historical REVIEW.md at {hash}"))?;
        if output.status.success() {
            docs.push(String::from_utf8_lossy(&output.stdout).to_string());
        }
    }
    Ok(docs)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        extract_completed_plan_items, harvest_completed_plan_items_for_review,
        render_completed_plan_review_item, CompletedPlanItem,
    };
    use crate::review_command::queue::{
        handoff_completed_items_to_review_queue, ARCHIVED_HEADER, EMPTY_COMPLETED_DOC,
        REVIEW_HEADER,
    };

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-review-test-{nanos}"))
    }

    fn init_git_repo(path: &PathBuf) {
        fs::create_dir_all(path).expect("failed to create repo dir");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(path)
            .status()
            .expect("failed to run git init");
        assert!(status.success(), "git init should succeed");
    }

    fn run_git_in<'a>(path: &PathBuf, args: impl IntoIterator<Item = &'a str>) {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Autodev Tests",
                "-c",
                "user.email=autodev-tests@example.com",
            ])
            .args(args)
            .current_dir(path)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git command failed: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn extracts_completed_plan_items_and_leaves_unfinished_tasks() {
        let plan = "# Plan\n\n- [x] `TASK-1` Done\n  - Verification: `cargo test one`\n\n- [ ] `TASK-2` Todo\n  - Verification: `cargo test two`\n";
        let (updated, completed) = extract_completed_plan_items(plan);

        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].task_id, "TASK-1");
        assert!(completed[0].markdown.contains("Verification"));
        assert!(!updated.contains("TASK-1"));
        assert!(updated.contains("TASK-2"));
    }

    #[test]
    fn harvest_review_item_flags_non_runnable_verification_command() {
        let item = CompletedPlanItem {
            task_id: "TASK-1".to_string(),
            markdown: "- [x] `TASK-1` Done\n  - Verification: `cargo --lib`".to_string(),
        };

        let rendered = render_completed_plan_review_item(&item);

        assert!(rendered.contains("⚠ Verification command not directly runnable"));
    }

    #[test]
    fn harvest_review_item_leaves_runnable_command_unannotated() {
        let item = CompletedPlanItem {
            task_id: "TASK-1".to_string(),
            markdown: "- [x] `TASK-1` Done\n  - Verification: `cargo test review_command::harvest::tests::harvest_review_item_leaves_runnable_command_unannotated`".to_string(),
        };

        let rendered = render_completed_plan_review_item(&item);

        assert!(!rendered.contains("⚠"));
    }

    #[test]
    fn harvest_completed_plan_items_flows_through_completed_queue() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        fs::write(
            temp.join("IMPLEMENTATION_PLAN.md"),
            r#"# Plan

- [x] `TASK-1` Done
  - Verification: `cargo test one`

- [ ] `TASK-2` Todo
  Spec: `specs/task-2.md`
  Why now: keeps a valid pending review fixture.
  Codebase evidence: `REVIEW.md`
  Source of truth: `REVIEW.md`
  Runtime owner: none
  UI consumers: none
  Generated artifacts: none
  Fixture boundary: test fixture only.
  Retired surfaces: none
  Owns: `REVIEW.md`
  Integration touchpoints: `COMPLETED.md`
  Scope boundary: review queue fixture only.
  Acceptance criteria: pending task remains after harvest.
  Verification: `cargo test review_command::tests::harvest_completed_plan_items_flows_through_completed_queue`
  Required tests: `cargo test review_command::tests::harvest_completed_plan_items_flows_through_completed_queue`
  Contract generation: none -- no generated contract
  Cross-surface tests: none -- no UI/runtime boundary
  Review/closeout: reviewer checks completed handoff only.
  Completion artifacts: none
  Dependencies: none
  Estimated scope: XS
  Completion signal: fixture remains valid.
"#,
        )
        .expect("write plan");
        fs::write(temp.join("REVIEW.md"), format!("{REVIEW_HEADER}\n\n")).expect("write review");
        fs::write(temp.join("ARCHIVED.md"), format!("{ARCHIVED_HEADER}\n\n"))
            .expect("write archived");

        let harvest =
            harvest_completed_plan_items_for_review(&temp, false).expect("harvest plan items");
        assert_eq!(harvest.removed_count, 1);
        assert_eq!(harvest.appended_count, 1);
        assert_eq!(harvest.skipped_count, 0);
        assert!(!fs::read_to_string(temp.join("IMPLEMENTATION_PLAN.md"))
            .expect("read plan")
            .contains("TASK-1"));

        let moved = handoff_completed_items_to_review_queue(
            &temp.join("COMPLETED.md"),
            &temp.join("REVIEW.md"),
        )
        .expect("move completed to review");
        assert_eq!(moved, 1);
        assert_eq!(
            fs::read_to_string(temp.join("COMPLETED.md")).expect("read completed"),
            EMPTY_COMPLETED_DOC
        );
        let review = fs::read_to_string(temp.join("REVIEW.md")).expect("read review");
        assert!(review.contains("`TASK-1`"));

        fs::remove_dir_all(temp).expect("cleanup temp dir");
    }

    #[test]
    fn direct_harvest_preserves_review_preamble_and_skips_historical_reviewed_items() {
        let temp = unique_temp_dir();
        init_git_repo(&temp);
        fs::write(
            temp.join("REVIEW.md"),
            "# REVIEW\n\nThis preamble stays.\n\n- `TASK-OLD`: reviewed already; status `awaiting_auto_review`.\n",
        )
        .expect("write review");
        run_git_in(&temp, ["add", "REVIEW.md"]);
        run_git_in(&temp, ["commit", "-m", "review old task"]);
        fs::write(temp.join("REVIEW.md"), "# REVIEW\n\nThis preamble stays.\n")
            .expect("remove old task");
        run_git_in(&temp, ["add", "REVIEW.md"]);
        run_git_in(&temp, ["commit", "-m", "archive old task"]);
        fs::write(
            temp.join("IMPLEMENTATION_PLAN.md"),
            "# Plan\n\n- [x] `TASK-OLD` Done before\n\n- [x] `TASK-NEW` Done now\n  - Verification: `cargo test new`\n",
        )
        .expect("write plan");

        let harvest = harvest_completed_plan_items_for_review(&temp, true).expect("direct harvest");
        assert_eq!(harvest.removed_count, 2);
        assert_eq!(harvest.appended_count, 1);
        assert_eq!(harvest.skipped_count, 1);

        let review = fs::read_to_string(temp.join("REVIEW.md")).expect("read review");
        assert!(review.contains("This preamble stays."));
        assert!(!review.contains("TASK-OLD"));
        assert!(review.contains("`TASK-NEW`"));
        assert!(!temp.join("COMPLETED.md").exists());
        let plan = fs::read_to_string(temp.join("IMPLEMENTATION_PLAN.md")).expect("read plan");
        assert!(!plan.contains("TASK-OLD"));
        assert!(!plan.contains("TASK-NEW"));

        fs::remove_dir_all(temp).expect("cleanup temp dir");
    }

    #[test]
    fn review_queue_write_rejects_invalid_execution_row() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        fs::write(
            temp.join("IMPLEMENTATION_PLAN.md"),
            "- [ ] `TASK-1` Invalid\n  Spec: `specs/task.md`\n  Dependencies: after the audit\n",
        )
        .expect("write plan");

        let err =
            harvest_completed_plan_items_for_review(&temp, true).expect_err("invalid row rejected");
        assert!(format!("{err:#}").contains("review queue write rejected invalid execution row"));

        fs::remove_dir_all(temp).ok();
    }
}
