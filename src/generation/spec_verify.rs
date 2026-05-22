//! Validation, normalization, and cross-spec lints for generated spec
//! snapshots.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::generation::markdown::{
    markdown_section_body_bounds, split_markdown_section, strip_ordered_list_marker,
};
use crate::generation::prompts::{REQUIRED_SPEC_SECTIONS, SPEC_ACCEPTANCE_CRITERIA_HEADER};
use crate::generation::GeneratedSpecDocument;
use crate::util::{atomic_write, list_markdown_files};

pub(crate) fn verify_generated_specs(output_dir: &Path) -> Result<Vec<GeneratedSpecDocument>> {
    let specs_dir = output_dir.join("specs");
    if !specs_dir.is_dir() {
        bail!("spec generation did not write {}", specs_dir.display());
    }
    let specs = list_markdown_files(&specs_dir)?;
    if specs.is_empty() {
        bail!(
            "spec generation did not write any markdown files under {}",
            specs_dir.display()
        );
    }
    let mut docs = Vec::new();
    for spec in &specs {
        let original = fs::read_to_string(spec)
            .with_context(|| format!("failed to read {}", spec.display()))?;
        let normalized = normalize_generated_spec_markdown(&original);
        if normalized != original {
            atomic_write(spec, normalized.as_bytes())
                .with_context(|| format!("failed to normalize {}", spec.display()))?;
        }
        if !normalized.starts_with("# Specification:") {
            bail!(
                "generated spec {} must start with `# Specification:`",
                spec.display()
            );
        }
        for section in REQUIRED_SPEC_SECTIONS {
            if !generated_spec_has_section(&normalized, section) {
                bail!(
                    "generated spec {} must include `{}`",
                    spec.display(),
                    section
                );
            }
        }
        if !generated_spec_has_acceptance_criteria(&normalized) {
            bail!(
                "generated spec {} must include `{}` with at least one bullet",
                spec.display(),
                SPEC_ACCEPTANCE_CRITERIA_HEADER
            );
        }
        docs.push(GeneratedSpecDocument {
            path: spec.clone(),
            text: normalized,
        });
    }
    lint_generated_spec_set(&docs)?;
    Ok(docs)
}

fn generated_spec_has_section(markdown: &str, header: &str) -> bool {
    split_markdown_section(markdown, header)
        .map(|(_, body)| !body.trim().is_empty())
        .unwrap_or(false)
}

pub(crate) fn generated_spec_has_acceptance_criteria(markdown: &str) -> bool {
    let Some((_, section_body)) = split_markdown_section(markdown, SPEC_ACCEPTANCE_CRITERIA_HEADER)
    else {
        return false;
    };

    section_body.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- ") || trimmed.starts_with("* ")
    }) || acceptance_criteria_has_structured_items(section_body)
}

fn acceptance_criteria_has_structured_items(section_body: &str) -> bool {
    let mut saw_heading = false;
    let mut saw_body = false;

    for line in section_body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("### ") {
            if saw_heading && saw_body {
                return true;
            }
            saw_heading = true;
            saw_body = false;
            continue;
        }
        if saw_heading && !trimmed.is_empty() && !trimmed.starts_with("## ") {
            saw_body = true;
        }
    }

    saw_heading && saw_body
}

pub(crate) fn normalize_generated_spec_markdown(markdown: &str) -> String {
    normalize_ordered_acceptance_list(markdown)
}

fn normalize_ordered_acceptance_list(markdown: &str) -> String {
    let Some((body_start, section_end)) =
        markdown_section_body_bounds(markdown, SPEC_ACCEPTANCE_CRITERIA_HEADER)
    else {
        return markdown.to_string();
    };
    let section_body = &markdown[body_start..section_end];
    let normalized_body = normalize_ordered_list_to_bullets(section_body);
    if normalized_body == section_body {
        return markdown.to_string();
    }

    let mut normalized = String::with_capacity(markdown.len() + 16);
    normalized.push_str(&markdown[..body_start]);
    normalized.push_str(&normalized_body);
    normalized.push_str(&markdown[section_end..]);
    normalized
}

