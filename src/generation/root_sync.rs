//! Syncing reviewed generation outputs back into the repository root.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};

use crate::generation::plan_verify::{
    append_blocks_to_section, collect_available_spec_refs, ensure_required_plan_sections,
    extract_plan_task_blocks, validate_plan_spec_refs, PlanSection,
};
use crate::generation::spec_verify::spec_topic_slug;
use crate::generation::{GeneratedSpecDocument, GenerationMode};
use crate::util::atomic_write;

#[derive(Default)]
pub(crate) struct SpecSyncSummary {
    pub(crate) appended_paths: Vec<PathBuf>,
    pub(crate) skipped_count: usize,
}

pub(crate) fn sync_generated_specs_to_root(
    repo_root: &Path,
    generated_specs: &[GeneratedSpecDocument],
) -> Result<SpecSyncSummary> {
    sync_generated_specs_to_root_for_date(repo_root, generated_specs, Local::now().date_naive())
}

fn sync_generated_specs_to_root_for_date(
    repo_root: &Path,
    generated_specs: &[GeneratedSpecDocument],
    today: NaiveDate,
) -> Result<SpecSyncSummary> {
    let root_specs_dir = repo_root.join("specs");
    fs::create_dir_all(&root_specs_dir)
        .with_context(|| format!("failed to create {}", root_specs_dir.display()))?;
    let mut summary = SpecSyncSummary::default();
    let date_prefix = today.format("%d%m%y").to_string();

    for spec in generated_specs {
        let source_name = spec
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .context("generated spec must have a file stem")?;
        let slug = spec_topic_slug(source_name);
        let extension = spec
            .path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("md");
        remove_same_day_topic_snapshots(&root_specs_dir, &date_prefix, &slug, extension)?;
        let destination = root_specs_dir.join(format!("{date_prefix}-{slug}.{extension}"));
        fs::copy(&spec.path, &destination).with_context(|| {
            format!(
                "failed to copy {} -> {}",
                spec.path.display(),
                destination.display()
            )
        })?;
        summary.appended_paths.push(destination);
    }

    Ok(summary)
}

pub(crate) fn sync_generated_plan_to_root_preserving_open_tasks(
    repo_root: &Path,
    generated_plan: &Path,
) -> Result<PathBuf> {
    let root_plan = repo_root.join("IMPLEMENTATION_PLAN.md");
    let generated_markdown = fs::read_to_string(generated_plan)
        .with_context(|| format!("failed to read {}", generated_plan.display()))?;
    let merged = if root_plan.exists() {
        let existing = fs::read_to_string(&root_plan)
            .with_context(|| format!("failed to read {}", root_plan.display()))?;
        merge_generated_plan_with_existing_open_tasks(&generated_markdown, &existing)?
    } else {
        generated_markdown
    };
    atomic_write(&root_plan, merged.as_bytes())
        .with_context(|| format!("failed to write {}", root_plan.display()))?;
    Ok(root_plan)
}

pub(crate) fn rewrite_generated_plan_spec_refs(
    generated_plan: &Path,
    root_specs: &SpecSyncSummary,
) -> Result<()> {
    if root_specs.appended_paths.is_empty() {
        return Ok(());
    }

    let markdown = fs::read_to_string(generated_plan)
        .with_context(|| format!("failed to read {}", generated_plan.display()))?;
    let rewritten = rewrite_plan_spec_refs_to_root(&markdown, root_specs);
    if rewritten == markdown {
        return Ok(());
    }

    atomic_write(generated_plan, rewritten.as_bytes())
        .with_context(|| format!("failed to rewrite {}", generated_plan.display()))?;
    Ok(())
}

