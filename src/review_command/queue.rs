//! REVIEW.md queue parsing, batch selection, and stale-item triage.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::util::atomic_write;

pub(crate) const EMPTY_COMPLETED_DOC: &str = "# COMPLETED\n\n";
pub(crate) const REVIEW_HEADER: &str = "# REVIEW";
pub(crate) const ARCHIVED_HEADER: &str = "# ARCHIVED";

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StaleTriageResult {
    pub(crate) followup_path: PathBuf,
    pub(crate) removed_count: usize,
    pub(crate) appended_count: usize,
}

pub(crate) fn has_reviewable_items(path: &Path) -> Result<bool> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(!extract_review_items(&content).is_empty())
}

/// Read REVIEW.md and return the first `batch_size` items. A `batch_size` of 0
/// means "pick every item" (legacy behavior — brittle on large queues).
#[cfg(test)]
pub(crate) fn select_review_batch(
    review_path: &Path,
    batch_size: usize,
) -> Result<(Vec<String>, usize)> {
    let (batch, total, _) =
        select_review_batch_excluding(review_path, batch_size, &HashSet::new())?;
    Ok((batch, total))
}

/// Read REVIEW.md and return the first `batch_size` items whose stable
/// identity is not present in `excluded_identities`. The total still reflects
/// the entire queue so the operator can see how much review work remains.
pub(crate) fn select_review_batch_excluding(
    review_path: &Path,
    batch_size: usize,
    excluded_identities: &HashSet<String>,
) -> Result<(Vec<String>, usize, usize)> {
    let content = fs::read_to_string(review_path)
        .with_context(|| format!("failed to read {}", review_path.display()))?;
    let items = extract_review_items(&content);
    let total = items.len();
    let skipped = items
        .iter()
        .filter(|item| excluded_identities.contains(&item_identity(item)))
        .count();
    let candidates = items
        .into_iter()
        .filter(|item| !excluded_identities.contains(&item_identity(item)))
        .collect::<Vec<_>>();
    if batch_size == 0 || candidates.len() <= batch_size {
        return Ok((candidates, total, skipped));
    }
    let batch = candidates.into_iter().take(batch_size).collect();
    Ok((batch, total, skipped))
}

pub(crate) fn mechanically_triage_stale_review_items(
    repo_root: &Path,
    review_path: &Path,
    stale_items: &[String],
) -> Result<StaleTriageResult> {
    let stale_identities = stale_items
        .iter()
        .map(|item| item_identity(item))
        .collect::<HashSet<_>>();
    let review_content = fs::read_to_string(review_path)
        .with_context(|| format!("failed to read {}", review_path.display()))?;
    let review_items = extract_review_items(&review_content);
    let before_count = review_items.len();
    let remaining_items = review_items
        .into_iter()
        .filter(|item| !stale_identities.contains(&item_identity(item)))
        .collect::<Vec<_>>();
    let removed_count = before_count.saturating_sub(remaining_items.len());
    write_queue(review_path, REVIEW_HEADER, &remaining_items)?;

    let followup_path = stale_followup_path(repo_root);
    let appended_count = append_stale_review_followups(&followup_path, stale_items)?;

    Ok(StaleTriageResult {
        followup_path,
        removed_count,
        appended_count,
    })
}

pub(crate) fn stale_followup_path(repo_root: &Path) -> PathBuf {
    let implementation_plan = repo_root.join("IMPLEMENTATION_PLAN.md");
    if implementation_plan.exists() {
        implementation_plan
    } else {
        repo_root.join("WORKLIST.md")
    }
}