fn normalize_ordered_list_to_bullets(section_body: &str) -> String {
    let mut normalized = String::with_capacity(section_body.len());
    for raw_line in section_body.split_inclusive('\n') {
        let (line, newline) = raw_line
            .strip_suffix('\n')
            .map(|line| (line, "\n"))
            .unwrap_or((raw_line, ""));
        let trimmed = line.trim_start();
        if let Some(content) = strip_ordered_list_marker(trimmed) {
            let indent_len = line.len().saturating_sub(trimmed.len());
            normalized.push_str(&line[..indent_len]);
            normalized.push_str("- ");
            normalized.push_str(content.trim_start());
            normalized.push_str(newline);
        } else {
            normalized.push_str(raw_line);
        }
    }
    normalized
}

fn lint_generated_spec_set(specs: &[GeneratedSpecDocument]) -> Result<()> {
    lint_duplicate_spec_topics(specs)?;
    lint_signature_policy_consistency(specs)?;
    lint_session_resume_wire_contract(specs)?;
    lint_session_persistence_abort_language(specs)?;
    Ok(())
}

fn lint_duplicate_spec_topics(specs: &[GeneratedSpecDocument]) -> Result<()> {
    let mut seen = std::collections::BTreeMap::<String, &GeneratedSpecDocument>::new();
    for spec in specs {
        let slug = spec
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(spec_topic_slug)
            .context("generated spec must have a file stem")?;
        if let Some(previous) = seen.insert(slug.clone(), spec) {
            bail!(
                "generated specs duplicate the `{}` topic: {} and {}",
                slug,
                previous.path.display(),
                spec.path.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn lint_signature_policy_consistency(
    specs: &[GeneratedSpecDocument],
) -> Result<()> {
    let Some(transcript) = find_generated_spec(specs, "deterministic-transcripts") else {
        return Ok(());
    };
    let Some(adversarial) = find_generated_spec(specs, "adversarial-robustness") else {
        return Ok(());
    };
    let transcript_requires_cosign = transcript.text.contains("requires both signatures")
        || transcript.text.contains("requires both player signatures")
        || transcript
            .text
            .contains("rejects `build()` without both player signatures");
    let adversarial_allows_unsigned = adversarial.text.contains("recorded as unsigned");
    if transcript_requires_cosign && adversarial_allows_unsigned {
        bail!(
            "generated specs disagree about transcript signature policy: {} requires both player signatures, but {} allows unsigned completed transcripts",
            transcript.path.display(),
            adversarial.path.display()
        );
    }
    Ok(())
}

pub(crate) fn lint_session_resume_wire_contract(
    specs: &[GeneratedSpecDocument],
) -> Result<()> {
    let Some(session) = find_generated_spec(specs, "session-persistence") else {
        return Ok(());
    };
    let Some(wire) = find_generated_spec(specs, "wire-protocol") else {
        return Ok(());
    };

    let hello_line = markdown_line_containing(&wire.text, "| `Hello` |").unwrap_or_default();
    if session.text.contains("resume_session") && !hello_line.contains("resume_session") {
        bail!(
            "generated specs disagree about the Hello message: {} extends Hello with `resume_session`, but {} does not include that field",
            session.path.display(),
            wire.path.display()
        );
    }
    if session.text.contains("last_hand_digests") && !hello_line.contains("last_hand_digests") {
        bail!(
            "generated specs disagree about the Hello message: {} extends Hello with `last_hand_digests`, but {} does not include that field",
            session.path.display(),
            wire.path.display()
        );
    }

    let hello_ack_line = markdown_line_containing(&wire.text, "| `HelloAck` |").unwrap_or_default();
    if session.text.contains("HelloAck` with `resumed: true`")
        && !hello_ack_line.contains("resumed")
    {
        bail!(
            "generated specs disagree about HelloAck: {} requires a `resumed` field, but {} does not include it",
            session.path.display(),
            wire.path.display()
        );
    }

    Ok(())
}

fn lint_session_persistence_abort_language(specs: &[GeneratedSpecDocument]) -> Result<()> {
    let Some(session) = find_generated_spec(specs, "session-persistence") else {
        return Ok(());
    };
    if session.text.contains("not silently lost") && session.text.contains("silently aborted") {
        bail!(
            "generated spec {} contradicts itself about in-flight hand recovery: it says hands are not silently lost and also says they are silently aborted",
            session.path.display()
        );
    }
    Ok(())
}

fn find_generated_spec<'a>(
    specs: &'a [GeneratedSpecDocument],
    needle: &str,
) -> Option<&'a GeneratedSpecDocument> {
    specs.iter().find(|doc| {
        doc.path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(|stem| stem.contains(needle))
            .unwrap_or(false)
    })
}

fn markdown_line_containing<'a>(markdown: &'a str, needle: &str) -> Option<&'a str> {
    markdown.lines().find(|line| line.contains(needle))
}

