//! Mechanical pre-pass for the audit `init-context` phase.
//!
//! Background: the LLM-driven init-context stage was burning ~12 min and
//! ~2.6M tokens per run just to refresh `audit-archive/<RUN_ID>`,
//! `audit-everything/<RUN_ID>`, and `super/<RUN_ID>` pointers across
//! AGENTS.md / ARCHITECTURE.md / CONTEXT.md (plus README.md when present).
//! That work is mechanical -- it's `sed`, not reasoning. This module does
//! the substitutions up front so the LLM only sees true ambiguity.
//!
//! Substitutions are deliberately conservative:
//!   - Only replace the three known archive/audit/super prefixes.
//!   - Only touch a file if at least one substitution applies.
//!   - Write atomically so a crash mid-rewrite leaves the file consistent.
//!
//! The auto-discovery helper `run_id_from_archive_dir()` reads the canonical
//! archive root (see `gc_command::archive_root_for`) and returns the
//! lexicographically newest run-id under it -- timestamp-prefixed IDs sort
//! the same as their wall-clock order, which is the contract those IDs ship
//! with.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::gc_command::archive_root_for;
use crate::util::atomic_write;

/// Files we sweep for run-id pointers. Skipped silently when absent.
const TARGET_FILES: &[&str] = &["AGENTS.md", "ARCHITECTURE.md", "CONTEXT.md", "README.md"];

/// Path prefixes that take an `<OLD>` -> `<NEW>` segment swap.
const PREFIXES: &[&str] = &["audit-archive/", "audit-everything/", "super/"];

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InitContextRefresh {
    pub(crate) files_touched: Vec<PathBuf>,
    pub(crate) substitutions: Vec<(String, String)>,
}

impl InitContextRefresh {
    pub(crate) fn is_empty(&self) -> bool {
        self.files_touched.is_empty()
    }
}

/// Sweep the canonical doctrine files and rewrite stale run-id pointers to
/// the new run id. If `old_run_id` is `None` we still update bare
/// `audit-everything/...` and `super/...` references *and* the
/// `audit-archive/...` references that already match the auto-discovered
/// previous archived run, but we never invent an `old` value.
pub(crate) fn mechanical_refresh(
    repo: &Path,
    old_run_id: Option<&str>,
    new_run_id: &str,
) -> Result<InitContextRefresh> {
    let resolved_old = old_run_id
        .map(|s| s.to_string())
        .or_else(|| run_id_from_archive_dir(repo));

    let mut substitutions: Vec<(String, String)> = Vec::new();
    if let Some(old) = resolved_old.as_deref() {
        if old != new_run_id {
            for prefix in PREFIXES {
                substitutions.push((
                    format!("{prefix}{old}"),
                    format!("{prefix}{new_run_id}"),
                ));
            }
        }
    }

    // Structured "Latest audit:" date lines: rewrite the trailing token.
    // We only touch lines that look like "Latest audit: <token>" with no
    // intervening punctuation, to avoid mangling prose. Captured as a
    // pseudo-substitution recorded after a successful line rewrite.
    let mut files_touched: Vec<PathBuf> = Vec::new();
    let mut recorded_extra: Vec<(String, String)> = Vec::new();

    for name in TARGET_FILES {
        let path = repo.join(name);
        if !path.is_file() {
            continue;
        }
        let original = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let mut updated = original.clone();
        for (from, to) in &substitutions {
            if updated.contains(from) {
                updated = updated.replace(from, to);
            }
        }
        // Per-line "Latest audit:" rewrites.
        let mut line_buf: Vec<String> = Vec::with_capacity(updated.lines().count() + 1);
        let mut line_changed = false;
        for line in updated.lines() {
            if let Some(rewritten) = rewrite_latest_audit_line(line, new_run_id) {
                if rewritten != line {
                    recorded_extra.push((line.to_string(), rewritten.clone()));
                    line_changed = true;
                }
                line_buf.push(rewritten);
            } else {
                line_buf.push(line.to_string());
            }
        }
        if line_changed {
            let trailing_newline = updated.ends_with('\n');
            updated = line_buf.join("\n");
            if trailing_newline {
                updated.push('\n');
            }
        }
        if updated != original {
            atomic_write(&path, updated.as_bytes())?;
            files_touched.push(path);
        }
    }

    substitutions.extend(recorded_extra);
    Ok(InitContextRefresh {
        files_touched,
        substitutions,
    })
}

fn rewrite_latest_audit_line(line: &str, new_run_id: &str) -> Option<String> {
    // Match patterns like "Latest audit: <token>" or "- Latest audit: <token>"
    // where `<token>` is one whitespace-delimited word (no commas, no prose).
    let trimmed_start = line.trim_start();
    let lead_len = line.len() - trimmed_start.len();
    let prefix_markers = ["Latest audit:", "Latest run:", "Last audit:"];
    for marker in prefix_markers {
        if let Some(rest) = trimmed_start.strip_prefix(marker) {
            // After the marker we expect optional whitespace then a token.
            let rest_trimmed = rest.trim_start();
            // If there's prose (commas, "as of", etc.) we leave the line alone.
            if rest_trimmed.contains(',') || rest_trimmed.contains(" - ") {
                return Some(line.to_string());
            }
            let token = rest_trimmed.split_whitespace().next().unwrap_or("");
            if token.is_empty() {
                return Some(line.to_string());
            }
            // Preserve trailing content (e.g. backticks or markdown bold)
            // by only replacing the first whitespace-delimited token.
            let replaced_rest = rest_trimmed.replacen(token, new_run_id, 1);
            let leading_ws = " ".repeat(rest.len() - rest_trimmed.len()).max(" ".to_string());
            let _ = leading_ws; // markers all expect a single space after ":"
            let rebuilt = format!(
                "{lead}{marker} {body}",
                lead = &line[..lead_len],
                marker = marker,
                body = replaced_rest
            );
            return Some(rebuilt);
        }
    }
    None
}

