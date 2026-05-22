//! Audit-manifest matching: unresolved findings within a task's owned paths.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::completion_artifacts::verification::backtick_fragments;
use crate::task_parser::{task_field_body_until_any, TASK_FIELD_BOUNDARIES};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct AuditManifest {
    #[serde(default)]
    files: Vec<AuditManifestFile>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct AuditManifestFile {
    path: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    verdict: String,
}

pub(crate) fn unresolved_owned_audit_findings(
    repo_root: &Path,
    task_id: &str,
    task_markdown: &str,
) -> Vec<String> {
    if !task_id.starts_with("AUD-") {
        return Vec::new();
    }
    let manifest_path = repo_root.join("audit/MANIFEST.json");
    if !manifest_path.exists() {
        return Vec::new();
    }
    let owned_patterns = audit_owned_path_patterns(task_markdown);
    if owned_patterns.is_empty() {
        return Vec::new();
    }

    let manifest_text = match fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(err) => {
            return vec![format!(
                "failed to read `{}`: {err}",
                manifest_path.display()
            )]
        }
    };
    let manifest = match serde_json::from_str::<AuditManifest>(&manifest_text) {
        Ok(manifest) => manifest,
        Err(err) => return vec![format!("invalid `{}`: {err}", manifest_path.display())],
    };

    let mut unresolved = manifest
        .files
        .into_iter()
        .filter(audit_manifest_file_is_unresolved)
        .filter(|file| {
            owned_patterns
                .iter()
                .any(|pattern| audit_owned_pattern_matches(pattern, &file.path))
        })
        .map(|file| format!("{} {} ({})", file.verdict, file.path, file.status))
        .collect::<Vec<_>>();
    unresolved.sort();
    unresolved
}

fn audit_manifest_file_is_unresolved(file: &AuditManifestFile) -> bool {
    matches!(
        file.verdict.as_str(),
        "DRIFT-LARGE" | "DRIFT-SMALL" | "REFACTOR" | "RETIRE"
    ) || matches!(file.status.as_str(), "ApplyFailed" | "Escalated")
}

fn audit_owned_path_patterns(task_markdown: &str) -> Vec<String> {
    let Some(body) = task_field_body_until_any(task_markdown, "Owns:", TASK_FIELD_BOUNDARIES)
    else {
        return Vec::new();
    };

    let mut patterns = Vec::new();
    for fragment in body.lines().flat_map(backtick_fragments) {
        if audit_owned_token_looks_path_like(&fragment) {
            patterns.push(normalize_audit_owned_pattern(&fragment));
        }
    }
    if patterns.is_empty() {
        for token in body
            .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == ';')
            .map(|token| token.trim_matches(|ch: char| "`:.()[]".contains(ch)))
            .filter(|token| audit_owned_token_looks_path_like(token))
        {
            patterns.push(normalize_audit_owned_pattern(token));
        }
    }
    patterns.sort();
    patterns.dedup();
    patterns
}

fn audit_owned_token_looks_path_like(token: &str) -> bool {
    let token = token.trim();
    !token.is_empty()
        && (token.contains('/')
            || token.contains('*')
            || token.ends_with(".md")
            || token.ends_with(".rs")
            || token.ends_with(".ts")
            || token.ends_with(".tsx")
            || token == "AGENTS.md"
            || token == "WORKLIST.md"
            || token == "IMPLEMENTATION_PLAN.md"
            || token == "REVIEW.md")
}

fn normalize_audit_owned_pattern(pattern: &str) -> String {
    pattern
        .trim()
        .trim_matches('`')
        .trim_start_matches("./")
        .trim_matches(|ch: char| ch == ',' || ch == ';')
        .to_string()
}

fn audit_owned_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./");
    let path = path.trim_start_matches("./");
    if pattern == path {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if let Some(prefix) = pattern.strip_suffix("/**/*") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if let Some(prefix) = pattern.strip_suffix('/') {
        return path.starts_with(&format!("{prefix}/"));
    }
    if !pattern.contains('*') {
        return false;
    }
    wildcard_match(pattern.as_bytes(), path.as_bytes())
}

fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star = None::<usize>;
    let mut match_after_star = 0usize;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            match_after_star = t;
            p += 1;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            match_after_star += 1;
            t = match_after_star;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

pub(crate) fn summarize_unresolved_audit_findings(findings: &[String]) -> String {
    const MAX_RENDERED: usize = 8;
    let mut rendered = findings
        .iter()
        .take(MAX_RENDERED)
        .map(|finding| format!("`{finding}`"))
        .collect::<Vec<_>>();
    if findings.len() > MAX_RENDERED {
        rendered.push(format!("... and {} more", findings.len() - MAX_RENDERED));
    }
    rendered.join(", ")
}
