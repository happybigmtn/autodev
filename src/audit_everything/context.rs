//! Context-layer artifacts: the skill-policy reference files, the doctrine
//! hash, and the context bundle assembled for first-pass workers.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::audit_everything::prompts::{CODEBASE_IMPROVEMENT_POLICY, GSTACK_SKILL_POLICY};
use crate::audit_everything::run_paths::{codebase_improvement_policy_path, RunPaths};
use crate::audit_everything::{collect_regular_files, sha256_hex};
use crate::util::atomic_write;

pub(crate) fn write_skill_policy_reference(paths: &RunPaths) -> Result<()> {
    std::fs::create_dir_all(&paths.report_root)
        .with_context(|| format!("failed to create {}", paths.report_root.display()))?;
    atomic_write(
        &paths.report_root.join("GSTACK-SKILL-POLICY.md"),
        GSTACK_SKILL_POLICY.as_bytes(),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            paths.report_root.join("GSTACK-SKILL-POLICY.md").display()
        )
    })?;
    atomic_write(
        &codebase_improvement_policy_path(paths),
        CODEBASE_IMPROVEMENT_POLICY.as_bytes(),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            codebase_improvement_policy_path(paths).display()
        )
    })
}

pub(crate) fn read_context_bundle(paths: &RunPaths) -> Result<String> {
    let bundle = paths.report_root.join("CONTEXT-BUNDLE.md");
    if bundle.exists() {
        return std::fs::read_to_string(&bundle)
            .with_context(|| format!("failed to read {}", bundle.display()));
    }
    write_context_bundle(paths)?;
    std::fs::read_to_string(&bundle).with_context(|| format!("failed to read {}", bundle.display()))
}

pub(crate) fn write_context_bundle(paths: &RunPaths) -> Result<()> {
    std::fs::create_dir_all(&paths.report_root)
        .with_context(|| format!("failed to create {}", paths.report_root.display()))?;
    let mut body = String::new();
    body.push_str("# Context Bundle\n\n");
    append_named_file(
        &mut body,
        "AGENTS.md",
        &paths.worktree_root.join("AGENTS.md"),
        true,
    )?;
    append_named_file(
        &mut body,
        "ARCHITECTURE.md",
        &paths.worktree_root.join("ARCHITECTURE.md"),
        true,
    )?;
    append_named_file(
        &mut body,
        "GSTACK-SKILL-POLICY.md",
        &paths.report_root.join("GSTACK-SKILL-POLICY.md"),
        true,
    )?;
    append_named_file(
        &mut body,
        "CODEBASE-IMPROVEMENT-POLICY.md",
        &codebase_improvement_policy_path(paths),
        true,
    )?;
    let doctrine_dir = paths.worktree_root.join("doctrine");
    if doctrine_dir.is_dir() {
        let mut doctrine_files = collect_regular_files(&doctrine_dir)?;
        doctrine_files.sort();
        for path in doctrine_files {
            let rel = path
                .strip_prefix(&paths.worktree_root)
                .unwrap_or(&path)
                .display()
                .to_string();
            append_named_file(&mut body, &rel, &path, false)?;
        }
    }
    atomic_write(
        &paths.report_root.join("CONTEXT-BUNDLE.md"),
        body.as_bytes(),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            paths.report_root.join("CONTEXT-BUNDLE.md").display()
        )
    })
}

fn append_named_file(body: &mut String, name: &str, path: &Path, required: bool) -> Result<()> {
    if !path.exists() {
        if required {
            bail!("required context file missing: {}", path.display());
        }
        return Ok(());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if text.trim().is_empty() {
        if required {
            bail!("required context file is empty: {}", path.display());
        }
        return Ok(());
    }
    body.push_str(&format!("## {name}\n\n```markdown\n{text}\n```\n\n"));
    Ok(())
}

pub(crate) fn hash_file_if_exists(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(sha256_hex(&bytes)))
}

pub(crate) fn hash_doctrine(repo_root: &Path) -> Result<String> {
    let doctrine_dir = repo_root.join("doctrine");
    if !doctrine_dir.is_dir() {
        return Ok(sha256_hex(b""));
    }
    let mut files = collect_regular_files(&doctrine_dir)?;
    files.sort();
    let mut bytes = Vec::new();
    for path in files {
        bytes.extend(path.display().to_string().as_bytes());
        bytes.push(0);
        bytes.extend(
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
        );
        bytes.push(0);
    }
    Ok(sha256_hex(&bytes))
}
