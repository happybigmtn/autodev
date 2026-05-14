//! Receipt anchors that survive sibling commits.
//!
//! Receipts that embed only a file SHA fall out of sync the moment an
//! unrelated commit touches the same path (autonomy's parallel-host queue
//! sync loop produced 475/841 of recent commits this way). The drift then
//! demotes `[x]` rows in `IMPLEMENTATION_PLAN.md` and we spend cycles
//! re-running verification.
//!
//! [`ReceiptAnchor`] captures both the HEAD commit at the time the receipt
//! was written and a stable content hash of the receipt's owned paths.
//! Verification prefers the commit anchor (Match) but accepts a
//! ContentMatch when HEAD has moved while the owned files are still
//! byte-identical. Drift is reserved for the case where the content
//! actually changed.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Commit footer key for the commit-graph anchor.
pub(crate) const RECEIPT_ANCHOR_COMMIT_KEY: &str = "Receipt-Anchor-Commit:";
/// Commit footer key for the stable content anchor.
pub(crate) const RECEIPT_ANCHOR_CONTENT_KEY: &str = "Receipt-Anchor-Content:";
/// Commit footer key for the owned-path list.
pub(crate) const RECEIPT_ANCHOR_PATHS_KEY: &str = "Receipt-Anchor-Paths:";

/// Records what HEAD and what file content the receipt was issued against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReceiptAnchor {
    pub(crate) commit_sha: String,
    pub(crate) content_sha256: String,
    pub(crate) paths: Vec<PathBuf>,
}

/// Verification outcome for a recorded anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnchorStatus {
    /// HEAD is descendant of the anchor commit AND content matches.
    Match,
    /// Content still matches even though HEAD moved (sibling commit touched
    /// some unrelated file or rewrote history without changing ours).
    ContentMatch,
    /// Owned-path content has changed since the receipt was issued.
    Drift,
}

/// Captures the current HEAD plus a stable content hash over `paths`.
///
/// `paths` are interpreted relative to `repo`. Missing paths contribute an
/// explicit `<missing>` token to the digest so deletions register as drift.
pub(crate) fn compute_anchor(repo: &Path, paths: &[PathBuf]) -> Result<ReceiptAnchor> {
    let mut sorted: Vec<PathBuf> = paths.to_vec();
    sorted.sort();
    sorted.dedup();

    let commit_sha = match git_head(repo) {
        Ok(sha) => sha,
        Err(_) => String::new(),
    };
    let content_sha256 = hash_paths(repo, &sorted)?;
    Ok(ReceiptAnchor {
        commit_sha,
        content_sha256,
        paths: sorted,
    })
}

/// Compares the recorded anchor against the current state of `repo`.
pub(crate) fn verify_anchor(repo: &Path, anchor: &ReceiptAnchor) -> AnchorStatus {
    let current_content = match hash_paths(repo, &anchor.paths) {
        Ok(hash) => hash,
        Err(_) => return AnchorStatus::Drift,
    };
    if current_content != anchor.content_sha256 {
        return AnchorStatus::Drift;
    }
    let head_descends = !anchor.commit_sha.is_empty()
        && git_is_ancestor(repo, &anchor.commit_sha, "HEAD").unwrap_or(false);
    if head_descends {
        AnchorStatus::Match
    } else {
        AnchorStatus::ContentMatch
    }
}

/// Renders the anchor into the receipt commit footer.
///
/// Footers are deterministic so verification readers can match either the
/// commit anchor or fall back to the content anchor. Receipts written
/// before this module landed use a literal SHA and parse as ContentMatch
/// by default (see [`parse_footer`]).
pub(crate) fn render_footer(anchor: &ReceiptAnchor) -> String {
    let paths = anchor
        .paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{RECEIPT_ANCHOR_COMMIT_KEY} {commit}\n{RECEIPT_ANCHOR_CONTENT_KEY} {content}\n{RECEIPT_ANCHOR_PATHS_KEY} {paths}",
        commit = anchor.commit_sha,
        content = anchor.content_sha256,
    )
}