/// Read the canonical archive root for `repo` and return the
/// lexicographically newest run-id under it. Returns `None` if the archive
/// root does not exist, is empty, or none of its entries are directories.
pub(crate) fn run_id_from_archive_dir(repo: &Path) -> Option<String> {
    let archive_root = archive_root_for(repo)?;
    if !archive_root.is_dir() {
        return None;
    }
    let mut newest: Option<String> = None;
    for entry in fs::read_dir(&archive_root).ok()? {
        let entry = entry.ok()?;
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        newest = Some(match newest {
            Some(prior) if prior >= name => prior,
            _ => name,
        });
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use std::sync::atomic::{AtomicU64, Ordering};
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn tempdir() -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let base = std::env::temp_dir().join(format!(
            "autodev-init-context-{}-{nanos}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&base).expect("mkdir tempdir");
        base
    }

    fn cleanup(p: &Path) {
        let _ = fs::remove_dir_all(p);
    }

    #[test]
    fn init_context_mechanical_refresh_substitutes_across_files() {
        let dir = tempdir();
        fs::write(
            dir.join("AGENTS.md"),
            "See audit-archive/old-run-id and audit-everything/old-run-id for context.\n",
        )
        .unwrap();
        fs::write(
            dir.join("ARCHITECTURE.md"),
            "Super run lives under super/old-run-id.\n",
        )
        .unwrap();
        let refresh = mechanical_refresh(&dir, Some("old-run-id"), "new-run-id")
            .expect("refresh ok");
        assert_eq!(refresh.files_touched.len(), 2);
        let agents = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(agents.contains("audit-archive/new-run-id"));
        assert!(agents.contains("audit-everything/new-run-id"));
        assert!(!agents.contains("old-run-id"));
        let arch = fs::read_to_string(dir.join("ARCHITECTURE.md")).unwrap();
        assert!(arch.contains("super/new-run-id"));
        cleanup(&dir);
    }

    #[test]
    fn init_context_mechanical_refresh_is_noop_when_no_matches() {
        let dir = tempdir();
        let body = "This file has no run-id pointers.\n";
        fs::write(dir.join("AGENTS.md"), body).unwrap();
        let refresh =
            mechanical_refresh(&dir, Some("old-run-id"), "new-run-id").expect("refresh ok");
        assert!(refresh.is_empty());
        // File untouched.
        let after = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert_eq!(after, body);
        cleanup(&dir);
    }

    #[test]
    fn init_context_run_id_from_archive_picks_newest() {
        let dir = tempdir();
        // Synthesize an autonomy-style archive layout.
        let archive_root = dir.join("ops/evidence/audit-archive");
        fs::create_dir_all(archive_root.join("20250101-000000")).unwrap();
        fs::create_dir_all(archive_root.join("20260101-120000")).unwrap();
        fs::create_dir_all(archive_root.join("20251231-235959")).unwrap();
        let newest = run_id_from_archive_dir(&dir).expect("expected newest run id");
        assert_eq!(newest, "20260101-120000");
        cleanup(&dir);
    }

    #[test]
    fn init_context_run_id_from_archive_returns_none_when_empty() {
        let dir = tempdir();
        // No archive dir at all -> None.
        assert!(run_id_from_archive_dir(&dir).is_none());
        cleanup(&dir);
    }

    #[test]
    fn init_context_integration_updates_agents_md_with_old_archive() {
        let dir = tempdir();
        // Seed an autonomy-style archive so auto-discovery can work even if
        // the caller does not pass `old_run_id` explicitly.
        let archive_root = dir.join("ops/evidence/audit-archive");
        fs::create_dir_all(archive_root.join("20260101-000000")).unwrap();
        fs::write(
            dir.join("AGENTS.md"),
            "Latest audit lives at audit-archive/20260101-000000/.\n",
        )
        .unwrap();
        let refresh = mechanical_refresh(&dir, None, "20260513-120000").expect("refresh ok");
        assert_eq!(refresh.files_touched.len(), 1);
        let after = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(after.contains("audit-archive/20260513-120000"));
        assert!(!after.contains("20260101-000000"));
        cleanup(&dir);
    }

    #[test]
    fn init_context_latest_audit_line_rewrite() {
        let line = "Latest audit: 20250101-000000";
        let rewritten = rewrite_latest_audit_line(line, "20260513-120000").unwrap();
        assert_eq!(rewritten, "Latest audit: 20260513-120000");
    }

    #[test]
    fn init_context_latest_audit_line_with_prose_left_alone() {
        let line = "Latest audit: 20250101-000000, see notes below.";
        let rewritten = rewrite_latest_audit_line(line, "20260513-120000").unwrap();
        assert_eq!(rewritten, line);
    }
}