pub(crate) fn spec_topic_slug(source_name: &str) -> String {
    strip_known_prefix(source_name)
        .trim_matches('-')
        .trim()
        .replace('_', "-")
        .to_ascii_lowercase()
}

fn strip_known_prefix(name: &str) -> String {
    let mut value = strip_fixed_numeric_prefix(name);
    if value.len() >= 7
        && value.chars().take(6).all(|ch| ch.is_ascii_digit())
        && value.as_bytes().get(6) == Some(&b'-')
    {
        value = value[7..].to_string();
    }
    value
}

fn strip_fixed_numeric_prefix(name: &str) -> String {
    let bytes = name.as_bytes();
    if bytes.len() > 4 && bytes[0..3].iter().all(u8::is_ascii_digit) && bytes[3] == b'-' {
        name[4..].to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        generated_spec_has_acceptance_criteria, lint_session_resume_wire_contract,
        lint_signature_policy_consistency, normalize_generated_spec_markdown,
    };
    use crate::generation::tests::generated_spec;

    #[test]
    fn detects_acceptance_criteria_section_with_bullets() {
        let spec = r#"# Specification: Example

## Overview

Something.

## Acceptance Criteria

- One
- Two
"#;

        assert!(generated_spec_has_acceptance_criteria(spec));
    }

    #[test]
    fn rejects_acceptance_criteria_section_without_bullets() {
        let spec = r#"# Specification: Example

## Acceptance Criteria

This should be bulletized.
"#;

        assert!(!generated_spec_has_acceptance_criteria(spec));
    }

    #[test]
    fn normalizes_numbered_acceptance_criteria_into_bullets() {
        let spec = r#"# Specification: Example

## Acceptance Criteria

1. One
2. Two

## Verification

- Check
"#;

        let normalized = normalize_generated_spec_markdown(spec);

        assert!(normalized.contains("## Acceptance Criteria\n\n- One\n- Two"));
        assert!(generated_spec_has_acceptance_criteria(&normalized));
    }

    #[test]
    fn accepts_structured_acceptance_items_with_subheadings() {
        let spec = r#"# Specification: Example

## Acceptance Criteria

### AC-01: One

This is a concrete acceptance item.

### AC-02: Two

This is another acceptance item.
"#;

        assert!(generated_spec_has_acceptance_criteria(spec));
    }

    #[test]
    fn rejects_conflicting_signature_policy_specs() {
        let specs = vec![
            generated_spec(
                "deterministic-transcripts",
                "# Specification: Deterministic Transcripts\n\nrequires both signatures\n",
            ),
            generated_spec(
                "adversarial-robustness",
                "# Specification: Adversarial Robustness\n\nrecorded as unsigned\n",
            ),
        ];

        let error =
            lint_signature_policy_consistency(&specs).expect_err("expected signature mismatch");

        assert!(error.to_string().contains("signature policy"));
    }

    #[test]
    fn rejects_session_resume_contract_drift() {
        let specs = vec![
            generated_spec(
                "session-persistence",
                "# Specification: Session Persistence\n\nresume_session\nlast_hand_digests\nHelloAck` with `resumed: true`\n",
            ),
            generated_spec(
                "wire-protocol",
                "# Specification: Wire Protocol\n\n| `Hello` | `session_id` |\n| `HelloAck` | `session_id` |\n",
            ),
        ];

        let error = lint_session_resume_wire_contract(&specs).expect_err("expected Hello mismatch");

        assert!(error.to_string().contains("Hello message"));
    }
}
