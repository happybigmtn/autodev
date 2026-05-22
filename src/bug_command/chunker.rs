//! Repo chunking: group tracked files into bounded audit chunks and write the
//! cheap static pre-index artifacts.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Result};

use crate::bug_command::types::{FileCandidate, RepoChunk};
use crate::util::{atomic_write, git_stdout};

pub(crate) fn collect_repo_chunks(
    repo_root: &Path,
    chunk_size: usize,
    max_chunks: Option<usize>,
) -> Result<Vec<RepoChunk>> {
    if chunk_size == 0 {
        bail!("chunk size must be greater than zero");
    }

    let tracked = git_stdout(repo_root, ["ls-files"])?;
    let mut grouped = BTreeMap::<String, Vec<FileCandidate>>::new();
    for line in tracked.lines() {
        let path = line.trim();
        if path.is_empty() || !should_audit_path(path) {
            continue;
        }
        let scope = top_level_scope(path);
        let candidate = build_file_candidate(repo_root, path);
        grouped.entry(scope).or_default().push(candidate);
    }

    let mut chunks = Vec::new();
    let mut ordinal = 1usize;
    let token_budget = chunk_size.saturating_mul(700).max(700);
    for (scope, mut candidates) in grouped {
        candidates.sort_by(|a, b| {
            b.risk_score
                .cmp(&a.risk_score)
                .then_with(|| a.path.cmp(&b.path))
        });
        let mut current = Vec::<FileCandidate>::new();
        let mut current_tokens = 0usize;
        for candidate in candidates {
            let would_exceed_files = current.len() >= chunk_size;
            let would_exceed_tokens =
                !current.is_empty() && current_tokens + candidate.estimated_tokens > token_budget;
            if would_exceed_files || would_exceed_tokens {
                push_repo_chunk(&mut chunks, &scope, &mut ordinal, current)?;
                if max_chunks.is_some_and(|limit| chunks.len() >= limit) {
                    return Ok(chunks);
                }
                current = Vec::new();
                current_tokens = 0;
            }
            current_tokens += candidate.estimated_tokens;
            current.push(candidate);
        }
        if !current.is_empty() {
            push_repo_chunk(&mut chunks, &scope, &mut ordinal, current)?;
            if max_chunks.is_some_and(|limit| chunks.len() >= limit) {
                return Ok(chunks);
            }
        }
    }
    Ok(chunks)
}

fn push_repo_chunk(
    chunks: &mut Vec<RepoChunk>,
    scope: &str,
    ordinal: &mut usize,
    candidates: Vec<FileCandidate>,
) -> Result<()> {
    if candidates.is_empty() {
        return Ok(());
    }
    let id = format!("chunk-{:03}-{}", *ordinal, slugify(scope));
    let mut files = Vec::new();
    let mut risk_notes = Vec::new();
    for candidate in candidates {
        files.push(candidate.path.clone());
        for note in candidate.risk_notes {
            risk_notes.push(format!("{}: {note}", candidate.path));
        }
    }
    chunks.push(RepoChunk {
        ordinal: *ordinal,
        id,
        scope_label: scope.to_string(),
        files,
        risk_notes,
    });
    *ordinal += 1;
    Ok(())
}

fn build_file_candidate(repo_root: &Path, path: &str) -> FileCandidate {
    let full_path = repo_root.join(path);
    let bytes = fs::read(&full_path).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    let estimated_tokens = (text.len() / 4).max(1);
    let risk_notes = risk_notes_for_file(path, &text);
    FileCandidate {
        path: path.to_string(),
        estimated_tokens,
        risk_score: risk_notes.len(),
        risk_notes,
    }
}

fn risk_notes_for_file(path: &str, text: &str) -> Vec<String> {
    let mut notes = Vec::new();
    let lower_path = path.to_ascii_lowercase();
    if lower_path.contains("auth") || lower_path.contains("token") || lower_path.contains("secret")
    {
        notes.push("auth/credential surface".to_string());
    }
    let patterns = [
        ("unwrap(", "panic-prone unwrap"),
        ("expect(", "panic-prone expect"),
        ("unsafe ", "unsafe block"),
        ("Command::new", "process execution"),
        ("std::process", "process execution"),
        ("fs::write", "filesystem write"),
        ("remove_dir_all", "destructive filesystem operation"),
        ("DELETE ", "destructive database/query operation"),
        ("UPDATE ", "state mutation query"),
        ("TODO", "unfinished TODO"),
        ("FIXME", "unfinished FIXME"),
    ];
    for (needle, label) in patterns {
        if text.contains(needle) {
            notes.push(label.to_string());
        }
    }
    notes.sort();
    notes.dedup();
    notes
}