fn rewrite_plan_spec_refs_to_root(markdown: &str, root_specs: &SpecSyncSummary) -> String {
    let slug_to_root = root_specs
        .appended_paths
        .iter()
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?;
            let slug = spec_topic_slug(stem);
            let relative = Path::new("specs").join(path.file_name()?);
            Some((slug, relative.display().to_string()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    let mut changed = false;
    let rewritten_lines = markdown
        .lines()
        .map(|line| rewrite_plan_spec_line(line, &slug_to_root, &mut changed))
        .collect::<Vec<_>>();
    if !changed {
        return markdown.to_string();
    }

    let mut rewritten = rewritten_lines.join("\n");
    if markdown.ends_with('\n') {
        rewritten.push('\n');
    }
    rewritten
}

fn rewrite_plan_spec_line(
    line: &str,
    slug_to_root: &std::collections::BTreeMap<String, String>,
    changed: &mut bool,
) -> String {
    let Some(spec_index) = line.find("Spec:") else {
        return line.to_string();
    };
    let prefix = &line[..spec_index];
    let rest = line[spec_index + "Spec:".len()..].trim();
    let unquoted = rest.trim_matches('`');
    let path = Path::new(unquoted);
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return line.to_string();
    };
    let slug = spec_topic_slug(stem);
    let Some(root_path) = slug_to_root.get(&slug) else {
        return line.to_string();
    };
    let normalized = format!("{prefix}Spec: `{root_path}`");
    if normalized != line {
        *changed = true;
    }
    normalized
}

fn remove_same_day_topic_snapshots(
    root_specs_dir: &Path,
    date_prefix: &str,
    slug: &str,
    extension: &str,
) -> Result<()> {
    for existing in find_same_day_topic_snapshots(root_specs_dir, date_prefix, slug, extension)? {
        fs::remove_file(&existing)
            .with_context(|| format!("failed to remove {}", existing.display()))?;
    }
    Ok(())
}

fn find_same_day_topic_snapshots(
    root_specs_dir: &Path,
    date_prefix: &str,
    slug: &str,
    extension: &str,
) -> Result<Vec<PathBuf>> {
    let canonical_stem = format!("{date_prefix}-{slug}");
    let duplicate_prefix = format!("{canonical_stem}-");
    let mut matches = Vec::new();
    for entry in fs::read_dir(root_specs_dir)
        .with_context(|| format!("failed to read {}", root_specs_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read {}", root_specs_dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some(extension) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if stem == canonical_stem {
            matches.push(path);
            continue;
        }
        let Some(suffix) = stem.strip_prefix(&duplicate_prefix) else {
            continue;
        };
        if !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()) {
            matches.push(path);
        }
    }
    Ok(matches)
}

