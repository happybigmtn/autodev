use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::task_parser::parse_tasks;
use crate::util::atomic_write;

pub(crate) fn promote_design_plan_items_to_root_queue(
    repo_root: &Path,
    pass_dir: &Path,
) -> Result<Option<usize>> {
    let plan_items_path = pass_dir.join("DESIGN-PLAN-ITEMS.md");
    let root_plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_items_path.exists() || !root_plan_path.exists() {
        return Ok(None);
    }

    let plan_items = fs::read_to_string(&plan_items_path)
        .with_context(|| format!("failed to read {}", plan_items_path.display()))?;
    let mut root_plan = fs::read_to_string(&root_plan_path)
        .with_context(|| format!("failed to read {}", root_plan_path.display()))?;
    let blocks = extract_unchecked_design_plan_item_blocks(&plan_items);
    if blocks.is_empty() {
        return Ok(None);
    }

    let existing_task_ids = parse_tasks(&root_plan)
        .into_iter()
        .map(|task| task.id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut missing = Vec::new();
    for block in blocks {
        let Some(task_id) = design_plan_block_task_id(&block) else {
            continue;
        };
        if !existing_task_ids.contains(&task_id) {
            missing.push(block);
        }
    }
    if missing.is_empty() {
        return Ok(None);
    }

    let insertion = format!(
        "\n<!-- auto design promoted unresolved design/runtime tasks from {} -->\n{}\n",
        plan_items_path.display(),
        missing.join("\n\n")
    );
    if let Some(index) = root_plan.find("\n## Follow-On Work") {
        root_plan.insert_str(index, &insertion);
    } else {
        if !root_plan.ends_with('\n') {
            root_plan.push('\n');
        }
        root_plan.push_str(&insertion);
    }
    atomic_write(&root_plan_path, root_plan.as_bytes())
        .with_context(|| format!("failed to write {}", root_plan_path.display()))?;
    Ok(Some(missing.len()))
}

fn extract_unchecked_design_plan_item_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("- [ ] `") || line.trim_start().starts_with("- [~] `") {
            if !current.is_empty() {
                blocks.push(current.join("\n"));
                current.clear();
            }
            current.push(line.to_string());
        } else if !current.is_empty() {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        blocks.push(current.join("\n"));
    }
    blocks
        .into_iter()
        .filter(|block| {
            let lower = block.to_ascii_lowercase();
            block.contains("Dependencies:")
                && block.contains("Verification:")
                && (lower.contains("runtime owner")
                    || lower.contains("source of truth")
                    || lower.contains("ui consumer"))
        })
        .collect()
}

fn design_plan_block_task_id(block: &str) -> Option<String> {
    let header = block.lines().next()?.trim_start();
    let rest = header
        .strip_prefix("- [ ] `")
        .or_else(|| header.strip_prefix("- [~] `"))?;
    let end = rest.find('`')?;
    Some(rest[..end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::promote_design_plan_items_to_root_queue;
    use crate::design_command::testkit::temp_dir;
    use crate::task_parser::{parse_tasks, TaskStatus};

    #[test]
    fn design_plan_items_promote_missing_executor_tasks_to_root_queue() {
        let root = temp_dir("design-plan-promotion");
        let pass_dir = root.join(".auto/design/pass-01");
        fs::create_dir_all(&pass_dir).unwrap();
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n",
        )
        .unwrap();
        fs::write(
            pass_dir.join("DESIGN-PLAN-ITEMS.md"),
            "- [ ] `DESIGN-001` Runtime-backed surface\n\n    Runtime owner: `src/api.rs`\n    UI consumers: `src/App.tsx`\n    Verification: `cargo test design_001`\n    Dependencies: none\n",
        )
        .unwrap();

        assert_eq!(
            promote_design_plan_items_to_root_queue(&root, &pass_dir).unwrap(),
            Some(1)
        );
        let root_plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        assert!(root_plan.contains("`DESIGN-001`"));
        assert!(
            root_plan.find("`DESIGN-001`").unwrap() < root_plan.find("## Follow-On Work").unwrap()
        );
        let tasks = parse_tasks(&root_plan);
        let promoted = tasks
            .iter()
            .find(|task| task.id == "DESIGN-001")
            .expect("promoted design task should be parser-visible");
        assert_eq!(promoted.status, TaskStatus::Pending);
        assert!(promoted.dependencies.is_empty());

        assert_eq!(
            promote_design_plan_items_to_root_queue(&root, &pass_dir).unwrap(),
            None
        );
    }
}
