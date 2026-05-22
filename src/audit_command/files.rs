//! File enumeration, glob matching, prompt construction, and hashing helpers
//! for `auto audit`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::audit_command::BUNDLED_RUBRIC;
use crate::util::git_stdout;

pub(crate) fn enumerate_tracked_files(
    repo_root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<Vec<String>> {
    let listing = git_stdout(repo_root, ["ls-files", "-z"])?;
    let mut files: Vec<String> = listing
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    files.retain(|path| {
        matches_any(path, include) && !matches_any(path, exclude) && repo_root.join(path).exists()
    });
    files.sort();
    files.dedup();
    Ok(files)
}

pub(crate) fn matches_any(path: &str, globs: &[String]) -> bool {
    globs.iter().any(|pat| glob_match(pat, path))
}

/// Minimal glob matcher supporting `*`, `**`, and literal components. Good
/// enough for path filters without pulling in the `glob` crate.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    glob_match_recursive(pattern.as_bytes(), path.as_bytes())
}

fn glob_match_recursive(pattern: &[u8], path: &[u8]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    // `**` matches any (possibly empty) sequence of characters including `/`.
    if pattern.starts_with(b"**/") {
        let rest = &pattern[3..];
        for i in 0..=path.len() {
            if glob_match_recursive(rest, &path[i..]) {
                return true;
            }
            if path.get(i) == Some(&b'/') {
                // continue scanning; `**/` happy to skip across `/`
            }
        }
        return false;
    }
    if pattern == b"**" {
        return true;
    }
    match pattern[0] {
        b'*' => {
            let rest = &pattern[1..];
            for i in 0..=path.len() {
                // `*` does not match `/` by POSIX glob convention
                if i > 0 && path[i - 1] == b'/' {
                    return glob_match_recursive(rest, &path[i - 1..]);
                }
                if glob_match_recursive(rest, &path[i..]) {
                    return true;
                }
            }
            false
        }
        _ => {
            if path.is_empty() {
                return false;
            }
            if pattern[0] != path[0] {
                return false;
            }
            glob_match_recursive(&pattern[1..], &path[1..])
        }
    }
}

pub(crate) fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

pub(crate) fn file_artifact_dir(output_dir: &Path, rel_path: &str) -> PathBuf {
    let hash = sha256_hex(rel_path.as_bytes());
    output_dir.join("files").join(&hash[..16])
}

pub(crate) fn build_file_prompt(
    repo_root: &Path,
    abs_path: &Path,
    doctrine: &str,
    rubric: &str,
    output_dir: &Path,
    rel_path: &str,
) -> Result<String> {
    let content = std::fs::read_to_string(abs_path).with_context(|| {
        format!(
            "failed to read {} (binary file? pass --exclude to skip)",
            abs_path.display()
        )
    })?;
    let file_dir = file_artifact_dir(output_dir, rel_path);
    let file_dir_rel = file_dir
        .strip_prefix(repo_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| file_dir.display().to_string());
    Ok(format!(
        r#"{rubric}

---

# Doctrine (operator-authored)

{doctrine}

---

# File under audit

Path: `{rel_path}`

Artifact directory for your outputs: `{file_dir_rel}`

```
{content}
```
"#,
        rubric = rubric,
        doctrine = doctrine,
        rel_path = rel_path,
        file_dir_rel = file_dir_rel,
        content = content,
    ))
}

pub(crate) fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

pub(crate) fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

pub(crate) fn literal_git_pathspec(pathspec: &str) -> String {
    format!(":(literal){pathspec}")
}

pub(crate) fn repo_relative_pathspec(repo_root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(repo_root).with_context(|| {
        format!(
            "audit commit path {} is outside repo {}",
            path.display(),
            repo_root.display()
        )
    })?;
    let pathspec = relative.to_string_lossy().replace('\\', "/");
    if pathspec.is_empty() {
        bail!("audit commit path {} resolved to repo root", path.display());
    }
    Ok(pathspec)
}

/// Resolve the bundled or operator-supplied rubric text.
pub(crate) fn resolve_rubric(repo_root: &Path, rubric_prompt: Option<&Path>) -> Result<String> {
    match rubric_prompt {
        Some(path) => {
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                repo_root.join(path)
            };
            std::fs::read_to_string(&resolved)
                .with_context(|| format!("failed to read {}", resolved.display()))
        }
        None => Ok(BUNDLED_RUBRIC.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn glob_match_handles_double_star_prefix() {
        assert!(glob_match("**/target/**", "foo/target/bar"));
        assert!(glob_match("**/target/**", "target/bar"));
        assert!(!glob_match("**/target/**", "foo/barfoo/bar"));
    }

    #[test]
    fn glob_match_handles_extension_wildcard() {
        assert!(glob_match("**/*.rs", "src/lib.rs"));
        assert!(glob_match("**/*.rs", "foo/bar/baz.rs"));
        assert!(!glob_match("**/*.rs", "src/lib.py"));
    }

    #[test]
    fn glob_match_handles_literal_path() {
        assert!(glob_match("AGENTS.md", "AGENTS.md"));
        assert!(!glob_match("AGENTS.md", "foo/AGENTS.md"));
    }
}