fn top_level_scope(path: &str) -> String {
    if path.contains('/') {
        path.split('/').next().unwrap_or("root").to_string()
    } else {
        "root".to_string()
    }
}

pub(crate) fn should_audit_path(path: &str) -> bool {
    if path.starts_with(".auto/")
        || path.starts_with("bug/")
        || path.starts_with("nemesis/")
        || path.starts_with("genesis/")
        || path.starts_with("target/")
    {
        return false;
    }
    if path
        .split('/')
        .next()
        .is_some_and(|component| component.starts_with("gen-"))
    {
        return false;
    }

    let lower = path.to_ascii_lowercase();
    let excluded_exts = [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".ico", ".pdf", ".mp4", ".mov", ".zip",
        ".gz", ".tar", ".woff", ".woff2", ".ttf", ".otf", ".mp3", ".wav",
    ];
    !excluded_exts.iter().any(|ext| lower.ends_with(ext))
}

pub(crate) fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

pub(crate) fn write_chunk_manifest(chunk_dir: &Path, chunk: &RepoChunk) -> Result<()> {
    let mut manifest = String::new();
    manifest.push_str("# Bug Audit Chunk\n\n");
    manifest.push_str(&format!("- Chunk: `{}`\n", chunk.id));
    manifest.push_str(&format!("- Scope: `{}`\n", chunk.scope_label));
    manifest.push_str(&format!("- Files: `{}`\n\n", chunk.files.len()));
    manifest.push_str("## Files\n\n");
    for file in &chunk.files {
        manifest.push_str(&format!("- `{file}`\n"));
    }
    if !chunk.risk_notes.is_empty() {
        manifest.push_str("\n## Static Risk Hints\n\n");
        for note in &chunk.risk_notes {
            manifest.push_str(&format!("- {note}\n"));
        }
    }
    atomic_write(&chunk_dir.join("manifest.md"), manifest.as_bytes())
}

pub(crate) fn write_bug_pre_index(output_dir: &Path, chunks: &[RepoChunk]) -> Result<()> {
    let mut markdown = String::new();
    markdown.push_str("# Bug Pre-Index\n\n");
    markdown.push_str("Cheap static hints generated before model audit. These are prioritization hints, not findings.\n\n");
    for chunk in chunks {
        if chunk.risk_notes.is_empty() {
            continue;
        }
        markdown.push_str(&format!("## `{}`\n\n", chunk.id));
        for note in &chunk.risk_notes {
            markdown.push_str(&format!("- {note}\n"));
        }
        markdown.push('\n');
    }
    if !markdown.contains("## `") {
        markdown.push_str("No static risk hints found.\n");
    }
    atomic_write(&output_dir.join("pre-index.md"), markdown.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{collect_repo_chunks, should_audit_path, slugify};

    #[test]
    fn excludes_generated_and_binary_paths() {
        assert!(!should_audit_path(".auto/log.txt"));
        assert!(!should_audit_path("gen-20260403/specs/foo.md"));
        assert!(!should_audit_path("bug/chunks/chunk-001/report.md"));
        assert!(!should_audit_path("assets/logo.png"));
        assert!(should_audit_path("src/main.rs"));
        assert!(should_audit_path("Cargo.toml"));
    }

    #[test]
    fn slugifies_scope_labels() {
        assert_eq!(slugify("src/lib"), "src-lib");
        assert_eq!(slugify("Cargo.toml"), "cargo-toml");
    }

    #[test]
    fn chunk_collection_requires_non_zero_size() {
        let repo_root = Path::new("/tmp");
        let result = collect_repo_chunks(repo_root, 0, None);
        assert!(result.is_err());
    }
}
