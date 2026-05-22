//! Bug-run report writing and output-directory preparation.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;

use crate::bug_command::types::{ChunkOutcome, FinalReviewResult, FixResult};
use crate::util::{atomic_write, copy_tree, timestamp_slug};

pub(crate) fn write_bug_summary(
    output_dir: &Path,
    outcomes: &[ChunkOutcome],
    fixes: &[FixResult],
    final_reviews: &[FinalReviewResult],
    report_only: bool,
) -> Result<()> {
    let all_accepted = outcomes
        .iter()
        .flat_map(|outcome| outcome.accepted.clone())
        .collect::<Vec<_>>();
    let all_verified = outcomes
        .iter()
        .flat_map(|outcome| outcome.verified.clone())
        .collect::<Vec<_>>();
    let all_reviews = outcomes
        .iter()
        .flat_map(|outcome| outcome.reviews.clone())
        .collect::<Vec<_>>();

    let mut markdown = String::new();
    markdown.push_str("# BUG_REPORT\n\n");
    markdown.push_str(&format!(
        "- Generated: `{}`\n",
        Local::now().format("%Y-%m-%d %H:%M:%S %Z")
    ));
    markdown.push_str(&format!("- Chunks audited: `{}`\n", outcomes.len()));
    markdown.push_str(&format!(
        "- Findings reported: `{}`\n",
        outcomes
            .iter()
            .map(|outcome| outcome.findings.len())
            .sum::<usize>()
    ));
    markdown.push_str(&format!("- Findings accepted: `{}`\n", all_accepted.len()));
    markdown.push_str(&format!("- Findings verified: `{}`\n", all_verified.len()));
    markdown.push_str(&format!(
        "- Findings disproved: `{}`\n",
        outcomes
            .iter()
            .map(|outcome| outcome.disproved_count)
            .sum::<usize>()
    ));
    markdown.push_str(&format!("- Implementation results: `{}`\n", fixes.len()));
    markdown.push_str(&format!(
        "- Final review results: `{}`\n",
        final_reviews.len()
    ));
    markdown.push_str(&format!("- Review verdicts: `{}`\n", all_reviews.len()));
    markdown.push_str(&format!(
        "- Mode: `{}`\n\n",
        if report_only {
            "report-only"
        } else {
            "verify-and-implement"
        }
    ));

    markdown.push_str("## Chunk Summary\n\n");
    for outcome in outcomes {
        markdown.push_str(&format!(
            "- `{}` (`{}`): {} reported, {} accepted, {} verified, {} disproved\n",
            outcome.chunk.id,
            outcome.chunk.scope_label,
            outcome.findings.len(),
            outcome.accepted.len(),
            outcome.verified.len(),
            outcome.disproved_count
        ));
    }

    markdown.push_str("\n## Verified Findings\n\n");
    if all_verified.is_empty() {
        markdown.push_str("No verified findings survived the review pass.\n");
    } else {
        for finding in &all_verified {
            markdown.push_str(&format!(
                "### `{}` {} (`{}` / {} points)\n\n",
                finding.bug_id, finding.title, finding.impact, finding.points
            ));
            markdown.push_str(&format!("- Chunk: `{}`\n", finding.chunk_id));
            markdown.push_str(&format!("- Location: `{}`\n", finding.location));
            markdown.push_str(&format!("- Description: {}\n", finding.description));
            markdown.push_str(&format!(
                "- Skeptic confidence: `{}`\n",
                finding.skeptic_confidence_percent
            ));
            markdown.push_str(&format!(
                "- Skeptic counter: {}\n\n",
                finding.skeptic_counter_argument
            ));
        }
    }

    markdown.push_str("## Verification Review\n\n");
    if all_reviews.is_empty() {
        markdown.push_str("No verification review output captured.\n");
    } else {
        for review in &all_reviews {
            markdown.push_str(&format!(
                "- `{}`: `{}` ({})\n",
                review.bug_id, review.verdict, review.confidence
            ));
        }
    }

    if !report_only {
        markdown.push_str("\n## Implementation Results\n\n");
        if fixes.is_empty() {
            markdown.push_str("No implementation output captured.\n");
        } else {
            for fix in fixes {
                markdown.push_str(&format!("- `{}`: `{}`\n", fix.bug_id, fix.status));
            }
        }

        markdown.push_str("\n## Final Codex Review\n\n");
        if final_reviews.is_empty() {
            markdown.push_str("No final Codex review output captured.\n");
        } else {
            for result in final_reviews {
                markdown.push_str(&format!("- `{}`: `{}`\n", result.bug_id, result.status));
            }
        }
    }

    atomic_write(&output_dir.join("BUG_REPORT.md"), markdown.as_bytes())?;
    atomic_write(
        &output_dir.join("verified-findings.json"),
        serde_json::to_string_pretty(&all_verified)?.as_bytes(),
    )?;
    Ok(())
}

fn prepare_output_dir(repo_root: &Path, output_dir: &Path) -> Result<Option<PathBuf>> {
    if !output_dir.exists() {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        return Ok(None);
    }
    if !output_dir.is_dir() {
        bail!(
            "bug output path {} is not a directory",
            output_dir.display()
        );
    }

    let has_contents = fs::read_dir(output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
        .next()
        .transpose()?
        .is_some();
    let archived = if has_contents {
        let snapshot_root = repo_root.join(".auto").join("fresh-input").join(format!(
            "{}-previous-{}",
            output_dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("bug"),
            timestamp_slug()
        ));
        copy_tree(output_dir, &snapshot_root).with_context(|| {
            format!(
                "failed to archive existing bug output from {} into {}",
                output_dir.display(),
                snapshot_root.display()
            )
        })?;
        Some(snapshot_root)
    } else {
        None
    };

    fs::remove_dir_all(output_dir)
        .with_context(|| format!("failed to clear {}", output_dir.display()))?;
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to recreate {}", output_dir.display()))?;
    Ok(archived)
}

pub(crate) fn prepare_bug_output_dir(
    repo_root: &Path,
    output_dir: &Path,
    resume: bool,
) -> Result<(Option<PathBuf>, bool)> {
    if !resume {
        return Ok((prepare_output_dir(repo_root, output_dir)?, false));
    }

    if !output_dir.exists() {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        return Ok((None, false));
    }
    if !output_dir.is_dir() {
        bail!(
            "bug output path {} is not a directory",
            output_dir.display()
        );
    }

    let has_contents = fs::read_dir(output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
        .next()
        .transpose()?
        .is_some();
    Ok((None, has_contents))
}