pub(crate) fn append_stale_review_followups(path: &Path, stale_items: &[String]) -> Result<usize> {
    let mut content = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        format!(
            "# {}\n\n",
            path.file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("FOLLOWUPS")
                .replace('-', " ")
                .to_ascii_uppercase()
        )
    };

    if !content.contains("## Auto Review Stale Follow-ups") {
        ensure_trailing_blank_line(&mut content);
        content.push_str("## Auto Review Stale Follow-ups\n\n");
    } else {
        ensure_trailing_blank_line(&mut content);
    }

    let mut appended = 0usize;
    for item in stale_items {
        let identity = item_identity(item);
        let marker = format!("Auto-review stale item: {identity}");
        if content.contains(&marker) {
            continue;
        }
        appended += 1;
        content.push_str(&format!("- [ ] {marker}\n"));
        content.push_str(
            "  - Source: `REVIEW.md`.\n\
             - Reason: `auto review` processed this item and then selected the \
             identical item set again, which means the reviewer did not archive, \
             remove, or convert it.\n\
             - Required outcome: review the current tree and either archive/remove \
             the stale REVIEW item if it is proven, or implement/document the \
             concrete blocker as a normal plan item.\n\
             - Original REVIEW.md item:\n",
        );
        content.push_str("    ```md\n");
        for line in item.lines() {
            content.push_str("    ");
            content.push_str(line);
            content.push('\n');
        }
        content.push_str("    ```\n\n");
    }

    atomic_write(path, content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(appended)
}

pub(crate) fn ensure_trailing_blank_line(content: &mut String) {
    while content.ends_with("\n\n\n") {
        content.pop();
    }
    if !content.ends_with('\n') {
        content.push('\n');
    }
    if !content.ends_with("\n\n") {
        content.push('\n');
    }
}

pub(crate) fn item_identity(item: &str) -> String {
    let first_line = item
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .trim_start_matches("## ")
        .trim_start_matches("- ")
        .trim_start_matches("* ")
        .trim()
        .to_string();
    first_line
}

/// Sorted identity set for a batch. Two batches with the same identity set
/// are considered "the same batch" even if the body prose drifted slightly.
pub(crate) fn batch_identity_set(batch: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = batch.iter().map(|item| item_identity(item)).collect();
    ids.sort();
    ids.dedup();
    ids
}

pub(crate) fn handoff_completed_items_to_review_queue(
    completed_path: &Path,
    review_path: &Path,
) -> Result<usize> {
    let completed_items = if completed_path.exists() {
        extract_review_items(
            &fs::read_to_string(completed_path)
                .with_context(|| format!("failed to read {}", completed_path.display()))?,
        )
    } else {
        Vec::new()
    };
    if completed_items.is_empty() {
        return Ok(0);
    }

    let mut review_items = if review_path.exists() {
        extract_review_items(
            &fs::read_to_string(review_path)
                .with_context(|| format!("failed to read {}", review_path.display()))?,
        )
    } else {
        Vec::new()
    };
    let moved_count = completed_items.len();
    review_items.extend(completed_items);

    write_queue(review_path, REVIEW_HEADER, &review_items)?;
    atomic_write(completed_path, EMPTY_COMPLETED_DOC.as_bytes())
        .with_context(|| format!("failed to reset {}", completed_path.display()))?;
    Ok(moved_count)
}

pub(crate) fn extract_review_items(content: &str) -> Vec<String> {
    #[derive(Clone, Copy)]
    enum ItemKind {
        Section,
        Bullet,
    }

    let mut items = Vec::new();
    let mut current = Vec::new();
    let mut kind: Option<ItemKind> = None;

    let flush =
        |items: &mut Vec<String>, current: &mut Vec<String>, kind: &mut Option<ItemKind>| {
            if !current.is_empty() {
                let item = current.join("\n").trim_end().to_string();
                if matches!(*kind, Some(ItemKind::Section)) || is_review_bullet_item(&item) {
                    items.push(item);
                }
                current.clear();
            }
            *kind = None;
        };

    for raw_line in content.lines() {
        let line = raw_line.trim_end();
        if line.starts_with("## ") {
            flush(&mut items, &mut current, &mut kind);
            current.push(line.to_string());
            kind = Some(ItemKind::Section);
            continue;
        }
        if is_bullet_review_item_start(line) {
            flush(&mut items, &mut current, &mut kind);
            current.push(line.to_string());
            kind = Some(ItemKind::Bullet);
            continue;
        }

        match kind {
            Some(ItemKind::Section) => current.push(line.to_string()),
            Some(ItemKind::Bullet) => {
                if line.trim().is_empty() || raw_line.starts_with(' ') || raw_line.starts_with('\t')
                {
                    current.push(line.to_string());
                } else {
                    flush(&mut items, &mut current, &mut kind);
                }
            }
            None => {}
        }
    }
    flush(&mut items, &mut current, &mut kind);
    items
}

