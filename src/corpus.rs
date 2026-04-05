use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::copy_tree;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlanningCorpus {
    pub(crate) planning_root: String,
    pub(crate) assessment_path: Option<String>,
    pub(crate) design_path: Option<String>,
    pub(crate) idea_path: Option<String>,
    pub(crate) report_path: Option<String>,
    pub(crate) plans_index_path: Option<String>,
    pub(crate) spec_path: Option<String>,
    pub(crate) specs_index_path: Option<String>,
    pub(crate) spec_documents: Vec<CorpusSpecDocument>,
    pub(crate) primary_plans: Vec<CorpusPlanDocument>,
    pub(crate) support_documents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CorpusPlanDocument {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) source_specs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CorpusSpecDocument {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) topic_key: String,
}

pub(crate) fn load_planning_corpus(planning_root: &Path) -> Result<PlanningCorpus> {
    let planning_root = resolve_planning_root(planning_root)?;
    let plans_dir = planning_root.join("plans");
    if !plans_dir.is_dir() {
        bail!(
            "planning root {} is missing plans/",
            planning_root.display()
        );
    }

    let mut primary_paths = Vec::new();
    let mut support_paths = Vec::new();
    collect_plan_paths(
        &plans_dir,
        &plans_dir,
        &mut primary_paths,
        &mut support_paths,
    )?;
    primary_paths.sort();
    support_paths.sort();

    let mut primary_plans = Vec::new();
    for path in primary_paths {
        let markdown = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        primary_plans.push(CorpusPlanDocument {
            path: relative_to_root(&planning_root, &path),
            title: extract_title(&markdown).unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|v| v.to_str())
                    .unwrap_or("untitled-plan")
                    .to_string()
            }),
            source_specs: match_specs_for_plan(&planning_root, &markdown)?,
        });
    }

    Ok(PlanningCorpus {
        planning_root: planning_root.display().to_string(),
        assessment_path: optional_doc_path(&planning_root, "ASSESSMENT.md"),
        design_path: optional_doc_path(&planning_root, "DESIGN.md"),
        idea_path: optional_doc_path(&planning_root, "IDEA.md"),
        report_path: optional_doc_path(&planning_root, "GENESIS-REPORT.md"),
        plans_index_path: optional_doc_path(&planning_root, "PLANS.md"),
        spec_path: optional_doc_path(&planning_root, "SPEC.md"),
        specs_index_path: optional_doc_path(&planning_root, "specs/INDEX.md"),
        spec_documents: load_spec_documents(&planning_root)?,
        primary_plans,
        support_documents: support_paths
            .into_iter()
            .map(|path| relative_to_root(&planning_root, &path))
            .collect(),
    })
}

pub(crate) fn emit_corpus_snapshot(corpus: &PlanningCorpus, output_root: &Path) -> Result<()> {
    let corpus_root = output_root.join("corpus");
    fs::create_dir_all(&corpus_root)
        .with_context(|| format!("failed to create {}", corpus_root.display()))?;

    let planning_root = PathBuf::from(&corpus.planning_root);
    for doc in [
        corpus.assessment_path.as_deref(),
        corpus.design_path.as_deref(),
        corpus.idea_path.as_deref(),
        corpus.report_path.as_deref(),
        corpus.plans_index_path.as_deref(),
        corpus.spec_path.as_deref(),
        corpus.specs_index_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        copy_relative_doc(&planning_root, &corpus_root, doc)?;
    }
    for spec in &corpus.spec_documents {
        copy_relative_doc(&planning_root, &corpus_root, &spec.path)?;
    }
    for plan in &corpus.primary_plans {
        copy_relative_doc(&planning_root, &corpus_root, &plan.path)?;
    }
    for support in &corpus.support_documents {
        copy_relative_doc(&planning_root, &corpus_root, support)?;
    }
    Ok(())
}

fn resolve_planning_root(planning_root: &Path) -> Result<PathBuf> {
    if !planning_root.exists() {
        bail!("planning root {} does not exist", planning_root.display());
    }
    if planning_root.join("plans").is_dir() {
        return Ok(planning_root.to_path_buf());
    }
    let nested = planning_root.join("corpus");
    if nested.join("plans").is_dir() {
        return Ok(nested);
    }
    bail!(
        "planning root {} is missing plans/",
        planning_root.display()
    )
}

fn collect_plan_paths(
    planning_root: &Path,
    current_dir: &Path,
    primary_paths: &mut Vec<PathBuf>,
    support_paths: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(current_dir)
        .with_context(|| format!("failed to read {}", current_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_plan_paths(planning_root, &path, primary_paths, support_paths)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            if current_dir == planning_root && is_primary_plan_file(&path) {
                primary_paths.push(path);
            } else {
                support_paths.push(path);
            }
        }
    }
    Ok(())
}

fn is_primary_plan_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|v| v.to_str())
        .and_then(|name| name.chars().next())
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
}

fn load_spec_documents(planning_root: &Path) -> Result<Vec<CorpusSpecDocument>> {
    let specs_dir = planning_root.join("specs");
    if !specs_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    collect_markdown_paths(&specs_dir, &mut paths)?;
    paths.sort();
    let mut docs = Vec::new();
    for path in paths {
        let markdown = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let title = extract_title(&markdown).unwrap_or_else(|| {
            path.file_stem()
                .and_then(|v| v.to_str())
                .unwrap_or("untitled-spec")
                .to_string()
        });
        let topic_key = path
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or("untitled-spec")
            .to_string();
        docs.push(CorpusSpecDocument {
            path: relative_to_root(planning_root, &path),
            title,
            topic_key,
        });
    }
    Ok(docs)
}

fn collect_markdown_paths(current_dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(current_dir)
        .with_context(|| format!("failed to read {}", current_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_paths(&path, paths)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    Ok(())
}

fn match_specs_for_plan(planning_root: &Path, markdown: &str) -> Result<Vec<String>> {
    let spec_docs = load_spec_documents(planning_root)?;
    let lower = markdown.to_ascii_lowercase();
    let mut matches = Vec::new();
    for spec in spec_docs {
        let title_key = spec
            .title
            .to_ascii_lowercase()
            .replace('#', "")
            .trim()
            .to_string();
        let topic_key = spec.topic_key.to_ascii_lowercase().replace('_', "-");
        if lower.contains(&title_key) || lower.contains(&topic_key) {
            matches.push(spec.path);
        }
    }
    matches.sort();
    matches.dedup();
    Ok(matches)
}

fn optional_doc_path(planning_root: &Path, relative: &str) -> Option<String> {
    let path = planning_root.join(relative);
    path.exists()
        .then(|| relative_to_root(planning_root, &path))
}

fn relative_to_root(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn copy_relative_doc(planning_root: &Path, output_root: &Path, relative: &str) -> Result<()> {
    let src = planning_root.join(relative);
    if !src.exists() {
        bail!("expected corpus document {} to exist", src.display());
    }
    let dst = output_root.join(relative);
    copy_tree(&src, &dst)
}

fn extract_title(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|title| title.trim().to_string())
}