pub(crate) fn scrub_root_generated_outputs(repo_root: &Path, mode: GenerationMode) -> Result<()> {
    let available_specs = collect_available_spec_refs(&repo_root.join("specs"))?;
    if mode == GenerationMode::Gen {
        let root_plan = repo_root.join("IMPLEMENTATION_PLAN.md");
        if root_plan.exists() {
            let markdown = fs::read_to_string(&root_plan)
                .with_context(|| format!("failed to read {}", root_plan.display()))?;
            validate_plan_spec_refs(
                &markdown,
                &available_specs,
                &format!("root implementation plan {}", root_plan.display()),
                false,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn merge_generated_plan_with_existing_open_tasks(
    generated: &str,
    existing: &str,
) -> Result<String> {
    let generated = ensure_required_plan_sections(generated);
    let generated_blocks = extract_plan_task_blocks(&generated)?;
    let existing_blocks = extract_plan_task_blocks(existing)?;
    let generated_ids = generated_blocks
        .iter()
        .map(|block| block.task_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let preserved_blocks = existing_blocks
        .into_iter()
        .filter(|block| !block.checked && !generated_ids.contains(block.task_id.as_str()))
        .collect::<Vec<_>>();
    if preserved_blocks.is_empty() {
        return Ok(generated);
    }
    let mut merged = generated;
    append_blocks_to_section(&mut merged, PlanSection::Priority, &preserved_blocks)?;
    append_blocks_to_section(&mut merged, PlanSection::FollowOn, &preserved_blocks)?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::{
        rewrite_plan_spec_refs_to_root, scrub_root_generated_outputs,
        sync_generated_specs_to_root_for_date, SpecSyncSummary,
    };
    use crate::generation::tests::temp_dir;
    use crate::generation::{GeneratedSpecDocument, GenerationMode};
    use chrono::NaiveDate;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn rewrites_plan_spec_refs_to_actual_root_snapshots() {
        let markdown = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `WS-01` Scaffold workspace
Spec: `specs/050426-workspace-build-system.md`

## Follow-On Work

- [ ] `TR-01` Build transcripts
Spec: `specs/050426-deterministic-transcripts.md`

## Completed / Already Satisfied
"#;

        let rewritten = rewrite_plan_spec_refs_to_root(
            markdown,
            &SpecSyncSummary {
                appended_paths: vec![
                    PathBuf::from("/tmp/specs/040426-workspace-build-system.md"),
                    PathBuf::from("/tmp/specs/040426-deterministic-transcripts.md"),
                ],
                skipped_count: 0,
            },
        );

        assert!(rewritten.contains("Spec: `specs/040426-workspace-build-system.md`"));
        assert!(rewritten.contains("Spec: `specs/040426-deterministic-transcripts.md`"));
        assert!(!rewritten.contains("050426"));
    }

    #[test]
    fn root_scrub_ignores_legacy_non_generated_spec_bullets() {
        let root = temp_dir("root-legacy-spec-bullet");
        let specs_dir = root.join("specs");
        fs::create_dir_all(&specs_dir).unwrap();
        fs::write(specs_dir.join("050426-real.md"), "# Specification: Real\n").unwrap();
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [~] `OLD-001` Legacy task\n  - Spec: `SECURITY_PLAN.md`; `steward/RETIRE.md`.\n  - Why now: existing queue item.\n\n- [ ] `NEW-001` Generated task\n    Spec: `specs/050426-real.md`\n    Why now: generated queue item.\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
        )
        .unwrap();

        scrub_root_generated_outputs(&root, GenerationMode::Gen)
            .expect("root scrub should ignore legacy non-generated Spec bullets");
    }

    #[test]
    fn root_scrub_ignores_missing_legacy_spec_refs() {
        let root = temp_dir("root-missing-legacy-spec-ref");
        let specs_dir = root.join("specs");
        fs::create_dir_all(&specs_dir).unwrap();
        fs::write(specs_dir.join("050426-real.md"), "# Specification: Real\n").unwrap();
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [~] `OLD-001` Legacy task\n  - Spec: `specs/olympiad/190426-missing.md`\n  - Why now: existing queue item.\n\n- [ ] `NEW-001` Generated task\n    Spec: `specs/050426-real.md`\n    Why now: generated queue item.\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n",
        )
        .unwrap();

        scrub_root_generated_outputs(&root, GenerationMode::Gen)
            .expect("root scrub should ignore missing legacy spec refs");
    }

    #[test]
    fn sync_replaces_same_day_duplicate_root_specs_with_canonical_snapshot() {
        let repo_root = temp_dir("spec-sync");
        let root_specs = repo_root.join("specs");
        fs::create_dir_all(&root_specs).unwrap();
        fs::write(
            root_specs.join("050426-example-topic.md"),
            "old canonical snapshot\n",
        )
        .unwrap();
        fs::write(
            root_specs.join("050426-example-topic-2.md"),
            "stale duplicate snapshot\n",
        )
        .unwrap();

        let output_dir = temp_dir("spec-output");
        let generated_path = output_dir.join("050426-example-topic.md");
        fs::write(&generated_path, "fresh generated snapshot\n").unwrap();
        let generated = GeneratedSpecDocument {
            path: generated_path,
            text: "fresh generated snapshot\n".to_string(),
        };

        let summary = sync_generated_specs_to_root_for_date(
            &repo_root,
            &[generated],
            NaiveDate::from_ymd_opt(2026, 4, 5).unwrap(),
        )
        .unwrap();

        assert_eq!(summary.appended_paths.len(), 1);
        assert_eq!(
            fs::read_to_string(root_specs.join("050426-example-topic.md")).unwrap(),
            "fresh generated snapshot\n"
        );
        assert!(!root_specs.join("050426-example-topic-2.md").exists());
    }
}