pub(crate) fn is_bullet_review_item_start(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("- `") else {
        return false;
    };
    let Some(end_tick) = rest.find('`') else {
        return false;
    };
    let identity = &rest[..end_tick];
    looks_like_review_identity(identity)
}

pub(crate) fn looks_like_review_identity(identity: &str) -> bool {
    let identity = identity.trim();
    !identity.is_empty()
        && identity.len() <= 100
        && !identity.contains('/')
        && !identity.contains('\\')
        && !identity.contains('.')
        && !identity.chars().any(char::is_whitespace)
        && identity
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':'))
        && identity
            .chars()
            .any(|ch| ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

pub(crate) fn is_review_bullet_item(item: &str) -> bool {
    let lower = item.to_ascii_lowercase();
    let has_review_marker = lower.contains("awaiting_auto_review")
        || lower.contains("implementation handoff")
        || lower.contains("validation")
        || lower.contains("changed surfaces")
        || lower.contains("remaining blockers")
        || lower.contains("completed at")
        || lower.contains("symphony/linear");
    let looks_like_archive_note = lower.contains("were archived")
        || lower.contains("was archived")
        || lower.contains("already archived")
        || lower.contains("were removed")
        || lower.contains("was removed");

    has_review_marker || !looks_like_archive_note
}

pub(crate) fn write_queue(path: &Path, title: &str, items: &[String]) -> Result<()> {
    let mut content = String::from(title);
    content.push_str("\n\n");
    if !items.is_empty() {
        content.push_str(&items.join("\n\n"));
        content.push('\n');
    }
    atomic_write(path, content.as_bytes())
}

pub(crate) fn ensure_review_doc(review_path: &Path) -> Result<()> {
    if !review_path.exists() {
        atomic_write(review_path, format!("{REVIEW_HEADER}\n\n").as_bytes())
            .with_context(|| format!("failed to initialize {}", review_path.display()))?;
    }
    Ok(())
}

pub(crate) fn ensure_review_docs(review_path: &Path, archived_path: &Path) -> Result<()> {
    ensure_review_doc(review_path)?;
    if !archived_path.exists() {
        atomic_write(archived_path, format!("{ARCHIVED_HEADER}\n\n").as_bytes())
            .with_context(|| format!("failed to initialize {}", archived_path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        batch_identity_set, ensure_review_docs, extract_review_items, item_identity,
        mechanically_triage_stale_review_items, select_review_batch, select_review_batch_excluding,
        ARCHIVED_HEADER, REVIEW_HEADER,
    };

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-review-test-{nanos}"))
    }

    #[test]
    fn extracts_bullet_review_items() {
        let content = "# COMPLETED\n\n- `VAL-001` Added validation\n  Validation:\n  `cargo test`\n\n- `SEC-001` Hardened auth\n  Note: tightened auth boundary\n";
        let items = extract_review_items(content);
        assert_eq!(
            items,
            vec![
                "- `VAL-001` Added validation\n  Validation:\n  `cargo test`".to_string(),
                "- `SEC-001` Hardened auth\n  Note: tightened auth boundary".to_string()
            ]
        );
    }

    #[test]
    fn extracts_section_review_items() {
        let content = "# COMPLETED\n\n## `VAL-001` Added validation\nValidation: pytest\n\n## `SEC-001` Hardened auth\nValidation: ruff check";
        let items = extract_review_items(content);
        assert_eq!(items.len(), 2);
        assert!(items[0].starts_with("## `VAL-001`"));
        assert!(items[1].starts_with("## `SEC-001`"));
    }

    #[test]
    fn extracts_mixed_section_and_bullet_review_items() {
        let content = "# REVIEW\n\n## `WEB-CRAPS-E`\n- Changed surfaces: `web/client/test/craps-catalog.test.ts`\n- Remaining blockers: failing full web suite\n\n- `WEB-HOUSE-AUDIT`: Symphony/Linear completion backfill recorded; status `awaiting_auto_review`.\n\n- `WEB-CHANNEL-COVERAGE`: Symphony/Linear completion backfill recorded; status `awaiting_auto_review`.\n\n## `PROD-GATE-CRAPS-PRODUCTION`\n- Files: `docs/ops/operator-evidence/production-confidence-2026-04-18.md`\n- Remaining blockers: release proof missing\n";
        let items = extract_review_items(content);
        assert_eq!(items.len(), 4);
        assert!(items[0].starts_with("## `WEB-CRAPS-E`"));
        assert!(items[1].starts_with("- `WEB-HOUSE-AUDIT`:"));
        assert!(items[2].starts_with("- `WEB-CHANNEL-COVERAGE`:"));
        assert!(items[3].starts_with("## `PROD-GATE-CRAPS-PRODUCTION`"));
        assert!(
            !items[0].contains("WEB-HOUSE-AUDIT"),
            "top-level backfill bullets must not be swallowed by the preceding section"
        );
    }

    #[test]
    fn extracts_multiline_multi_id_bullet_review_item() {
        let content = "# REVIEW\n\n- `V70-W-2e`, `V70-W-2f`, `V70-W-2l`, `V70-W-2p`,\n  `V70-W-3b-pre`, `V70-W-3b`, `V70-W-3e`, `V70-W-3f`:\n  remaining implementation-plan items completed at 2026-04-21 10:40 local;\n  validation `cargo test`; remaining blockers none.\n\n- `W2-NS-17`: Plane 2 exchange primitives completed at 2026-04-20 15:04 UTC;\n  validation `cargo test`; remaining blockers none.\n";
        let items = extract_review_items(content);
        assert_eq!(items.len(), 2);
        assert!(items[0].starts_with("- `V70-W-2e`, `V70-W-2f`"));
        assert!(items[0].contains("`V70-W-3f`:"));
        assert!(items[1].starts_with("- `W2-NS-17`:"));
    }

    #[test]
    fn does_not_split_section_on_required_backtick_bullets() {
        let content = "# REVIEW\n\n## `P-034A` Epoch Pipeline Orchestrator\n- `barely-human/src/governance/epoch_pipeline.rs`\n- `Required` Current-tree `cargo fmt -p barely-human -- --check` failed on unrelated drift.\n- `cargo test -p barely-human --lib epoch_pipeline_e1_through_e8_execute_deterministically_in_order` -> passed\n";
        let items = extract_review_items(content);
        assert_eq!(items.len(), 1);
        assert!(items[0].contains("`Required` Current-tree"));
    }

    #[test]
    fn ignores_explanatory_backtick_bullets_that_are_not_queue_items() {
        let content = "# REVIEW\n\nNo reviewable items remain.\n\n- `TASK-A`, `TASK-B`, and `TASK-C` were archived.\n";
        let items = extract_review_items(content);
        assert!(
            items.is_empty(),
            "comma-delimited explanatory bullets should not reopen an empty queue: {items:?}"
        );
    }

    #[test]
    fn initializes_review_and_archived_docs() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        let review_path = temp.join("REVIEW.md");
        let archived_path = temp.join("ARCHIVED.md");

        ensure_review_docs(&review_path, &archived_path).expect("init docs");

        assert_eq!(
            fs::read_to_string(review_path).expect("read review"),
            format!("{REVIEW_HEADER}\n\n")
        );
        assert_eq!(
            fs::read_to_string(archived_path).expect("read archived"),
            format!("{ARCHIVED_HEADER}\n\n")
        );

        fs::remove_dir_all(temp).expect("cleanup temp dir");
    }

    #[test]
    fn select_review_batch_respects_batch_size() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        let review_path = temp.join("REVIEW.md");
        fs::write(
            &review_path,
            "# REVIEW\n\n- `A-1` one\n- `B-2` two\n- `C-3` three\n- `D-4` four\n",
        )
        .expect("write review");

        let (batch, total) = select_review_batch(&review_path, 2).expect("select");
        assert_eq!(total, 4);
        assert_eq!(batch.len(), 2);
        assert!(batch[0].starts_with("- `A-1`"));
        assert!(batch[1].starts_with("- `B-2`"));

        let (all_batch, _) = select_review_batch(&review_path, 0).expect("select all");
        assert_eq!(
            all_batch.len(),
            4,
            "batch_size 0 must fall back to all items"
        );

        fs::remove_dir_all(temp).expect("cleanup");
    }

    #[test]
    fn select_review_batch_excluding_skips_stale_identities() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        let review_path = temp.join("REVIEW.md");
        fs::write(
            &review_path,
            "# REVIEW\n\n- `A-1` one\n- `B-2` two\n- `C-3` three\n",
        )
        .expect("write review");
        let excluded = HashSet::from(["`A-1` one".to_string(), "`B-2` two".to_string()]);

        let (batch, total, skipped) =
            select_review_batch_excluding(&review_path, 2, &excluded).expect("select");

        assert_eq!(total, 3);
        assert_eq!(skipped, 2);
        assert_eq!(batch, vec!["- `C-3` three".to_string()]);

        fs::remove_dir_all(temp).expect("cleanup");
    }

    #[test]
    fn stale_review_triage_moves_items_into_implementation_plan_followups() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        let review_path = temp.join("REVIEW.md");
        let plan_path = temp.join("IMPLEMENTATION_PLAN.md");
        fs::write(
            &review_path,
            "# REVIEW\n\n## 2026-04-21 Loom E2E QA Remediation Handoff\n\n- Required: durable proof.\n\n- `OLYMPIAD-SOLVER-HEURISTIC-BUNDLE-1`: proof passed.\n\n- `NEXT-ITEM` keep me\n",
        )
        .expect("write review");
        fs::write(&plan_path, "# IMPLEMENTATION_PLAN\n\n").expect("write plan");
        let stale_items = vec![
            "## 2026-04-21 Loom E2E QA Remediation Handoff\n\n- Required: durable proof."
                .to_string(),
            "- `OLYMPIAD-SOLVER-HEURISTIC-BUNDLE-1`: proof passed.".to_string(),
        ];

        let result = mechanically_triage_stale_review_items(&temp, &review_path, &stale_items)
            .expect("triage stale items");

        assert_eq!(result.followup_path, plan_path);
        assert_eq!(result.removed_count, 2);
        assert_eq!(result.appended_count, 2);
        let review = fs::read_to_string(&review_path).expect("read review");
        assert!(!review.contains("Loom E2E"));
        assert!(!review.contains("OLYMPIAD-SOLVER-HEURISTIC-BUNDLE-1"));
        assert!(review.contains("`NEXT-ITEM` keep me"));
        let plan = fs::read_to_string(&plan_path).expect("read plan");
        assert!(plan.contains("## Auto Review Stale Follow-ups"));
        assert!(plan.contains("Auto-review stale item: 2026-04-21 Loom E2E QA Remediation Handoff"));
        assert!(plan.contains(
            "Auto-review stale item: `OLYMPIAD-SOLVER-HEURISTIC-BUNDLE-1`: proof passed."
        ));
        assert!(plan.contains("Original REVIEW.md item"));

        fs::remove_dir_all(temp).expect("cleanup");
    }

    #[test]
    fn stale_review_triage_falls_back_to_worklist_without_plan() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        let review_path = temp.join("REVIEW.md");
        fs::write(&review_path, "# REVIEW\n\n- `A-1` stuck\n").expect("write review");

        let result = mechanically_triage_stale_review_items(
            &temp,
            &review_path,
            &["- `A-1` stuck".to_string()],
        )
        .expect("triage stale item");

        assert_eq!(result.followup_path, temp.join("WORKLIST.md"));
        assert_eq!(result.removed_count, 1);
        let worklist = fs::read_to_string(temp.join("WORKLIST.md")).expect("read worklist");
        assert!(worklist.contains("Auto-review stale item: `A-1` stuck"));
        let review = fs::read_to_string(&review_path).expect("read review");
        assert_eq!(review, "# REVIEW\n\n");

        fs::remove_dir_all(temp).expect("cleanup");
    }

    #[test]
    fn item_identity_strips_leading_markers_and_dedups() {
        assert_eq!(item_identity("- `A-1` thing"), "`A-1` thing");
        assert_eq!(item_identity("## `A-1` thing"), "`A-1` thing");
        assert_eq!(item_identity("   \n  - `A-1` thing"), "`A-1` thing");
        let ids = batch_identity_set(&[
            "- `A-1` one".to_string(),
            "## `B-2` two".to_string(),
            "- `A-1` one".to_string(),
        ]);
        assert_eq!(ids, vec!["`A-1` one".to_string(), "`B-2` two".to_string()]);
    }
}