/// Parses a commit footer back into an anchor.
///
/// Returns `None` if the footer is missing the required keys. Older
/// footers that only record an artifact SHA (literal `sha256:<hex>` on a
/// line) are recognised as content-only anchors with an empty commit SHA
/// so [`verify_anchor`] reports them as `ContentMatch` when the content is
/// still intact.
pub(crate) fn parse_footer(footer: &str) -> Option<ReceiptAnchor> {
    let mut commit_sha = None;
    let mut content_sha256 = None;
    let mut paths: Vec<PathBuf> = Vec::new();
    for line in footer.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(RECEIPT_ANCHOR_COMMIT_KEY) {
            commit_sha = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix(RECEIPT_ANCHOR_CONTENT_KEY) {
            content_sha256 = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix(RECEIPT_ANCHOR_PATHS_KEY) {
            paths = rest
                .trim()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
        }
    }
    match (commit_sha, content_sha256) {
        (Some(commit), Some(content)) => Some(ReceiptAnchor {
            commit_sha: commit,
            content_sha256: content,
            paths,
        }),
        _ => legacy_artifact_anchor(footer),
    }
}

/// Recognises legacy `sha256: <hex>` footers so they keep verifying.
fn legacy_artifact_anchor(footer: &str) -> Option<ReceiptAnchor> {
    let hex = footer
        .lines()
        .find_map(|line| line.trim().strip_prefix("sha256:"))?
        .trim()
        .to_string();
    if hex.is_empty() {
        return None;
    }
    Some(ReceiptAnchor {
        commit_sha: String::new(),
        content_sha256: hex,
        paths: Vec::new(),
    })
}

fn git_head(repo: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo.display()))?;
    if !output.status.success() {
        bail!(
            "git rev-parse HEAD failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo.display()))?;
    Ok(output.status.success())
}

fn hash_paths(repo: &Path, paths: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    for path in paths {
        let display = path.display().to_string();
        hasher.update(display.as_bytes());
        hasher.update([0u8]);
        let full = repo.join(path);
        match std::fs::read(&full) {
            Ok(bytes) => {
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
            Err(_) => {
                hasher.update(b"<missing>");
            }
        }
        hasher.update([0xffu8]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_repo(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-receipts-{label}-{nanos}"))
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("failed to launch git");
        assert!(status.success(), "git {:?} failed", args);
    }

    fn init_repo(label: &str) -> std::path::PathBuf {
        let repo = unique_repo(label);
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Autodev Test"]);
        repo
    }

    #[test]
    fn footer_round_trip() {
        let anchor = ReceiptAnchor {
            commit_sha: "abc123".to_string(),
            content_sha256: "deadbeef".to_string(),
            paths: vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")],
        };
        let footer = render_footer(&anchor);
        let parsed = parse_footer(&footer).expect("footer should parse");
        assert_eq!(parsed, anchor);
    }

    #[test]
    fn legacy_sha_footer_parses_as_content_only_anchor() {
        let footer = "Some other line\nsha256: f00dface\n";
        let parsed = parse_footer(footer).expect("legacy sha line should parse");
        assert!(parsed.commit_sha.is_empty());
        assert_eq!(parsed.content_sha256, "f00dface");
    }

    #[test]
    fn compute_and_verify_match() {
        let repo = init_repo("match");
        fs::write(repo.join("a.txt"), b"hello").expect("write");
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-m", "init"]);

        let anchor =
            compute_anchor(&repo, &[PathBuf::from("a.txt")]).expect("compute should succeed");
        assert_eq!(verify_anchor(&repo, &anchor), AnchorStatus::Match);

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[test]
    fn verify_reports_content_match_when_head_moved_but_file_unchanged() {
        let repo = init_repo("content-match");
        fs::write(repo.join("owned.txt"), b"payload").expect("write");
        fs::write(repo.join("sibling.txt"), b"first").expect("write");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);

        let anchor =
            compute_anchor(&repo, &[PathBuf::from("owned.txt")]).expect("compute should succeed");

        // A sibling commit rewrites an unrelated file; HEAD advances but
        // our owned content is untouched.
        fs::write(repo.join("sibling.txt"), b"second").expect("write");
        git(&repo, &["add", "sibling.txt"]);
        git(&repo, &["commit", "-m", "sibling churn"]);
        // Force HEAD to a divergent commit that is NOT a descendant of the
        // original anchor commit by resetting to the parent and committing
        // a new sibling change.
        let anchor_commit = anchor.commit_sha.clone();
        git(&repo, &["reset", "--hard", &anchor_commit]);
        fs::write(repo.join("sibling.txt"), b"divergent").expect("write");
        git(&repo, &["add", "sibling.txt"]);
        git(&repo, &["commit", "--amend", "-m", "rewrite parent"]);

        assert_eq!(verify_anchor(&repo, &anchor), AnchorStatus::ContentMatch);

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[test]
    fn verify_reports_drift_when_content_changes() {
        let repo = init_repo("drift");
        fs::write(repo.join("a.txt"), b"hello").expect("write");
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-m", "init"]);

        let anchor =
            compute_anchor(&repo, &[PathBuf::from("a.txt")]).expect("compute should succeed");
        fs::write(repo.join("a.txt"), b"changed").expect("write");

        assert_eq!(verify_anchor(&repo, &anchor), AnchorStatus::Drift);

        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[test]
    fn missing_path_registers_as_drift() {
        let repo = init_repo("missing");
        fs::write(repo.join("a.txt"), b"hello").expect("write");
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-m", "init"]);

        let anchor =
            compute_anchor(&repo, &[PathBuf::from("a.txt")]).expect("compute should succeed");
        fs::remove_file(repo.join("a.txt")).expect("remove");

        assert_eq!(verify_anchor(&repo, &anchor), AnchorStatus::Drift);

        fs::remove_dir_all(&repo).expect("cleanup");
    }
}
